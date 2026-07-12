//! Context block renderer — renders context block templates with template vars.
//!
//! Context blocks are manifest resources, so the provider keeps their template
//! bodies in memory and renders them directly.
//!
//! Multi-version package content may coexist under versioned storage paths.
//! Unversioned logical selectors (e.g. `pkg.nenjo_ai.packages.context.tools.tool_usage`)
//! resolve via [`PkgResolvePolicy`].

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use regex::{Captures, Regex};

use crate::arguments::scan_argument_selectors;
use crate::package_resolve::{
    PkgResolvePolicy, VersionedCandidate, logical_dotted_key, resolve_all_logical_winners,
    version_label_from_path,
};

/// Renders context blocks from templates with template variables.
///
/// Each block is a Jinja template rendered with the same `HashMap<String, String>`
/// vars used for system/developer prompts.
///
/// The rendered output is returned as a map of dotted keys
/// (e.g. `"nenjo.project"`) to rendered strings.
#[derive(Clone)]
pub struct ContextRenderer {
    blocks: Arc<[super::types::RenderContextBlock]>,
    policy: PkgResolvePolicy,
}

impl ContextRenderer {
    pub fn from_blocks(blocks: &[super::types::RenderContextBlock]) -> Self {
        Self::from_blocks_with_policy(blocks, PkgResolvePolicy::HighestSemver)
    }

    pub fn from_blocks_with_policy(
        blocks: &[super::types::RenderContextBlock],
        policy: PkgResolvePolicy,
    ) -> Self {
        Self {
            blocks: Arc::from(blocks.to_vec().into_boxed_slice()),
            policy,
        }
    }

    /// Clone this renderer with a different multi-version resolve policy.
    pub fn with_policy(&self, policy: PkgResolvePolicy) -> Self {
        Self {
            blocks: self.blocks.clone(),
            policy,
        }
    }

    pub fn policy(&self) -> &PkgResolvePolicy {
        &self.policy
    }

