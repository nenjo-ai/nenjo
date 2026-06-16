use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use nenjo_crypto_auth::{ContentKey, ContentScope, EnvelopeKeyProvider};
use nenjo_events::{
    Command, CronTaskContent, EncryptedPayload, HeartbeatInstructionsContent, ResourceType,
    Response, StreamEvent, TaskEncryptedContent, TaskExecuteContent,
};
use nenjo_platform::SensitiveContentKind;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    CodecContext, CodecResult, DecodeCommandResult, EnvelopeCodec, decrypt_text,
    encrypt_text_for_scope,
};

/// Secure-envelope codec that encrypts and decrypts content-bearing command and response payloads.
#[derive(Clone)]
pub struct SecureEnvelopeCodec {
    key_provider: Arc<dyn EnvelopeKeyProvider>,
    org_id: Uuid,
    config: SecureEnvelopeCodecConfig,
}

/// Runtime policy for secure-envelope command decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecureEnvelopeCodecConfig {
    /// Require sensitive commands to carry encrypted payload fields.
    pub require_secured_commands: bool,
}

impl Default for SecureEnvelopeCodecConfig {
    fn default() -> Self {
        Self {
            require_secured_commands: true,
        }
    }
}

impl SecureEnvelopeCodec {
    pub fn new<K>(key_provider: K, org_id: Uuid) -> Self
    where
        K: EnvelopeKeyProvider,
    {
        Self::new_with_config(key_provider, org_id, SecureEnvelopeCodecConfig::default())
    }

    pub fn new_with_config<K>(
        key_provider: K,
        org_id: Uuid,
        config: SecureEnvelopeCodecConfig,
    ) -> Self
    where
        K: EnvelopeKeyProvider,
    {
        Self {
            key_provider: Arc::new(key_provider),
            org_id,
            config,
        }
    }

    /// Decode a single encrypted payload using the scope encoded in the payload.
    pub async fn decode_payload_text(&self, payload: &EncryptedPayload) -> Result<String> {
        self.decrypt_enc_payload(payload.account_id, payload).await
    }

    async fn decrypt_user_payload(
        &self,
        user_id: Uuid,
        payload: &EncryptedPayload,
    ) -> Result<String> {
        let key = self.key_provider.load_or_refresh_user_key(user_id).await?;
        match decrypt_text(&key, payload) {
            Ok(plaintext) => Ok(plaintext),
            Err(error) => {
                warn!(
                    object_id = %payload.object_id,
                    key_version = payload.key_version,
                    algorithm = %payload.algorithm,
                    error = %error,
                    "Encrypted payload decrypt failed; refreshing key and retrying once"
                );
                let refreshed_key =
                    self.key_provider.refresh_user_key(user_id).await?.context(
                        "Encrypted chat content received before sender ACK sync completed",
                    )?;
                Ok(decrypt_text(&refreshed_key, payload)?)
            }
        }
    }

    async fn decrypt_org_payload(&self, payload: &EncryptedPayload) -> Result<String> {
        let key = self
            .key_provider
            .load_or_refresh_key(ContentScope::Org)
            .await?;
        match decrypt_text(&key, payload) {
            Ok(plaintext) => Ok(plaintext),
            Err(error) => {
                warn!(
                    object_id = %payload.object_id,
                    key_version = payload.key_version,
                    algorithm = %payload.algorithm,
                    error = %error,
                    "Encrypted payload decrypt failed; refreshing key and retrying once"
                );
                let refreshed_key = self
                    .key_provider
                    .refresh_key(ContentScope::Org)
                    .await?
                    .context(
                        "Encrypted org content received before worker OCK enrollment completed",
                    )?;
                Ok(decrypt_text(&refreshed_key, payload)?)
            }
        }
    }

    async fn decrypt_enc_payload(
        &self,
        user_id: Uuid,
        payload: &EncryptedPayload,
    ) -> Result<String> {
        match ContentScope::from_payload(payload) {
            ContentScope::User => self.decrypt_user_payload(user_id, payload).await,
            ContentScope::Org => self.decrypt_org_payload(payload).await,
        }
    }

    async fn encrypt_enc_payload(
        &self,
        key: &ContentKey,
        account_id: Uuid,
        encryption_scope: Option<&str>,
        object_type: &str,
        payload: Option<Value>,
    ) -> Result<Option<EncryptedPayload>> {
        let Some(payload) = payload else {
            return Ok(None);
        };
        let scope = if encryption_scope == Some("org") {
            ContentScope::Org
        } else {
            ContentScope::User
        };
        let key_version = match scope {
            ContentScope::User => self
                .key_provider
                .current_user_key_version(account_id)
                .await
                .unwrap_or(1),
            ContentScope::Org => self
                .key_provider
                .current_key_version(scope)
                .await
                .unwrap_or(1),
        };
        let plaintext = serde_json::to_string(&payload)?;
        if plaintext == "null" || plaintext == "{}" || plaintext == "[]" {
            return Ok(None);
        }
        let encrypted_payload = encrypt_text_for_scope(
            key,
            scope,
            account_id,
            Uuid::new_v4(),
            object_type.to_string(),
            &plaintext,
            key_version,
        )?;
        Ok(Some(encrypted_payload))
    }

