//! Embedded Nenjo knowledge pack.
//!
//! The builtin pack is a read-only virtual document tree rooted at
//! `builtin://nenjo/`. It gives agents a stable, product-versioned reference
//! for platform concepts and design patterns without requiring hosted docs.

mod embedded;
mod generated;
mod search;
mod types;

pub use embedded::{
    BUILTIN_KNOWLEDGE_DISCOVERY, builtin_knowledge_pack, builtin_knowledge_summary,
};
pub use types::*;
