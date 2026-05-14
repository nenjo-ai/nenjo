//! Context block renderer — renders context block templates with template vars.
//!
//! Context blocks are manifest resources, so the provider keeps their template
//! bodies in memory and renders them directly.

use std::collections::HashMap;
use std::sync::Arc;

/// Renders context blocks from templates with template variables.
///
/// Each block is a Jinja template rendered with the same `HashMap<String, String>`
/// vars used for system/developer prompts.
///
/// The rendered output is returned as a map of dotted keys
/// (e.g. `"nenjo.available_agents"`) to rendered strings.
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

    /// Render all context blocks and return a map of dotted key → rendered string.
    pub fn render_all(&self, vars: &HashMap<String, String>) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for block in self.blocks.iter() {
            let rendered = nenjo_xml::template::render_template(&block.template, vars);
            if !rendered.is_empty() {
                let key = Self::dotted_key(&block.path, &block.name);
                map.insert(key, rendered);
            }
        }
        map
    }

    /// Render a single named context block.
    pub fn render(&self, name: &str, vars: &HashMap<String, String>) -> String {
        match self.blocks.iter().find(|b| b.name == name) {
            Some(block) => nenjo_xml::template::render_template(&block.template, vars),
            None => String::new(),
        }
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
            path: "nenjo/core".into(),
            template: "<methodology>{{ self.role }}</methodology>".into(),
        }]);
        let vars = HashMap::from([("self.role".into(), "system".into())]);

        let rendered_blocks = renderer.render_all(&vars);
        let mut prompt_vars = vars.clone();
        prompt_vars.extend(rendered_blocks);

        let prompt =
            nenjo_xml::template::render_template("{{ nenjo.core.methodology }}", &prompt_vars);

        assert_eq!(prompt, "<methodology>system</methodology>");
    }
}
