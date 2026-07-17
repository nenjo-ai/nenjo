//! Tool implementations for platform manifest and REST operations.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use nenjo::{Slug, Tool, ToolCategory, ToolOrigin, ToolResult};
use nenjo_events::EncryptedPayload;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use uuid::Uuid;

use crate::{
    ContentScope, ManifestAccessPolicy, ManifestMcpBackend, ManifestMcpContract,
    PlatformManifestClient, ScopeResource, SensitivePayloadEncoder,
    client::{NotificationMessagePage, NotificationMessageRecord},
    rest::notifications::notification_tools,
};

const AGENT_READ_TOOLS: &[&str] = &["list_agents", "get_agent"];
const AGENT_WRITE_TOOLS: &[&str] = &["configure_agent"];
const ABILITY_READ_TOOLS: &[&str] = &["list_abilities", "get_ability"];
const ABILITY_WRITE_TOOLS: &[&str] = &["configure_ability"];
const COMMAND_READ_TOOLS: &[&str] = &["list_commands", "get_command"];
const COMMAND_WRITE_TOOLS: &[&str] = &["configure_command"];
const DOMAIN_READ_TOOLS: &[&str] = &["list_domains", "get_domain"];
const DOMAIN_WRITE_TOOLS: &[&str] = &["configure_domain"];
const PROJECT_MANIFEST_READ_TOOLS: &[&str] = &["list_projects", "get_project"];
const PROJECT_MANIFEST_WRITE_TOOLS: &[&str] =
    &["create_project", "update_project", "delete_project"];
const LIBRARY_MANIFEST_WRITE_TOOLS: &[&str] = &[
    "create_knowledge_pack",
    "update_knowledge_pack",
    "create_knowledge_doc",
    "update_knowledge_doc",
    "delete_knowledge_doc",
];
const ROUTINE_READ_TOOLS: &[&str] = &["list_routines", "get_routine"];
const ROUTINE_WRITE_TOOLS: &[&str] = &["configure_routine"];
const MODEL_READ_TOOLS: &[&str] = &["list_models", "get_model"];
const MODEL_WRITE_TOOLS: &[&str] = &["create_model", "update_model", "delete_model"];
const COUNCIL_READ_TOOLS: &[&str] = &["list_councils", "get_council"];
const COUNCIL_WRITE_TOOLS: &[&str] = &[
    "create_council",
    "update_council",
    "add_council_member",
    "remove_council_member",
    "delete_council",
];
const CONTEXT_BLOCK_READ_TOOLS: &[&str] = &["list_context_blocks", "get_context_block"];
const CONTEXT_BLOCK_WRITE_TOOLS: &[&str] = &["configure_context_block"];
const NOTIFICATION_READ_TOOLS: &[&str] = &["search_notification_recipients", "list_notifications"];
const NOTIFICATION_WRITE_TOOLS: &[&str] = &["send_notification"];
const NOTIFICATION_OBJECT_TYPE: &str = "push.notification";

/// Platform-owned emitter used by notification tools to publish encrypted push events.
///
/// The `current_session_id` is the transcript session that produced the
/// notification. For routine execution this is the stable step run id, so a
/// dashboard follow-up chat can load the same local transcript context.
#[derive(Debug, Clone, Default)]
pub struct PlatformNotificationRecipient {
    pub user_id: Option<Uuid>,
    pub handle: Option<String>,
}

pub trait PlatformNotificationEmitter: Send + Sync {
    /// Emit an encrypted push notification for an agent slug.
    ///
    /// If `recipient` is set, the encrypted payload is scoped to that user.
    /// Without a recipient, the payload is org-scoped and broadcast to the org.
    fn send_push_notification(
        &self,
        agent: &str,
        current_session_id: Option<Uuid>,
        encrypted_payload: EncryptedPayload,
        recipient: Option<PlatformNotificationRecipient>,
    ) -> Result<()>;
}