    async fn decode_json_payload<T>(&self, user_id: Uuid, payload: &EncryptedPayload) -> Result<T>
    where
        T: DeserializeOwned,
    {
        Ok(serde_json::from_str(
            &self.decrypt_enc_payload(user_id, payload).await?,
        )?)
    }

    fn validate_sensitive_payload_kind(
        payload: &EncryptedPayload,
        kind: SensitiveContentKind,
    ) -> Result<()> {
        let expected_object_type = kind.encrypted_object_type();
        if payload.object_type != expected_object_type {
            bail!(
                "encrypted payload object_type '{}' did not match expected '{}'",
                payload.object_type,
                expected_object_type
            );
        }
        Ok(())
    }

    fn unsecured_command_result(
        &self,
        command: Command,
        command_label: &str,
        field: &str,
    ) -> DecodeCommandResult {
        if !self.config.require_secured_commands {
            warn!(
                command = %command_label,
                %field,
                "Allowing unsecured sensitive command without encrypted payload"
            );
            return DecodeCommandResult::Command(Box::new(command));
        }

        warn!(
            command = %command_label,
            %field,
            "Dropping unsecured sensitive command without encrypted payload"
        );
        DecodeCommandResult::Drop
    }

    fn drop_command_decode_failure(
        command: &str,
        reason: &str,
        error: impl std::fmt::Display,
    ) -> DecodeCommandResult {
        warn!(
            %command,
            %reason,
            error = %error,
            "Dropping command after secure envelope decode failure"
        );
        DecodeCommandResult::Drop
    }

    fn manifest_inline_payload_requires_encryption(resource_type: ResourceType) -> bool {
        matches!(
            resource_type,
            ResourceType::Agent
                | ResourceType::Ability
                | ResourceType::Domain
                | ResourceType::ContextBlock
                | ResourceType::Document
                | ResourceType::Project
        )
    }

    fn stream_event_requires_encryption(event: &StreamEvent) -> bool {
        match event {
            StreamEvent::RunFailed { payload, .. }
            | StreamEvent::AssistantTextDelta { payload, .. }
            | StreamEvent::ToolCallStarted { payload, .. }
            | StreamEvent::ToolOutputDelta { payload, .. }
            | StreamEvent::ToolCallCompleted { payload, .. }
            | StreamEvent::HookStarted { payload, .. }
            | StreamEvent::HookCompleted { payload, .. }
            | StreamEvent::AsyncOperationEvent { payload, .. }
            | StreamEvent::AsyncOperationTranscript { payload, .. }
            | StreamEvent::Done { payload, .. } => payload.is_some(),
            StreamEvent::Error { .. } => true,
            StreamEvent::RunStarted { .. }
            | StreamEvent::RunCompleted { .. }
            | StreamEvent::RunCancelled { .. }
            | StreamEvent::ModelRequestStarted { .. }
            | StreamEvent::ModelRequestCompleted { .. }
            | StreamEvent::DomainEntered { .. }
            | StreamEvent::DomainExited { .. }
            | StreamEvent::MessageCompacted { .. }
            | StreamEvent::Paused
            | StreamEvent::Resumed => false,
        }
    }

    async fn decode_task_execute_content(
        &self,
        actor_user_id: Uuid,
        payload: Option<TaskExecuteContent>,
        encrypted_payload: &EncryptedPayload,
    ) -> Result<TaskExecuteContent> {
        Self::validate_sensitive_payload_kind(
            encrypted_payload,
            SensitiveContentKind::TaskContent,
        )?;
        match payload {
            Some(mut payload) => {
                let encrypted = self
                    .decode_json_payload::<TaskEncryptedContent>(actor_user_id, encrypted_payload)
                    .await?;
                payload.description = encrypted.description;
                payload.acceptance_criteria = encrypted.acceptance_criteria;
                Ok(payload)
            }
            None => {
                self.decode_json_payload::<TaskExecuteContent>(actor_user_id, encrypted_payload)
                    .await
            }
        }
    }

    async fn encrypt_user_payload(
        &self,
        user_id: Uuid,
        key: &ContentKey,
        object_type: &str,
        payload: Option<Value>,
    ) -> Result<Option<EncryptedPayload>> {
        self.encrypt_enc_payload(key, user_id, None, object_type, payload)
            .await
    }

    async fn encrypt_org_payload(
        &self,
        key: &ContentKey,
        object_type: &str,
        payload: Option<Value>,
    ) -> Result<Option<EncryptedPayload>> {
        self.encrypt_enc_payload(key, self.org_id, Some("org"), object_type, payload)
            .await
    }

