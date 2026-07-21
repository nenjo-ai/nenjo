//! Runtime hooks for agent lifecycle events.
//!
//! Hooks are resolved from manifests by the provider, activated by the harness,
//! and executed by the agent runtime around the lifecycle points it owns.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use nenjo_models::ChatMessage;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::Slug;
use crate::manifest::{CommandManifest, HookManifest, ManifestIdentity, SkillManifest};

const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;
const MAX_HOOK_OUTPUT_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookEvent {
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    Other(String),
}

impl HookEvent {
    pub fn from_name(name: impl Into<String>) -> Self {
        match name.into().as_str() {
            "UserPromptSubmit" => Self::UserPromptSubmit,
            "PreToolUse" => Self::PreToolUse,
            "PostToolUse" => Self::PostToolUse,
            "Stop" => Self::Stop,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::Other(value) => value.as_str(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum HookSource {
    Command {
        name: String,
        command: String,
    },
    Skill {
        name: String,
        skill_root_dir: Option<PathBuf>,
    },
    Domain {
        name: String,
    },
    Other {
        kind: String,
        name: String,
    },
}

impl HookSource {
    pub fn command(command: &CommandManifest) -> Self {
        Self::Command {
            name: command.name.clone(),
            command: command.command.clone(),
        }
    }

    pub fn skill(skill: &SkillManifest) -> Self {
        Self::Skill {
            name: skill.name.clone(),
            skill_root_dir: (!skill.root_dir.as_os_str().is_empty())
                .then(|| skill.root_dir.clone()),
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            Self::Command { .. } => "command",
            Self::Skill { .. } => "skill",
            Self::Domain { .. } => "domain",
            Self::Other { kind, .. } => kind.as_str(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Command { name, .. }
            | Self::Skill { name, .. }
            | Self::Domain { name }
            | Self::Other { name, .. } => name.as_str(),
        }
    }

    pub fn command_name(&self) -> Option<&str> {
        match self {
            Self::Command { command, .. } => Some(command.as_str()),
            Self::Skill { .. } | Self::Domain { .. } | Self::Other { .. } => None,
        }
    }

    pub fn skill_root_dir(&self) -> Option<&Path> {
        match self {
            Self::Skill { skill_root_dir, .. } => skill_root_dir.as_deref(),
            Self::Command { .. } | Self::Domain { .. } | Self::Other { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedHookCommand {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedHook {
    pub slug: Slug,
    pub name: String,
    pub event: HookEvent,
    pub matcher: Option<String>,
    pub hook_type: String,
    pub command: Option<ResolvedHookCommand>,
    pub timeout: Duration,
    pub plugin_root_dir: Option<PathBuf>,
    pub metadata: Value,
}

impl ResolvedHook {
    pub fn from_manifest(hook: &HookManifest) -> Self {
        Self {
            slug: hook.manifest_slug().clone(),
            name: hook.name.clone(),
            event: HookEvent::from_name(hook.event.clone()),
            matcher: hook.matcher.clone(),
            hook_type: hook.hook_type.clone(),
            command: hook.command.as_ref().map(|command| ResolvedHookCommand {
                command: command.path.clone(),
                args: command.args.clone(),
            }),
            timeout: Duration::from_secs(hook.timeout_seconds.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS)),
            plugin_root_dir: hook.plugin_root_dir.clone(),
            metadata: hook.metadata.clone(),
        }
    }

    pub fn label(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone)]
pub struct ActiveHook {
    pub source: HookSource,
    pub hook: ResolvedHook,
}

#[derive(Debug, Clone)]
pub struct ActiveHookScope {
    pub source: HookSource,
    pub hooks: Vec<ResolvedHook>,
}

impl ActiveHookScope {
    pub fn command(command: &CommandManifest, hooks: Vec<ResolvedHook>) -> Self {
        Self {
            source: HookSource::command(command),
            hooks,
        }
    }

    pub fn skill(skill: &SkillManifest, hooks: Vec<ResolvedHook>) -> Self {
        Self {
            source: HookSource::skill(skill),
            hooks,
        }
    }
}

#[derive(Debug)]
pub struct HookRuntime {
    session_id: Uuid,
    workspace_dir: PathBuf,
    transcript_dir: PathBuf,
    hooks: Arc<RwLock<Vec<ActiveHook>>>,
}

impl HookRuntime {
    pub fn new(
        session_id: Uuid,
        workspace_dir: impl Into<PathBuf>,
        transcript_dir: impl Into<PathBuf>,
        scopes: Vec<ActiveHookScope>,
    ) -> Self {
        let hooks = active_hooks_from_scopes(scopes);
        Self {
            session_id,
            workspace_dir: workspace_dir.into(),
            transcript_dir: transcript_dir.into(),
            hooks: Arc::new(RwLock::new(hooks)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks
            .read()
            .map(|hooks| hooks.is_empty())
            .unwrap_or(true)
    }

    pub fn activate_scope(&self, scope: ActiveHookScope) {
        self.activate_scopes(vec![scope]);
    }

    pub fn activate_scopes(&self, scopes: Vec<ActiveHookScope>) {
        let mut active_hooks = active_hooks_from_scopes(scopes);
        if active_hooks.is_empty() {
            return;
        }
        if let Ok(mut hooks) = self.hooks.write() {
            for active_hook in active_hooks.drain(..) {
                if hooks
                    .iter()
                    .any(|existing| same_active_hook(existing, &active_hook))
                {
                    continue;
                }
                hooks.push(active_hook);
            }
        }
    }

    pub fn matching_hooks(&self, event: &HookEvent, subject: Option<&str>) -> Vec<ActiveHook> {
        let Ok(hooks) = self.hooks.read() else {
            return Vec::new();
        };
        hooks
            .iter()
            .filter(|active| {
                active.hook.event == *event
                    && matcher_matches(active.hook.matcher.as_deref(), subject)
            })
            .cloned()
            .collect()
    }

    pub async fn execute(&self, active: &ActiveHook, event: HookRuntimeEvent<'_>) -> HookExecution {
        match active.hook.hook_type.as_str() {
            "command" => self.execute_command_hook(active, event).await,
            unsupported => HookExecution {
                success: false,
                blocked: false,
                reason: None,
                system_message: None,
                additional_context: None,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("Unsupported hook type: {unsupported}"),
            },
        }
    }

    async fn execute_command_hook(
        &self,
        active: &ActiveHook,
        event: HookRuntimeEvent<'_>,
    ) -> HookExecution {
        let Some(command) = active.hook.command.as_ref() else {
            return HookExecution {
                success: false,
                blocked: false,
                reason: None,
                system_message: None,
                additional_context: None,
                exit_code: None,
                stdout: String::new(),
                stderr: "Command hook is missing a command path".to_string(),
            };
        };

        let input = match self.input_json(active, event).await {
            Ok(input) => input,
            Err(error) => {
                return HookExecution {
                    success: false,
                    blocked: false,
                    reason: None,
                    system_message: None,
                    additional_context: None,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("Failed to build hook input: {error}"),
                };
            }
        };

        let mut process = match build_hook_process(active, command, &self.workspace_dir) {
            Ok(process) => process,
            Err(error) => {
                return HookExecution {
                    success: false,
                    blocked: false,
                    reason: None,
                    system_message: None,
                    additional_context: None,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("Failed to build hook command: {error}"),
                };
            }
        };

        process.stdin(std::process::Stdio::piped());
        process.stdout(std::process::Stdio::piped());
        process.stderr(std::process::Stdio::piped());
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) => {
                return HookExecution {
                    success: false,
                    blocked: false,
                    reason: None,
                    system_message: None,
                    additional_context: None,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("Failed to spawn hook command: {error}"),
                };
            }
        };

        if let Some(mut stdin) = child.stdin.take()
            && let Err(error) = stdin.write_all(input.to_string().as_bytes()).await
        {
            return HookExecution {
                success: false,
                blocked: false,
                reason: None,
                system_message: None,
                additional_context: None,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("Failed to write hook input: {error}"),
            };
        }

        let output = match tokio::time::timeout(active.hook.timeout, child.wait_with_output()).await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return HookExecution {
                    success: false,
                    blocked: false,
                    reason: None,
                    system_message: None,
                    additional_context: None,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!("Failed to wait for hook command: {error}"),
                };
            }
            Err(_) => {
                return HookExecution {
                    success: false,
                    blocked: false,
                    reason: None,
                    system_message: None,
                    additional_context: None,
                    exit_code: None,
                    stdout: String::new(),
                    stderr: format!(
                        "Hook command timed out after {}s",
                        active.hook.timeout.as_secs()
                    ),
                };
            }
        };

        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
        truncate_output(&mut stdout);
        truncate_output(&mut stderr);
        let parsed = parse_hook_stdout(&stdout);
        let decision = HookDecision::from_payload(parsed.as_ref());
        let blocked = output.status.code() == Some(2)
            || decision == Some(HookDecision::Block)
            || (active.hook.event == HookEvent::Stop
                && decision == Some(HookDecision::RequestNextTurn));
        let reason = hook_reason(
            &active.hook.event,
            decision,
            parsed.as_ref(),
            output.status.code(),
            &stderr,
        );
        let system_message = parsed
            .as_ref()
            .and_then(|payload| payload.get("systemMessage"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        HookExecution {
            success: output.status.success(),
            blocked,
            reason,
            system_message,
            additional_context: additional_context_for_event(
                active.hook.event.as_str(),
                parsed.as_ref(),
                &stdout,
                output.status.success(),
                blocked,
            ),
            exit_code: output.status.code(),
            stdout,
            stderr,
        }
    }

    async fn input_json(&self, active: &ActiveHook, event: HookRuntimeEvent<'_>) -> Result<Value> {
        let transcript_path = match &event {
            HookRuntimeEvent::UserPromptSubmit { messages, .. } => {
                self.write_transcript(messages, "").await?
            }
            HookRuntimeEvent::Stop {
                messages,
                final_text,
            } => self.write_transcript(messages, final_text).await?,
            HookRuntimeEvent::PreToolUse { .. } | HookRuntimeEvent::PostToolUse { .. } => {
                self.ensure_transcript_path().await?
            }
        };
        let base = json!({
            "session_id": self.session_id,
            "transcript_path": transcript_path.to_string_lossy(),
            "cwd": self.workspace_dir.to_string_lossy(),
            "permission_mode": "default",
            "hook_event_name": active.hook.event.as_str(),
        });
        let mut object = base
            .as_object()
            .cloned()
            .expect("base hook JSON must be an object");
        match event {
            HookRuntimeEvent::UserPromptSubmit { prompt, .. } => {
                object.insert("prompt".to_string(), Value::String(prompt.to_string()));
            }
            HookRuntimeEvent::PreToolUse {
                tool_name,
                tool_input,
                tool_use_id,
            } => {
                object.insert(
                    "tool_name".to_string(),
                    Value::String(tool_name.to_string()),
                );
                object.insert("tool_input".to_string(), tool_input.clone());
                if let Some(tool_use_id) = tool_use_id {
                    object.insert(
                        "tool_use_id".to_string(),
                        Value::String(tool_use_id.to_string()),
                    );
                }
            }
            HookRuntimeEvent::PostToolUse {
                tool_name,
                tool_input,
                tool_response,
                tool_use_id,
            } => {
                object.insert(
                    "tool_name".to_string(),
                    Value::String(tool_name.to_string()),
                );
                object.insert("tool_input".to_string(), tool_input.clone());
                object.insert("tool_response".to_string(), tool_response.clone());
                if let Some(tool_use_id) = tool_use_id {
                    object.insert(
                        "tool_use_id".to_string(),
                        Value::String(tool_use_id.to_string()),
                    );
                }
            }
            HookRuntimeEvent::Stop { .. } => {}
        }
        Ok(Value::Object(object))
    }

    async fn ensure_transcript_path(&self) -> Result<PathBuf> {
        let transcript_dir = self.transcript_dir.clone();
        tokio::fs::create_dir_all(&transcript_dir)
            .await
            .with_context(|| format!("failed to create {}", transcript_dir.display()))?;
        let transcript_path = transcript_dir.join(format!("{}.jsonl", self.session_id));
        if !transcript_path.exists() {
            tokio::fs::write(&transcript_path, "")
                .await
                .with_context(|| format!("failed to write {}", transcript_path.display()))?;
        }
        Ok(transcript_path)
    }

    async fn write_transcript(
        &self,
        messages: &[ChatMessage],
        final_text: &str,
    ) -> Result<PathBuf> {
        let transcript_dir = self.transcript_dir.clone();
        tokio::fs::create_dir_all(&transcript_dir)
            .await
            .with_context(|| format!("failed to create {}", transcript_dir.display()))?;
        let transcript_path = transcript_dir.join(format!("{}.jsonl", self.session_id));
        let mut lines = Vec::with_capacity(messages.len() + 1);
        for message in messages {
            lines.push(claude_transcript_line(&message.role, &message.content));
        }
        if !final_text.trim().is_empty()
            && !messages
                .iter()
                .rev()
                .any(|message| message.role == "assistant" && message.content == final_text)
        {
            lines.push(claude_transcript_line("assistant", final_text));
        }
        tokio::fs::write(&transcript_path, lines.join("\n"))
            .await
            .with_context(|| format!("failed to write {}", transcript_path.display()))?;
        Ok(transcript_path)
    }
}

fn active_hooks_from_scopes(scopes: Vec<ActiveHookScope>) -> Vec<ActiveHook> {
    scopes
        .into_iter()
        .flat_map(|scope| {
            scope.hooks.into_iter().map(move |hook| ActiveHook {
                source: scope.source.clone(),
                hook,
            })
        })
        .collect()
}

fn same_active_hook(left: &ActiveHook, right: &ActiveHook) -> bool {
    left.hook.slug == right.hook.slug
        && left.source.kind() == right.source.kind()
        && left.source.name() == right.source.name()
}

#[derive(Debug, Clone)]
pub enum HookRuntimeEvent<'a> {
    UserPromptSubmit {
        prompt: &'a str,
        messages: &'a [ChatMessage],
    },
    PreToolUse {
        tool_name: &'a str,
        tool_input: &'a Value,
        tool_use_id: Option<&'a str>,
    },
    PostToolUse {
        tool_name: &'a str,
        tool_input: &'a Value,
        tool_response: &'a Value,
        tool_use_id: Option<&'a str>,
    },
    Stop {
        messages: &'a [ChatMessage],
        final_text: &'a str,
    },
}

#[derive(Debug, Clone)]
pub struct HookExecution {
    pub success: bool,
    pub blocked: bool,
    pub reason: Option<String>,
    pub system_message: Option<String>,
    pub additional_context: Option<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct HookBlock {
    pub hook: String,
    pub reason: String,
    pub system_message: Option<String>,
}

pub fn resolve_command_hooks(
    manifest_hooks: &[HookManifest],
    command: &CommandManifest,
) -> Vec<ResolvedHook> {
    manifest_hooks
        .iter()
        .filter(|hook| {
            command
                .hooks
                .iter()
                .any(|hook_ref| hook_matches_ref(hook, hook_ref))
        })
        .map(ResolvedHook::from_manifest)
        .collect()
}

pub fn resolve_skill_hooks(
    manifest_hooks: &[HookManifest],
    skill: &SkillManifest,
) -> Vec<ResolvedHook> {
    manifest_hooks
        .iter()
        .filter(|hook| {
            skill
                .hooks
                .iter()
                .any(|hook_ref| hook_matches_ref(hook, hook_ref))
        })
        .map(ResolvedHook::from_manifest)
        .collect()
}

fn hook_matches_ref(hook: &HookManifest, hook_ref: &Slug) -> bool {
    hook_ref == &hook.slug
}

fn matcher_matches(matcher: Option<&str>, subject: Option<&str>) -> bool {
    let Some(matcher) = matcher.map(str::trim).filter(|matcher| !matcher.is_empty()) else {
        return true;
    };
    if matcher == "*" {
        return true;
    }
    let Some(subject) = subject else {
        return matcher == "*";
    };
    matcher == subject
        || matcher
            .split('|')
            .map(str::trim)
            .any(|part| part == subject || part == "*")
}

fn build_hook_process(
    active: &ActiveHook,
    command: &ResolvedHookCommand,
    workspace_dir: &Path,
) -> Result<tokio::process::Command> {
    let mut process = if command_looks_like_shell(&command.command) {
        let mut shell = tokio::process::Command::new("sh");
        shell.arg("-c").arg(&command.command);
        shell
    } else {
        let path = resolve_hook_command_path(active, &command.command)?;
        let mut shell = tokio::process::Command::new("bash");
        shell.arg(path);
        for arg in &command.args {
            shell.arg(arg);
        }
        shell
    };
    process.current_dir(workspace_dir);
    process.env("CLAUDE_PROJECT_DIR", workspace_dir);
    process.env("NENJO_WORKSPACE_DIR", workspace_dir);
    if let Some(plugin_root) = &active.hook.plugin_root_dir {
        process.env("CLAUDE_PLUGIN_ROOT", plugin_root);
        process.env("CLAUDE_PLUGIN_DIR", plugin_root);
        process.env("NENJO_PLUGIN_ROOT", plugin_root);
    }
    if let Some(skill_root) = active.source.skill_root_dir() {
        process.env("CLAUDE_SKILL_DIR", skill_root);
        process.env("NENJO_SKILL_DIR", skill_root);
    }
    Ok(process)
}

fn command_looks_like_shell(command: &str) -> bool {
    command.chars().any(|ch| {
        ch.is_whitespace()
            || matches!(
                ch,
                '$' | '"' | '\'' | '|' | '&' | ';' | '<' | '>' | '(' | ')'
            )
    })
}

fn resolve_hook_command_path(active: &ActiveHook, raw_path: &str) -> Result<PathBuf> {
    let path = Path::new(raw_path);
    if raw_path.trim().is_empty() {
        anyhow::bail!("hook command path must not be empty");
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("hook command path must stay inside the plugin root");
    }
    let Some(plugin_root) = active.hook.plugin_root_dir.as_ref() else {
        anyhow::bail!("relative hook command path requires plugin_root_dir");
    };
    Ok(plugin_root.join(path))
}

fn parse_hook_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .find_map(|line| serde_json::from_str::<Value>(line).ok())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookDecision {
    Allow,
    Block,
    AppendContext,
    RequestNextTurn,
}

impl HookDecision {
    fn from_payload(payload: Option<&Value>) -> Option<Self> {
        match payload
            .and_then(|payload| payload.get("decision"))
            .and_then(Value::as_str)
        {
            Some("allow") => Some(Self::Allow),
            Some("block") => Some(Self::Block),
            Some("append_context") => Some(Self::AppendContext),
            Some("request_next_turn") => Some(Self::RequestNextTurn),
            Some(_) | None => None,
        }
    }
}

fn hook_reason(
    event: &HookEvent,
    decision: Option<HookDecision>,
    parsed: Option<&Value>,
    exit_code: Option<i32>,
    stderr: &str,
) -> Option<String> {
    parsed
        .and_then(|payload| payload.get("reason"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            (event == &HookEvent::Stop && decision == Some(HookDecision::RequestNextTurn))
                .then(|| {
                    parsed
                        .and_then(|payload| payload.get("prompt"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .flatten()
        })
        .or_else(|| (exit_code == Some(2)).then(|| stderr.to_string()))
        .filter(|value| !value.trim().is_empty())
}

fn additional_context_for_event(
    hook_event: &str,
    parsed: Option<&Value>,
    stdout: &str,
    success: bool,
    blocked: bool,
) -> Option<String> {
    if hook_event != HookEvent::UserPromptSubmit.as_str() || !success || blocked {
        return None;
    }
    if let Some(parsed) = parsed {
        return parsed
            .pointer("/hookSpecificOutput/additionalContext")
            .or_else(|| parsed.get("additionalContext"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
    }
    let trimmed = stdout.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn truncate_output(output: &mut String) {
    if output.len() <= MAX_HOOK_OUTPUT_BYTES {
        return;
    }
    output.truncate(output.floor_char_boundary(MAX_HOOK_OUTPUT_BYTES));
    output.push_str("\n... [hook output truncated]");
}

fn claude_transcript_line(role: &str, content: &str) -> String {
    json!({
        "type": role,
        "message": {
            "role": role,
            "content": [
                {
                    "type": "text",
                    "text": content
                }
            ]
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{HookCommandManifest, HookManifest, SkillManifest};

    #[tokio::test]
    async fn stop_hook_can_block_with_reason() {
        let temp = tempfile::tempdir().unwrap();
        let plugin = temp.path().join("plugin");
        tokio::fs::create_dir_all(plugin.join("hooks"))
            .await
            .unwrap();
        let script = plugin.join("hooks").join("stop.sh");
        tokio::fs::write(
            &script,
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
transcript="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
test -f "$transcript"
printf '{"decision":"block","reason":"continue please","systemMessage":"again"}'
"#,
        )
        .await
        .unwrap();

        let hook = ResolvedHook::from_manifest(&HookManifest {
            slug: crate::Slug::derive("stop-hook"),
            name: "stop-hook".to_string(),
            description: None,
            event: "Stop".to_string(),
            matcher: Some("*".to_string()),
            hook_type: "command".to_string(),
            command: Some(HookCommandManifest {
                path: "hooks/stop.sh".to_string(),
                args: Vec::new(),
            }),
            timeout_seconds: Some(5),
            plugin_root_path: None,
            plugin_root_dir: Some(plugin),
            source_type: "package".to_string(),
            read_only: false,
            metadata: Value::Null,
        });
        let scope = ActiveHookScope {
            source: HookSource::Other {
                kind: "test".to_string(),
                name: "test".to_string(),
            },
            hooks: vec![hook],
        };
        let runtime = HookRuntime::new(
            Uuid::new_v4(),
            temp.path(),
            temp.path().join("state").join("hooks"),
            vec![scope],
        );
        let active = runtime
            .matching_hooks(&HookEvent::Stop, None)
            .into_iter()
            .next()
            .unwrap();
        let messages = vec![ChatMessage::assistant("done".to_string())];
        let execution = runtime
            .execute(
                &active,
                HookRuntimeEvent::Stop {
                    messages: &messages,
                    final_text: "done",
                },
            )
            .await;
        assert!(execution.success);
        assert!(execution.blocked);
        assert_eq!(execution.reason.as_deref(), Some("continue please"));
        assert_eq!(execution.system_message.as_deref(), Some("again"));
    }

    #[tokio::test]
    async fn stop_hook_request_next_turn_blocks_with_prompt_reason() {
        let temp = tempfile::tempdir().unwrap();
        let plugin = temp.path().join("plugin");
        tokio::fs::create_dir_all(plugin.join("hooks"))
            .await
            .unwrap();
        let script = plugin.join("hooks").join("stop.sh");
        tokio::fs::write(
            &script,
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
transcript="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
test -f "$transcript"
printf '{"decision":"request_next_turn","prompt":"revise before stopping","systemMessage":"continue"}'
"#,
        )
        .await
        .unwrap();

        let hook = ResolvedHook::from_manifest(&HookManifest {
            slug: crate::Slug::derive("stop-hook"),
            name: "stop-hook".to_string(),
            description: None,
            event: "Stop".to_string(),
            matcher: Some("*".to_string()),
            hook_type: "command".to_string(),
            command: Some(HookCommandManifest {
                path: "hooks/stop.sh".to_string(),
                args: Vec::new(),
            }),
            timeout_seconds: Some(5),
            plugin_root_path: None,
            plugin_root_dir: Some(plugin),
            source_type: "package".to_string(),
            read_only: false,
            metadata: Value::Null,
        });
        let scope = ActiveHookScope {
            source: HookSource::Other {
                kind: "test".to_string(),
                name: "test".to_string(),
            },
            hooks: vec![hook],
        };
        let runtime = HookRuntime::new(
            Uuid::new_v4(),
            temp.path(),
            temp.path().join("state").join("hooks"),
            vec![scope],
        );
        let active = runtime
            .matching_hooks(&HookEvent::Stop, None)
            .into_iter()
            .next()
            .unwrap();
        let messages = vec![ChatMessage::assistant("done".to_string())];
        let execution = runtime
            .execute(
                &active,
                HookRuntimeEvent::Stop {
                    messages: &messages,
                    final_text: "done",
                },
            )
            .await;
        assert!(execution.success);
        assert!(execution.blocked);
        assert_eq!(execution.reason.as_deref(), Some("revise before stopping"));
        assert_eq!(execution.system_message.as_deref(), Some("continue"));
    }

    #[tokio::test]
    async fn user_prompt_submit_hook_can_add_context_and_receives_transcript() {
        let temp = tempfile::tempdir().unwrap();
        let plugin = temp.path().join("plugin");
        tokio::fs::create_dir_all(plugin.join("hooks"))
            .await
            .unwrap();
        let script = plugin.join("hooks").join("prompt.sh");
        tokio::fs::write(
            &script,
            r#"#!/usr/bin/env bash
set -euo pipefail
input="$(cat)"
prompt="$(printf '%s' "$input" | sed -n 's/.*"prompt":"\([^"]*\)".*/\1/p')"
transcript="$(printf '%s' "$input" | sed -n 's/.*"transcript_path":"\([^"]*\)".*/\1/p')"
test "$prompt" = "review this"
test -f "$transcript"
grep -q "review this" "$transcript"
printf '{"hookSpecificOutput":{"additionalContext":"review checklist"}}'
"#,
        )
        .await
        .unwrap();

        let hook = ResolvedHook::from_manifest(&HookManifest {
            slug: crate::Slug::derive("prompt-hook"),
            name: "prompt-hook".to_string(),
            description: None,
            event: "UserPromptSubmit".to_string(),
            matcher: Some("*".to_string()),
            hook_type: "command".to_string(),
            command: Some(HookCommandManifest {
                path: "hooks/prompt.sh".to_string(),
                args: Vec::new(),
            }),
            timeout_seconds: Some(5),
            plugin_root_path: None,
            plugin_root_dir: Some(plugin),
            source_type: "package".to_string(),
            read_only: false,
            metadata: Value::Null,
        });
        let scope = ActiveHookScope {
            source: HookSource::Other {
                kind: "test".to_string(),
                name: "test".to_string(),
            },
            hooks: vec![hook],
        };
        let runtime = HookRuntime::new(
            Uuid::new_v4(),
            temp.path(),
            temp.path().join("state").join("hooks"),
            vec![scope],
        );
        let active = runtime
            .matching_hooks(&HookEvent::UserPromptSubmit, None)
            .into_iter()
            .next()
            .unwrap();
        let messages = vec![ChatMessage::user("review this".to_string())];
        let execution = runtime
            .execute(
                &active,
                HookRuntimeEvent::UserPromptSubmit {
                    prompt: "review this",
                    messages: &messages,
                },
            )
            .await;

        assert!(execution.success);
        assert!(!execution.blocked);
        assert_eq!(
            execution.additional_context.as_deref(),
            Some("review checklist")
        );
    }

    #[test]
    fn resolves_and_activates_skill_hooks_once() {
        let hook_manifest = HookManifest {
            slug: crate::Slug::derive("acme-stop-review"),
            name: "Acme Stop Review".to_string(),
            description: None,
            event: "Stop".to_string(),
            matcher: Some("*".to_string()),
            hook_type: "command".to_string(),
            command: Some(HookCommandManifest {
                path: "hooks/stop.sh".to_string(),
                args: Vec::new(),
            }),
            timeout_seconds: None,
            plugin_root_path: None,
            plugin_root_dir: None,
            source_type: "package".to_string(),
            read_only: false,
            metadata: Value::Null,
        };
        let skill: SkillManifest = serde_json::from_value(json!({
            "id": Uuid::new_v4(),
            "slug": "acme-review",
            "name": "Acme Review",
            "root_dir": "/tmp/acme/skills/review",
            "hooks": ["acme-stop-review"]
        }))
        .unwrap();
        let resolved_hooks = resolve_skill_hooks(&[hook_manifest], &skill);
        assert_eq!(resolved_hooks.len(), 1);

        let runtime = HookRuntime::new(
            Uuid::new_v4(),
            "/tmp/workspace",
            "/tmp/state/hooks",
            Vec::new(),
        );
        let scope = ActiveHookScope::skill(&skill, resolved_hooks);
        runtime.activate_scope(scope.clone());
        runtime.activate_scope(scope);

        assert_eq!(runtime.matching_hooks(&HookEvent::Stop, None).len(), 1);
        assert_eq!(
            runtime.matching_hooks(&HookEvent::Stop, None)[0].hook.slug,
            Slug::derive("acme-stop-review")
        );
    }
}