const MANIFEST_TOOL_GROUPS: &[(ScopeResource, &[&str], &[&str])] = &[
    (ScopeResource::Agents, AGENT_READ_TOOLS, AGENT_WRITE_TOOLS),
    (
        ScopeResource::Abilities,
        ABILITY_READ_TOOLS,
        ABILITY_WRITE_TOOLS,
    ),
    (
        ScopeResource::Commands,
        COMMAND_READ_TOOLS,
        COMMAND_WRITE_TOOLS,
    ),
    (
        ScopeResource::Domains,
        DOMAIN_READ_TOOLS,
        DOMAIN_WRITE_TOOLS,
    ),
    (
        ScopeResource::Projects,
        PROJECT_MANIFEST_READ_TOOLS,
        PROJECT_MANIFEST_WRITE_TOOLS,
    ),
    (ScopeResource::Library, &[], LIBRARY_MANIFEST_WRITE_TOOLS),
    (
        ScopeResource::Routines,
        ROUTINE_READ_TOOLS,
        ROUTINE_WRITE_TOOLS,
    ),
    (ScopeResource::Models, MODEL_READ_TOOLS, MODEL_WRITE_TOOLS),
    (
        ScopeResource::Councils,
        COUNCIL_READ_TOOLS,
        COUNCIL_WRITE_TOOLS,
    ),
    (
        ScopeResource::ContextBlocks,
        CONTEXT_BLOCK_READ_TOOLS,
        CONTEXT_BLOCK_WRITE_TOOLS,
    ),
];

pub fn add_manifest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Arc<dyn ManifestMcpBackend>,
    policy: &ManifestAccessPolicy,
) {
    let specs = manifest_tool_specs();
    for (resource, read_tools, write_tools) in MANIFEST_TOOL_GROUPS {
        if policy.can_expose_manifest_read_tools(*resource) {
            add_named_manifest_tools(tools, backend.clone(), &specs, read_tools);
        }
        if policy.can_write_resource(*resource) {
            add_named_manifest_tools(tools, backend.clone(), &specs, write_tools);
        }
    }
}

pub fn add_notification_tools<E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    notification_backend: Option<PlatformNotificationToolsBackend<E>>,
    policy: &ManifestAccessPolicy,
) where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    if policy.can_read_resource(ScopeResource::Notify)
        && let Some(backend) = notification_backend.as_ref()
    {
        add_named_notification_tools(tools, backend.clone(), NOTIFICATION_READ_TOOLS);
    }
    if policy.can_write_resource(ScopeResource::Notify)
        && let Some(backend) = notification_backend.as_ref()
    {
        add_named_notification_tools(tools, backend.clone(), NOTIFICATION_WRITE_TOOLS);
    }
}

fn add_named_manifest_tools(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: Arc<dyn ManifestMcpBackend>,
    specs: &HashMap<String, nenjo::ToolSpec>,
    tool_names: &[&str],
) {
    for tool_name in tool_names {
        let Some(spec) = specs.get(*tool_name) else {
            continue;
        };
        if tools.iter().any(|existing| existing.name() == spec.name) {
            continue;
        }
        tools.push(Arc::new(ManifestContractTool::new(
            spec.clone(),
            backend.clone(),
        )));
    }
}

fn add_named_notification_tools<E>(
    tools: &mut Vec<Arc<dyn Tool>>,
    backend: PlatformNotificationToolsBackend<E>,
    tool_names: &[&str],
) where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    for tool_name in tool_names {
        if tools.iter().any(|existing| existing.name() == *tool_name) {
            continue;
        }
        if let Some(tool) = NotificationTool::from_name(tool_name, backend.clone()) {
            tools.push(Arc::new(tool));
        }
    }
}

fn manifest_tool_specs() -> HashMap<String, nenjo::ToolSpec> {
    ManifestMcpContract::tools()
        .into_iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect()
}

struct ManifestContractTool {
    spec: nenjo::ToolSpec,
    backend: Arc<dyn ManifestMcpBackend>,
}

pub struct PlatformNotificationToolsBackend<E> {
    pub client: Arc<PlatformManifestClient>,
    pub payload_encoder: E,
    pub cached_org_id: Option<Uuid>,
    pub agent: Slug,
    pub current_session_id: Option<Uuid>,
    pub notification_sink: Option<Arc<dyn PlatformNotificationEmitter>>,
}

