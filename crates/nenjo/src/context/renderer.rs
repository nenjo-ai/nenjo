//! Context block renderer — renders context block templates with template vars.
//!
//! Context blocks are just templates rendered with the same vars as the main
//! system/developer prompts. No special logic — every block is a Jinja template.

use std::collections::HashMap;

use super::types::RenderContextBlock;

#[derive(Debug, Clone)]
struct StoredBlock {
    path: String,
    name: String,
    template: String,
}

/// Renders context blocks from DB templates with template variables.
///
/// Each block is a Jinja template rendered with the same `HashMap<String, String>`
/// vars used for system/developer prompts. The rendered output is returned as a
/// map of dotted keys (e.g. `"nenjo.available_agents"`) to rendered strings.
#[derive(Clone)]
pub struct ContextRenderer {
    blocks: Vec<StoredBlock>,
}

impl ContextRenderer {
    pub fn from_blocks(blocks: &[RenderContextBlock]) -> Self {
        let stored = blocks
            .iter()
            .map(|b| StoredBlock {
                path: b.path.clone(),
                name: b.name.clone(),
                template: b.template.clone(),
            })
            .collect();
        Self { blocks: stored }
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
        for block in &self.blocks {
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
