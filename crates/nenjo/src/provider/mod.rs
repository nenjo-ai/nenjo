//! Provider — the root object for the Nenjo SDK.
//!
//! Holds the bootstrap manifest, LLM provider factory, tool factory, memory
//! backend, and provider-level knowledge packs. Build manifest-backed agents
//! via [`Provider::agent`], or start a
//! blank agent builder with [`Provider::new_agent`].

pub mod builder;
pub mod error;
pub mod runtime;
pub mod tool_factory;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

pub use crate::routines::RoutineRunner;
pub use builder::ProviderBuilder;
pub use error::ProviderError;
pub use nenjo_models::{ModelProviderFactory, TypedModelProviderFactory};
pub use runtime::{ProviderMemory, ProviderRuntime};
pub use tool_factory::{NoopToolFactory, ToolContext, ToolFactory};

use crate::agents::builder::AgentBuilder;
use crate::agents::prompts::{self as prompts, PromptContext};
use crate::arguments::ResolvedArgumentBinding;
use crate::config::AgentConfig;
use crate::context::ContextRenderer;
use crate::manifest::{
    AbilityManifest, AgentManifest, DomainManifest, HasManifestSlug, KnowledgePackManifest,
    KnowledgePackSource, Manifest, ModelManifest, ProjectManifest,
};
use crate::memory::Memory;
use crate::tools::Tool;
use crate::types::RenderContextVars;
use crate::{IntoSlug, ManifestReader, Slug};
use nenjo_knowledge::tools::{
    KnowledgePackEntry, KnowledgePackSummary, KnowledgeRef, KnowledgeRegistry,
};
use nenjo_knowledge::{
    FilesystemKnowledgePack, KnowledgePack, KnowledgeSearchService, PackageKnowledgePack,
};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// The root object for the Nenjo SDK.
///
/// Created via [`ProviderBuilder`]. Holds the bootstrap manifest and runtime
/// factories. Use [`agent`](Self::agent) for manifest-backed agents, or
/// [`new_agent`](Self::new_agent) when the caller supplies an agent manifest
/// and model explicitly.
pub struct Provider<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> {
    inner: Arc<ProviderInner<ModelFactory, ToolFactoryImpl, Mem>>,
}

/// Compatibility provider with erased model factory, tool factory, and memory
/// backend types.
pub type ErasedProvider =
    Provider<dyn ModelProviderFactory + 'static, dyn ToolFactory + 'static, dyn Memory + 'static>;

impl<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> Clone
    for Provider<ModelFactory, ToolFactoryImpl, Mem>
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

pub(crate) struct ProviderInner<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> {
    manifest: ManifestIndex,
    context_renderer: ContextRenderer,
    services: ProviderServices<ModelFactory, ToolFactoryImpl, Mem>,
}

pub(crate) struct ManifestIndex {
    manifest: Arc<Manifest>,
    agents_by_slug: HashMap<Slug, usize>,
    abilities_by_name: HashMap<String, usize>,
    domains_by_slug: HashMap<Slug, usize>,
    domains_by_command: HashMap<String, usize>,
    models_by_slug: HashMap<Slug, usize>,
    routines_by_slug: HashMap<Slug, usize>,
    projects_by_slug: HashMap<Slug, usize>,
    councils_by_slug: HashMap<Slug, usize>,
}

impl ManifestIndex {
    fn new(manifest: Arc<Manifest>) -> Self {
        Self {
            agents_by_slug: index_by_manifest_slug(&manifest.agents),
            abilities_by_name: index_abilities_by_name(&manifest.abilities),
            domains_by_slug: index_domains_by_slug(&manifest.domains),
            domains_by_command: index_domains_by_command(&manifest.domains),
            models_by_slug: index_by_manifest_slug(&manifest.models),
            routines_by_slug: index_by_manifest_slug(&manifest.routines),
            projects_by_slug: index_by_manifest_slug(&manifest.projects),
            councils_by_slug: index_by_manifest_slug(&manifest.councils),
            manifest,
        }
    }

    fn agent(&self, slug: &Slug) -> Option<&AgentManifest> {
        self.agents_by_slug
            .get(slug)
            .map(|index| &self.manifest.agents[*index])
    }

    fn ability(&self, name: &str) -> Option<&AbilityManifest> {
        self.abilities_by_name
            .get(name)
            .map(|index| &self.manifest.abilities[*index])
    }

    fn domain(&self, selector: &str) -> Option<&DomainManifest> {
        self.domains_by_command
            .get(selector)
            .or_else(|| self.domains_by_slug.get(&Slug::derive(selector)))
            .map(|index| &self.manifest.domains[*index])
    }

