//! Memory serialization for runtime-owned session context.

use anyhow::Result;

use crate::context::{
    MemoriesContext, MemoriesCoreContext, MemoriesProjectContext, MemoriesSharedContext,
    MemoryCategoryContext,
};

use super::Memory;
use super::types::MemoryScope;

/// Convert memory categories into context structs for XML serialization.
fn categories_to_contexts(
    categories: &[super::types::MemoryCategory],
) -> Vec<MemoryCategoryContext> {
    categories
        .iter()
        .map(|c| {
            let text = c
                .facts
                .iter()
                .map(|f| f.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            MemoryCategoryContext {
                name: c.category.clone(),
                text,
            }
        })
        .collect()
}

/// Build the session memory snapshot from all three tiers.
pub async fn build_memory_context<M>(memory: &M, scope: &MemoryScope) -> Result<String>
where
    M: Memory + ?Sized,
{
    let core_cats = memory.list_categories(&scope.core).await?;
    // Skip project tier if it resolves to the same namespace as core (system agents
    // with no project have both point to `agent_{name}_core`).
    let project_cats = if scope.project == scope.core {
        vec![]
    } else {
        memory.list_categories(&scope.project).await?
    };
    let shared_cats = memory.list_categories(&scope.shared).await?;

    if core_cats.is_empty() && project_cats.is_empty() && shared_cats.is_empty() {
        return Ok(String::new());
    }

    let core = if !core_cats.is_empty() {
        let ctx = MemoriesCoreContext {
            categories: categories_to_contexts(&core_cats),
        };
        Some(ctx)
    } else {
        None
    };

    let project = if !project_cats.is_empty() {
        let ctx = MemoriesProjectContext {
            categories: categories_to_contexts(&project_cats),
        };
        Some(ctx)
    } else {
        None
    };

    let shared = if !shared_cats.is_empty() {
        let ctx = MemoriesSharedContext {
            categories: categories_to_contexts(&shared_cats),
        };
        Some(ctx)
    } else {
        None
    };

    let full = MemoriesContext {
        core,
        project,
        shared,
    };
    Ok(nenjo_xml::to_xml_pretty(&full, 2))
}
