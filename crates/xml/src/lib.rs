//! Generic prompt rendering: XML serialization, template engine.
//!
//! This crate provides infrastructure for building structured LLM prompts.
//! It has no knowledge of any specific domain — callers provide template
//! variables as a `HashMap<String, String>` with dotted keys.

pub mod template;
pub mod types;
pub mod xml;

pub use types::{metadata_json_to_xml, render_items};
pub use xml::{to_xml, to_xml_pretty, xml_escape, xml_unescape};
