//! Runtime-readiness validation for resolved Nenjo module packages.
//!
//! This module owns checks that can be performed from package manifests and the
//! resolved package graph alone. It intentionally excludes database state,
//! ownership policy, and install job bookkeeping; platform callers run those
//! checks after this shared package validation passes.
//!
//! The validation contract mirrors runtime behavior: rendered prompt surfaces
//! are rendered strictly with package/context named templates, routine fields
//! are not rendered unless runtime renders them, and assignment references are
//! checked against the resolved install graph before platform materializes IDs.

mod assignments;
pub mod diagnostics;
mod graph;
mod mcp_servers;
mod render;
mod routines;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;

pub use diagnostics::{
    PackageRuntimeValidationReport, PackageValidationDiagnostic, PackageValidationSeverity,
};

use crate::{
    PackageError, PackageKind, PackageRegistryManifest, ResolvedModule, ResolvedPackage,
    ResolvedPackageGraph,
};

use self::graph::{
    collect_strings, context_import_name, package_selector_aliases, pkg_selector_is_allowed,
    scan_arg_selectors, scan_context_selectors, scan_pkg_selectors, selector_to_package_name,
    unique_modules, validate_context_graph, validate_module_imports,
};
use self::render::{RenderFixture, validate_template_selectors};

pub fn validate_package_runtime(
    graph: &ResolvedPackageGraph,
) -> crate::Result<PackageRuntimeValidationReport> {
    let mut report = validate_packages(&graph.packages, |_| {});
    validate_non_reusable_dependency_resources(graph, &mut report);
    finish_report(report)
}

pub fn validate_registry_runtime(
    _registry: &PackageRegistryManifest,
    packages: &BTreeMap<String, ResolvedPackage>,
) -> crate::Result<PackageRuntimeValidationReport> {
    let report = validate_packages(packages, |_| {});
    finish_report(report)
}

pub fn validate_registry_runtime_with_progress(
    _registry: &PackageRegistryManifest,
    packages: &BTreeMap<String, ResolvedPackage>,
    progress: impl FnMut(PackageRuntimeValidationStage),
) -> crate::Result<PackageRuntimeValidationReport> {
    let report = validate_packages(packages, progress);
    finish_report(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageRuntimeValidationStage {
    RenderFixture,
    ModuleImports,
    PromptSelectors,
    KnowledgeSelectors,
    Assignments,
    McpServers,
    Routines,
    StrictRender,
    ContextGraph,
}

impl PackageRuntimeValidationStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::RenderFixture => "building render fixture",
            Self::ModuleImports => "validating module imports",
            Self::PromptSelectors => "validating prompt selectors",
            Self::KnowledgeSelectors => "validating knowledge selectors",
            Self::Assignments => "validating assignments",
            Self::McpServers => "validating MCP server configurations",
            Self::Routines => "validating routine graphs",
            Self::StrictRender => "strict-rendering prompts",
            Self::ContextGraph => "validating context graph",
        }
    }
}

fn finish_report(
    report: PackageRuntimeValidationReport,
) -> crate::Result<PackageRuntimeValidationReport> {
    if report.is_valid() {
        Ok(report)
    } else {
        Err(PackageError::Message(format!(
            "package runtime validation failed: {}",
            report.error_summary()
        )))
    }
}