impl<E> Clone for PlatformNotificationToolsBackend<E>
where
    E: Clone,
{
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            payload_encoder: self.payload_encoder.clone(),
            cached_org_id: self.cached_org_id,
            agent: self.agent.clone(),
            current_session_id: self.current_session_id,
            notification_sink: self.notification_sink.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct NotificationContentPayload {
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
}

#[derive(Debug, Serialize)]
struct NotificationListSummary {
    notifications: Vec<NotificationSummary>,
    has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_before: Option<String>,
}

#[derive(Debug, Serialize)]
struct NotificationSummary {
    username: String,
    created_at: String,
    updated_at: String,
    payload: serde_json::Value,
}

impl<E> PlatformNotificationToolsBackend<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    async fn org_id(&self) -> Result<Uuid> {
        if let Some(org_id) = self.cached_org_id {
            return Ok(org_id);
        }

        self.client
            .current_org_id()
            .await
            .context("failed to derive org_id from authenticated API key")
    }

    async fn encode_notification_payload(
        &self,
        account_id: Uuid,
        scope: ContentScope,
        payload: &NotificationContentPayload,
    ) -> Result<EncryptedPayload> {
        let object_id = Uuid::new_v4();
        let encrypted_payload = self
            .payload_encoder
            .encode_payload_with_scope(
                scope,
                account_id,
                object_id,
                NOTIFICATION_OBJECT_TYPE,
                &serde_json::to_value(payload)
                    .context("failed to encode notification content payload")?,
            )
            .await?
            .context("notification payload encoder did not produce encrypted payload")?;
        serde_json::from_value(encrypted_payload).context("invalid encrypted notification payload")
    }

    async fn resolve_notification_recipient_user_id(
        &self,
        args: &SendNotificationArgs,
    ) -> Result<Option<Uuid>> {
        if let Some(user_id) = args.recipient_user_id {
            return Ok(Some(user_id));
        }
        let Some(handle) = args
            .recipient_handle
            .as_deref()
            .and_then(normalize_recipient_handle)
        else {
            return Ok(None);
        };

        let response = self
            .client
            .search_notification_recipients(&crate::client::NotificationRecipientSearchQuery {
                query: Some(handle.clone()),
                limit: Some(10),
            })
            .await?;
        let recipients = response
            .as_array()
            .context("notification recipient search response was not an array")?;
        for recipient in recipients {
            let username = recipient
                .get("username")
                .and_then(|value| value.as_str())
                .map(str::to_ascii_lowercase);
            if username.as_deref() != Some(handle.as_str()) {
                continue;
            }
            let user_id = recipient
                .get("user_id")
                .and_then(|value| value.as_str())
                .context("notification recipient did not include user_id")?
                .parse::<Uuid>()
                .context("notification recipient included invalid user_id")?;
            return Ok(Some(user_id));
        }

        bail!("notification recipient @{handle} was not found")
    }

    async fn notification_list_summary(
        &self,
        page: NotificationMessagePage,
    ) -> Result<NotificationListSummary> {
        let mut notifications = Vec::with_capacity(page.messages.len());
        for message in page.messages {
            notifications
                .push(notification_summary_from_record(&self.payload_encoder, message).await?);
        }
        let next_before = notifications
            .last()
            .map(|notification| notification.created_at.clone())
            .filter(|_| page.has_more);
        Ok(NotificationListSummary {
            notifications,
            has_more: page.has_more,
            next_before,
        })
    }
}

async fn notification_summary_from_record<E>(
    payload_encoder: &E,
    message: NotificationMessageRecord,
) -> Result<NotificationSummary>
where
    E: SensitivePayloadEncoder + Send + Sync,
{
    let payload = match message.encrypted_payload.as_ref() {
        Some(encrypted_payload) => payload_encoder
            .decode_payload(encrypted_payload)
            .await?
            .context("notification encrypted_payload could not be decrypted")?,
        None => json!({ "body": message.content }),
    };

    Ok(NotificationSummary {
        username: message.username,
        created_at: message.created_at,
        updated_at: message.updated_at,
        payload,
    })
}