    fn model(&self, slug: &Slug) -> Option<&ModelManifest> {
        self.models_by_slug
            .get(slug)
            .map(|index| &self.manifest.models[*index])
    }

    fn routine(&self, slug: &Slug) -> Option<&crate::manifest::RoutineManifest> {
        self.routines_by_slug
            .get(slug)
            .map(|index| &self.manifest.routines[*index])
    }

    fn project(&self, slug: &Slug) -> Option<&ProjectManifest> {
        self.projects_by_slug
            .get(slug)
            .map(|index| &self.manifest.projects[*index])
    }

    fn council(&self, slug: &Slug) -> Option<&crate::manifest::CouncilManifest> {
        self.councils_by_slug
            .get(slug)
            .map(|index| &self.manifest.councils[*index])
    }
}

fn index_by_manifest_slug<T: HasManifestSlug>(items: &[T]) -> HashMap<Slug, usize> {
    let mut index = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        index.entry(item.manifest_slug()).or_insert(position);
    }
    index
}

fn ability_version_candidate(item: &AbilityManifest) -> crate::package_resolve::VersionedCandidate {
    use crate::package_resolve::VersionedCandidate;
    let path = item.path.clone().unwrap_or_default();
    VersionedCandidate {
        package_name: item
            .metadata
            .pointer("/package/name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        package_version: item
            .metadata
            .pointer("/package/version")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| crate::package_resolve::version_label_from_path(&path)),
        path,
        name: item.name.clone(),
    }
}

fn knowledge_pack_version_candidate(
    manifest: &KnowledgePackManifest,
) -> crate::package_resolve::VersionedCandidate {
    use crate::package_resolve::VersionedCandidate;
    let path = manifest
        .root_path
        .as_ref()
        .map(|p| p.display().to_string())
        .filter(|p| !p.is_empty())
        .or_else(|| {
            manifest
                .metadata
                .pointer("/package/module_path")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| manifest.root_uri.clone());
    let name = manifest
        .selector
        .rsplit([':', '.', '/'])
        .next()
        .unwrap_or(manifest.name.as_str())
        .to_string();
    VersionedCandidate {
        package_name: manifest
            .metadata
            .pointer("/package/name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        package_version: manifest.version.clone().or_else(|| {
            manifest
                .metadata
                .pointer("/package/version")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }),
        path,
        name,
    }
}

fn index_abilities_by_name(items: &[AbilityManifest]) -> HashMap<String, usize> {
    use crate::package_resolve::{PkgResolvePolicy, pick_version_winner};
    let mut by_name: HashMap<String, Vec<(usize, crate::package_resolve::VersionedCandidate)>> =
        HashMap::new();
    for (position, item) in items.iter().enumerate() {
        by_name
            .entry(item.name.clone())
            .or_default()
            .push((position, ability_version_candidate(item)));
    }
    let mut index = HashMap::new();
    for (name, group) in by_name {
        if let Some(pos) = pick_version_winner(&group, &PkgResolvePolicy::HighestSemver) {
            index.insert(name, pos);
        }
    }
    index
}

/// Resolve one ability by logical name under a multi-version policy.
pub(crate) fn resolve_ability_by_name<'a>(
    abilities: &'a [AbilityManifest],
    name: &str,
    policy: &crate::package_resolve::PkgResolvePolicy,
) -> Option<&'a AbilityManifest> {
    use crate::package_resolve::pick_version_winner;
    let matching: Vec<(usize, crate::package_resolve::VersionedCandidate)> = abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| ability.name == name)
        .map(|(idx, ability)| (idx, ability_version_candidate(ability)))
        .collect();
    pick_version_winner(&matching, policy).map(|idx| &abilities[idx])
}

/// Resolve a domain by name/slug/command under a multi-version policy.
pub(crate) fn resolve_domain_by_selector<'a>(
    domains: &'a [DomainManifest],
    selector: &str,
    policy: &crate::package_resolve::PkgResolvePolicy,
) -> Option<&'a DomainManifest> {
    use crate::package_resolve::{VersionedCandidate, pick_version_winner};
    let want = Slug::derive(selector);
    let matching: Vec<(usize, VersionedCandidate)> = domains
        .iter()
        .enumerate()
        .filter(|(_, domain)| {
            domain.command == selector
                || domain.slug() == want
                || Slug::derive(&domain.name) == want
                || domain.name == selector
        })
        .map(|(idx, domain)| {
            (
                idx,
                VersionedCandidate {
                    package_name: None,
                    package_version: crate::package_resolve::version_label_from_path(&domain.path),
                    path: domain.path.clone(),
                    name: domain.name.clone(),
                },
            )
        })
        .collect();
    pick_version_winner(&matching, policy).map(|idx| &domains[idx])
}