fn validate_packages(
    packages: &BTreeMap<String, ResolvedPackage>,
    mut progress: impl FnMut(PackageRuntimeValidationStage),
) -> PackageRuntimeValidationReport {
    let mut report = PackageRuntimeValidationReport::default();
    progress(PackageRuntimeValidationStage::RenderFixture);
    let fixture = match RenderFixture::build(packages) {
        Ok(fixture) => Some(fixture),
        Err(error) => {
            report.diagnostics.push(PackageValidationDiagnostic::error(
                "<registry>",
                "<registry>",
                None,
                None,
                format!("failed to build render fixture: {error:#}"),
            ));
            None
        }
    };

    progress(PackageRuntimeValidationStage::ModuleImports);
    for package in packages.values() {
        for module in unique_modules(package) {
            validate_one(
                package,
                module,
                "module imports",
                None,
                || validate_module_imports(package, module),
                &mut report,
            );
        }
    }

    progress(PackageRuntimeValidationStage::PromptSelectors);
    for package in packages.values() {
        if let Err(error) = validate_package_arguments(package) {
            push_package_error(
                package,
                None,
                Some("manifest.arguments"),
                error,
                &mut report,
            );
            continue;
        }
        let current_selectors = match package_selector_aliases(&package.name) {
            Ok(selectors) => selectors,
            Err(error) => {
                push_package_error(package, None, None, error, &mut report);
                continue;
            }
        };
        let dependency_selectors = match package
            .dependencies()
            .keys()
            .map(|name| package_selector_aliases(name))
            .collect::<anyhow::Result<Vec<_>>>()
            .map(|selectors| selectors.into_iter().flatten().collect::<BTreeSet<_>>())
        {
            Ok(selectors) => selectors,
            Err(error) => {
                push_package_error(package, None, None, error, &mut report);
                continue;
            }
        };
        for module in unique_modules(package) {
            validate_one(
                package,
                module,
                "prompt selectors",
                None,
                || {
                    validate_prompt_selectors(
                        package,
                        module,
                        &current_selectors,
                        &dependency_selectors,
                    )
                },
                &mut report,
            );
        }
    }

    progress(PackageRuntimeValidationStage::KnowledgeSelectors);
    for package in packages.values() {
        for module in unique_modules(package) {
            validate_one(
                package,
                module,
                "knowledge selectors",
                None,
                || validate_knowledge_selectors(module),
                &mut report,
            );
        }
    }

    progress(PackageRuntimeValidationStage::Assignments);
    for package in packages.values() {
        for module in unique_modules(package) {
            validate_one(
                package,
                module,
                "assignments",
                Some("manifest.assignments"),
                || assignments::validate_assignments(packages, module),
                &mut report,
            );
        }
    }

    progress(PackageRuntimeValidationStage::McpServers);
    for package in packages.values() {
        for module in unique_modules(package) {
            validate_one(
                package,
                module,
                "MCP server configuration",
                None,
                || mcp_servers::validate_mcp_server_manifest(module),
                &mut report,
            );
        }
    }

    progress(PackageRuntimeValidationStage::Routines);
    for package in packages.values() {
        for module in unique_modules(package) {
            validate_one(
                package,
                module,
                "routine graph",
                None,
                || routines::validate_routine_manifest(packages, module),
                &mut report,
            );
        }
    }

    progress(PackageRuntimeValidationStage::StrictRender);
    if let Some(fixture) = fixture {
        for package in packages.values() {
            for module in unique_modules(package) {
                validate_rendered_fields(package, module, &fixture, &mut report);
            }
        }
    }

    progress(PackageRuntimeValidationStage::ContextGraph);
    for package in packages.values() {
        if let Err(error) = validate_context_graph(package) {
            push_package_error(package, None, None, error, &mut report);
        }
    }

    report
}

/// Routines, hooks, MCP servers, and models are package-local runtime
/// configuration. They have stable registry-scoped identities, but are not
/// reusable exports from a dependency package.
fn validate_non_reusable_dependency_resources(
    graph: &ResolvedPackageGraph,
    report: &mut PackageRuntimeValidationReport,
) {
    for (package_name, package) in &graph.packages {
        if package_name == &graph.root_package {
            continue;
        }
        for module in unique_modules(package) {
            if !matches!(
                module.kind,
                PackageKind::Routine
                    | PackageKind::Hook
                    | PackageKind::McpServer
                    | PackageKind::Model
            ) {
                continue;
            }
            report.diagnostics.push(PackageValidationDiagnostic::error(
                &package.name,
                &module.source_path,
                Some(module.kind),
                None,
                format!(
                    "dependency package '{}' exports {}, but {} resources must be installed by the root package",
                    package_name,
                    module.kind.as_str(),
                    module.kind.as_str(),
                ),
            ));
        }
    }
}