    async fn encode_stream_event(
        &self,
        user_id: Uuid,
        ack: &ContentKey,
        event: StreamEvent,
    ) -> Result<Option<StreamEvent>> {
        match event {
            StreamEvent::RunFailed {
                run_id,
                session_id,
                payload,
                ..
            } => Ok(Some(StreamEvent::RunFailed {
                run_id,
                session_id,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "run_failed_payload", payload)
                    .await?,
            })),
            StreamEvent::AssistantTextDelta {
                run_id,
                request_id,
                payload,
                ..
            } => Ok(Some(StreamEvent::AssistantTextDelta {
                run_id,
                request_id,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "assistant_text_delta", payload)
                    .await?,
            })),
            StreamEvent::ToolCallStarted {
                run_id,
                batch_id,
                call_id,
                parent_call_id,
                tool_name,
                payload,
                ..
            } => Ok(Some(StreamEvent::ToolCallStarted {
                run_id,
                batch_id,
                call_id,
                parent_call_id,
                tool_name,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "tool_call_payload", payload)
                    .await?,
            })),
            StreamEvent::ToolOutputDelta {
                run_id,
                call_id,
                stream,
                payload,
                ..
            } => Ok(Some(StreamEvent::ToolOutputDelta {
                run_id,
                call_id,
                stream,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "tool_output_delta", payload)
                    .await?,
            })),
            StreamEvent::ToolCallCompleted {
                run_id,
                batch_id,
                call_id,
                parent_call_id,
                success,
                payload,
                ..
            } => Ok(Some(StreamEvent::ToolCallCompleted {
                run_id,
                batch_id,
                call_id,
                parent_call_id,
                success,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "tool_result_payload", payload)
                    .await?,
            })),
            StreamEvent::HookStarted {
                agent,
                hook,
                hook_event,
                hook_type,
                source,
                payload,
                ..
            } => Ok(Some(StreamEvent::HookStarted {
                agent,
                hook,
                hook_event,
                hook_type,
                source,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "hook_start_payload", payload)
                    .await?,
            })),
            StreamEvent::HookCompleted {
                agent,
                hook,
                hook_event,
                hook_type,
                source,
                success,
                blocked,
                payload,
                ..
            } => Ok(Some(StreamEvent::HookCompleted {
                agent,
                hook,
                hook_event,
                hook_type,
                source,
                success,
                blocked,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "hook_result_payload", payload)
                    .await?,
            })),
            StreamEvent::AsyncOperationEvent {
                operation_id,
                kind,
                label,
                status,
                signal,
                model_visible,
                parent_operation_id,
                parent_tool_name,
                summary,
                payload,
                ..
            } => Ok(Some(StreamEvent::AsyncOperationEvent {
                operation_id,
                kind,
                label,
                status,
                signal,
                model_visible,
                parent_operation_id,
                parent_tool_name,
                summary,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "async_operation_payload", payload)
                    .await?,
            })),
            StreamEvent::AsyncOperationTranscript {
                operation_id,
                kind,
                label,
                event,
                payload,
                ..
            } => Ok(Some(StreamEvent::AsyncOperationTranscript {
                operation_id,
                kind,
                label,
                event,
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(
                        user_id,
                        ack,
                        "async_operation_transcript_payload",
                        payload,
                    )
                    .await?,
            })),
            StreamEvent::Error {
                message, payload, ..
            } => Ok(Some(StreamEvent::Error {
                message: "Execution failed".to_string(),
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(
                        user_id,
                        ack,
                        "agent_error",
                        Some(payload.unwrap_or_else(|| serde_json::json!({ "message": message }))),
                    )
                    .await?,
            })),
            StreamEvent::Done {
                payload,
                encrypted_payload: _,
                total_input_tokens,
                total_output_tokens,
                project,
                agent,
                session_id,
            } => Ok(Some(StreamEvent::Done {
                payload: None,
                encrypted_payload: self
                    .encrypt_user_payload(user_id, ack, "agent_response", payload)
                    .await?,
                total_input_tokens,
                total_output_tokens,
                project,
                agent,
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
impl nenjo_platform::api_client::PayloadCodec for SecureEnvelopeCodec {
    async fn decode_text(&self, payload: &EncryptedPayload) -> Result<Option<String>> {
        Ok(Some(
            SecureEnvelopeCodec::decode_payload_text(self, payload).await?,
        ))
    }
}

#[async_trait]
impl EnvelopeCodec for SecureEnvelopeCodec {
    async fn encode_command(&self, command: Command) -> CodecResult<Command> {
        Ok(Some(command))
    }

    async fn decode_command(
        &self,
        ctx: &CodecContext,
        command: Command,
    ) -> Result<DecodeCommandResult, crate::CodecError> {
        let actor_user_id = ctx.actor_user_id;
        let command_label = command.to_string();
        match command {
            Command::ChatMessage {
                id,
                content: _,
                encrypted_content: Some(payload),
                hidden,
                project,
                routine,
                agent,
                target_type,
                target,
                session_id,
                domain_session_id,
                domain_activation,
            } => match self.decrypt_enc_payload(actor_user_id, &payload).await {
                Ok(content) => Ok(DecodeCommandResult::Command(Box::new(
                    Command::ChatMessage {
                        id,
                        content,
                        encrypted_content: None,
                        hidden,
                        project,
                        routine,
                        agent,
                        target_type,
                        target,
                        session_id,
                        domain_session_id,
                        domain_activation,
                    },
                ))),
                Err(error) => Ok(Self::drop_command_decode_failure(
                    &command_label,
                    "encrypted_chat_decode_failed",
                    error,
                )),
            },
            command @ Command::ChatMessage {
                encrypted_content: None,
                ..
            } => Ok(self.unsecured_command_result(command, &command_label, "encrypted_content")),
            Command::ChatCommand {
                id,
                command,
                content: _,
                encrypted_content: Some(payload),
                project,
                agent,
                target_type,
                target,
                session_id,
                domain_session_id,
                domain_activation,
            } => match self.decrypt_enc_payload(actor_user_id, &payload).await {
                Ok(content) => Ok(DecodeCommandResult::Command(Box::new(
                    Command::ChatCommand {
                        id,
                        command,
                        content,
                        encrypted_content: None,
                        project,
                        agent,
                        target_type,
                        target,
                        session_id,
                        domain_session_id,
                        domain_activation,
                    },
                ))),
                Err(error) => Ok(Self::drop_command_decode_failure(
                    &command_label,
                    "encrypted_chat_decode_failed",
                    error,
                )),
            },
            command @ Command::ChatCommand {
                encrypted_content: None,
                ..
            } => Ok(self.unsecured_command_result(command, &command_label, "encrypted_content")),
            Command::TaskExecute {
                task_id,
                project,
                execution_run_id,
                routine,
                agent,
                payload,
                encrypted_payload: Some(encrypted_payload),
            } => {
                let payload = match self
                    .decode_task_execute_content(actor_user_id, payload, &encrypted_payload)
                    .await
                {
                    Ok(payload) => payload,
                    Err(error) => {
                        return Ok(Self::drop_command_decode_failure(
                            &command_label,
                            "encrypted_task_decode_failed",
                            error,
                        ));
                    }
                };
                Ok(DecodeCommandResult::Command(Box::new(
                    Command::TaskExecute {
                        task_id,
                        project,
                        execution_run_id,
                        routine,
                        agent,
                        payload: Some(payload),
                        encrypted_payload: None,
                    },
                )))
            }
            command @ Command::TaskExecute {
                encrypted_payload: None,
                ..
            } => Ok(self.unsecured_command_result(command, &command_label, "encrypted_payload")),
            Command::CronEnable {
                routine,
                project,
                schedule,
                timezone,
                task: _,
                encrypted_task: Some(encrypted_task),
            } => {
                if let Err(error) = Self::validate_sensitive_payload_kind(
                    &encrypted_task,
                    SensitiveContentKind::RoutineCronTask,
                ) {
                    return Ok(Self::drop_command_decode_failure(
                        &command_label,
                        "encrypted_cron_task_decode_failed",
                        error,
                    ));
                }
                let task = match self
                    .decode_json_payload::<CronTaskContent>(actor_user_id, &encrypted_task)
                    .await
                {
                    Ok(task) => task,
                    Err(error) => {
                        return Ok(Self::drop_command_decode_failure(
                            &command_label,
                            "encrypted_cron_task_decode_failed",
                            error,
                        ));
                    }
                };
                Ok(DecodeCommandResult::Command(Box::new(
                    Command::CronEnable {
                        routine,
                        project,
                        schedule,
                        timezone,
                        task: Some(task),
                        encrypted_task: None,
                    },
                )))
            }
            command @ Command::CronEnable {
                task: Some(_),
                encrypted_task: None,
                ..
            } => Ok(self.unsecured_command_result(command, &command_label, "encrypted_task")),
            Command::CronTrigger {
                routine,
                project,
                task: _,
                encrypted_task: Some(encrypted_task),
            } => {
                if let Err(error) = Self::validate_sensitive_payload_kind(
                    &encrypted_task,
                    SensitiveContentKind::RoutineCronTask,
                ) {
                    return Ok(Self::drop_command_decode_failure(
                        &command_label,
                        "encrypted_cron_task_decode_failed",
                        error,
                    ));
                }
                let task = match self
                    .decode_json_payload::<CronTaskContent>(actor_user_id, &encrypted_task)
                    .await
                {
                    Ok(task) => task,
                    Err(error) => {
                        return Ok(Self::drop_command_decode_failure(
                            &command_label,
                            "encrypted_cron_task_decode_failed",
                            error,
                        ));
                    }
                };
                Ok(DecodeCommandResult::Command(Box::new(
                    Command::CronTrigger {
                        routine,
                        project,
                        task: Some(task),
                        encrypted_task: None,
                    },
                )))
            }
            command @ Command::CronTrigger {
                task: Some(_),
                encrypted_task: None,
                ..
            } => Ok(self.unsecured_command_result(command, &command_label, "encrypted_task")),
            Command::AgentHeartbeatEnable {
                agent,
                interval,
                timezone,
                instructions: _,
                encrypted_instructions: Some(encrypted_instructions),
            } => {
                if let Err(error) = Self::validate_sensitive_payload_kind(
                    &encrypted_instructions,
                    SensitiveContentKind::HeartbeatInstructions,
                ) {
                    return Ok(Self::drop_command_decode_failure(
                        &command_label,
                        "encrypted_heartbeat_instructions_decode_failed",
                        error,
                    ));
                }
                let instructions = match self
                    .decode_json_payload::<HeartbeatInstructionsContent>(
                        actor_user_id,
                        &encrypted_instructions,
                    )
                    .await
                {
                    Ok(instructions) => instructions,
                    Err(error) => {
                        return Ok(Self::drop_command_decode_failure(
                            &command_label,
                            "encrypted_heartbeat_instructions_decode_failed",
                            error,
                        ));
                    }
                };
                Ok(DecodeCommandResult::Command(Box::new(
                    Command::AgentHeartbeatEnable {
                        agent,
                        interval,
                        timezone,
                        instructions: Some(instructions),
                        encrypted_instructions: None,
                    },
                )))
            }
            command @ Command::AgentHeartbeatEnable {
                instructions: Some(_),
                encrypted_instructions: None,
                ..
            } => Ok(self.unsecured_command_result(
                command,
                &command_label,
                "encrypted_instructions",
            )),
            Command::AgentHeartbeatTrigger {
                agent,
                instructions: _,
                encrypted_instructions: Some(encrypted_instructions),
            } => {
                if let Err(error) = Self::validate_sensitive_payload_kind(
                    &encrypted_instructions,
                    SensitiveContentKind::HeartbeatInstructions,
                ) {
                    return Ok(Self::drop_command_decode_failure(
                        &command_label,
                        "encrypted_heartbeat_instructions_decode_failed",
                        error,
                    ));
                }
                let instructions = match self
                    .decode_json_payload::<HeartbeatInstructionsContent>(
                        actor_user_id,
                        &encrypted_instructions,
                    )
                    .await
                {
                    Ok(instructions) => instructions,
                    Err(error) => {
                        return Ok(Self::drop_command_decode_failure(
                            &command_label,
                            "encrypted_heartbeat_instructions_decode_failed",
                            error,
                        ));
                    }
                };
                Ok(DecodeCommandResult::Command(Box::new(
                    Command::AgentHeartbeatTrigger {
                        agent,
                        instructions: Some(instructions),
                        encrypted_instructions: None,
                    },
                )))
            }
            command @ Command::AgentHeartbeatTrigger {
                instructions: Some(_),
                encrypted_instructions: None,
                ..
            } => Ok(self.unsecured_command_result(
                command,
                &command_label,
                "encrypted_instructions",
            )),
            Command::ManifestChanged {
                schema,
                resource_id,
                resource_type,
                resource,
                action,
                project,
                payload,
                encrypted_payload,
            } => {
                let Some(encrypted_payload) = encrypted_payload else {
                    if payload.is_some()
                        && Self::manifest_inline_payload_requires_encryption(resource_type)
                    {
                        let command = Command::ManifestChanged {
                            schema,
                            resource_id,
                            resource_type,
                            resource,
                            action,
                            project,
                            payload,
                            encrypted_payload: None,
                        };
                        return Ok(self.unsecured_command_result(
                            command,
                            &command_label,
                            "encrypted_payload",
                        ));
                    }
                    return Ok(DecodeCommandResult::Command(Box::new(
                        Command::ManifestChanged {
                            schema,
                            resource_id,
                            resource_type,
                            resource,
                            action,
                            project,
                            payload,
                            encrypted_payload: None,
                        },
                    )));
                };

                let object_type = encrypted_payload.object_type.clone();
                let object_id = encrypted_payload.object_id;
                let Some(content_kind) =
                    SensitiveContentKind::from_encrypted_object_type(&object_type)
                else {
                    return Ok(Self::drop_command_decode_failure(
                        &command_label,
                        "encrypted_manifest_decode_failed",
                        format!("unsupported encrypted manifest object_type '{object_type}'"),
                    ));
                };
                if !content_kind.matches_resource_type(resource_type) {
                    return Ok(Self::drop_command_decode_failure(
                        &command_label,
                        "encrypted_manifest_decode_failed",
                        format!(
                            "encrypted manifest object_type '{object_type}' did not match \
                             resource_type '{resource_type}'"
                        ),
                    ));
                }
                let decrypted = match self
                    .decrypt_enc_payload(actor_user_id, &encrypted_payload)
                    .await
                {
                    Ok(plaintext) => serde_json::from_str::<Value>(&plaintext)
                        .unwrap_or(Value::String(plaintext)),
                    Err(error) => {
                        return Ok(Self::drop_command_decode_failure(
                            &command_label,
                            "encrypted_manifest_decode_failed",
                            error,
                        ));
                    }
                };
                info!(
                    %resource_type,
                    ?action,
                    object_type,
                    "Decoded manifest payload"
                );
                debug!(
                    %resource_type,
                    ?action,
                    project = ?project,
                    object_type,
                    %object_id,
                    "Decoded encrypted manifest payload details"
                );

                let payload = serde_json::json!({
                    "__nenjo_decrypted_manifest_payload": true,
                    "schema": "manifest.decrypted_resource.v1",
                    "object_type": object_type,
                    "object_id": object_id,
                    "inline_payload": payload,
                    "decrypted_payload": decrypted,
                });

                Ok(DecodeCommandResult::Command(Box::new(
                    Command::ManifestChanged {
                        schema,
                        resource_id,
                        resource_type,
                        resource,
                        action,
                        project,
                        payload: Some(payload),
                        encrypted_payload: None,
                    },
                )))
            }
            Command::WorkerAccountKeyUpdated { wrapped_ack } => Ok(DecodeCommandResult::Command(
                Box::new(Command::WorkerAccountKeyUpdated { wrapped_ack }),
            )),
            other => Ok(DecodeCommandResult::Command(Box::new(other))),
        }
    }

    async fn encode_response(
        &self,
        ctx: &CodecContext,
        response: Response,
    ) -> CodecResult<Response> {
        let actor_user_id = ctx.actor_user_id;
        match response {
            Response::AgentResponse {
                session_id,
                payload,
            } => {
                let Some(ack) = self.key_provider.load_user_key(actor_user_id).await? else {
                    if Self::stream_event_requires_encryption(&payload) {
                        warn!(
                            %actor_user_id,
                            event = %payload,
                            "Dropping sensitive agent response because actor content key is unavailable"
                        );
                        return Ok(None);
                    }
                    return Ok(Some(Response::AgentResponse {
                        session_id,
                        payload,
                    }));
                };
                let Some(payload) = self
                    .encode_stream_event(actor_user_id, &ack, payload)
                    .await?
                else {
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
                let Some(ock) = self.key_provider.load_key(ContentScope::Org).await? else {
                    if payload.is_some() {
                        warn!(
                            %execution_run_id,
                            event_type = %event_type,
                            step_name = %step_name,
                            "Dropping plaintext task step payload because org content key is unavailable"
                        );
                    }
                    return Ok(Some(Response::TaskStepEvent {
                        execution_run_id,
                        task_id,
                        event_type,
                        step_name,
                        step_type,
                        duration_ms,
                        data,
                        payload: None,
                        encrypted_payload: None,
                        agent,
                    }));
                };
                let encrypted_payload = self
                    .encrypt_org_payload(&ock, "task_step_payload", payload)
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
                routine,
                routine_name,
                agent,
            } => Ok(Some(Response::ExecutionCompleted {
                id,
                success,
                error: Self::redact_error_text(error, "Execution failed"),
                total_input_tokens,
                total_output_tokens,
                execution_type,
                routine,
                routine_name,
                agent,
            })),
            other => Ok(Some(other)),
        }
    }

    async fn decode_response(&self, response: Response) -> CodecResult<Response> {
        Ok(Some(response))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use anyhow::{Context, Result, anyhow};
    use async_trait::async_trait;
    use nenjo_crypto_auth::{ContentKey, ContentScope, EnvelopeKeyProvider};
    use nenjo_events::{
        Command, ResourceAction, ResourceType, Response, StreamEvent, TaskExecuteContent,
    };
    use tokio::sync::RwLock;
    use uuid::Uuid;

    use super::{SecureEnvelopeCodec, SecureEnvelopeCodecConfig};
    use crate::{CodecContext, DecodeCommandResult, EnvelopeCodec, encrypt_text_for_scope};

    #[derive(Clone)]
    struct StubKeyProvider {
        user_keys: Arc<RwLock<HashMap<Uuid, ContentKey>>>,
    }

    impl StubKeyProvider {
        async fn insert_user_key(&self, user_id: Uuid, key: ContentKey) {
            self.user_keys.write().await.insert(user_id, key);
        }
    }

    #[async_trait]
    impl EnvelopeKeyProvider for StubKeyProvider {
        async fn load_key(&self, _scope: ContentScope) -> Result<Option<ContentKey>> {
            Ok(None)
        }

        async fn load_or_refresh_key(&self, _scope: ContentScope) -> Result<ContentKey> {
            Err(anyhow!("not used in this test"))
        }

        async fn refresh_key(&self, _scope: ContentScope) -> Result<Option<ContentKey>> {
            Ok(None)
        }

        async fn clear_cached_key(&self, _scope: ContentScope) {}

        async fn current_key_version(&self, _scope: ContentScope) -> Option<u32> {
            Some(1)
        }

        async fn load_user_key(&self, user_id: Uuid) -> Result<Option<ContentKey>> {
            Ok(self.user_keys.read().await.get(&user_id).cloned())
        }

        async fn load_or_refresh_user_key(&self, user_id: Uuid) -> Result<ContentKey> {
            self.load_user_key(user_id)
                .await?
                .context("Encrypted chat content received before sender ACK sync completed")
        }

        async fn refresh_user_key(&self, user_id: Uuid) -> Result<Option<ContentKey>> {
            self.load_user_key(user_id).await
        }

        async fn current_user_key_version(&self, user_id: Uuid) -> Option<u32> {
            if self.user_keys.read().await.contains_key(&user_id) {
                Some(1)
            } else {
                None
            }
        }
    }

    #[tokio::test]
    async fn chat_message_user_payload_uses_actor_specific_ack() {
        let actor_user_id = Uuid::new_v4();
        let actor_key = ContentKey::from_bytes([9_u8; 32]);
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new(provider.clone(), Uuid::new_v4());
        let actor_ctx = CodecContext::for_actor(actor_user_id);
        let encrypted_payload = encrypt_text_for_scope(
            &actor_key,
            ContentScope::User,
            actor_user_id,
            Uuid::new_v4(),
            "chat_message",
            "secondary actor secret",
            1,
        )
        .expect("encrypt actor-scoped chat payload");

        let before_sync = codec
            .decode_command(
                &actor_ctx,
                Command::ChatMessage {
                    id: Some("actor-confusion".into()),
                    content: String::new(),
                    encrypted_content: Some(encrypted_payload.clone()),
                    hidden: false,
                    project: None,
                    routine: None,
                    agent: None,
                    target_type: None,
                    target: None,
                    domain_session_id: None,
                    domain_activation: None,
                    session_id: Uuid::new_v4(),
                },
            )
            .await;
        match before_sync.expect("decode result before sync") {
            DecodeCommandResult::Drop => {}
            other => panic!("unexpected decode result before sync: {other:?}"),
        }

        provider
            .insert_user_key(actor_user_id, actor_key.clone())
            .await;

        let after_sync = codec
            .decode_command(
                &actor_ctx,
                Command::ChatMessage {
                    id: Some("actor-confusion".into()),
                    content: String::new(),
                    encrypted_content: Some(encrypted_payload),
                    hidden: false,
                    project: None,
                    routine: None,
                    agent: None,
                    target_type: None,
                    target: None,
                    domain_session_id: None,
                    domain_activation: None,
                    session_id: Uuid::new_v4(),
                },
            )
            .await
            .expect("actor-specific decrypt should succeed after ACK sync");

        match after_sync {
            DecodeCommandResult::Command(command) => match *command {
                Command::ChatMessage {
                    content,
                    encrypted_content,
                    ..
                } => {
                    assert_eq!(content, "secondary actor secret");
                    assert!(encrypted_content.is_none());
                }
                other => panic!("unexpected decoded command payload: {other:?}"),
            },
            other => panic!("unexpected decoded command result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_message_without_encrypted_content_drops() {
        let actor_user_id = Uuid::new_v4();
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new(provider, Uuid::new_v4());
        let result = codec
            .decode_command(
                &CodecContext::for_actor(actor_user_id),
                Command::ChatMessage {
                    id: Some("plain".into()),
                    content: "plaintext prompt".into(),
                    encrypted_content: None,
                    hidden: false,
                    project: None,
                    routine: None,
                    agent: None,
                    target_type: None,
                    target: None,
                    domain_session_id: None,
                    domain_activation: None,
                    session_id: Uuid::new_v4(),
                },
            )
            .await
            .expect("decode should classify plaintext chat");

        assert!(matches!(result, DecodeCommandResult::Drop));
    }

    #[tokio::test]
    async fn chat_message_without_encrypted_content_can_be_allowed_by_config() {
        let actor_user_id = Uuid::new_v4();
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new_with_config(
            provider,
            Uuid::new_v4(),
            SecureEnvelopeCodecConfig {
                require_secured_commands: false,
            },
        );
        let result = codec
            .decode_command(
                &CodecContext::for_actor(actor_user_id),
                Command::ChatMessage {
                    id: Some("plain".into()),
                    content: "plaintext prompt".into(),
                    encrypted_content: None,
                    hidden: false,
                    project: None,
                    routine: None,
                    agent: None,
                    target_type: None,
                    target: None,
                    domain_session_id: None,
                    domain_activation: None,
                    session_id: Uuid::new_v4(),
                },
            )
            .await
            .expect("decode should allow plaintext chat when configured");

        match result {
            DecodeCommandResult::Command(command) => match *command {
                Command::ChatMessage {
                    content,
                    encrypted_content,
                    ..
                } => {
                    assert_eq!(content, "plaintext prompt");
                    assert!(encrypted_content.is_none());
                }
                other => panic!("unexpected decoded command payload: {other:?}"),
            },
            other => panic!("unexpected decoded command result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn task_execute_without_encrypted_payload_drops() {
        let actor_user_id = Uuid::new_v4();
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new(provider, Uuid::new_v4());
        let result = codec
            .decode_command(
                &CodecContext::for_actor(actor_user_id),
                Command::TaskExecute {
                    task_id: Uuid::new_v4(),
                    project: "demo".into(),
                    execution_run_id: Uuid::new_v4(),
                    routine: None,
                    agent: None,
                    payload: Some(TaskExecuteContent {
                        title: "plaintext task".into(),
                        description: Some("sensitive description".into()),
                        slug: None,
                        acceptance_criteria: Some("sensitive criteria".into()),
                        tags: Vec::new(),
                        status: None,
                        priority: None,
                        task_type: None,
                        complexity: None,
                    }),
                    encrypted_payload: None,
                },
            )
            .await
            .expect("decode should classify plaintext task");

        assert!(matches!(result, DecodeCommandResult::Drop));
    }

    #[tokio::test]
    async fn manifest_sensitive_inline_payload_without_encryption_drops() {
        let actor_user_id = Uuid::new_v4();
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new(provider, Uuid::new_v4());
        let result = codec
            .decode_command(
                &CodecContext::for_actor(actor_user_id),
                Command::ManifestChanged {
                    schema: "manifest.changed.v1".into(),
                    resource_id: Uuid::new_v4(),
                    resource_type: ResourceType::Agent,
                    resource: "demo-agent".into(),
                    action: ResourceAction::Updated,
                    project: None,
                    payload: Some(serde_json::json!({
                        "schema": "manifest.resource.v1",
                        "data": {
                            "prompt_config": {
                                "developer_prompt": "plaintext prompt"
                            }
                        }
                    })),
                    encrypted_payload: None,
                },
            )
            .await
            .expect("decode should classify plaintext manifest inline payload");

        assert!(matches!(result, DecodeCommandResult::Drop));
    }

    #[tokio::test]
    async fn sensitive_agent_response_without_actor_key_drops() {
        let actor_user_id = Uuid::new_v4();
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new(provider, Uuid::new_v4());
        let response = Response::AgentResponse {
            session_id: Some(Uuid::new_v4()),
            payload: StreamEvent::AssistantTextDelta {
                run_id: "run".into(),
                request_id: "request".into(),
                payload: Some(serde_json::json!({ "delta": "plaintext model output" })),
                encrypted_payload: None,
            },
        };

        let encoded = codec
            .encode_response(&CodecContext::for_actor(actor_user_id), response)
            .await
            .expect("encode should classify missing actor key");

        assert!(encoded.is_none());
    }

    #[tokio::test]
    async fn nonsensitive_agent_response_without_actor_key_is_kept() {
        let actor_user_id = Uuid::new_v4();
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new(provider, Uuid::new_v4());
        let session_id = Uuid::new_v4();
        let response = Response::AgentResponse {
            session_id: Some(session_id),
            payload: StreamEvent::RunStarted {
                run_id: "run".into(),
                session_id: session_id.to_string(),
                parent_run_id: None,
                agent_id: None,
                agent_name: None,
            },
        };

        let encoded = codec
            .encode_response(&CodecContext::for_actor(actor_user_id), response)
            .await
            .expect("encode should keep non-sensitive event");

        assert!(matches!(
            encoded,
            Some(Response::AgentResponse {
                payload: StreamEvent::RunStarted { .. },
                ..
            })
        ));
    }

    #[tokio::test]
    async fn task_step_payload_without_org_key_is_stripped() {
        let actor_user_id = Uuid::new_v4();
        let provider = StubKeyProvider {
            user_keys: Arc::new(RwLock::new(HashMap::new())),
        };
        let codec = SecureEnvelopeCodec::new(provider, Uuid::new_v4());
        let response = Response::TaskStepEvent {
            execution_run_id: Uuid::new_v4().to_string(),
            task_id: Some(Uuid::new_v4().to_string()),
            event_type: "step_completed".into(),
            step_name: "agent_response".into(),
            step_type: "agent".into(),
            duration_ms: None,
            data: serde_json::json!({ "ok": true }),
            payload: Some(serde_json::json!({ "output_preview": "plaintext output" })),
            encrypted_payload: None,
            agent: None,
        };

        let encoded = codec
            .encode_response(&CodecContext::for_actor(actor_user_id), response)
            .await
            .expect("encode should strip plaintext task step payload");

        match encoded {
            Some(Response::TaskStepEvent {
                payload,
                encrypted_payload,
                ..
            }) => {
                assert!(payload.is_none());
                assert!(encrypted_payload.is_none());
            }
            other => panic!("unexpected encoded response: {other:?}"),
        }
    }
}
