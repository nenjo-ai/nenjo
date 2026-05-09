use crate::project_knowledge::ProjectKnowledgePack;
use anyhow::{Result, anyhow, bail};
use nenjo::builtin_knowledge::{BuiltinKnowledgePack, builtin_knowledge_pack};
use nenjo::knowledge::{
    KnowledgeDocEdgeType, KnowledgeDocFilter, KnowledgeDocManifest, KnowledgeDocNeighbor,
    KnowledgeDocSearchHit, KnowledgeDocTree, KnowledgePack, KnowledgePackManifest,
};

#[derive(Debug, Clone)]
pub(crate) enum ResolvedKnowledgePack {
    Builtin(BuiltinKnowledgePack),
    Project(ProjectKnowledgePack),
}

impl KnowledgePack for ResolvedKnowledgePack {
    fn manifest(&self) -> &dyn KnowledgePackManifest {
        match self {
            Self::Builtin(pack) => pack.manifest(),
            Self::Project(pack) => pack.manifest(),
        }
    }

    fn doc_content(&self, manifest: &KnowledgeDocManifest) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Self::Builtin(pack) => pack.doc_content(manifest),
            Self::Project(pack) => pack.doc_content(manifest),
        }
    }

    fn read_manifest(&self, path: &str) -> Option<&KnowledgeDocManifest> {
        match self {
            Self::Builtin(pack) => pack.read_manifest(path),
            Self::Project(pack) => pack.read_manifest(path),
        }
    }

    fn list_docs(&self, filter: KnowledgeDocFilter) -> Vec<&KnowledgeDocManifest> {
        match self {
            Self::Builtin(pack) => pack.list_docs(filter),
            Self::Project(pack) => pack.list_docs(filter),
        }
    }

    fn list_tree(&self, prefix: Option<&str>) -> KnowledgeDocTree {
        match self {
            Self::Builtin(pack) => pack.list_tree(prefix),
            Self::Project(pack) => pack.list_tree(prefix),
        }
    }

    fn search_paths(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        match self {
            Self::Builtin(pack) => pack.search_paths(query, filter),
            Self::Project(pack) => pack.search_paths(query, filter),
        }
    }

    fn search_docs(&self, query: &str, filter: KnowledgeDocFilter) -> Vec<KnowledgeDocSearchHit> {
        match self {
            Self::Builtin(pack) => pack.search_docs(query, filter),
            Self::Project(pack) => pack.search_docs(query, filter),
        }
    }

    fn neighbors(
        &self,
        path: &str,
        edge_type: Option<KnowledgeDocEdgeType>,
    ) -> Vec<KnowledgeDocNeighbor> {
        match self {
            Self::Builtin(pack) => pack.neighbors(path, edge_type),
            Self::Project(pack) => pack.neighbors(path, edge_type),
        }
    }
}

pub(crate) fn builtin_pack() -> ResolvedKnowledgePack {
    ResolvedKnowledgePack::Builtin(builtin_knowledge_pack().clone())
}

pub(crate) fn project_pack_selector(project_slug: &str) -> String {
    format!("project:{project_slug}")
}

pub(crate) fn is_current_project_pack_selector(selector: &str) -> bool {
    selector == "project"
}

pub(crate) fn parse_project_pack_selector(selector: &str) -> Result<&str> {
    let slug = selector
        .strip_prefix("project:")
        .ok_or_else(|| anyhow!("project knowledge packs must use project:<slug>"))?;
    if slug.is_empty() {
        bail!("project knowledge pack selector must include a slug")
    }
    Ok(slug)
}

pub(crate) fn unknown_pack(selector: &str) -> anyhow::Error {
    anyhow!(
        "unknown knowledge pack '{selector}'; use 'builtin:nenjo', 'project' when an active project is available, or 'project:<slug>'"
    )
}

pub(crate) fn ensure_known_pack_selector(selector: &str) -> Result<()> {
    if is_nenjo_pack_selector(selector)
        || is_current_project_pack_selector(selector)
        || selector.starts_with("project:")
    {
        Ok(())
    } else {
        bail!("unknown knowledge pack selector '{selector}'")
    }
}

pub(crate) fn is_nenjo_pack_selector(selector: &str) -> bool {
    selector == "builtin:nenjo"
}