    fn dotted_key(path: &str, name: &str) -> String {
        if path.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", path.replace('/', "."), name)
        }
    }

    fn template_key(path: &str, name: &str) -> String {
        if path.is_empty() {
            name.to_string()
        } else {
            format!("{path}/{name}")
        }
    }

    fn candidate_for(block: &super::types::RenderContextBlock) -> VersionedCandidate {
        VersionedCandidate {
            package_name: block.package_name.clone(),
            package_version: block
                .package_version
                .clone()
                .or_else(|| version_label_from_path(&block.path)),
            path: block.path.clone(),
            name: block.name.clone(),
        }
    }

    /// Map logical + versioned dotted keys / short names → template include key.
    fn reference_map(&self) -> HashMap<String, String> {
        let mut refs = HashMap::new();
        let candidates: Vec<(usize, VersionedCandidate)> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (i, Self::candidate_for(b)))
            .collect();

        // Exact versioned keys always available.
        for block in self.blocks.iter() {
            let dotted_key = Self::dotted_key(&block.path, &block.name);
            let template_key = Self::template_key(&block.path, &block.name);
            refs.insert(dotted_key, template_key.clone());
            refs.entry(format!("context.{}", block.name))
                .or_insert(template_key);
        }

        // Logical unversioned keys → policy winner.
        let winners = resolve_all_logical_winners(&candidates, &self.policy);
        for (logical_key, idx) in winners {
            let block = &self.blocks[idx];
            let template_key = Self::template_key(&block.path, &block.name);
            refs.insert(logical_key, template_key.clone());
            // Short context.name prefers the policy winner.
            refs.insert(format!("context.{}", block.name), template_key);
        }

        // Also expose logical path form without forcing a name collision.
        for block in self.blocks.iter() {
            let logical = logical_dotted_key(&block.path, &block.name);
            if !refs.contains_key(&logical) {
                // No winner registered (shouldn't happen if candidate was included).
                continue;
            }
        }

        refs
    }

    fn named_templates(&self) -> HashMap<String, String> {
        let refs = self.reference_map();
        let mut templates = HashMap::new();
        for block in self.blocks.iter() {
            let dotted_key = Self::dotted_key(&block.path, &block.name);
            let template_key = Self::template_key(&block.path, &block.name);
            let template = Self::normalize_context_refs(&block.template, &refs);
            templates.insert(template_key, template.clone());
            templates.entry(dotted_key).or_insert(template.clone());
            // Logical key also maps to the same rendered template body for includes.
            let logical = logical_dotted_key(&block.path, &block.name);
            templates.entry(logical).or_insert(template);
        }
        templates
    }

    fn normalize_context_refs(template: &str, refs: &HashMap<String, String>) -> String {
        static REF_RE: OnceLock<Regex> = OnceLock::new();
        let ref_re = REF_RE.get_or_init(|| {
            Regex::new(r"\{\{\s*([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\s*\}\}")
                .expect("valid context block reference regex")
        });

        ref_re
            .replace_all(template, |captures: &Captures<'_>| {
                let Some(matched) = captures.get(0) else {
                    return String::new();
                };
                let preceding_backslashes = template[..matched.start()]
                    .chars()
                    .rev()
                    .take_while(|c| *c == '\\')
                    .count();
                if preceding_backslashes % 2 == 1 {
                    return matched.as_str().to_string();
                }
                let reference = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
                match refs.get(reference) {
                    Some(template_key) => format!("{{% include \"{}\" %}}", template_key),
                    None => matched.as_str().to_string(),
                }
            })
            .into_owned()
    }

    /// Render all context blocks and return a map of dotted key → rendered string.
    pub fn render_all(&self, vars: &HashMap<String, String>) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let templates = self.named_templates();
        let candidates: Vec<(usize, VersionedCandidate)> = self
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (i, Self::candidate_for(b)))
            .collect();
        let winners = resolve_all_logical_winners(&candidates, &self.policy);

        for block in self.blocks.iter() {
            let template_key = Self::template_key(&block.path, &block.name);
            let rendered = nenjo_xml::template::render_template_with_named_templates(
                &format!("{{% include \"{}\" %}}", template_key),
                vars,
                &templates,
            );
            if !rendered.is_empty() {
                let key = Self::dotted_key(&block.path, &block.name);
                map.insert(key, rendered.clone());
                let logical = logical_dotted_key(&block.path, &block.name);
                map.entry(logical).or_insert(rendered.clone());
                map.entry(format!("context.{}", block.name))
                    .or_insert(rendered);
            }
        }

        // Ensure logical keys for winners are present even if or_insert skipped.
        for (logical_key, idx) in winners {
            let block = &self.blocks[idx];
            let template_key = Self::template_key(&block.path, &block.name);
            let rendered = nenjo_xml::template::render_template_with_named_templates(
                &format!("{{% include \"{}\" %}}", template_key),
                vars,
                &templates,
            );
            if !rendered.is_empty() {
                map.insert(logical_key, rendered.clone());
                map.insert(format!("context.{}", block.name), rendered);
            }
        }
        map
    }

    /// Render a single named context block.
    pub fn render(&self, name: &str, vars: &HashMap<String, String>) -> String {
        let candidates: Vec<(usize, VersionedCandidate)> = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.name == name)
            .map(|(i, b)| (i, Self::candidate_for(b)))
            .collect();
        let idx = if candidates.len() <= 1 {
            candidates.first().map(|(i, _)| *i)
        } else {
            // Multiple versions share the short name — apply policy via synthetic key.
            let key = candidates
                .first()
                .map(|(_, c)| c.logical_dotted_key())
                .unwrap_or_default();
            resolve_all_logical_winners(&candidates, &self.policy)
                .get(&key)
                .copied()
                .or_else(|| candidates.first().map(|(i, _)| *i))
        };
        match idx.and_then(|i| self.blocks.get(i)) {
            Some(block) => {
                let templates = self.named_templates();
                let template_key = Self::template_key(&block.path, &block.name);
                nenjo_xml::template::render_template_with_named_templates(
                    &format!("{{% include \"{}\" %}}", template_key),
                    vars,
                    &templates,
                )
            }
            None => String::new(),
        }
    }

    /// Render an arbitrary prompt template with context-block includes enabled.
    ///
    /// Exact references to known context blocks, such as
    /// `{{ pkg.nenjo.core.methodology }}`, are normalized to MiniJinja includes.
    /// Other variables and expressions are left as-is.
    pub fn render_template(&self, template: &str, vars: &HashMap<String, String>) -> String {
        let refs = self.reference_map();
        let templates = self.named_templates();
        let normalized = Self::normalize_context_refs(template, &refs);
        nenjo_xml::template::render_template_with_named_templates(&normalized, vars, &templates)
    }

    /// Return all `args.*` selectors referenced by context block templates.
    pub fn argument_selectors(&self) -> Vec<String> {
        let mut selectors = self
            .blocks
            .iter()
            .flat_map(|block| scan_argument_selectors(&block.template))
            .collect::<Vec<_>>();
        selectors.sort();
        selectors.dedup();
        selectors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::types::RenderContextBlock;
    use std::collections::BTreeMap;

    fn block(path: &str, name: &str, template: &str) -> RenderContextBlock {
        RenderContextBlock {
            name: name.into(),
            path: path.into(),
            template: template.into(),
            package_name: None,
            package_version: None,
        }
    }

    fn versioned_block(
        path: &str,
        name: &str,
        template: &str,
        package: &str,
        version: &str,
    ) -> RenderContextBlock {
        RenderContextBlock {
            name: name.into(),
            path: path.into(),
            template: template.into(),
            package_name: Some(package.into()),
            package_version: Some(version.into()),
        }
    }

    #[test]
    fn renders_path_blocks_as_nested_dotted_vars() {
        let renderer = ContextRenderer::from_blocks(&[block(
            "pkg/nenjo/core",
            "methodology",
            "<methodology>{{ self.role }}</methodology>",
        )]);
        let vars = HashMap::from([("self.role".into(), "system".into())]);

        let rendered_blocks = renderer.render_all(&vars);
        let mut prompt_vars = vars.clone();
        prompt_vars.extend(rendered_blocks);
        let rendered =
            nenjo_xml::template::render_template("{{ pkg.nenjo.core.methodology }}", &prompt_vars);
        assert!(rendered.contains("system"));
    }

    #[test]
    fn unversioned_selector_prefers_highest_semver() {
        let renderer = ContextRenderer::from_blocks(&[
            versioned_block(
                "pkg/nenjo_ai/packages/v1_0_3/context/tools",
                "tool_usage",
                "BODY-V103",
                "context",
                "1.0.3",
            ),
            versioned_block(
                "pkg/nenjo_ai/packages/v1_0_4/context/tools",
                "tool_usage",
                "BODY-V104",
                "context",
                "1.0.4",
            ),
        ]);
        let rendered = renderer.render_template(
            "{{ pkg.nenjo_ai.packages.context.tools.tool_usage }}",
            &HashMap::new(),
        );
        assert_eq!(rendered.trim(), "BODY-V104");
    }

    #[test]
    fn unversioned_selector_respects_dependency_lock() {
        let mut lock = BTreeMap::new();
        lock.insert("context".to_string(), "1.0.3".to_string());
        let renderer = ContextRenderer::from_blocks_with_policy(
            &[
                versioned_block(
                    "pkg/nenjo_ai/packages/v1_0_3/context/tools",
                    "tool_usage",
                    "BODY-V103",
                    "@nenjo-ai/context",
                    "1.0.3",
                ),
                versioned_block(
                    "pkg/nenjo_ai/packages/v1_0_4/context/tools",
                    "tool_usage",
                    "BODY-V104",
                    "@nenjo-ai/context",
                    "1.0.4",
                ),
            ],
            PkgResolvePolicy::DependencyLock(lock),
        );
        let rendered = renderer.render_template(
            "{{ pkg.nenjo_ai.packages.context.tools.tool_usage }}",
            &HashMap::new(),
        );
        assert_eq!(rendered.trim(), "BODY-V103");
    }

    #[test]
    fn versioned_selector_still_targets_exact_instance() {
        let renderer = ContextRenderer::from_blocks(&[
            versioned_block(
                "pkg/nenjo_ai/packages/v1_0_3/context/tools",
                "tool_usage",
                "BODY-V103",
                "context",
                "1.0.3",
            ),
            versioned_block(
                "pkg/nenjo_ai/packages/v1_0_4/context/tools",
                "tool_usage",
                "BODY-V104",
                "context",
                "1.0.4",
            ),
        ]);
        let rendered = renderer.render_template(
            "{{ pkg.nenjo_ai.packages.v1_0_3.context.tools.tool_usage }}",
            &HashMap::new(),
        );
        assert_eq!(rendered.trim(), "BODY-V103");
    }

    #[test]
    fn renders_nested_context_includes() {
        let renderer = ContextRenderer::from_blocks(&[
            block("pkg/nenjo/core", "methodology", "METHOD"),
            block(
                "pkg/nenjo/core",
                "summary",
                "<summary>{{ pkg.nenjo.core.methodology }}</summary>",
            ),
        ]);
        let rendered_blocks = renderer.render_all(&HashMap::new());
        assert!(
            rendered_blocks
                .get("pkg.nenjo.core.summary")
                .is_some_and(|v| v.contains("METHOD"))
        );
    }

    #[test]
    fn renders_chained_context_includes() {
        let renderer = ContextRenderer::from_blocks(&[
            block("pkg/nenjo/core", "base", "BASE"),
            block(
                "pkg/nenjo/core",
                "middle",
                "middle[{{ pkg.nenjo.core.base }}]",
            ),
            block("pkg/nenjo/core", "top", "top[{{ pkg.nenjo.core.middle }}]"),
        ]);
        let rendered_blocks = renderer.render_all(&HashMap::new());
        assert!(
            rendered_blocks
                .get("pkg.nenjo.core.top")
                .is_some_and(|v| v.contains("BASE") && v.contains("middle"))
        );
    }

    #[test]
    fn render_template_mixes_agent_vars_and_context_includes() {
        let renderer =
            ContextRenderer::from_blocks(&[block("pkg/nenjo/core", "methodology", "METHOD")]);
        let vars = HashMap::from([("agent.name".into(), "Nenji".into())]);
        let rendered = renderer.render_template(
            "System: {{ agent.name }}\n{{ pkg.nenjo.core.methodology }}",
            &vars,
        );
        assert!(rendered.contains("Nenji"));
        assert!(rendered.contains("METHOD"));
    }

    #[test]
    fn render_template_supports_include_and_dotted_forms() {
        let renderer =
            ContextRenderer::from_blocks(&[block("pkg/nenjo/core", "methodology", "METHOD")]);
        let rendered = renderer.render_template(
            r#"{% include "pkg/nenjo/core/methodology" %} {% include "pkg.nenjo.core.methodology" %}"#,
            &HashMap::new(),
        );
        assert!(rendered.contains("METHOD"));
    }

    #[test]
    fn render_template_preserves_filters_on_non_context_vars() {
        let renderer =
            ContextRenderer::from_blocks(&[block("pkg/nenjo/core", "methodology", "METHOD")]);
        let vars = HashMap::from([("pkg.nenjo.core.methodology_label".into(), "label".into())]);
        // Filter expressions are left for MiniJinja (not rewritten to includes).
        let rendered = renderer.render_template(
            "{{ pkg.nenjo.core.methodology_label }} {{ pkg.nenjo.core.methodology }}",
            &vars,
        );
        assert!(rendered.contains("label"));
        assert!(rendered.contains("METHOD"));
    }

    #[test]
    fn render_template_preserves_escaped_braces() {
        let renderer =
            ContextRenderer::from_blocks(&[block("pkg/nenjo/core", "methodology", "METHOD")]);
        let rendered = renderer.render_template(
            r"literal \{{ pkg.nenjo.core.methodology }}",
            &HashMap::new(),
        );
        assert_eq!(rendered, "literal {{ pkg.nenjo.core.methodology }}");
    }
}