fn normalize_recipient_handle(value: &str) -> Option<String> {
    let handle = value.trim().trim_start_matches('@').to_ascii_lowercase();
    (!handle.is_empty()).then_some(handle)
}

fn notification_recipient_summaries(recipients: serde_json::Value) -> Result<serde_json::Value> {
    let recipients = recipients
        .as_array()
        .context("notification recipient search response was not an array")?;
    let summaries = recipients
        .iter()
        .map(|recipient| {
            json!({
                "username": recipient.get("username").cloned().unwrap_or(serde_json::Value::Null),
                "name": recipient.get("name").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::Value::Array(summaries))
}

#[derive(Debug, Clone, Copy)]
enum NotificationToolKind {
    SearchNotificationRecipients,
    ListNotifications,
    SendNotification,
}

impl NotificationToolKind {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "search_notification_recipients" => Some(Self::SearchNotificationRecipients),
            "list_notifications" => Some(Self::ListNotifications),
            "send_notification" => Some(Self::SendNotification),
            _ => None,
        }
    }

    fn tool_name(&self) -> &'static str {
        match self {
            Self::SearchNotificationRecipients => "search_notification_recipients",
            Self::ListNotifications => "list_notifications",
            Self::SendNotification => "send_notification",
        }
    }
}

struct NotificationTool<E> {
    kind: NotificationToolKind,
    backend: PlatformNotificationToolsBackend<E>,
    spec: nenjo::ToolSpec,
}

impl<E> NotificationTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn from_name(name: &str, backend: PlatformNotificationToolsBackend<E>) -> Option<Self> {
        let kind = NotificationToolKind::from_name(name)?;
        Some(Self {
            kind,
            backend,
            spec: notification_tool_spec(kind)?,
        })
    }
}

