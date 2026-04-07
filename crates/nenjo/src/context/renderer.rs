//! Context block renderer — renders context block templates with template vars.
//!
//! Context blocks are just templates rendered with the same vars as the main
//! system/developer prompts. The renderer uses a [`TemplateSource`] trait to
//! load templates lazily on demand, avoiding holding all templates in memory.

use std::collections::HashMap;
use std::sync::Arc;

use tracing::warn;

/// A source of context block templates. Implementations load template text
/// by (path, name) — either from memory or from disk.
pub trait TemplateSource: Send + Sync {
    /// Load the template text for a context block. Returns `None` if the
    /// block doesn't exist or can't be read.
    fn load_template(&self, path: &str, name: &str) -> Option<String>;
}

/// Block metadata — lightweight, always in memory.
#[derive(Debug, Clone)]
struct BlockMeta {
    path: String,
    name: String,
}

/// Renders context blocks from templates with template variables.
///
/// Each block is a Jinja template rendered with the same `HashMap<String, String>`
/// vars used for system/developer prompts. Templates are loaded lazily from the
/// [`TemplateSource`] — only when rendering is requested.
///
/// The rendered output is returned as a map of dotted keys
/// (e.g. `"nenjo.available_agents"`) to rendered strings.
#[derive(Clone)]
pub struct ContextRenderer {
    blocks: Vec<BlockMeta>,
    source: Arc<dyn TemplateSource>,
}

impl ContextRenderer {
    /// Create a renderer with a template source and block metadata.
    pub fn new(source: Arc<dyn TemplateSource>, blocks: Vec<(String, String)>) -> Self {
        let metas = blocks
            .into_iter()
            .map(|(path, name)| BlockMeta { path, name })
            .collect();
        Self {
            blocks: metas,
            source,
        }
    }

    /// Create a renderer backed by an in-memory template map.
    ///
    /// This is the backward-compatible constructor used when all templates
    /// are already loaded (e.g. from the API or tests).
    pub fn from_blocks(blocks: &[super::types::RenderContextBlock]) -> Self {
        let source = Arc::new(InMemoryTemplateSource::from_blocks(blocks));
        let metas = blocks
            .iter()
            .map(|b| BlockMeta {
                path: b.path.clone(),
                name: b.name.clone(),
            })
            .collect();
        Self {
            blocks: metas,
            source,
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
    ///
    /// Templates are loaded lazily from the source on each call. Blocks whose
    /// templates cannot be loaded are silently skipped.
    pub fn render_all(&self, vars: &HashMap<String, String>) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for block in &self.blocks {
            let template = match self.source.load_template(&block.path, &block.name) {
                Some(t) => t,
                None => {
                    warn!(path = %block.path, name = %block.name, "Context block template not found");
                    continue;
                }
            };
            let rendered = nenjo_xml::template::render_template(&template, vars);
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
            Some(block) => match self.source.load_template(&block.path, &block.name) {
                Some(template) => nenjo_xml::template::render_template(&template, vars),
                None => String::new(),
            },
            None => String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// In-memory template source (backward compat / tests)
// ---------------------------------------------------------------------------

/// A template source that holds all templates in memory.
#[derive(Clone)]
pub struct InMemoryTemplateSource {
    /// Map of `"path\0name"` → template text.
    templates: HashMap<String, String>,
}

impl InMemoryTemplateSource {
    fn key(path: &str, name: &str) -> String {
        format!("{path}\0{name}")
    }

    pub fn from_blocks(blocks: &[super::types::RenderContextBlock]) -> Self {
        let templates = blocks
            .iter()
            .map(|b| (Self::key(&b.path, &b.name), b.template.clone()))
            .collect();
        Self { templates }
    }
}

impl TemplateSource for InMemoryTemplateSource {
    fn load_template(&self, path: &str, name: &str) -> Option<String> {
        self.templates.get(&Self::key(path, name)).cloned()
    }
}
