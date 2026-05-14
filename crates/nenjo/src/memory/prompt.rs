//! Memory & artifact → prompt template variable injection.

use std::collections::HashMap;

use anyhow::Result;

use crate::context::{
    ArtifactContext, ArtifactsContext, ArtifactsProjectContext, ArtifactsWorkspaceContext,
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

/// Build memory template variables from all 3 tiers.
///
/// Returns a `HashMap` with keys: `memories`, `memories.core`,
/// `memories.project`, `memories.shared` (only non-empty tiers).
pub async fn build_memory_vars<M>(
    memory: &M,
    scope: &MemoryScope,
) -> Result<HashMap<String, String>>
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

    let mut vars = HashMap::new();

    if core_cats.is_empty() && project_cats.is_empty() && shared_cats.is_empty() {
        return Ok(vars);
    }

    let core = if !core_cats.is_empty() {
        let ctx = MemoriesCoreContext {
            categories: categories_to_contexts(&core_cats),
        };
        vars.insert(
            "memories.core".to_string(),
            nenjo_xml::to_xml_pretty(&ctx, 2),
        );
        Some(ctx)
    } else {
        None
    };

    let project = if !project_cats.is_empty() {
        let ctx = MemoriesProjectContext {
            categories: categories_to_contexts(&project_cats),
        };
        vars.insert(
            "memories.project".to_string(),
            nenjo_xml::to_xml_pretty(&ctx, 2),
        );
        Some(ctx)
    } else {
        None
    };

    let shared = if !shared_cats.is_empty() {
        let ctx = MemoriesSharedContext {
            categories: categories_to_contexts(&shared_cats),
        };
        vars.insert(
            "memories.shared".to_string(),
            nenjo_xml::to_xml_pretty(&ctx, 2),
        );
        Some(ctx)
    } else {
        None
    };

    let full = MemoriesContext {
        core,
        project,
        shared,
    };
    vars.insert("memories".to_string(), nenjo_xml::to_xml_pretty(&full, 2));

    Ok(vars)
}

/// Build artifact template variables from project + workspace scopes.
///
/// Returns a `HashMap` with keys: `artifacts`, `artifacts.project`,
/// `artifacts.workspace` (only non-empty scopes).
pub async fn build_artifact_vars<M>(
    memory: &M,
    scope: &MemoryScope,
) -> Result<HashMap<String, String>>
where
    M: Memory + ?Sized,
{
    // Skip project tier if it resolves to the same path as global (system agents).
    let project_entries = if scope.artifacts_project == scope.artifacts_global {
        vec![]
    } else {
        memory.list_artifacts(&scope.artifacts_project).await?
    };
    let workspace_entries = memory.list_artifacts(&scope.artifacts_global).await?;

    let mut vars = HashMap::new();

    if project_entries.is_empty() && workspace_entries.is_empty() {
        return Ok(vars);
    }

    fn entries_to_contexts(entries: &[super::types::ArtifactEntry]) -> Vec<ArtifactContext> {
        entries
            .iter()
            .map(|e| ArtifactContext {
                name: e.filename.clone(),
                description: e.description.clone(),
                created_by: e.created_by.clone(),
                size: format_size(e.size_bytes),
            })
            .collect()
    }

    let project = if !project_entries.is_empty() {
        let ctx = ArtifactsProjectContext {
            artifacts: entries_to_contexts(&project_entries),
        };
        vars.insert(
            "artifacts.project".to_string(),
            nenjo_xml::to_xml_pretty(&ctx, 2),
        );
        Some(ctx)
    } else {
        None
    };

    let workspace = if !workspace_entries.is_empty() {
        let ctx = ArtifactsWorkspaceContext {
            artifacts: entries_to_contexts(&workspace_entries),
        };
        vars.insert(
            "artifacts.workspace".to_string(),
            nenjo_xml::to_xml_pretty(&ctx, 2),
        );
        Some(ctx)
    } else {
        None
    };

    let full = ArtifactsContext { project, workspace };
    vars.insert("artifacts".to_string(), nenjo_xml::to_xml_pretty(&full, 2));

    Ok(vars)
}

fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
