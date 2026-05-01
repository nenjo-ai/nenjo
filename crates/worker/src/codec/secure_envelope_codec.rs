use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nenjo_eventbus::{CodecResult, EventCodec};
use nenjo_events::{
    Command, EncryptedPayload, Response, StreamEvent, TaskEncryptedContent, TaskExecuteContent,
    ToolCall,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use crate::crypto::{AccountContentKey, decrypt_text, encrypt_text};

use super::key_provider::EnvelopeKeyProvider;

/// Event-bus codec that encrypts and decrypts content-bearing command and response payloads.
pub struct SecureEnvelopeCodec {
    key_provider: Arc<dyn EnvelopeKeyProvider>,
    account_id: Uuid,
}

impl SecureEnvelopeCodec {
    /// Builds a secure codec using the provided key provider and owning account id.
    pub fn new<K>(key_provider: K, account_id: Uuid) -> Self
    where
        K: EnvelopeKeyProvider,
    {
        Self {
            key_provider: Arc::new(key_provider),
            account_id,
        }
    }

    async fn decrypt_enc_payload(&self, payload: &EncryptedPayload) -> Result<String> {
        let ack = self.key_provider.load_or_refresh_ack().await?;
        match decrypt_text(&ack, payload) {
            Ok(plaintext) => Ok(plaintext),
            Err(error) => {
                warn!(
                    object_id = %payload.object_id,
                    key_version = payload.key_version,
                    algorithm = %payload.algorithm,
                    error = %error,
                    "Encrypted payload decrypt failed; refreshing ACK and retrying once"
                );
                let refreshed_ack = self
                    .key_provider
                    .refresh_ack()
                    .await?
                    .context("Encrypted content received before worker enrollment completed")?;
                Ok(decrypt_text(&refreshed_ack, payload)?)
            }
        }
    }

    async fn encrypt_enc_payload(
        &self,
        ack: &AccountContentKey,
        object_type: &str,
        payload: Option<Value>,
    ) -> Result<Option<EncryptedPayload>> {
        let Some(payload) = payload else {
            return Ok(None);
        };
        let key_version = self.key_provider.current_key_version().await.unwrap_or(1);
        let plaintext = serde_json::to_string(&payload)?;
        if plaintext == "null" || plaintext == "{}" || plaintext == "[]" {
            return Ok(None);
        }
        Ok(Some(encrypt_text(
            ack,
            self.account_id,
            Uuid::new_v4(),
            object_type.to_string(),
            &plaintext,
            key_version,
        )?))
    }

    async fn decode_json_payload<T>(&self, payload: &EncryptedPayload) -> Result<T>
    where
        T: DeserializeOwned,
    {
        Ok(serde_json::from_str(
            &self.decrypt_enc_payload(payload).await?,
        )?)
    }

    async fn encode_stream_event(
        &self,
        ack: &AccountContentKey,
        event: StreamEvent,
    ) -> Result<Option<StreamEvent>> {
        match event {
            StreamEvent::ToolCalls {
                tool_calls,
                agent_name,
                parent_tool_name,
                payload,
                ..
            } => Ok(Some(StreamEvent::ToolCalls {
                tool_calls: tool_calls
                    .into_iter()
                    .map(|call| ToolCall {
                        tool_name: call.tool_name,
                        tool_args: "{}".to_string(),
                    })
                    .collect(),
                agent_name,
                parent_tool_name,
                payload: None,
                encrypted_payload: self
                    .encrypt_enc_payload(ack, "tool_call_payload", payload)
                    .await?,
            })),
            StreamEvent::ToolCompleted {
                tool_name,
                success,
                parent_tool_name,
                payload,
                ..
            } => Ok(Some(StreamEvent::ToolCompleted {
                tool_name,
                success,
                parent_tool_name,
                payload: None,
                encrypted_payload: self
                    .encrypt_enc_payload(ack, "tool_result_payload", payload)
                    .await?,
            })),
            StreamEvent::AbilityActivated {
                agent,
                ability,
                ability_tool_name,
                payload,
                ..
            } => Ok(Some(StreamEvent::AbilityActivated {
                agent,
                ability,
                ability_tool_name,
                payload: None,
                encrypted_payload: self
                    .encrypt_enc_payload(ack, "ability_task_payload", payload)
                    .await?,
            })),
            StreamEvent::AbilityCompleted {
                agent,
                ability,
                ability_tool_name,
                success,
                payload,
                ..
            } => Ok(Some(StreamEvent::AbilityCompleted {
                agent,
                ability,
                ability_tool_name,
                success,
                payload: None,
                encrypted_payload: self
                    .encrypt_enc_payload(ack, "ability_result_payload", payload)
                    .await?,
            })),
            StreamEvent::Error {
                message, payload, ..
            } => Ok(Some(StreamEvent::Error {
                message: "Execution failed".to_string(),
                payload: None,
                encrypted_payload: self
                    .encrypt_enc_payload(
                        ack,
                        "agent_error",
                        Some(payload.unwrap_or_else(|| serde_json::json!({ "message": message }))),
                    )
                    .await?,
            })),
            StreamEvent::Done {
                payload,
                encrypted_payload: _,
                project_id,
                agent_id,
                session_id,
            } => Ok(Some(StreamEvent::Done {
                payload: None,
                encrypted_payload: self
                    .encrypt_enc_payload(ack, "agent_response", payload)
                    .await?,
                project_id,
                agent_id,
                session_id,
            })),
            other => Ok(Some(other)),
        }
    }

    fn redact_error_text(error: Option<String>, generic: &str) -> Option<String> {
        error.map(|value| {
            if value.trim().is_empty() {
                value
            } else {
                generic.to_string()
            }
        })
    }
}

#[async_trait]
impl EventCodec for SecureEnvelopeCodec {
    async fn encode_command(&self, command: Command) -> CodecResult<Command> {
        Ok(Some(command))
    }

    async fn decode_command(&self, command: Command) -> CodecResult<Command> {
        match command {
            Command::ChatMessage {
                id,
                content: _,
                encrypted_content: Some(payload),
                hidden,
                project_id,
                routine_id,
                agent_id,
                session_id,
                domain_session_id,
            } => Ok(Some(Command::ChatMessage {
                id,
                content: self.decrypt_enc_payload(&payload).await?,
                encrypted_content: None,
                hidden,
                project_id,
                routine_id,
                agent_id,
                session_id,
                domain_session_id,
            })),
            Command::TaskExecute {
                task_id,
                project_id,
                execution_run_id,
                routine_id,
                assigned_agent_id,
                payload,
                encrypted_payload: Some(encrypted_payload),
            } => Ok(Some(Command::TaskExecute {
                task_id,
                project_id,
                execution_run_id,
                routine_id,
                assigned_agent_id,
                payload: match payload {
                    Some(mut payload) => {
                        let encrypted = self
                            .decode_json_payload::<TaskEncryptedContent>(&encrypted_payload)
                            .await?;
                        payload.description = encrypted.description;
                        payload.acceptance_criteria = encrypted.acceptance_criteria;
                        Some(payload)
                    }
                    None => Some(
                        self.decode_json_payload::<TaskExecuteContent>(&encrypted_payload)
                            .await?,
                    ),
                },
                encrypted_payload: None,
            })),
            other => Ok(Some(other)),
        }
    }

    async fn encode_response(&self, response: Response) -> CodecResult<Response> {
        let Some(ack) = self.key_provider.load_ack().await? else {
            return Ok(Some(response));
        };

        match response {
            Response::AgentResponse {
                session_id,
                payload,
            } => {
                let Some(payload) = self.encode_stream_event(&ack, payload).await? else {
                    return Ok(None);
                };
                Ok(Some(Response::AgentResponse {
                    session_id,
                    payload,
                }))
            }
            Response::TaskStepEvent {
                execution_run_id,
                task_id,
                event_type,
                step_name,
                step_type,
                duration_ms,
                data,
                payload,
                encrypted_payload: _,
                agent,
            } => {
                let encrypted_payload = self
                    .encrypt_enc_payload(&ack, "task_step_payload", payload)
                    .await?;
                Ok(Some(Response::TaskStepEvent {
                    execution_run_id,
                    task_id,
                    event_type,
                    step_name,
                    step_type,
                    duration_ms,
                    data,
                    payload: None,
                    encrypted_payload,
                    agent,
                }))
            }
            Response::TaskCompleted {
                execution_run_id,
                task_id,
                success,
                error,
                merge_error,
                total_input_tokens,
                total_output_tokens,
            } => Ok(Some(Response::TaskCompleted {
                execution_run_id,
                task_id,
                success,
                error: Self::redact_error_text(error, "Execution failed"),
                merge_error: Self::redact_error_text(merge_error, "Merge failed"),
                total_input_tokens,
                total_output_tokens,
            })),
            Response::ExecutionCompleted {
                id,
                success,
                error,
                total_input_tokens,
                total_output_tokens,
                execution_type,
                routine_id,
                routine_name,
                agent_id,
            } => Ok(Some(Response::ExecutionCompleted {
                id,
                success,
                error: Self::redact_error_text(error, "Execution failed"),
                total_input_tokens,
                total_output_tokens,
                execution_type,
                routine_id,
                routine_name,
                agent_id,
            })),
            other => Ok(Some(other)),
        }
    }

    async fn decode_response(&self, response: Response) -> CodecResult<Response> {
        Ok(Some(response))
    }
}