fn validate_one(
    package: &ResolvedPackage,
    module: &ResolvedModule,
    label: &str,
    field_path: Option<&str>,
    check: impl FnOnce() -> anyhow::Result<()>,
    report: &mut PackageRuntimeValidationReport,
) {
    if let Err(error) = check() {
        report.diagnostics.push(PackageValidationDiagnostic::error(
            &package.name,
            &module.source_path,
            Some(module.kind),
            field_path.map(str::to_string),
            format!("{label} validation failed: {error:#}"),
        ));
    }
}

fn push_package_error(
    package: &ResolvedPackage,
    source_path: Option<&str>,
    field_path: Option<&str>,
    error: anyhow::Error,
    report: &mut PackageRuntimeValidationReport,
) {
    report.diagnostics.push(PackageValidationDiagnostic::error(
        &package.name,
        source_path.unwrap_or(&package.path),
        None,
        field_path.map(str::to_string),
        error.to_string(),
    ));
}

fn validate_prompt_selectors(
    package: &ResolvedPackage,
    module: &ResolvedModule,
    package_selectors: &BTreeSet<String>,
    dependency_selectors: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let mut strings = Vec::new();
    collect_strings(&module.manifest.manifest, &mut strings);
    let imported_context = module
        .imports
        .iter()
        .filter(|import| import.surface == "context")
        .map(|import| context_import_name(&module.path, &import.reference))
        .collect::<anyhow::Result<BTreeSet<_>>>()?;

    for value in strings {
        for selector in scan_pkg_selectors(value) {
            if !pkg_selector_is_allowed(&selector, package_selectors, dependency_selectors) {
                anyhow::bail!(
                    "{} references pkg selector {}, but {} is not the current package or a package dependency",
                    module.path,
                    selector,
                    selector_to_package_name(&selector)
                );
            }
        }
        for context in scan_context_selectors(value) {
            if !imported_context.contains(&context) {
                anyhow::bail!(
                    "{} references context.{context}, but it is not declared in wrapper imports.context",
                    module.path
                );
            }
        }
        for selector in scan_arg_selectors(value) {
            if !package
                .manifest
                .arguments
                .iter()
                .any(|argument| argument.selector.as_str() == selector)
            {
                anyhow::bail!(
                    "{} references {}, but it is not declared in package arguments",
                    module.path,
                    selector
                );
            }
        }
    }
    Ok(())
}

fn validate_package_arguments(package: &ResolvedPackage) -> anyhow::Result<()> {
    let mut names = BTreeSet::new();
    let mut selectors = BTreeSet::new();
    for argument in &package.manifest.arguments {
        if !names.insert(argument.name.clone()) {
            anyhow::bail!("declares duplicate argument name '{}'", argument.name);
        }
        if !selectors.insert(argument.selector.clone()) {
            anyhow::bail!(
                "declares duplicate argument selector '{}'",
                argument.selector
            );
        }
        if let Some(default) = &argument.default {
            argument
                .value_type
                .coerce_render_value(default)
                .with_context(|| {
                    format!("argument '{}' has invalid default value", argument.name)
                })?;
        }
        if let Some(sample) = &argument.sample {
            argument
                .value_type
                .coerce_render_value(sample)
                .with_context(|| {
                    format!("argument '{}' has invalid sample value", argument.name)
                })?;
        }
    }
    Ok(())
}