fn index_domains_by_slug(items: &[DomainManifest]) -> HashMap<Slug, usize> {
    use crate::package_resolve::{VersionedCandidate, prefer_highest_semver};
    // Prefer highest version when multiple domains share a logical slug/name.
    let mut by_key: HashMap<Slug, Vec<(usize, VersionedCandidate)>> = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        let cand = VersionedCandidate {
            package_name: None,
            package_version: crate::package_resolve::version_label_from_path(&item.path),
            path: item.path.clone(),
            name: item.name.clone(),
        };
        by_key
            .entry(item.manifest_slug())
            .or_default()
            .push((position, cand.clone()));
        by_key
            .entry(Slug::derive(&item.name))
            .or_default()
            .push((position, cand));
    }
    let mut index = HashMap::new();
    for (key, group) in by_key {
        if let Some((pos, _)) = prefer_highest_semver(group) {
            index.insert(key, pos);
        }
    }
    index
}

fn index_domains_by_command(items: &[DomainManifest]) -> HashMap<String, usize> {
    use crate::package_resolve::{VersionedCandidate, prefer_highest_semver};
    let mut by_cmd: HashMap<String, Vec<(usize, VersionedCandidate)>> = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        let cand = VersionedCandidate {
            package_name: None,
            package_version: crate::package_resolve::version_label_from_path(&item.path),
            path: item.path.clone(),
            name: item.name.clone(),
        };
        by_cmd
            .entry(item.command.clone())
            .or_default()
            .push((position, cand));
    }
    let mut index = HashMap::new();
    for (cmd, group) in by_cmd {
        if let Some((pos, _)) = prefer_highest_semver(group) {
            index.insert(cmd, pos);
        }
    }
    index
}

fn manifest_knowledge_pack_entry(manifest: &KnowledgePackManifest) -> Option<KnowledgePackEntry> {
    match manifest.source_type {
        KnowledgePackSource::Library | KnowledgePackSource::Local => {
            let knowledge_ref = filesystem_manifest_knowledge_ref(manifest)?;
            let root = manifest.root_path.as_ref().or_else(|| {
                warn!(
                    selector = %manifest.selector,
                    "Skipping filesystem knowledge pack without root_path"
                );
                None
            })?;
            let pack = FilesystemKnowledgePack::load(root).or_else(|| {
                warn!(
                    selector = %manifest.selector,
                    path = %root.display(),
                    "Skipping unreadable filesystem knowledge pack"
                );
                None
            })?;
            let writable =
                matches!(manifest.source_type, KnowledgePackSource::Library) && !manifest.read_only;
            Some(
                KnowledgePackEntry::new(knowledge_ref, pack)
                    .with_writable(writable)
                    .with_metadata(manifest.name.clone(), manifest.description.clone()),
            )
        }
        KnowledgePackSource::Package => {
            let root = manifest.root_path.as_ref().or_else(|| {
                warn!(
                    selector = %manifest.selector,
                    "Skipping package knowledge pack without root_path"
                );
                None
            })?;
            let version = manifest.version.as_deref().unwrap_or("1");
            let pack = PackageKnowledgePack::load(root, version)
                .map_err(|error| {
                    warn!(
                        selector = %manifest.selector,
                        path = %root.display(),
                        error = %error,
                        "Skipping unreadable package knowledge pack"
                    );
                })
                .ok()?;
            let knowledge_ref = package_manifest_knowledge_ref(manifest, &pack)?;
            Some(
                KnowledgePackEntry::new(knowledge_ref, pack)
                    .with_writable(false)
                    .with_metadata(manifest.name.clone(), manifest.description.clone()),
            )
        }
        KnowledgePackSource::Connector => {
            warn!(
                selector = %manifest.selector,
                "Skipping connector knowledge pack without a local runtime resolver"
            );
            None
        }
    }
}

fn filesystem_manifest_knowledge_ref(manifest: &KnowledgePackManifest) -> Option<KnowledgeRef> {
    if let Ok(knowledge_ref) = manifest.selector.parse::<KnowledgeRef>() {
        return Some(knowledge_ref);
    }
    let derived_ref = match manifest.source_type {
        KnowledgePackSource::Library => KnowledgeRef::library(manifest.slug.as_str()),
        KnowledgePackSource::Local => KnowledgeRef::local(manifest.slug.as_str()),
        KnowledgePackSource::Package | KnowledgePackSource::Connector => return None,
    };
    derived_ref
        .map_err(|error| {
            warn!(
                selector = %manifest.selector,
                slug = %manifest.slug,
                error = %error,
                "Skipping knowledge pack with invalid manifest selector"
            );
        })
        .ok()
}

