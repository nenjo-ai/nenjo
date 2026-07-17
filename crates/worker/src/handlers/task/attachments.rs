//! Terminal task outcomes and encrypted routine-handoff attachments.

use anyhow::{Result, anyhow};
use nenjo_events::{
    RoutineHandoffSource, TaskAttachmentId, TaskAttachmentKind, TaskAttachmentManifest,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{ResponseSender, TaskCommandContext, TaskWorktreeManager};

pub(super) const MAX_HANDOFF_PLAINTEXT_BYTES: usize = 128 * 1024;
pub(super) const MAX_FINAL_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_TERMINAL_HANDOFF_BYTES: usize = 512 * 1024;
const _: () = assert!(MAX_FINAL_OUTPUT_BYTES <= 128 * 1024);

#[derive(Debug, Clone)]
pub(super) struct TaskExecutionOutcome {
    pub(super) success: bool,
    pub(super) error: Option<String>,
    pub(super) total_input_tokens: u64,
    pub(super) total_output_tokens: u64,
    pub(super) attachments: Vec<TaskAttachmentManifest>,
}

impl TaskExecutionOutcome {
    pub(super) fn success(total_input_tokens: u64, total_output_tokens: u64) -> Self {
        Self {
            success: true,
            error: None,
            total_input_tokens,
            total_output_tokens,
            attachments: Vec::new(),
        }
    }

    pub(super) fn failed<Error>(
        error: Error,
        total_input_tokens: u64,
        total_output_tokens: u64,
    ) -> Self
    where
        Error: Into<String>,
    {
        Self {
            success: false,
            error: Some(error.into()),
            total_input_tokens,
            total_output_tokens,
            attachments: Vec::new(),
        }
    }

    pub(super) fn with_attachments(mut self, attachments: Vec<TaskAttachmentManifest>) -> Self {
        self.attachments = attachments;
        self
    }
}

/// Encrypt the direct agent's terminal text as the canonical task output.
pub(super) async fn build_final_output_attachment<S, W>(
    ctx: &TaskCommandContext<S, W>,
    plaintext: &str,
) -> Result<Vec<TaskAttachmentManifest>>
where
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    if plaintext.trim().is_empty() {
        return Ok(Vec::new());
    }
    let size = plaintext.len();
    if size > MAX_FINAL_OUTPUT_BYTES {
        return Err(anyhow!(
            "task_final_output_too_large: output is {size} bytes; maximum is {MAX_FINAL_OUTPUT_BYTES}"
        ));
    }
    let id = Uuid::new_v4();
    let encrypted_payload = ctx
        .attachment_encoder
        .encrypt_attachment(id, plaintext)
        .await?;
    Ok(vec![TaskAttachmentManifest {
        id: TaskAttachmentId::new(id),
        kind: TaskAttachmentKind::FinalOutput,
        name: "Final output".to_string(),
        content_type: "text/markdown".to_string(),
        byte_size: u64::try_from(size).unwrap_or(u64::MAX),
        encrypted_payload,
        content_digest: format!("sha256:{:x}", Sha256::digest(plaintext.as_bytes())),
        source: None,
    }])
}

pub(super) async fn build_handoff_attachments<S, W>(
    ctx: &TaskCommandContext<S, W>,
    routine_id: Option<Uuid>,
    handoffs: &[nenjo::routines::RoutineHandoff],
) -> Result<Vec<TaskAttachmentManifest>>
where
    S: ResponseSender,
    W: TaskWorktreeManager,
{
    let plaintexts = validated_handoff_plaintexts(handoffs)?;
    let mut attachments = Vec::with_capacity(handoffs.len());
    for (handoff, plaintext) in handoffs.iter().zip(plaintexts) {
        let id = Uuid::new_v4();
        let encrypted_payload = ctx
            .attachment_encoder
            .encrypt_attachment(id, &plaintext)
            .await?;
        let content_digest = format!("sha256:{:x}", Sha256::digest(plaintext.as_bytes()));
        let edge_condition = match handoff.edge_condition {
            nenjo::manifest::RoutineEdgeCondition::Always => "always",
            nenjo::manifest::RoutineEdgeCondition::OnPass => "on_pass",
            nenjo::manifest::RoutineEdgeCondition::OnFail => "on_fail",
        };
        attachments.push(TaskAttachmentManifest {
            id: TaskAttachmentId::new(id),
            kind: TaskAttachmentKind::RoutineHandoff,
            name: format!("{} → {}", handoff.source_step, handoff.target_step),
            content_type: "application/json".to_string(),
            byte_size: u64::try_from(plaintext.len()).unwrap_or(u64::MAX),
            encrypted_payload,
            content_digest,
            source: Some(RoutineHandoffSource {
                routine_id,
                source_step_slug: handoff.source_step.to_string(),
                destination_step_slug: handoff.target_step.to_string(),
                edge_condition: edge_condition.to_string(),
            }),
        });
    }
    Ok(attachments)
}

fn validated_handoff_plaintexts(
    handoffs: &[nenjo::routines::RoutineHandoff],
) -> Result<Vec<String>> {
    let mut aggregate_size = 0usize;
    handoffs
        .iter()
        .map(|handoff| {
            let plaintext = serde_json::to_string(&handoff.handoff)?;
            let plaintext_size = plaintext.len();
            if plaintext_size > MAX_HANDOFF_PLAINTEXT_BYTES {
                return Err(anyhow!(
                    "routine_handoff_too_large: handoff is {plaintext_size} bytes; maximum is {MAX_HANDOFF_PLAINTEXT_BYTES}"
                ));
            }
            aggregate_size = aggregate_size.saturating_add(plaintext_size);
            if aggregate_size > MAX_TERMINAL_HANDOFF_BYTES {
                return Err(anyhow!(
                    "routine_handoff_payload_too_large: terminal handoffs total {aggregate_size} bytes; maximum is {MAX_TERMINAL_HANDOFF_BYTES}"
                ));
            }
            Ok(plaintext)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{MAX_HANDOFF_PLAINTEXT_BYTES, validated_handoff_plaintexts};

    fn handoff(value: serde_json::Value) -> nenjo::routines::RoutineHandoff {
        nenjo::routines::RoutineHandoff {
            source_step: nenjo::Slug::derive("source"),
            target_step: nenjo::Slug::derive("done"),
            handoff: value,
            purpose: None,
            summary: None,
            edge_condition: nenjo::manifest::RoutineEdgeCondition::Always,
        }
    }

    #[test]
    fn terminal_handoff_limits_reject_single_and_aggregate_overflow() {
        let oversized = handoff(serde_json::Value::String(
            "x".repeat(MAX_HANDOFF_PLAINTEXT_BYTES),
        ));
        assert!(
            validated_handoff_plaintexts(&[oversized])
                .unwrap_err()
                .to_string()
                .contains("routine_handoff_too_large")
        );

        let chunk = handoff(serde_json::Value::String("x".repeat(110 * 1024)));
        let aggregate = vec![chunk; 5];
        assert!(
            validated_handoff_plaintexts(&aggregate)
                .unwrap_err()
                .to_string()
                .contains("routine_handoff_payload_too_large")
        );
    }

    #[test]
    fn terminal_handoffs_preserve_edge_cardinality() {
        let handoffs = vec![
            handoff(serde_json::json!({"value": 1})),
            handoff(serde_json::json!({"value": 2})),
        ];
        let plaintexts = validated_handoff_plaintexts(&handoffs).unwrap();
        assert_eq!(plaintexts.len(), 2);
        assert_ne!(plaintexts[0], plaintexts[1]);
    }
}