fn validate_knowledge_selectors(module: &ResolvedModule) -> anyhow::Result<()> {
    if module.kind != PackageKind::Knowledge {
        return Ok(());
    }
    let docs = module
        .manifest
        .manifest
        .get("docs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("{} knowledge manifest must define docs", module.path))?;
    let mut selectors = BTreeSet::new();
    for (index, doc) in docs.iter().enumerate() {
        let selector = doc
            .get("selector")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{} knowledge doc at index {} must define selector",
                    module.path,
                    index
                )
            })?;
        validate_jinja_selector(selector).with_context(|| {
            format!(
                "{} knowledge doc selector '{}' is not Jinja-compatible",
                module.path, selector
            )
        })?;
        if !selectors.insert(selector.to_string()) {
            anyhow::bail!(
                "{} declares duplicate knowledge selector '{}'",
                module.path,
                selector
            );
        }
        for edge in doc
            .get("related")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(target) = edge.get("target").and_then(serde_json::Value::as_str) {
                validate_jinja_selector(target).with_context(|| {
                    format!(
                        "{} knowledge doc selector '{}' has invalid related target '{}'",
                        module.path, selector, target
                    )
                })?;
            }
        }
    }
    Ok(())
}

fn validate_jinja_selector(selector: &str) -> anyhow::Result<()> {
    let selector = selector.trim();
    if selector.is_empty() {
        anyhow::bail!("selector cannot be empty");
    }
    for segment in selector.split('.') {
        if segment.is_empty() {
            anyhow::bail!("selector cannot contain empty segments");
        }
        let mut chars = segment.chars();
        let first = chars.next().expect("segment is not empty");
        if !(first == '_' || first.is_ascii_alphabetic()) {
            anyhow::bail!("selector segment '{segment}' must start with a letter or underscore");
        }
        if !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric()) {
            anyhow::bail!(
                "selector segment '{segment}' may contain only letters, numbers, and underscores"
            );
        }
    }
    Ok(())
}

fn validate_rendered_fields(
    package: &ResolvedPackage,
    module: &ResolvedModule,
    fixture: &RenderFixture,
    report: &mut PackageRuntimeValidationReport,
) {
    for (field_path, template) in rendered_fields(module) {
        let selector_result = validate_template_selectors(fixture, module, template);
        let render_result =
            selector_result.and_then(|_| fixture.render_field(module, &field_path, template));
        if let Err(error) = render_result {
            report.diagnostics.push(PackageValidationDiagnostic::error(
                &package.name,
                &module.source_path,
                Some(module.kind),
                Some(field_path),
                error.to_string(),
            ));
        }
    }
}

fn rendered_fields(module: &ResolvedModule) -> Vec<(String, &str)> {
    let manifest = &module.manifest.manifest;
    let mut fields = Vec::new();
    match module.kind {
        PackageKind::Agent => {
            if let Some(prompt_config) = manifest.get("prompt_config") {
                push_string_field(
                    prompt_config,
                    "system_prompt",
                    "manifest.prompt_config.system_prompt",
                    &mut fields,
                );
                push_string_field(
                    prompt_config,
                    "developer_prompt",
                    "manifest.prompt_config.developer_prompt",
                    &mut fields,
                );
                if let Some(templates) = prompt_config.get("templates") {
                    push_string_field(
                        templates,
                        "chat",
                        "manifest.prompt_config.templates.chat",
                        &mut fields,
                    );
                    push_string_field(
                        templates,
                        "task",
                        "manifest.prompt_config.templates.task",
                        &mut fields,
                    );
                    push_string_field(
                        templates,
                        "gate",
                        "manifest.prompt_config.templates.gate",
                        &mut fields,
                    );
                }
            }
        }
        PackageKind::Ability => {
            if let Some(prompt_config) = manifest.get("prompt_config") {
                push_string_field(
                    prompt_config,
                    "developer_prompt",
                    "manifest.prompt_config.developer_prompt",
                    &mut fields,
                );
            }
        }
        PackageKind::Domain => {
            if let Some(prompt_config) = manifest.get("prompt_config") {
                push_string_field(
                    prompt_config,
                    "developer_prompt_addon",
                    "manifest.prompt_config.developer_prompt_addon",
                    &mut fields,
                );
                push_string_field(
                    prompt_config,
                    "entry_message",
                    "manifest.prompt_config.entry_message",
                    &mut fields,
                );
                push_string_field(
                    prompt_config,
                    "exit_message",
                    "manifest.prompt_config.exit_message",
                    &mut fields,
                );
            }
        }
        PackageKind::Command => {
            push_string_field(manifest, "content", "manifest.content", &mut fields);
            push_string_field(manifest, "template", "manifest.template", &mut fields);
        }
        PackageKind::ContextBlock => {
            push_string_field(manifest, "template", "manifest.template", &mut fields);
        }
        _ => {}
    }
    fields
}