fn package_manifest_knowledge_ref(
    manifest: &KnowledgePackManifest,
    pack: &PackageKnowledgePack,
) -> Option<KnowledgeRef> {
    if let Ok(knowledge_ref) = manifest.selector.parse::<KnowledgeRef>() {
        return Some(knowledge_ref);
    }
    if let Some(selector) = pack.selector()
        && let Ok(knowledge_ref) = selector
            .parse::<KnowledgeRef>()
            .or_else(|_| KnowledgeRef::package(selector))
    {
        return Some(knowledge_ref);
    }
    KnowledgeRef::package(pack.manifest().pack_id())
        .map_err(|error| {
            warn!(
                selector = %manifest.selector,
                pack_id = %pack.manifest().pack_id(),
                error = %error,
                "Skipping package knowledge pack with invalid manifest selector"
            );
        })
        .ok()
}

#[derive(Default)]
pub(crate) struct ProviderKnowledgeState {
    pack_entries: Vec<KnowledgePackEntry>,
    live_manifest_reader: Option<Arc<dyn ManifestReader>>,
    search_service: Arc<KnowledgeSearchService>,
}

impl Clone for ProviderKnowledgeState {
    fn clone(&self) -> Self {
        Self {
            pack_entries: self.pack_entries.clone(),
            live_manifest_reader: self.live_manifest_reader.clone(),
            search_service: self.search_service.clone(),
        }
    }
}

struct ManifestBackedKnowledgeRegistry {
    manifest: Arc<Manifest>,
    explicit_entries: Vec<KnowledgePackEntry>,
    live_manifest_reader: Option<Arc<dyn ManifestReader>>,
    policy: crate::package_resolve::PkgResolvePolicy,
}

impl ManifestBackedKnowledgeRegistry {
    async fn collect_pack_manifests(&self) -> Vec<KnowledgePackManifest> {
        let mut packs = self.manifest.knowledge_packs.clone();
        if let Some(reader) = &self.live_manifest_reader {
            match reader.list_knowledge_packs().await {
                Ok(live) => {
                    for pack in live {
                        // Keep multi-version coexistence: upsert by versioned slug identity.
                        if let Some(existing) = packs
                            .iter_mut()
                            .find(|item| item.manifest_slug() == pack.manifest_slug())
                        {
                            *existing = pack;
                        } else {
                            packs.push(pack);
                        }
                    }
                }
                Err(error) => {
                    warn!(error = %error, "Failed to refresh live knowledge pack manifest");
                }
            }
        }
        packs
    }

    fn entry_for_manifest(
        &self,
        manifest: &KnowledgePackManifest,
    ) -> Option<(
        KnowledgePackEntry,
        crate::package_resolve::VersionedCandidate,
    )> {
        let entry = manifest_knowledge_pack_entry(manifest)?;
        let candidate = knowledge_pack_version_candidate(manifest);
        Some((entry, candidate))
    }

    async fn all_versioned_entries(
        &self,
    ) -> Vec<(
        KnowledgePackEntry,
        crate::package_resolve::VersionedCandidate,
    )> {
        let mut out = Vec::new();
        for pack in self.collect_pack_manifests().await {
            if let Some(pair) = self.entry_for_manifest(&pack) {
                out.push(pair);
            }
        }
        for entry in &self.explicit_entries {
            // Explicit packs have no package version metadata; treat as unversioned.
            out.push((
                entry.clone(),
                crate::package_resolve::VersionedCandidate {
                    package_name: None,
                    package_version: None,
                    path: entry.selector(),
                    name: entry.selector(),
                },
            ));
        }
        out
    }

    /// Deduplicate by selector under policy so agents see one logical pack.
    async fn resolved_entries(&self) -> Vec<KnowledgePackEntry> {
        use crate::package_resolve::pick_version_winner;
        use std::collections::BTreeMap;

        let all = self.all_versioned_entries().await;
        let mut by_selector: BTreeMap<
            String,
            Vec<(usize, crate::package_resolve::VersionedCandidate)>,
        > = BTreeMap::new();
        for (idx, (entry, cand)) in all.iter().enumerate() {
            by_selector
                .entry(entry.selector())
                .or_default()
                .push((idx, cand.clone()));
        }
        let mut winners = Vec::new();
        for (_selector, group) in by_selector {
            if let Some(idx) = pick_version_winner(&group, &self.policy) {
                winners.push(all[idx].0.clone());
            }
        }
        winners
    }
}