#[async_trait]
impl<E> Tool for NotificationTool<E>
where
    E: SensitivePayloadEncoder + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let output = match self.kind {
            NotificationToolKind::SearchNotificationRecipients => {
                let args: SearchNotificationRecipientsArgs = parse_notification_tool_args(
                    args,
                    "search_notification_recipients",
                    "Expected optional {\"query\":\"...\"}.",
                )?;
                let recipients = self
                    .backend
                    .client
                    .search_notification_recipients(
                        &crate::client::NotificationRecipientSearchQuery {
                            query: args.query,
                            limit: args.limit,
                        },
                    )
                    .await?;
                notification_recipient_summaries(recipients)?
            }
            NotificationToolKind::ListNotifications => {
                let args: ListNotificationsArgs = parse_notification_tool_args(
                    args,
                    "list_notifications",
                    "Expected optional {\"limit\": 50, \"before\": \"<RFC3339 timestamp>\"}.",
                )?;
                let page = self
                    .backend
                    .client
                    .list_notifications(&crate::client::NotificationListQuery {
                        session_id: None,
                        limit: args.limit,
                        before: args.before,
                    })
                    .await?;
                serde_json::to_value(self.backend.notification_list_summary(page).await?)?
            }
            NotificationToolKind::SendNotification => {
                let args: SendNotificationArgs = parse_notification_tool_args(
                    args,
                    "send_notification",
                    "Expected {\"body\":\"...\"}.",
                )?;
                if args.body.trim().is_empty() {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("body is required".into()),
                    });
                }
                let recipient_user_id = self
                    .backend
                    .resolve_notification_recipient_user_id(&args)
                    .await?;
                let (account_id, scope) = if let Some(user_id) = recipient_user_id {
                    (user_id, ContentScope::User)
                } else {
                    (self.backend.org_id().await?, ContentScope::Org)
                };
                let encrypted_payload = self
                    .backend
                    .encode_notification_payload(
                        account_id,
                        scope,
                        &NotificationContentPayload {
                            body: args.body.trim().to_string(),
                            tag: args.tag.filter(|tag| !tag.trim().is_empty()),
                        },
                    )
                    .await?;
                let sink = self
                    .backend
                    .notification_sink
                    .as_ref()
                    .context("notification sink is not available")?;
                let recipient_handle = args
                    .recipient_handle
                    .as_deref()
                    .and_then(normalize_recipient_handle);
                let recipient = if recipient_user_id.is_some() || recipient_handle.is_some() {
                    Some(PlatformNotificationRecipient {
                        user_id: recipient_user_id,
                        handle: recipient_handle,
                    })
                } else {
                    None
                };
                sink.send_push_notification(
                    self.backend.agent.as_str(),
                    self.backend.current_session_id,
                    encrypted_payload,
                    recipient,
                )?;
                json!({
                    "sent": true,
                    "agent": self.backend.agent.as_str(),
                })
            }
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        self.spec.category
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Platform
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchNotificationRecipientsArgs {
    query: Option<String>,
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListNotificationsArgs {
    limit: Option<i64>,
    before: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendNotificationArgs {
    body: String,
    tag: Option<String>,
    recipient_handle: Option<String>,
    recipient_user_id: Option<Uuid>,
}

fn notification_tool_spec(kind: NotificationToolKind) -> Option<nenjo::ToolSpec> {
    notification_tools()
        .into_iter()
        .find(|tool| tool.name == kind.tool_name())
}

fn parse_notification_tool_args<T>(
    args: serde_json::Value,
    tool_name: &str,
    expected_shape: &str,
) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(args.clone()).map_err(|error| {
        let received = serde_json::to_string(&args).unwrap_or_else(|_| "<unprintable>".into());
        anyhow!("invalid {tool_name} args: {error}. {expected_shape} Received: {received}")
    })
}

impl ManifestContractTool {
    fn new(spec: nenjo::ToolSpec, backend: Arc<dyn ManifestMcpBackend>) -> Self {
        Self { spec, backend }
    }
}

#[async_trait]
impl Tool for ManifestContractTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.spec.parameters.clone()
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let value =
            ManifestMcpContract::dispatch(self.backend.as_ref(), &self.spec.name, args).await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&value)?,
            error: None,
        })
    }

    fn category(&self) -> ToolCategory {
        self.spec.category
    }

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Platform
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    #[test]
    fn task_write_dependency_bundles_include_full_reference_read_tools_only() {
        let policy = ManifestAccessPolicy::new(vec!["tasks:write".into()]);

        assert!(policy.can_expose_manifest_read_tools(ScopeResource::Agents));
        assert_eq!(AGENT_READ_TOOLS, ["list_agents", "get_agent"]);
        assert!(!policy.can_write_resource(ScopeResource::Agents));

        assert!(policy.can_expose_manifest_read_tools(ScopeResource::Projects));
        assert_eq!(
            PROJECT_MANIFEST_READ_TOOLS,
            ["list_projects", "get_project"]
        );
        assert!(!policy.can_write_resource(ScopeResource::Projects));

        assert!(policy.can_expose_manifest_read_tools(ScopeResource::Routines));
        assert_eq!(ROUTINE_READ_TOOLS, ["list_routines", "get_routine"]);
        assert!(!policy.can_write_resource(ScopeResource::Routines));
    }

    #[derive(Clone, Default)]
    struct RecordingPayloadEncoder {
        last_payload: Arc<Mutex<Option<serde_json::Value>>>,
        last_scope: Arc<Mutex<Option<ContentScope>>>,
        last_account_id: Arc<Mutex<Option<Uuid>>>,
    }

    struct RecordedNotification {
        agent: String,
        current_session_id: Option<Uuid>,
        encrypted_payload: EncryptedPayload,
        recipient: Option<PlatformNotificationRecipient>,
    }

    #[derive(Default)]
    struct RecordingNotificationSink {
        last_notification: Mutex<Option<RecordedNotification>>,
    }

    impl PlatformNotificationEmitter for RecordingNotificationSink {
        fn send_push_notification(
            &self,
            agent: &str,
            current_session_id: Option<Uuid>,
            encrypted_payload: EncryptedPayload,
            recipient: Option<PlatformNotificationRecipient>,
        ) -> Result<()> {
            *self.last_notification.lock().unwrap() = Some(RecordedNotification {
                agent: agent.to_string(),
                current_session_id,
                encrypted_payload,
                recipient,
            });
            Ok(())
        }
    }

    #[async_trait]
    impl SensitivePayloadEncoder for RecordingPayloadEncoder {
        async fn encode_payload(
            &self,
            account_id: Uuid,
            object_id: Uuid,
            object_type: &str,
            payload: &serde_json::Value,
        ) -> Result<Option<serde_json::Value>> {
            self.encode_payload_with_scope(
                ContentScope::Org,
                account_id,
                object_id,
                object_type,
                payload,
            )
            .await
        }

        async fn encode_payload_with_scope(
            &self,
            scope: ContentScope,
            account_id: Uuid,
            object_id: Uuid,
            object_type: &str,
            payload: &serde_json::Value,
        ) -> Result<Option<serde_json::Value>> {
            *self.last_payload.lock().unwrap() = Some(payload.clone());
            *self.last_scope.lock().unwrap() = Some(scope);
            *self.last_account_id.lock().unwrap() = Some(account_id);
            Ok(Some(json!({
                "account_id": account_id,
                "encryption_scope": scope.encryption_scope_value().unwrap_or("user"),
                "object_id": object_id,
                "object_type": object_type,
                "algorithm": "AES-256-GCM",
                "key_version": 1,
                "nonce": "nonce",
                "ciphertext": "encrypted-test-payload"
            })))
        }

        async fn decode_payload(
            &self,
            payload: &serde_json::Value,
        ) -> Result<Option<serde_json::Value>> {
            Ok(Some(json!({
                "body": payload
                    .get("body")
                    .and_then(|value| value.as_str())
                    .unwrap_or("decrypted notification body"),
                "tag": payload.get("tag").and_then(|value| value.as_str())
            })))
        }
    }

    fn notification_tools_backend(
        encoder: RecordingPayloadEncoder,
        sink: Arc<RecordingNotificationSink>,
    ) -> PlatformNotificationToolsBackend<RecordingPayloadEncoder> {
        PlatformNotificationToolsBackend {
            client: Arc::new(PlatformManifestClient::new("http://localhost", "test").unwrap()),
            payload_encoder: encoder,
            cached_org_id: Some(Uuid::new_v4()),
            agent: Slug::derive("notify-agent"),
            current_session_id: None,
            notification_sink: Some(sink),
        }
    }

    #[tokio::test]
    async fn send_notification_encrypts_body_before_emitting() {
        let encoder = RecordingPayloadEncoder::default();
        let sink = Arc::new(RecordingNotificationSink::default());
        let backend = notification_tools_backend(encoder.clone(), sink.clone());
        let policy = ManifestAccessPolicy::new(vec!["notify:write".into()]);
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        add_notification_tools(&mut tools, Some(backend), &policy);

        let tool = tools
            .iter()
            .find(|tool| tool.name() == "send_notification")
            .expect("send_notification tool should be exposed");
        let result = tool
            .execute(json!({
                "body": "Private notification body",
                "tag": "build-complete"
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(!result.output.contains("Private notification body"));
        let payload = encoder.last_payload.lock().unwrap();
        let payload = payload.as_ref().expect("plaintext passed to encoder");
        assert_eq!(payload["body"], "Private notification body");
        assert_eq!(payload["tag"], "build-complete");
        assert_eq!(*encoder.last_scope.lock().unwrap(), Some(ContentScope::Org));

        let notification = sink.last_notification.lock().unwrap();
        let notification = notification.as_ref().expect("notification emitted");
        assert_eq!(notification.agent, "notify-agent");
        assert!(notification.current_session_id.is_none());
        assert!(notification.recipient.is_none());
        assert_eq!(
            notification.encrypted_payload.object_type,
            "push.notification"
        );
        assert_eq!(
            notification.encrypted_payload.encryption_scope.as_deref(),
            Some("org")
        );
        assert_eq!(
            notification.encrypted_payload.ciphertext,
            "encrypted-test-payload"
        );
    }

    #[tokio::test]
    async fn send_notification_with_recipient_encrypts_for_user_scope() {
        let encoder = RecordingPayloadEncoder::default();
        let sink = Arc::new(RecordingNotificationSink::default());
        let backend = notification_tools_backend(encoder.clone(), sink.clone());
        let policy = ManifestAccessPolicy::new(vec!["notify:write".into()]);
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        add_notification_tools(&mut tools, Some(backend), &policy);

        let recipient_user_id = Uuid::new_v4();
        let tool = tools
            .iter()
            .find(|tool| tool.name() == "send_notification")
            .expect("send_notification tool should be exposed");
        let result = tool
            .execute(json!({
                "body": "Private notification body",
                "recipient_user_id": recipient_user_id
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(
            *encoder.last_scope.lock().unwrap(),
            Some(ContentScope::User)
        );
        assert_eq!(
            *encoder.last_account_id.lock().unwrap(),
            Some(recipient_user_id)
        );

        let notification = sink.last_notification.lock().unwrap();
        let notification = notification.as_ref().expect("notification emitted");
        assert_eq!(notification.encrypted_payload.account_id, recipient_user_id);
        assert_eq!(
            notification.encrypted_payload.encryption_scope.as_deref(),
            Some("user")
        );
        assert_eq!(
            notification
                .recipient
                .as_ref()
                .and_then(|target| target.user_id),
            Some(recipient_user_id)
        );
    }

    #[tokio::test]
    async fn send_notification_forwards_current_session_id() {
        let encoder = RecordingPayloadEncoder::default();
        let sink = Arc::new(RecordingNotificationSink::default());
        let current_session_id = Uuid::new_v4();
        let mut backend = notification_tools_backend(encoder, sink.clone());
        backend.current_session_id = Some(current_session_id);
        let policy = ManifestAccessPolicy::new(vec!["notify:write".into()]);
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        add_notification_tools(&mut tools, Some(backend), &policy);

        let tool = tools
            .iter()
            .find(|tool| tool.name() == "send_notification")
            .expect("send_notification tool should be exposed");
        let result = tool
            .execute(json!({ "body": "Step scoped notification" }))
            .await
            .unwrap();

        assert!(result.success);
        let notification = sink.last_notification.lock().unwrap();
        let notification = notification.as_ref().expect("notification emitted");
        assert_eq!(notification.current_session_id, Some(current_session_id));
    }

    #[tokio::test]
    async fn notification_summary_decrypts_payload_and_uses_username() {
        let encoder = RecordingPayloadEncoder::default();
        let message = NotificationMessageRecord {
            id: Uuid::new_v4(),
            project_id: None,
            agent_id: Some(Uuid::new_v4()),
            user_id: Uuid::new_v4(),
            username: "kratos".to_string(),
            sender: "assistant".to_string(),
            content: "Encrypted notification".to_string(),
            session_id: Uuid::new_v4(),
            created_at: "2026-06-19T20:00:00Z".to_string(),
            updated_at: "2026-06-19T20:00:00Z".to_string(),
            metadata: Some(json!({ "recipient_user_id": Uuid::new_v4() })),
            encrypted_payload: Some(json!({
                "body": "Build finished",
                "tag": "build"
            })),
        };

        let summary = notification_summary_from_record(&encoder, message)
            .await
            .unwrap();
        let output = serde_json::to_value(summary).unwrap();

        assert_eq!(output["username"], "kratos");
        assert_eq!(output["payload"]["body"], "Build finished");
        assert_eq!(output["payload"]["tag"], "build");
        assert!(output.get("sender").is_none());
        assert!(output.get("user_id").is_none());
        assert!(output.get("encrypted_payload").is_none());
        assert!(!output.to_string().contains("recipient_user_id"));
    }

    #[test]
    fn notification_recipient_summary_omits_user_ids() {
        let recipients = notification_recipient_summaries(json!([
            {
                "user_id": Uuid::new_v4(),
                "username": "kratos",
                "name": "Kratos"
            }
        ]))
        .unwrap();

        assert_eq!(recipients[0]["username"], "kratos");
        assert_eq!(recipients[0]["name"], "Kratos");
        assert!(recipients[0].get("user_id").is_none());
    }
}
