//! Context block renderer — renders context block templates with template vars.
//!
//! Context blocks are manifest resources, so the provider keeps their template
//! bodies in memory and renders them directly.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use regex::{Captures, Regex};

use crate::arguments::scan_argument_selectors;

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
}

impl ContextRenderer {
    pub fn from_blocks(blocks: &[super::types::RenderContextBlock]) -> Self {
        Self {
            blocks: Arc::from(blocks.to_vec().into_boxed_slice()),
        }
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

    fn reference_map(&self) -> HashMap<String, String> {
        let mut refs = HashMap::new();
        for block in self.blocks.iter() {
            let dotted_key = Self::dotted_key(&block.path, &block.name);
            let template_key = Self::template_key(&block.path, &block.name);
            refs.insert(dotted_key, template_key.clone());
            refs.entry(format!("context.{}", block.name))
                .or_insert(template_key);
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
            templates.entry(dotted_key).or_insert(template);
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
                map.entry(format!("context.{}", block.name))
                    .or_insert(rendered);
            }
        }
        map
    }

    /// Render a single named context block.
    pub fn render(&self, name: &str, vars: &HashMap<String, String>) -> String {
        match self.blocks.iter().find(|b| b.name == name) {
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

    #[test]
    fn renders_path_blocks_as_nested_dotted_vars() {
        let renderer = ContextRenderer::from_blocks(&[RenderContextBlock {
            name: "methodology".into(),
            path: "pkg/nenjo/core".into(),
            template: "<methodology>{{ self.role }}</methodology>".into(),
        }]);
        let vars = HashMap::from([("self.role".into(), "system".into())]);

        let rendered_blocks = renderer.render_all(&vars);
        let mut prompt_vars = vars.clone();
        prompt_vars.extend(rendered_blocks);

        let prompt =
            nenjo_xml::template::render_template("{{ pkg.nenjo.core.methodology }}", &prompt_vars);

        assert_eq!(prompt, "<methodology>system</methodology>");
    }

    #[test]
    fn renders_context_name_alias_for_imported_blocks() {
        let renderer = ContextRenderer::from_blocks(&[RenderContextBlock {
            name: "methodology".into(),
            path: "shared/context".into(),
            template: "<methodology>{{ self.role }}</methodology>".into(),
        }]);
        let vars = HashMap::from([("self.role".into(), "system".into())]);

        let rendered_blocks = renderer.render_all(&vars);

        assert_eq!(
            rendered_blocks
                .get("context.methodology")
                .map(String::as_str),
            Some("<methodology>system</methodology>")
        );
    }

    #[test]
    fn renders_context_blocks_that_reference_other_context_blocks() {
        let renderer = ContextRenderer::from_blocks(&[
            RenderContextBlock {
                name: "methodology".into(),
                path: "pkg/nenjo/core".into(),
                template: "<methodology>{{ agent.name }}</methodology>".into(),
            },
            RenderContextBlock {
                name: "summary".into(),
                path: "pkg/nenjo/core".into(),
                template: "<summary>{{ pkg.nenjo.core.methodology }}</summary>".into(),
            },
        ]);
        let vars = HashMap::from([("agent.name".into(), "Nenji".into())]);

        let rendered_blocks = renderer.render_all(&vars);

        assert_eq!(
            rendered_blocks
                .get("pkg.nenjo.core.summary")
                .map(String::as_str),
            Some("<summary><methodology>Nenji</methodology></summary>")
        );
    }

    #[test]
    fn renders_multi_level_context_block_dependencies() {
        let renderer = ContextRenderer::from_blocks(&[
            RenderContextBlock {
                name: "base".into(),
                path: "pkg/nenjo/core".into(),
                template: "base={{ agent.name }}".into(),
            },
            RenderContextBlock {
                name: "middle".into(),
                path: "pkg/nenjo/core".into(),
                template: "middle[{{ pkg.nenjo.core.base }}]".into(),
            },
            RenderContextBlock {
                name: "top".into(),
                path: "pkg/nenjo/core".into(),
                template: "top[{{ pkg.nenjo.core.middle }}]".into(),
            },
        ]);
        let vars = HashMap::from([("agent.name".into(), "Nenji".into())]);

        let rendered_blocks = renderer.render_all(&vars);

        assert_eq!(
            rendered_blocks
                .get("pkg.nenjo.core.top")
                .map(String::as_str),
            Some("top[middle[base=Nenji]]")
        );
    }

    #[test]
    fn renders_context_alias_references_inside_context_blocks() {
        let renderer = ContextRenderer::from_blocks(&[
            RenderContextBlock {
                name: "methodology".into(),
                path: "pkg/nenjo/core".into(),
                template: "methodology={{ agent.name }}".into(),
            },
            RenderContextBlock {
                name: "summary".into(),
                path: "local".into(),
                template: "summary[{{ context.methodology }}]".into(),
            },
        ]);
        let vars = HashMap::from([("agent.name".into(), "Nenji".into())]);

        let rendered_blocks = renderer.render_all(&vars);

        assert_eq!(
            rendered_blocks.get("local.summary").map(String::as_str),
            Some("summary[methodology=Nenji]")
        );
    }

    #[test]
    fn renders_root_context_blocks_by_name() {
        let renderer = ContextRenderer::from_blocks(&[
            RenderContextBlock {
                name: "root".into(),
                path: String::new(),
                template: "root={{ agent.name }}".into(),
            },
            RenderContextBlock {
                name: "wrapper".into(),
                path: String::new(),
                template: "wrapper[{{ root }}]".into(),
            },
        ]);
        let vars = HashMap::from([("agent.name".into(), "Nenji".into())]);

        let rendered_blocks = renderer.render_all(&vars);

        assert_eq!(
            rendered_blocks.get("wrapper").map(String::as_str),
            Some("wrapper[root=Nenji]")
        );
    }

    #[test]
    fn prompt_rendering_normalizes_context_block_refs() {
        let renderer = ContextRenderer::from_blocks(&[RenderContextBlock {
            name: "methodology".into(),
            path: "pkg/nenjo/core".into(),
            template: "<methodology>{{ agent.name }}</methodology>".into(),
        }]);
        let vars = HashMap::from([("agent.name".into(), "Nenji".into())]);

        let rendered = renderer.render_template(
            "System: {{ agent.name }}\n{{ pkg.nenjo.core.methodology }}",
            &vars,
        );

        assert_eq!(rendered, "System: Nenji\n<methodology>Nenji</methodology>");
    }

    #[test]
    fn supports_direct_minijinja_include_syntax() {
        let renderer = ContextRenderer::from_blocks(&[RenderContextBlock {
            name: "methodology".into(),
            path: "pkg/nenjo/core".into(),
            template: "<methodology>{{ agent.name }}</methodology>".into(),
        }]);
        let vars = HashMap::from([("agent.name".into(), "Nenji".into())]);

        let rendered = renderer.render_template(
            r#"{% include "pkg/nenjo/core/methodology" %} {% include "pkg.nenjo.core.methodology" %}"#,
            &vars,
        );

        assert_eq!(
            rendered,
            "<methodology>Nenji</methodology> <methodology>Nenji</methodology>"
        );
    }

    #[test]
    fn only_normalizes_exact_context_block_references() {
        let renderer = ContextRenderer::from_blocks(&[RenderContextBlock {
            name: "methodology".into(),
            path: "pkg/nenjo/core".into(),
            template: "<methodology>base</methodology>".into(),
        }]);
        let vars = HashMap::from([("pkg.nenjo.core.methodology_label".into(), "label".into())]);

        let rendered = renderer.render_template(
            "{{ pkg.nenjo.core.methodology_label }} {{ pkg.nenjo.core.methodology | e }}",
            &vars,
        );

        assert_eq!(rendered, "label ");
    }

    #[test]
    fn escaped_context_block_refs_remain_literal() {
        let renderer = ContextRenderer::from_blocks(&[RenderContextBlock {
            name: "methodology".into(),
            path: "pkg/nenjo/core".into(),
            template: "<methodology>base</methodology>".into(),
        }]);

        let rendered = renderer.render_template(
            r"literal \{{ pkg.nenjo.core.methodology }}",
            &HashMap::new(),
        );

        assert_eq!(rendered, "literal {{ pkg.nenjo.core.methodology }}");
    }

    #[test]
    fn unknown_direct_includes_fall_back_to_original_prompt() {
        let renderer = ContextRenderer::from_blocks(&[]);

        let rendered =
            renderer.render_template(r#"{% include "missing/block" %}"#, &HashMap::new());

        assert_eq!(rendered, r#"{% include "missing/block" %}"#);
    }

    #[test]
    fn include_cycles_fall_back_to_the_including_template() {
        let renderer = ContextRenderer::from_blocks(&[
            RenderContextBlock {
                name: "a".into(),
                path: "cycle".into(),
                template: "{{ cycle.b }}".into(),
            },
            RenderContextBlock {
                name: "b".into(),
                path: "cycle".into(),
                template: "{{ cycle.a }}".into(),
            },
        ]);

        let rendered = renderer.render_template("{{ cycle.a }}", &HashMap::new());

        assert_eq!(rendered, r#"{% include "cycle/a" %}"#);
    }
}