#[async_trait::async_trait]
impl KnowledgeRegistry for ManifestBackedKnowledgeRegistry {
    async fn list_packs(&self) -> anyhow::Result<Vec<KnowledgePackSummary>> {
        let mut packs = self
            .resolved_entries()
            .await
            .into_iter()
            .map(|entry| {
                KnowledgePackSummary::new(
                    entry.selector(),
                    entry.pack().manifest(),
                    entry.display_name().map(str::to_string),
                    entry.display_description().map(str::to_string),
                    entry.writable(),
                )
            })
            .collect::<Vec<_>>();
        packs.sort_by(|a, b| a.selector.cmp(&b.selector));
        Ok(packs)
    }

    async fn resolve_pack(&self, selector: &str) -> anyhow::Result<Arc<dyn KnowledgePack>> {
        use crate::package_resolve::pick_version_winner;

        let all = self.all_versioned_entries().await;
        let matching: Vec<(usize, crate::package_resolve::VersionedCandidate)> = all
            .iter()
            .enumerate()
            .filter(|(_, (entry, _))| entry.selector() == selector)
            .map(|(idx, (_, cand))| (idx, cand.clone()))
            .collect();
        if matching.is_empty() {
            return Err(anyhow::anyhow!("unknown knowledge pack '{selector}'"));
        }
        let idx = pick_version_winner(&matching, &self.policy)
            .ok_or_else(|| anyhow::anyhow!("unknown knowledge pack '{selector}'"))?;
        Ok(all[idx].0.pack().clone())
    }
}

pub(crate) struct ProviderServices<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> {
    pub(crate) model_factory: Arc<ModelFactory>,
    pub(crate) tool_factory: Arc<ToolFactoryImpl>,
    pub(crate) memory: Option<Arc<Mem>>,
    pub(crate) agent_config: AgentConfig,
    pub(crate) render_ctx_extra: RenderContextVars,
    pub(crate) argument_bindings: Vec<ResolvedArgumentBinding>,
    pub(crate) knowledge: ProviderKnowledgeState,
}

impl<ModelFactory: ?Sized, ToolFactoryImpl: ?Sized, Mem: ?Sized> Clone
    for ProviderServices<ModelFactory, ToolFactoryImpl, Mem>
{
    fn clone(&self) -> Self {
        Self {
            model_factory: self.model_factory.clone(),
            tool_factory: self.tool_factory.clone(),
            memory: self.memory.clone(),
            agent_config: self.agent_config.clone(),
            render_ctx_extra: self.render_ctx_extra.clone(),
            argument_bindings: self.argument_bindings.clone(),
            knowledge: self.knowledge.clone(),
        }
    }
}

impl ErasedProvider {
    /// Start building a Provider.
    pub fn builder() -> ProviderBuilder {
        ProviderBuilder::new()
    }
}