fn push_string_field<'a>(
    value: &'a serde_json::Value,
    key: &str,
    field_path: &str,
    fields: &mut Vec<(String, &'a str)>,
) {
    if let Some(template) = value.get(key).and_then(serde_json::Value::as_str) {
        fields.push((field_path.to_string(), template));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModulePackageManifest, ResourceManifest};

    fn package(name: &str, modules: Vec<ResolvedModule>) -> ResolvedPackage {
        package_with_dependencies(name, BTreeMap::new(), modules)
    }

    fn package_with_dependencies(
        name: &str,
        dependencies: BTreeMap<String, String>,
        modules: Vec<ResolvedModule>,
    ) -> ResolvedPackage {
        let modules = modules
            .into_iter()
            .flat_map(|module| {
                let mut items = vec![(module.path.clone(), module.clone())];
                items.push((module.key(), module));
                items
            })
            .collect();
        ResolvedPackage {
            name: name.to_string(),
            path: format!("{name}/package.yaml"),
            version: "1.0.0".to_string(),
            hash: "hash".to_string(),
            manifest: ModulePackageManifest {
                schema: "nenjo.package.v1".to_string(),
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: None,
                dependencies,
                arguments: Vec::new(),
                modules: Vec::new(),
                metadata: serde_json::Value::Null,
            },
            modules,
        }
    }

    fn module(path: &str, kind: PackageKind, manifest: serde_json::Value) -> ResolvedModule {
        ResolvedModule {
            package_name: "pkg".to_string(),
            package_version: "1.0.0".to_string(),
            path: path.to_string(),
            source_path: path.to_string(),
            hash: "hash".to_string(),
            kind,
            manifest: ResourceManifest {
                schema: format!("nenjo.{}.v1", kind.as_str()),
                slug: None,
                root_uri: None,
                selector: None,
                imports: BTreeMap::new(),
                manifest,
            },
            imports: Vec::new(),
            files: Vec::new(),
        }
    }

    fn validate_single(package: ResolvedPackage) -> String {
        let registry = PackageRegistryManifest {
            schema: "nenjo.registry.v1".to_string(),
            name: None,
            description: None,
            registries: Vec::new(),
            packages: BTreeMap::new(),
        };
        let packages = BTreeMap::from([(package.name.clone(), package)]);
        validate_registry_runtime(&registry, &packages)
            .expect_err("validation should fail")
            .to_string()
    }

    fn validate_single_ok(package: ResolvedPackage) {
        let registry = PackageRegistryManifest {
            schema: "nenjo.registry.v1".to_string(),
            name: None,
            description: None,
            registries: Vec::new(),
            packages: BTreeMap::new(),
        };
        let packages = BTreeMap::from([(package.name.clone(), package)]);
        validate_registry_runtime(&registry, &packages).expect("validation should pass");
    }

    #[test]
    fn rejects_runtime_configuration_resources_from_dependency_packages() {
        let root = package_with_dependencies(
            "app",
            BTreeMap::from([("shared-runtime".to_string(), "^1.0.0".to_string())]),
            Vec::new(),
        );
        let dependency = package(
            "shared-runtime",
            vec![
                module(
                    "models/review.yaml",
                    PackageKind::Model,
                    serde_json::json!({ "name": "review" }),
                ),
                module(
                    "mcp/review.yaml",
                    PackageKind::McpServer,
                    serde_json::json!({ "name": "review-server" }),
                ),
                module(
                    "hooks/review.yaml",
                    PackageKind::Hook,
                    serde_json::json!({ "name": "review-hook" }),
                ),
                module(
                    "routines/review.yaml",
                    PackageKind::Routine,
                    serde_json::json!({ "name": "review" }),
                ),
            ],
        );
        let graph = ResolvedPackageGraph {
            root_package: "app".to_string(),
            packages: BTreeMap::from([
                (root.name.clone(), root),
                (dependency.name.clone(), dependency),
            ]),
        };

        let error = validate_package_runtime(&graph)
            .expect_err("dependency model must be rejected")
            .to_string();

        for kind in ["model", "mcp_server", "hook", "routine"] {
            assert!(
                error.contains(&format!(
                    "dependency package 'shared-runtime' exports {kind}"
                )),
                "missing dependency validation error for {kind}: {error}"
            );
        }
    }

    #[test]
    fn accepts_scoped_official_package_selector_alias() {
        let agent = module(
            "agents/app.yaml",
            PackageKind::Agent,
            serde_json::json!({
                "name": "app",
                "prompt_config": {
                    "developer_prompt": "{{ pkg.nenjo_ai.packages.context.memory.remembrance }}"
                }
            }),
        );
        let context = module(
            "memory/remembrance.yaml",
            PackageKind::ContextBlock,
            serde_json::json!({
                "name": "remembrance",
                "template": "Remember prior work."
            }),
        );
        let packages = BTreeMap::from([
            (
                "app".to_string(),
                package_with_dependencies(
                    "app",
                    BTreeMap::from([("@nenjo-ai/context".to_string(), "^1.0.0".to_string())]),
                    vec![agent],
                ),
            ),
            (
                "@nenjo-ai/context".to_string(),
                package("@nenjo-ai/context", vec![context]),
            ),
        ]);
        let registry = PackageRegistryManifest {
            schema: "nenjo.registry.v1".to_string(),
            name: None,
            description: None,
            registries: Vec::new(),
            packages: BTreeMap::new(),
        };

        validate_registry_runtime(&registry, &packages).unwrap();
    }

    #[test]
    fn rejects_broken_prompt_rendering() {
        let ability = module(
            "abilities/build.yaml",
            PackageKind::Ability,
            serde_json::json!({
                "name": "build",
                "prompt_config": {
                    "developer_prompt": "Use {{ lib.<pack_slug> }}"
                }
            }),
        );

        let error = validate_single(package("pkg", vec![ability]));

        assert!(error.contains("failed to render"));
    }

    #[test]
    fn rejects_undefined_prompt_variable() {
        let ability = module(
            "abilities/build.yaml",
            PackageKind::Ability,
            serde_json::json!({
                "name": "build",
                "prompt_config": {
                    "developer_prompt": "{{ vars.dummy }}"
                }
            }),
        );

        let error = validate_single(package("pkg", vec![ability]));

        assert!(error.contains("failed to render"));
        assert!(error.contains("undefined"));
    }

    #[test]
    fn rejects_unresolved_package_selector() {
        let ability = module(
            "abilities/build.yaml",
            PackageKind::Ability,
            serde_json::json!({
                "name": "build",
                "prompt_config": {
                    "developer_prompt": "{{ pkg.missing.context }}"
                }
            }),
        );

        let error = validate_single(package("pkg", vec![ability]));

        assert!(error.contains("pkg selector"));
    }

    #[test]
    fn accepts_declared_runtime_argument_selector() {
        let ability = module(
            "abilities/build.yaml",
            PackageKind::Ability,
            serde_json::json!({
                "name": "build",
                "prompt_config": {
                    "developer_prompt": "{{ args.company }}"
                }
            }),
        );
        let mut package = package("pkg", vec![ability]);
        package.manifest.arguments = vec![
            serde_json::from_value(serde_json::json!({
                "name": "company_context",
                "selector": "args.company",
                "scope": "org",
                "type": "xml",
                "required": true,
                "sample": "<company>Acme</company>"
            }))
            .unwrap(),
        ];
        let registry = PackageRegistryManifest {
            schema: "nenjo.registry.v1".to_string(),
            name: None,
            description: None,
            registries: Vec::new(),
            packages: BTreeMap::new(),
        };
        let packages = BTreeMap::from([(package.name.clone(), package)]);

        validate_registry_runtime(&registry, &packages).unwrap();
    }

    #[test]
    fn rejects_undeclared_runtime_argument_selector() {
        let ability = module(
            "abilities/build.yaml",
            PackageKind::Ability,
            serde_json::json!({
                "name": "build",
                "prompt_config": {
                    "developer_prompt": "{{ args.company }}"
                }
            }),
        );

        let error = validate_single(package("pkg", vec![ability]));

        assert!(error.contains("undeclared runtime argument selector"));
    }

    #[test]
    fn rejects_assignment_with_wrong_kind() {
        let agent = module(
            "agent.yaml",
            PackageKind::Agent,
            serde_json::json!({
                "name": "agent",
                "assignments": {
                    "abilities": ["context/help.yaml"]
                }
            }),
        );
        let context = module(
            "context/help.yaml",
            PackageKind::ContextBlock,
            serde_json::json!({
                "name": "help",
                "template": "help"
            }),
        );

        let error = validate_single(package("pkg", vec![agent, context]));

        assert!(error.contains("not ability"));
    }

    #[test]
    fn accepts_routine_agent_reference_by_slug() {
        let routine = module(
            "routines/review.yaml",
            PackageKind::Routine,
            serde_json::json!({
                "name": "review",
                "trigger": "task",
                "entry_steps": ["start"],
                "steps": [
                    {"ref": "start", "type": "agent", "agent": "reviewer"},
                    {"ref": "done", "type": "terminal"}
                ],
                "edges": [
                    {
                        "from": "start",
                        "to": "done",
                        "condition": "always",
                        "metadata": {
                            "handoff_schema": {"type": "object"}
                        }
                    }
                ],
            }),
        );
        let reviewer = module(
            "agents/reviewer.yaml",
            PackageKind::Agent,
            serde_json::json!({
                "name": "reviewer",
                "prompt_config": {}
            }),
        );

        validate_single_ok(package("pkg", vec![routine, reviewer]));
    }

    #[test]
    fn rejects_ambiguous_routine_agent_reference_by_slug() {
        let routine = module(
            "routines/review.yaml",
            PackageKind::Routine,
            serde_json::json!({
                "name": "review",
                "trigger": "task",
                "entry_steps": ["start"],
                "steps": [
                    {"ref": "start", "type": "agent", "agent": "reviewer"}
                ],
                "edges": []
            }),
        );
        let reviewer = module(
            "agents/reviewer.yaml",
            PackageKind::Agent,
            serde_json::json!({
                "name": "reviewer",
                "prompt_config": {}
            }),
        );
        let duplicate = module(
            "agents/other-reviewer.yaml",
            PackageKind::Agent,
            serde_json::json!({
                "name": "reviewer",
                "prompt_config": {}
            }),
        );

        let error = validate_single(package("pkg", vec![routine, reviewer, duplicate]));

        assert!(error.contains("defines multiple agents with that slug"));
    }

    #[test]
    fn rejects_invalid_routine_graph() {
        let routine = module(
            "routines/review.yaml",
            PackageKind::Routine,
            serde_json::json!({
                "name": "review",
                "trigger": "task",
                "entry_steps": ["start"],
                "steps": [
                    {"ref": "start", "type": "agent", "agent": "agents/reviewer.yaml"}
                ],
                "edges": [
                    {"from": "start", "to": "missing", "condition": "always"}
                ]
            }),
        );
        let reviewer = module(
            "agents/reviewer.yaml",
            PackageKind::Agent,
            serde_json::json!({
                "name": "reviewer",
                "prompt_config": {}
            }),
        );

        let error = validate_single(package("pkg", vec![routine, reviewer]));

        assert!(error.contains("routine graph"));
    }
}
