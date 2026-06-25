use anyhow::Context;
use nenjo::Slug;
use nenjo::routines::graph::{
    RoutineGraph, RoutineGraphEdge, RoutineGraphEdgeCondition, RoutineGraphStep,
    RoutineGraphStepType, RoutineValidationError, validate_routine_graph,
};
use serde::Deserialize;

use crate::{PackageKind, ResolvedModule, ResolvedPackage, validate_source_path};

use super::assignments::find_module_by_source_path;

pub(crate) fn validate_routine_manifest(
    packages: &std::collections::BTreeMap<String, ResolvedPackage>,
    module: &ResolvedModule,
) -> anyhow::Result<()> {
    if module.kind != PackageKind::Routine {
        return Ok(());
    }
    let routine: PackageRoutineManifest = serde_json::from_value(module.manifest.manifest.clone())
        .with_context(|| format!("{} routine manifest has invalid shape", module.path))?;
    validate_routine_references(packages, &routine)?;
    let graph = package_routine_graph(&routine).with_context(|| {
        format!(
            "{} could not be adapted to the canonical routine graph contract",
            module.path
        )
    })?;
    validate_routine_graph(&graph)
        .map_err(|error| anyhow::anyhow!("{}", format_routine_validation_error(&error)))?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct PackageRoutineManifest {
    #[serde(default)]
    entry_steps: Vec<String>,
    #[serde(default)]
    metadata: serde_json::Value,
    #[serde(default)]
    steps: Vec<PackageRoutineStep>,
    #[serde(default)]
    edges: Vec<PackageRoutineEdge>,
}

#[derive(Debug, Deserialize)]
struct PackageRoutineStep {
    #[serde(default, rename = "ref", alias = "slug")]
    step_ref: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type", alias = "step_type")]
    step_type: String,
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    council: Option<String>,
    #[serde(default)]
    lambda: Option<String>,
    #[serde(default)]
    lambda_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackageRoutineEdge {
    #[serde(default, alias = "source_step")]
    from: String,
    #[serde(default, alias = "target_step")]
    to: String,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    metadata: serde_json::Value,
}

fn validate_routine_references(
    packages: &std::collections::BTreeMap<String, ResolvedPackage>,
    routine: &PackageRoutineManifest,
) -> anyhow::Result<()> {
    for step in &routine.steps {
        if step.step_type.trim() == "lambda" {
            anyhow::bail!(
                "routine step '{}' uses unsupported step type 'lambda'",
                step.step_ref
            );
        }
        if let Some(agent) = step
            .agent
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            let path = validate_source_path(agent)?;
            let Some(target) = find_module_by_source_path(packages, &path) else {
                anyhow::bail!(
                    "routine step '{}' references agent package path '{path}' that was not resolved",
                    step.step_ref
                );
            };
            if target.kind != PackageKind::Agent {
                anyhow::bail!(
                    "routine step '{}' references {path}, but it is {} not agent",
                    step.step_ref,
                    target.kind.as_str()
                );
            }
        }
        if let Some(council) = step
            .council
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            validate_source_path(council)?;
        }
    }
    Ok(())
}

fn package_routine_graph(routine: &PackageRoutineManifest) -> anyhow::Result<RoutineGraph> {
    let entry_steps = routine_entry_steps(routine)
        .into_iter()
        .enumerate()
        .map(|(index, value)| parse_routine_slug(&value, &format!("entry_steps[{index}]")))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let steps = routine
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let slug = step.step_ref.trim().to_string();
            if slug.is_empty() {
                anyhow::bail!("steps[{index}] must define ref");
            }
            let step_type =
                RoutineGraphStepType::parse(step.step_type.trim()).ok_or_else(|| {
                    anyhow::anyhow!(
                        "steps[{index}] ref '{slug}' has unsupported type '{}'",
                        step.step_type
                    )
                })?;
            Ok(RoutineGraphStep {
                slug: parse_routine_slug(&slug, &format!("steps[{index}].ref"))?,
                name: if step.name.trim().is_empty() {
                    slug.clone()
                } else {
                    step.name.clone()
                },
                step_type,
                has_agent: step
                    .agent
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty()),
                has_council: step
                    .council
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty()),
                has_lambda: step
                    .lambda
                    .as_ref()
                    .is_some_and(|value| !value.trim().is_empty())
                    || step
                        .lambda_id
                        .as_ref()
                        .is_some_and(|value| !value.trim().is_empty()),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let edges = routine
        .edges
        .iter()
        .enumerate()
        .map(|(index, edge)| {
            let from = edge.from.trim();
            let to = edge.to.trim();
            if from.is_empty() {
                anyhow::bail!("edges[{index}] must define from");
            }
            if to.is_empty() {
                anyhow::bail!("edges[{index}] must define to");
            }
            let condition = edge.condition.as_deref().unwrap_or("always");
            let condition =
                RoutineGraphEdgeCondition::parse(condition.trim()).ok_or_else(|| {
                    anyhow::anyhow!("edges[{index}] has unsupported condition '{}'", condition)
                })?;
            let mut metadata = normalize_edge_metadata(&edge.metadata)?;
            if let Some(max_attempts) = edge.max_attempts {
                let object = metadata
                    .as_object_mut()
                    .expect("normalize_edge_metadata returns an object");
                object.insert("max_attempts".to_string(), serde_json::json!(max_attempts));
            }
            Ok(RoutineGraphEdge {
                source_step: parse_routine_slug(from, &format!("edges[{index}].from"))?,
                target_step: parse_routine_slug(to, &format!("edges[{index}].to"))?,
                condition,
                metadata,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(RoutineGraph {
        entry_steps,
        steps,
        edges,
    })
}

fn routine_entry_steps(routine: &PackageRoutineManifest) -> Vec<String> {
    if !routine.entry_steps.is_empty() {
        return routine.entry_steps.clone();
    }
    routine
        .metadata
        .get("entry_steps")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_edge_metadata(value: &serde_json::Value) -> anyhow::Result<serde_json::Value> {
    if value.is_null() {
        return Ok(serde_json::json!({}));
    }
    if value.is_object() {
        validate_optional_string_field(value, "purpose")?;
        validate_optional_string_field(value, "handoff_instructions")?;
        validate_optional_string_field(value, "handoff")?;
        validate_optional_string_field(value, "task")?;
        return Ok(value.clone());
    }
    anyhow::bail!("edge metadata must be an object when provided");
}

fn validate_optional_string_field(value: &serde_json::Value, field: &str) -> anyhow::Result<()> {
    if value.get(field).is_some_and(|value| !value.is_string()) {
        anyhow::bail!("edge metadata.{field} must be a string when provided");
    }
    Ok(())
}

fn parse_routine_slug(value: &str, field: &str) -> anyhow::Result<Slug> {
    Slug::parse(value).with_context(|| format!("{field} has invalid routine step slug '{value}'"))
}

fn format_routine_validation_error(error: &RoutineValidationError) -> String {
    if error.issues.is_empty() {
        return "routine graph validation failed".to_string();
    }
    error
        .issues
        .iter()
        .map(|issue| {
            let mut message = issue.message.clone();
            if let Some(step) = &issue.step {
                message.push_str(&format!(" [step: {step}]"));
            }
            if let Some(edge) = &issue.edge {
                message.push_str(&format!(" [edge: {edge}]"));
            }
            message
        })
        .collect::<Vec<_>>()
        .join("; ")
}
