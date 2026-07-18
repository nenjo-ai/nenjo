use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, anyhow};
use nenjo::context::{
    AgentContext, FocusListContext, GitContext, MemoryProfileContext, ProjectContext,
    RenderContextVars, RoutineContext, RoutineHandoffContext, RoutineHandoffsContext,
    RoutineStepContext, TaskContext,
};

use crate::{PackageKind, ResolvedModule, ResolvedPackage};

use super::graph::{
    package_selector_aliases, scan_arg_selectors, scan_context_selectors,
    scan_pkg_reference_selectors, scan_pkg_selectors,
};

#[derive(Debug, Clone)]
pub(crate) struct RenderFixture {
    vars: HashMap<String, String>,
    named_templates: HashMap<String, String>,
    package_arg_selectors: BTreeMap<String, BTreeSet<String>>,
}

impl RenderFixture {
    pub(crate) fn build(packages: &BTreeMap<String, ResolvedPackage>) -> anyhow::Result<Self> {
        let mut named_templates = HashMap::new();
        let mut package_arg_selectors = BTreeMap::<String, BTreeSet<String>>::new();
        let referenced_bases = referenced_unscoped_selector_bases(packages);
        for package in packages.values() {
            for argument in &package.manifest.arguments {
                package_arg_selectors
                    .entry(package.name.clone())
                    .or_default()
                    .insert(argument.selector.to_string());
            }
            let selector_bases = selector_bases(package, &referenced_bases)?;
            for module in package.modules.values() {
                match module.kind {
                    PackageKind::ContextBlock => {
                        let template = module
                            .manifest
                            .manifest
                            .get("template")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        for base_selector in &selector_bases {
                            let module_key = dotted_module_key(module);
                            let dotted = format!("{base_selector}.{module_key}");
                            insert_template_aliases(
                                &mut named_templates,
                                &dotted,
                                template.clone(),
                            );
                            let direct = format!("{base_selector}.{}", module.name());
                            insert_template_aliases(
                                &mut named_templates,
                                &direct,
                                template.clone(),
                            );
                            let module_name =
                                format!("{base_selector}.{module_key}.{}", module.name());
                            insert_template_aliases(
                                &mut named_templates,
                                &module_name,
                                template.clone(),
                            );
                        }
                        let name = module.name();
                        insert_template_aliases(
                            &mut named_templates,
                            &format!("context.{name}"),
                            template,
                        );
                    }
                    PackageKind::Knowledge => {
                        for base_selector in &selector_bases {
                            add_knowledge_templates(&mut named_templates, base_selector, module)
                                .with_context(|| {
                                    format!(
                                        "failed to build knowledge selectors for {}",
                                        module.path
                                    )
                                })?;
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut vars = synthetic_vars();
        for package in packages.values() {
            for argument in &package.manifest.arguments {
                vars.insert(
                    argument.selector.to_string(),
                    argument.validation_value().with_context(|| {
                        format!(
                            "{} argument '{}' has invalid validation value",
                            package.path, argument.name
                        )
                    })?,
                );
            }
        }
        Ok(Self {
            vars,
            named_templates,
            package_arg_selectors,
        })
    }

    pub(crate) fn render_field(
        &self,
        _module: &ResolvedModule,
        field_path: &str,
        template: &str,
    ) -> anyhow::Result<()> {
        if template.trim().is_empty() {
            return Ok(());
        }
        let named_templates = self.named_templates.clone();
        let normalized = normalize_named_template_refs(template, &named_templates);
        nenjo_xml::template::try_render_template_with_named_templates_strict(
            &normalized,
            &self.vars,
            &named_templates,
        )
        .map(|_| ())
        .map_err(|error| anyhow!("{field_path} failed to render: {}", error.message))
    }

    pub(crate) fn selector_exists(&self, selector: &str) -> bool {
        let include_key = selector.replace('.', "/");
        self.named_templates.contains_key(selector)
            || self.named_templates.contains_key(&include_key)
            || self.named_templates.keys().any(|key| {
                key.strip_prefix(selector)
                    .is_some_and(|suffix| suffix.starts_with('.'))
            })
    }

    fn exact_selector_exists(&self, selector: &str) -> bool {
        let include_key = selector.replace('.', "/");
        self.named_templates.contains_key(selector)
            || self.named_templates.contains_key(&include_key)
    }

    pub(crate) fn package_arg_selector_exists(&self, package: &str, selector: &str) -> bool {
        self.package_arg_selectors
            .get(package)
            .is_some_and(|selectors| selectors.contains(selector))
    }
}

fn selector_bases(
    package: &ResolvedPackage,
    referenced_bases: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<Vec<String>> {
    let mut bases = package_selector_aliases(&package.name)?
        .into_iter()
        .collect::<Vec<_>>();
    if !package.name.starts_with('@') {
        bases.extend(
            referenced_bases
                .get(&package.name)
                .into_iter()
                .flatten()
                .cloned(),
        );
    }
    bases.sort();
    bases.dedup();
    Ok(bases)
}

fn referenced_unscoped_selector_bases(
    packages: &BTreeMap<String, ResolvedPackage>,
) -> BTreeMap<String, BTreeSet<String>> {
    let unscoped = packages
        .keys()
        .filter(|name| !name.starts_with('@'))
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut bases = BTreeMap::<String, BTreeSet<String>>::new();
    for package in packages.values() {
        for module in package.modules.values() {
            let mut strings = Vec::new();
            super::graph::collect_strings(&module.manifest.manifest, &mut strings);
            for value in strings {
                for selector in scan_pkg_selectors(value) {
                    let parts = selector.split('.').collect::<Vec<_>>();
                    if parts.len() == 3 && parts[0] == "pkg" && parts[1] != "packages" {
                        let name = parts[2].to_string();
                        if unscoped.contains(&name) {
                            bases.entry(name).or_default().insert(selector);
                        }
                    }
                }
            }
        }
    }
    bases
}

fn insert_template_aliases(
    named_templates: &mut HashMap<String, String>,
    dotted: &str,
    template: String,
) {
    named_templates.insert(dotted.to_string(), template.clone());
    named_templates.insert(dotted.replace('.', "/"), template);
}

fn dotted_module_key(module: &ResolvedModule) -> String {
    let stemmed = module
        .path
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&module.path);
    let stemmed = stemmed
        .strip_suffix("/manifest")
        .or_else(|| stemmed.strip_suffix("/index"))
        .unwrap_or(stemmed);
    stemmed.replace('/', ".")
}

fn add_knowledge_templates(
    named_templates: &mut HashMap<String, String>,
    base_selector: &str,
    module: &ResolvedModule,
) -> anyhow::Result<()> {
    let docs = module
        .manifest
        .manifest
        .get("docs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("knowledge manifest must define docs"))?;
    let root = knowledge_root_selector(module)
        .unwrap_or_else(|| format!("{base_selector}.{}", dotted_module_key(module)));
    insert_template_aliases(named_templates, &root, knowledge_pack_template(docs));
    for doc in docs {
        let Some(selector) = doc
            .get("selector")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let template = knowledge_doc_template(doc);
        insert_template_aliases(named_templates, &format!("{root}.{selector}"), template);
    }
    Ok(())
}

fn knowledge_pack_template(docs: &[serde_json::Value]) -> String {
    docs.iter()
        .filter_map(|doc| {
            let selector = doc.get("selector").and_then(serde_json::Value::as_str)?;
            let title = doc
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(selector);
            Some(format!("- {selector}: {title}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn knowledge_root_selector(module: &ResolvedModule) -> Option<String> {
    let selector = module
        .manifest
        .manifest
        .get("selector")
        .and_then(serde_json::Value::as_str)?
        .trim();
    let selector = selector
        .strip_prefix("pkg:")
        .map(|value| value.replace('-', "_"))
        .unwrap_or_else(|| selector.to_string());
    Some(format!("pkg.{}", selector.replace('-', "_")))
}

fn knowledge_doc_template(doc: &serde_json::Value) -> String {
    let title = doc
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let summary = doc
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if title.is_empty() {
        summary.to_string()
    } else if summary.is_empty() {
        title.to_string()
    } else {
        format!("{title}\n\n{summary}")
    }
}

pub(crate) fn validate_template_selectors(
    fixture: &RenderFixture,
    module: &ResolvedModule,
    template: &str,
) -> anyhow::Result<()> {
    for selector in scan_pkg_selectors(template) {
        if !fixture.selector_exists(&selector) {
            anyhow::bail!("references unresolved package selector {selector}");
        }
    }
    for selector in scan_pkg_reference_selectors(template) {
        if !fixture.exact_selector_exists(&selector) {
            anyhow::bail!("references unresolved package selector {selector}");
        }
    }
    for context in scan_context_selectors(template) {
        let selector = format!("context.{context}");
        if !fixture.selector_exists(&selector) {
            anyhow::bail!("references unresolved context selector {selector}");
        }
    }
    for selector in scan_arg_selectors(template) {
        if !fixture.package_arg_selector_exists(&module.package_name, &selector) {
            anyhow::bail!(
                "references undeclared runtime argument selector {selector} in package {}",
                module.package_name
            );
        }
    }
    Ok(())
}

fn normalize_named_template_refs(
    template: &str,
    named_templates: &HashMap<String, String>,
) -> String {
    let mut out = template.to_string();
    let mut keys = named_templates.keys().cloned().collect::<Vec<_>>();
    keys.sort_by_key(|key| std::cmp::Reverse(key.len()));
    for key in keys {
        if !key.contains('.') {
            continue;
        }
        let include = format!("{{% include \"{}\" %}}", key.replace('.', "/"));
        for needle in [
            format!("{{{{ {key} }}}}"),
            format!("{{{{{key}}}}}"),
            format!("{{{{ {key}}}}}"),
            format!("{{{{{key} }}}}"),
        ] {
            out = out.replace(&needle, &include);
        }
    }
    out
}

fn synthetic_vars() -> HashMap<String, String> {
    let mut ctx = RenderContextVars {
        _self: AgentContext {
            slug: "validator".into(),
            display_name: "Validator".into(),
            model_name: "validation-model".into(),
            description: Some("Package validation fixture".into()),
        },
        task: TaskContext {
            id: "validation-task".into(),
            slug: "validate-package".into(),
            status: "in_progress".into(),
            priority: "medium".into(),
            title: "Validate package".into(),
            instructions:
                "Synthetic validation task. All runtime-rendered templates render strictly".into(),
            labels: "validation,package".into(),
        },
        project: ProjectContext {
            name: "Validation Project".into(),
            slug: "validation-project".into(),
            description: "Project used for package runtime validation".into(),
            working_dir: "/workspace/validation-project".into(),
            context: "Validation project context".into(),
            metadata: "<metadata><item key=\"purpose\">validation</item></metadata>".into(),
            git: Some(GitContext {
                repo_url: "https://example.com/validation.git".into(),
                branch: "validation".into(),
                target_branch: "main".into(),
                work_dir: "/workspace/validation-project".into(),
            }),
        },
        routine: RoutineContext {
            slug: "validation-routine".into(),
            name: "Validation Routine".into(),
            execution_id: "validation-execution".into(),
            description: Some("Synthetic routine context".into()),
            step: RoutineStepContext {
                name: "Validation Step".into(),
                step_type: "agent".into(),
                instructions: "Validate package runtime behavior".into(),
                metadata: r#"{"purpose":"validation"}"#.into(),
            },
            handoffs: RoutineHandoffsContext {
                items: vec![RoutineHandoffContext {
                    source_step: "validation-source".into(),
                    target_step: "validation-step".into(),
                    purpose: Some("Synthetic handoff coverage".into()),
                    summary: Some("Validation handoff".into()),
                    payload: r#"{"work":"Validate handoff rendering"}"#.into(),
                }],
            },
        },
        memory_profile: MemoryProfileContext {
            core_focus: Some(FocusListContext {
                items: vec!["runtime validation".into()],
            }),
            project_focus: Some(FocusListContext {
                items: vec!["package prompts".into()],
            }),
            shared_focus: Some(FocusListContext {
                items: vec!["runtime safety".into()],
            }),
        },
        git: GitContext {
            repo_url: "https://example.com/validation.git".into(),
            branch: "validation".into(),
            target_branch: "main".into(),
            work_dir: "/workspace/validation-project".into(),
        },
        chat_message: "Validate this package".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        memory_vars: HashMap::from([
            (
                "memories".into(),
                "<memories>validation memory</memories>".into(),
            ),
            (
                "memories.core".into(),
                "<memories-core>core memory</memories-core>".into(),
            ),
            (
                "memories.project".into(),
                "<memories-project>project memory</memories-project>".into(),
            ),
            (
                "memories.shared".into(),
                "<memories-shared>shared memory</memories-shared>".into(),
            ),
        ]),
        artifact_vars: HashMap::from([
            (
                "artifacts".into(),
                "<artifacts>validation artifact</artifacts>".into(),
            ),
            (
                "artifacts.project".into(),
                "<project><artifact name=\"design.md\" /></project>".into(),
            ),
            (
                "artifacts.workspace".into(),
                "<workspace><artifact name=\"notes.md\" /></workspace>".into(),
            ),
        ]),
        knowledge_vars: HashMap::from([(
            "knowledge.validation.summary".into(),
            "Validation knowledge".into(),
        )]),
        context_blocks: HashMap::new(),
    };
    if let Some(project_git) = &ctx.project.git {
        ctx.context_blocks.insert(
            "project.git".into(),
            nenjo_xml::to_xml_pretty(project_git, 2),
        );
    }

    ctx.to_vars()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_vars_cover_declared_runtime_template_vars() {
        let vars = synthetic_vars();
        let missing = nenjo::context::template_var_defs()
            .into_iter()
            .filter(|var| !vars.contains_key(var.name))
            .map(|var| var.name)
            .collect::<Vec<_>>();

        assert!(
            missing.is_empty(),
            "synthetic validation vars missing declared runtime vars: {missing:?}"
        );
    }

    #[test]
    fn normalizes_named_template_refs_with_partial_spacing() {
        let templates =
            HashMap::from([("pkg.scope.context.block".to_string(), "body".to_string())]);

        assert_eq!(
            normalize_named_template_refs("{{ pkg.scope.context.block}}", &templates),
            "{% include \"pkg/scope/context/block\" %}"
        );
        assert_eq!(
            normalize_named_template_refs("{{pkg.scope.context.block }}", &templates),
            "{% include \"pkg/scope/context/block\" %}"
        );
    }
}