impl<ModelFactory, ToolFactoryImpl, Mem> Provider<ModelFactory, ToolFactoryImpl, Mem>
where
    ModelFactory: TypedModelProviderFactory + ?Sized + 'static,
    ToolFactoryImpl: ToolFactory + ?Sized + 'static,
    Mem: ProviderMemory + ?Sized + 'static,
{
    pub(crate) fn new_inner(
        manifest: Arc<Manifest>,
        services: ProviderServices<ModelFactory, ToolFactoryImpl, Mem>,
    ) -> Self {
        Self::from_services(manifest, services)
    }

    fn from_services(
        manifest: Arc<Manifest>,
        services: ProviderServices<ModelFactory, ToolFactoryImpl, Mem>,
    ) -> Self {
        let render_blocks: Vec<_> = manifest
            .context_blocks
            .iter()
            .map(prompts::render_context_block)
            .collect();
        let context_renderer = ContextRenderer::from_blocks(&render_blocks);

        Self {
            inner: Arc::new(ProviderInner {
                manifest: ManifestIndex::new(manifest),
                context_renderer,
                services,
            }),
        }
    }

    /// Get an agent builder by agent slug.
    pub async fn agent(&self, slug: impl IntoSlug) -> Result<AgentBuilder<Self>, ProviderError> {
        let slug = slug.into_slug();
        let agent = self
            .inner
            .manifest
            .agent(&slug)
            .ok_or_else(|| ProviderError::AgentNotFound(slug.to_string()))?;

        self.build_agent(agent).await
    }

    /// Access the bootstrap manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.inner.manifest.manifest
    }

    /// Get a clone of the manifest Arc (for mutation + rebuild).
    pub fn manifest_snapshot(&self) -> Arc<Manifest> {
        self.inner.manifest.manifest.clone()
    }

    /// Create a new Provider with the given manifest but same factories/memory/config.
    ///
    /// Used by the harness to hot-swap bootstrap data without rebuilding factories.
    pub fn with_manifest(&self, manifest: Manifest) -> Self {
        Self::from_services(Arc::new(manifest), self.inner.services.clone())
    }

    /// Create a new Provider with the same manifest and updated provider-level argument bindings.
    pub fn with_argument_bindings(&self, bindings: Vec<ResolvedArgumentBinding>) -> Self {
        let mut services = self.inner.services.clone();
        services.argument_bindings = bindings;
        Self::from_services(self.inner.manifest.manifest.clone(), services)
    }

    /// Access the memory backend, if configured.
    pub fn memory(&self) -> Option<&Arc<Mem>> {
        self.inner.services.memory.as_ref()
    }

    /// Access the agent config.
    pub fn agent_config(&self) -> &AgentConfig {
        &self.inner.services.agent_config
    }

    /// Access the tool factory.
    pub fn tool_factory(&self) -> &ToolFactoryImpl {
        self.inner.services.tool_factory.as_ref()
    }

    pub(crate) fn find_agent_manifest(&self, slug: &Slug) -> Option<&AgentManifest> {
        self.inner.manifest.agent(slug)
    }

    pub(crate) fn find_ability(&self, name: &str) -> Option<&AbilityManifest> {
        self.inner.manifest.ability(name)
    }

    pub(crate) fn find_domain(&self, selector: &str) -> Option<&DomainManifest> {
        self.inner.manifest.domain(selector)
    }

    pub(crate) fn find_project(&self, slug: &Slug) -> Option<&ProjectManifest> {
        self.inner.manifest.project(slug)
    }

    /// Look up a project manifest by slug from the indexed bootstrap manifest.
    pub fn project(&self, slug: impl IntoSlug) -> Result<&ProjectManifest, ProviderError> {
        let slug = slug.into_slug();
        self.inner
            .manifest
            .project(&slug)
            .ok_or_else(|| ProviderError::ProjectNotFound(slug.to_string()))
    }

    /// Look up a model manifest by slug from the indexed bootstrap manifest.
    pub fn model(&self, slug: impl IntoSlug) -> Result<&ModelManifest, ProviderError> {
        let slug = slug.into_slug();
        self.inner
            .manifest
            .model(&slug)
            .ok_or_else(|| ProviderError::ModelNotFound(slug.to_string()))
    }

    /// Look up a council manifest by slug from the indexed bootstrap manifest.
    pub fn council(
        &self,
        slug: impl IntoSlug,
    ) -> Result<&crate::manifest::CouncilManifest, ProviderError> {
        let slug = slug.into_slug();
        self.inner
            .manifest
            .council(&slug)
            .ok_or_else(|| ProviderError::CouncilNotFound(slug.to_string()))
    }

    // -----------------------------------------------------------------------
    // Routine execution
    // -----------------------------------------------------------------------

    /// Look up a routine by slug and return a builder for configuring execution.
    ///
    /// ```ignore
    /// let task = nenjo::TaskInput::new("Fix auth", "Repair the login flow")
    ///     .with_project("demo_project")
    ///     .with_task_id(task_id);
    /// let result = provider.routine("triage")?
    ///     .run(task)
    ///     .await?;
    /// ```
    pub fn routine(&self, slug: impl IntoSlug) -> Result<RoutineRunner<Self>, ProviderError> {
        let slug = slug.into_slug();
        let routine = self
            .inner
            .manifest
            .routine(&slug)
            .ok_or_else(|| ProviderError::RoutineNotFound(slug.to_string()))?
            .clone();

        Ok(RoutineRunner::new(self.clone(), routine))
    }

    /// Start configuring an agent that does not need to exist in the provider manifest.
    pub fn new_agent(&self) -> AgentBuilder<Self> {
        AgentBuilder::blank(
            self.clone(),
            self.inner.services.agent_config.clone(),
            self.inner.context_renderer.clone(),
        )
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    async fn build_agent(
        &self,
        agent: &AgentManifest,
    ) -> Result<AgentBuilder<Self>, ProviderError> {
        let model_manifest = self.resolve_model(agent)?;

        // Memory backend is passed to the builder; scope and tools are
        // constructed in build() based on the project context set at that point.

        let prompt_config = agent.prompt_config.clone();
        debug!(
            agent = %agent.name,
            system_prompt_len = prompt_config.system_prompt.len(),
            task_execution_len = prompt_config.templates.task_execution.len(),
            "Loaded typed prompt_config"
        );

        let agent_config = self.inner.services.agent_config.clone();
        let prompt_context = self.build_prompt_context(agent);

        let mut builder = AgentBuilder::new(super::agents::builder::AgentBuilderParams {
            agent_manifest: agent.clone(),
            model_manifest,
            tools: Vec::new(),
            prompt_context,
            agent_config,
            context_renderer: self.inner.context_renderer.clone(),
            provider_runtime: self.clone(),
        });

        if let Some(memory) = Mem::clone_runtime(self.inner.services.memory.as_ref()) {
            builder = builder.with_memory(memory);
        }

        // Enable delegation support so the runner can inject DelegateToTool.
        builder = builder.with_delegation_support(self.clone());

        Ok(builder)
    }

    pub(crate) fn create_knowledge_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.create_knowledge_tools_with_policy(
            crate::package_resolve::PkgResolvePolicy::HighestSemver,
        )
    }

    pub(crate) fn create_knowledge_tools_with_policy(
        &self,
        policy: crate::package_resolve::PkgResolvePolicy,
    ) -> Vec<Arc<dyn Tool>> {
        let knowledge = &self.inner.services.knowledge;
        let registry = Arc::new(ManifestBackedKnowledgeRegistry {
            manifest: self.inner.manifest.manifest.clone(),
            explicit_entries: knowledge.pack_entries.clone(),
            live_manifest_reader: knowledge.live_manifest_reader.clone(),
            policy,
        });
        let mut tools = vec![nenjo_knowledge::tools::knowledge_list_packs_tool(
            registry.clone(),
        )];
        if knowledge.live_manifest_reader.is_some() || !self.knowledge_pack_entries().is_empty() {
            tools.extend(
                nenjo_knowledge::tools::knowledge_traversal_tools_with_search(
                    registry,
                    knowledge.search_service.clone(),
                ),
            );
        }
        tools
    }

    fn resolve_model(&self, agent: &AgentManifest) -> Result<ModelManifest, ProviderError> {
        let model_slug = agent.model.as_ref().ok_or_else(|| {
            ProviderError::ModelNotFound(format!("agent '{}' has no model assigned", agent.name))
        })?;

        self.inner
            .manifest
            .model(model_slug)
            .cloned()
            .ok_or_else(|| {
                ProviderError::ModelNotFound(format!(
                    "model {model_slug} not found (agent '{}')",
                    agent.name
                ))
            })
    }

    fn build_prompt_context(&self, agent: &AgentManifest) -> PromptContext {
        let current_project = self
            .inner
            .manifest
            .manifest
            .projects
            .first()
            .cloned()
            .unwrap_or_else(|| ProjectManifest {
                name: String::new(),
                slug: Slug::derive("project"),
                description: None,
                settings: serde_json::Value::Null,
            });

        let mut render_ctx_extra = self.inner.services.render_ctx_extra.clone();
        render_ctx_extra.knowledge_vars = self.refresh_knowledge_prompt_vars();

        PromptContext {
            agent_name: agent.name.clone(),
            agent_description: agent.description.clone().unwrap_or_default(),
            current_project,
            active_domain: None,
            append_active_domain_addon: true,
            render_ctx_extra,
            argument_bindings: self.inner.services.argument_bindings.clone(),
        }
    }

    fn refresh_knowledge_prompt_vars(&self) -> std::collections::HashMap<String, String> {
        let entries = self.knowledge_pack_entries_resolved(
            &crate::package_resolve::PkgResolvePolicy::HighestSemver,
        );
        if entries.is_empty() {
            self.inner.services.render_ctx_extra.knowledge_vars.clone()
        } else {
            nenjo_knowledge::tools::knowledge_prompt_vars_from_entries(entries)
        }
    }

    fn knowledge_pack_entries(&self) -> Vec<KnowledgePackEntry> {
        let mut entries = self.inner.services.knowledge.pack_entries.clone();
        entries.extend(
            self.inner
                .manifest
                .manifest
                .knowledge_packs
                .iter()
                .filter_map(manifest_knowledge_pack_entry),
        );
        entries
    }

    /// Knowledge packs after multi-version policy selection (one entry per selector).
    fn knowledge_pack_entries_resolved(
        &self,
        policy: &crate::package_resolve::PkgResolvePolicy,
    ) -> Vec<KnowledgePackEntry> {
        use crate::package_resolve::pick_version_winner;
        use std::collections::BTreeMap;

        let mut loaded_entries: Vec<(
            KnowledgePackEntry,
            crate::package_resolve::VersionedCandidate,
        )> = Vec::new();
        for pack in &self.inner.manifest.manifest.knowledge_packs {
            if let Some(entry) = manifest_knowledge_pack_entry(pack) {
                let cand = knowledge_pack_version_candidate(pack);
                loaded_entries.push((entry, cand));
            }
        }

        let mut by_selector: BTreeMap<
            String,
            Vec<(usize, crate::package_resolve::VersionedCandidate)>,
        > = BTreeMap::new();
        for (idx, (entry, cand)) in loaded_entries.iter().enumerate() {
            by_selector
                .entry(entry.selector())
                .or_default()
                .push((idx, cand.clone()));
        }

        let mut winners = Vec::new();
        for (_selector, group) in by_selector {
            if let Some(idx) = pick_version_winner(&group, policy) {
                winners.push(loaded_entries[idx].0.clone());
            }
        }
        // Explicit packs (library etc.) always included.
        winners.extend(self.inner.services.knowledge.pack_entries.iter().cloned());
        winners
    }

    async fn create_model_provider(
        &self,
        model: &ModelManifest,
    ) -> Result<Arc<ModelFactory::Provider<'static>>, ProviderError> {
        self.inner
            .services
            .model_factory
            .create_typed_with_base_url(&model.model_provider, model.base_url.as_deref())
            .map_err(|e| {
                ProviderError::FactoryFailed(e.context(format!(
                    "failed to create LLM provider '{}'",
                    model.model_provider
                )))
            })
    }
}

#[async_trait::async_trait]
impl<ModelFactory, ToolFactoryImpl, Mem> ProviderRuntime
    for Provider<ModelFactory, ToolFactoryImpl, Mem>
where
    ModelFactory: TypedModelProviderFactory + ?Sized + 'static,
    ToolFactoryImpl: ToolFactory + ?Sized + 'static,
    Mem: ProviderMemory + ?Sized + 'static,
{
    type Model<'a>
        = ModelFactory::Provider<'static>
    where
        Self: 'a;
    type ToolFactory<'a>
        = ToolFactoryImpl
    where
        Self: 'a;
    type Memory<'a>
        = Mem::Runtime<'static>
    where
        Self: 'a;

    fn manifest_snapshot(&self) -> Arc<Manifest> {
        Provider::manifest_snapshot(self)
    }

    fn with_manifest(&self, manifest: Manifest) -> Self {
        Provider::with_manifest(self, manifest)
    }

    fn with_argument_bindings(&self, bindings: Vec<ResolvedArgumentBinding>) -> Self {
        Provider::with_argument_bindings(self, bindings)
    }

    fn tool_factory(&self) -> &Self::ToolFactory<'_> {
        self.tool_factory()
    }

    fn find_agent_manifest(&self, slug: &Slug) -> Option<&AgentManifest> {
        Provider::find_agent_manifest(self, slug)
    }

    fn find_ability(&self, name: &str) -> Option<&AbilityManifest> {
        Provider::find_ability(self, name)
    }

    fn find_domain(&self, selector: &str) -> Option<&DomainManifest> {
        Provider::find_domain(self, selector)
    }

    fn find_project(&self, slug: &Slug) -> Option<&ProjectManifest> {
        Provider::find_project(self, slug)
    }

    fn create_knowledge_tools(&self) -> Vec<Arc<dyn Tool>> {
        Provider::create_knowledge_tools(self)
    }

    fn create_knowledge_tools_with_policy(
        &self,
        policy: crate::package_resolve::PkgResolvePolicy,
    ) -> Vec<Arc<dyn Tool>> {
        Provider::create_knowledge_tools_with_policy(self, policy)
    }

    fn find_ability_with_policy(
        &self,
        name: &str,
        policy: &crate::package_resolve::PkgResolvePolicy,
    ) -> Option<&AbilityManifest> {
        resolve_ability_by_name(&self.inner.manifest.manifest.abilities, name, policy)
    }

    fn find_domain_with_policy(
        &self,
        selector: &str,
        policy: &crate::package_resolve::PkgResolvePolicy,
    ) -> Option<&DomainManifest> {
        resolve_domain_by_selector(&self.inner.manifest.manifest.domains, selector, policy)
            .or_else(|| self.find_domain(selector))
    }

    fn build_prompt_context(&self, agent: &AgentManifest) -> PromptContext {
        Provider::build_prompt_context(self, agent)
    }

    async fn create_model_provider(
        &self,
        model: &ModelManifest,
    ) -> Result<Arc<Self::Model<'static>>, ProviderError> {
        Provider::create_model_provider(self, model).await
    }

    fn new_agent(&self) -> AgentBuilder<Self> {
        Provider::new_agent(self)
    }

    async fn agent(&self, slug: &Slug) -> Result<AgentBuilder<Self>, ProviderError> {
        Provider::agent(self, slug.as_str()).await
    }

    fn routine(&self, slug: &Slug) -> Result<RoutineRunner<Self>, ProviderError> {
        Provider::routine(self, slug.as_str())
    }
}

#[cfg(test)]
mod tests;
