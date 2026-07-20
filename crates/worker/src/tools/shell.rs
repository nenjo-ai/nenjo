//! Shell command execution tool with sandboxing, rate limiting, and environment isolation.

use crate::tools::runtime::RuntimeAdapter;
use crate::tools::security::SecurityPolicy;
use crate::tools::{Tool, ToolCategory, ToolResult};
use anyhow::Context;
use async_trait::async_trait;
use nenjo::skills::SkillRuntimeState;
use nenjo::{
    AsyncControl, AsyncControls, AsyncOperationHandle, AsyncOperationStartReceipt,
    AsyncOperationTranscriptEvent, StartAsyncOperation, current_async_operation_runtime,
};
use serde::Serialize;
use serde_json::json;
use std::io;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Maximum shell command execution time before kill.
const SHELL_TIMEOUT_SECS: u64 = 60;
/// Commands still running after this window become asynchronous operations.
const SHELL_INITIAL_WAIT: Duration = Duration::from_secs(2);
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;
const OUTPUT_READ_CHUNK_BYTES: usize = 8_192;
const TRANSCRIPT_SNAPSHOT_BYTES: usize = 8_192;
static SHELL_OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);
/// Environment variables safe to pass to shell commands.
/// Only functional variables are included — never API keys or secrets.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

#[derive(Debug, Clone, Copy)]
enum ShellStream {
    Stdout,
    Stderr,
}

impl ShellStream {
    const fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

#[derive(Debug, Default)]
struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedStream {
    fn push(&mut self, chunk: &[u8]) -> (Vec<u8>, bool) {
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(self.bytes.len());
        let accepted_len = remaining.min(chunk.len());
        let accepted = chunk[..accepted_len].to_vec();
        self.bytes.extend_from_slice(&accepted);
        let newly_truncated = accepted_len < chunk.len() && !self.truncated;
        self.truncated |= accepted_len < chunk.len();
        (accepted, newly_truncated)
    }

    fn render(&self, marker: &str) -> String {
        let mut rendered = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.truncated {
            rendered.push_str(marker);
        }
        rendered
    }

    fn recent(&self) -> Option<String> {
        if self.bytes.is_empty() {
            return None;
        }
        let start = self.bytes.len().saturating_sub(TRANSCRIPT_SNAPSHOT_BYTES);
        Some(String::from_utf8_lossy(&self.bytes[start..]).into_owned())
    }
}

#[derive(Debug, Default)]
struct CapturedShellOutput {
    stdout: CapturedStream,
    stderr: CapturedStream,
}

impl CapturedShellOutput {
    fn stream_mut(&mut self, stream: ShellStream) -> &mut CapturedStream {
        match stream {
            ShellStream::Stdout => &mut self.stdout,
            ShellStream::Stderr => &mut self.stderr,
        }
    }

    fn render(&self) -> RenderedShellOutput {
        RenderedShellOutput {
            stdout: self.stdout.render("\n... [output truncated at 1MB]"),
            stderr: self.stderr.render("\n... [stderr truncated at 1MB]"),
        }
    }

    fn transcript_snapshots(&self) -> Vec<String> {
        [ShellStream::Stdout, ShellStream::Stderr]
            .into_iter()
            .filter_map(|stream| {
                let captured = match stream {
                    ShellStream::Stdout => &self.stdout,
                    ShellStream::Stderr => &self.stderr,
                };
                captured
                    .recent()
                    .map(|recent| format!("[{}; recent output]\n{recent}", stream.label()))
            })
            .collect()
    }
}

struct ShellOutputState {
    captured: CapturedShellOutput,
    reporter: Option<AsyncOperationHandle>,
}

#[derive(Clone)]
struct ShellOutputSink {
    state: Arc<Mutex<ShellOutputState>>,
}

impl ShellOutputSink {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ShellOutputState {
                captured: CapturedShellOutput::default(),
                reporter: None,
            })),
        }
    }

    async fn push(&self, stream: ShellStream, chunk: &[u8]) {
        let (reporter, accepted, newly_truncated) = {
            let mut state = self.state.lock().await;
            let (accepted, newly_truncated) = state.captured.stream_mut(stream).push(chunk);
            (state.reporter.clone(), accepted, newly_truncated)
        };
        let Some(reporter) = reporter else {
            return;
        };
        if !accepted.is_empty() {
            reporter
                .transcript(AsyncOperationTranscriptEvent::OutputChunk {
                    summary: format!(
                        "[{}] {}",
                        stream.label(),
                        String::from_utf8_lossy(&accepted)
                    ),
                })
                .await;
        }
        if newly_truncated {
            reporter
                .transcript(AsyncOperationTranscriptEvent::OutputChunk {
                    summary: format!("[{}] output truncated at 1MB", stream.label()),
                })
                .await;
        }
    }

    async fn activate(&self, reporter: AsyncOperationHandle) {
        let snapshots = {
            let mut state = self.state.lock().await;
            state.reporter = Some(reporter.clone());
            state.captured.transcript_snapshots()
        };
        for summary in snapshots {
            reporter
                .transcript(AsyncOperationTranscriptEvent::OutputChunk { summary })
                .await;
        }
    }

    async fn captured(&self) -> RenderedShellOutput {
        self.state.lock().await.captured.render()
    }
}

struct RunningShell {
    child: Child,
    output: ShellOutputSink,
    stdout_reader: Option<JoinHandle<io::Result<()>>>,
    stderr_reader: Option<JoinHandle<io::Result<()>>>,
}

enum ShellWait {
    Exited(ExitStatus),
    DeadlineReached,
}

impl RunningShell {
    fn spawn(mut command: tokio::process::Command) -> anyhow::Result<Self> {
        command
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().context("failed to spawn shell command")?;
        let stdout = child
            .stdout
            .take()
            .context("shell command stdout was not piped")?;
        let stderr = child
            .stderr
            .take()
            .context("shell command stderr was not piped")?;
        let output = ShellOutputSink::new();
        let stdout_reader = tokio::spawn(read_shell_stream(
            stdout,
            ShellStream::Stdout,
            output.clone(),
        ));
        let stderr_reader = tokio::spawn(read_shell_stream(
            stderr,
            ShellStream::Stderr,
            output.clone(),
        ));
        Ok(Self {
            child,
            output,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
        })
    }

    async fn wait_until(&mut self, deadline: Instant) -> anyhow::Result<ShellWait> {
        tokio::select! {
            status = self.child.wait() => Ok(ShellWait::Exited(
                status.context("failed to wait for shell command")?
            )),
            _ = tokio::time::sleep_until(deadline) => {
                match self.child.try_wait().context("failed to inspect shell command status")? {
                    Some(status) => Ok(ShellWait::Exited(status)),
                    None => Ok(ShellWait::DeadlineReached),
                }
            }
        }
    }

    async fn activate(&self, handle: AsyncOperationHandle) {
        self.output.activate(handle).await;
    }

    async fn finish(mut self) -> anyhow::Result<RenderedShellOutput> {
        await_output_reader(self.stdout_reader.take(), "stdout").await?;
        await_output_reader(self.stderr_reader.take(), "stderr").await?;
        Ok(self.output.captured().await)
    }

    async fn terminate(mut self) -> anyhow::Result<RenderedShellOutput> {
        let _ = self.child.kill().await;
        self.finish().await
    }
}

impl Drop for RunningShell {
    fn drop(&mut self) {
        if let Some(reader) = &self.stdout_reader {
            reader.abort();
        }
        if let Some(reader) = &self.stderr_reader {
            reader.abort();
        }
    }
}

async fn read_shell_stream<R>(
    mut reader: R,
    stream: ShellStream,
    output: ShellOutputSink,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0; OUTPUT_READ_CHUNK_BYTES];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        output.push(stream, &buffer[..read]).await;
    }
}

async fn await_output_reader(
    reader: Option<JoinHandle<io::Result<()>>>,
    stream: &str,
) -> anyhow::Result<()> {
    let Some(reader) = reader else {
        return Ok(());
    };
    reader
        .await
        .with_context(|| format!("shell {stream} reader task failed"))?
        .with_context(|| format!("failed to read shell {stream}"))
}

#[derive(Debug)]
struct RenderedShellOutput {
    stdout: String,
    stderr: String,
}

#[derive(Debug, Serialize)]
struct ShellOperationStarted<'a> {
    #[serde(rename = "type")]
    result_type: &'static str,
    #[serde(flatten)]
    async_operation: AsyncOperationStartReceipt,
    command: &'a str,
    working_directory: &'a std::path::Path,
}

/// Shell command execution tool with sandboxing
pub struct ShellTool<R>
where
    R: RuntimeAdapter,
{
    security: Arc<SecurityPolicy>,
    runtime: Arc<R>,
    skill_runtime: Arc<SkillRuntimeState>,
    description: String,
    initial_wait: Duration,
}

impl<R> ShellTool<R>
where
    R: RuntimeAdapter,
{
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<R>) -> Self {
        Self::with_skill_runtime(security, runtime, Arc::new(SkillRuntimeState::default()))
    }

    pub fn with_skill_runtime(
        security: Arc<SecurityPolicy>,
        runtime: Arc<R>,
        skill_runtime: Arc<SkillRuntimeState>,
    ) -> Self {
        let description = build_shell_description(&security.workspace_dir);
        Self {
            security,
            runtime,
            skill_runtime,
            description,
            initial_wait: SHELL_INITIAL_WAIT,
        }
    }

    #[cfg(test)]
    fn with_initial_wait(mut self, initial_wait: Duration) -> Self {
        self.initial_wait = initial_wait;
        self
    }
}

/// Build the shell tool description with OS-specific guidance detected at startup.
fn build_shell_description(working_directory: &std::path::Path) -> String {
    // Detect OS name and version via `uname -sr` (works on macOS and Linux).
    // Fall back to the Rust compile-time target OS if the command fails.
    let os_label = std::process::Command::new("uname")
        .arg("-sr")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| std::env::consts::OS.to_string());

    // macOS `sed -i` requires an empty backup-extension argument that GNU sed does not.
    let sed_note = if std::env::consts::OS == "macos" {
        "sed in-place edits require an empty-string extension on macOS: \
        `sed -i '' 's/old/new/' file` (GNU Linux syntax `sed -i` will fail). "
    } else {
        "sed in-place edits use the standard syntax: `sed -i 's/old/new/' file`. "
    };

    format!(
        "Execute a shell command in the scoped working directory. \
        The process already starts in: {}. Use relative paths; do not prefix commands with \
        `cd` to this directory. stdout and stderr are captured separately and returned, with \
        each stream truncated at 1MB. Do not add `2>&1` or pipe through `head` solely to \
        collect or limit output. \
        Commands that remain running become asynchronous shell operations; use the controls \
        returned by the tool to inspect, wait for, or stop them. \
        Environment: {os_label}. \
        {sed_note}\
        Output redirections (> and >>) are blocked by policy — use write to create or \
        replace files instead. Prefer read + write over sed/awk for file edits.",
        working_directory.display()
    )
}

#[async_trait]
impl<R> Tool for ShellTool<R>
where
    R: RuntimeAdapter + 'static,
{
    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
            },
            "required": ["command"]
        })
    }

    #[allow(clippy::incompatible_msrv)]
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;
        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        match self.security.validate_command_execution(command) {
            Ok(_) => {}
            Err(denial) => {
                return Ok(ToolResult {
                    success: false,
                    output: serde_json::to_string_pretty(&denial)?,
                    error: Some(denial.message),
                });
            }
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Execute with timeout to prevent hanging commands.
        // Clear the environment to prevent leaking API keys and other secrets
        // (CWE-200), then re-add only safe, functional variables.
        let mut cmd = match self
            .runtime
            .build_shell_command(command, &self.security.workspace_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };
        cmd.env_clear();

        for var in SAFE_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }

        // Forward tool-specific credentials (e.g. GITHUB_TOKEN for `gh` CLI).
        for (key, val) in &self.security.forwarded_env {
            cmd.env(key, val);
        }

        for (key, val) in self.skill_runtime.shell_env() {
            cmd.env(key, val);
        }

        let mut process = match RunningShell::spawn(cmd) {
            Ok(process) => process,
            Err(error) => return Ok(shell_execution_error(error)),
        };
        let started_at = Instant::now();
        let timeout_deadline = started_at + Duration::from_secs(SHELL_TIMEOUT_SECS);
        let async_runtime = self
            .runtime
            .supports_long_running()
            .then(current_async_operation_runtime)
            .flatten();
        let first_deadline = async_runtime
            .as_ref()
            .map_or(timeout_deadline, |_| started_at + self.initial_wait);

        match process.wait_until(first_deadline).await {
            Ok(ShellWait::Exited(status)) => finish_shell_tool_result(process, status).await,
            Err(error) => {
                let _ = process.terminate().await;
                Ok(shell_execution_error(error))
            }
            Ok(ShellWait::DeadlineReached) => {
                let Some(async_runtime) = async_runtime else {
                    let _ = process.terminate().await;
                    return Ok(shell_timeout_result());
                };
                let operation_id = format!(
                    "shell_{}",
                    SHELL_OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed)
                );
                let controls = AsyncControls::new(AsyncControl::Inspect)
                    .with(AsyncControl::Stop)
                    .with(AsyncControl::Wait);
                let handle = async_runtime
                    .start(StartAsyncOperation {
                        id: operation_id.clone(),
                        kind: nenjo::tools::AsyncOperationKind::Shell,
                        label: shell_operation_label(command),
                        parent_operation_id: None,
                        parent_tool_name: Some("shell".into()),
                        started_summary: "Shell command is still running".into(),
                        model_visible: true,
                        controls,
                    })
                    .await;
                process.activate(handle.clone()).await;
                let background_command = command.to_owned();
                let background_handle = handle.clone();
                let join = tokio::spawn(async move {
                    finish_async_shell(
                        process,
                        background_handle,
                        background_command,
                        timeout_deadline,
                    )
                    .await;
                });
                handle.attach_join(join).await;

                Ok(ToolResult {
                    success: true,
                    output: serde_json::to_string_pretty(&ShellOperationStarted {
                        result_type: "operation_started",
                        async_operation: AsyncOperationStartReceipt::new(
                            operation_id,
                            nenjo::tools::AsyncOperationKind::Shell,
                            controls,
                        ),
                        command,
                        working_directory: &self.security.workspace_dir,
                    })?,
                    error: None,
                })
            }
        }
    }
}

async fn finish_shell_tool_result(
    process: RunningShell,
    status: ExitStatus,
) -> anyhow::Result<ToolResult> {
    match process.finish().await {
        Ok(output) => Ok(ToolResult {
            success: status.success(),
            output: output.stdout,
            error: (!output.stderr.is_empty()).then_some(output.stderr),
        }),
        Err(error) => Ok(shell_execution_error(error)),
    }
}

async fn finish_async_shell(
    mut process: RunningShell,
    handle: AsyncOperationHandle,
    command: String,
    timeout_deadline: Instant,
) {
    match process.wait_until(timeout_deadline).await {
        Ok(ShellWait::Exited(status)) => match process.finish().await {
            Ok(output) => {
                let result = shell_execution_output(&command, Some(&status), &output);
                if status.success() {
                    handle
                        .complete("Shell command completed successfully", Some(result))
                        .await;
                } else {
                    handle
                        .fail_with_output(
                            format!("Shell command exited with {status}"),
                            Some(result),
                        )
                        .await;
                }
            }
            Err(error) => handle.fail(error.to_string()).await,
        },
        Ok(ShellWait::DeadlineReached) => {
            let output = process.terminate().await.ok();
            handle
                .fail_with_output(
                    format!("Shell command timed out after {SHELL_TIMEOUT_SECS}s and was killed"),
                    output
                        .as_ref()
                        .map(|output| shell_execution_output(&command, None, output)),
                )
                .await;
        }
        Err(error) => {
            let output = process.terminate().await.ok();
            handle
                .fail_with_output(
                    format!("Failed to execute shell command: {error}"),
                    output
                        .as_ref()
                        .map(|output| shell_execution_output(&command, None, output)),
                )
                .await;
        }
    }
}

fn shell_execution_output(
    command: &str,
    status: Option<&ExitStatus>,
    output: &RenderedShellOutput,
) -> serde_json::Value {
    json!({
        "command": command,
        "success": status.is_some_and(ExitStatus::success),
        "exit_code": status.and_then(ExitStatus::code),
        "stdout": output.stdout,
        "stderr": output.stderr,
    })
}

fn shell_operation_label(command: &str) -> String {
    let mut characters = command.chars();
    let mut label: String = characters.by_ref().take(80).collect();
    if characters.next().is_some() {
        label.push('…');
    }
    label
}

fn shell_execution_error(error: impl std::fmt::Display) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!("Failed to execute command: {error}")),
    }
}

fn shell_timeout_result() -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!(
            "Command timed out after {SHELL_TIMEOUT_SECS}s and was killed"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::runtime::RuntimeAdapter;
    use crate::tools::security::{AutonomyLevel, SecurityPolicy};

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    /// Simple native runtime for tests (mirrors worker NativeRuntime).
    struct TestNativeRuntime;

    impl RuntimeAdapter for TestNativeRuntime {
        fn name(&self) -> &str {
            "test-native"
        }
        fn has_shell_access(&self) -> bool {
            true
        }
        fn has_filesystem_access(&self) -> bool {
            true
        }
        fn storage_path(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }
        fn supports_long_running(&self) -> bool {
            true
        }
        fn build_shell_command(
            &self,
            command: &str,
            workspace_dir: &std::path::Path,
        ) -> anyhow::Result<tokio::process::Command> {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg(command).current_dir(workspace_dir);
            Ok(cmd)
        }
    }

    fn test_runtime() -> Arc<TestNativeRuntime> {
        Arc::new(TestNativeRuntime)
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime())
            .with_initial_wait(Duration::from_millis(5));
        assert_eq!(tool.name(), "shell");
        assert_eq!(tool.initial_wait, Duration::from_millis(5));
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert!(!tool.description().is_empty());
        assert!(tool.description().contains("asynchronous shell operations"));
        assert!(tool.description().contains("already starts in"));
        assert!(
            tool.description()
                .contains("stdout and stderr are captured")
        );
        assert!(
            tool.description()
                .contains("do not prefix commands with `cd`")
        );
        assert!(
            tool.description()
                .contains(&std::env::temp_dir().display().to_string())
        );
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("command"))
        );
        assert!(schema["properties"].get("approved").is_none());
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn running_shell_reports_when_process_survives_initial_deadline() {
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg("sleep 0.2");
        let mut process = RunningShell::spawn(command).unwrap();

        let state = process
            .wait_until(Instant::now() + Duration::from_millis(10))
            .await
            .unwrap();

        assert!(matches!(state, ShellWait::DeadlineReached));
        process.terminate().await.unwrap();
    }

    #[tokio::test]
    async fn running_shell_drains_stdout_and_stderr_concurrently() {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg("printf 'from stdout'; printf 'from stderr' >&2");
        let mut process = RunningShell::spawn(command).unwrap();

        let state = process
            .wait_until(Instant::now() + Duration::from_secs(1))
            .await
            .unwrap();
        let ShellWait::Exited(status) = state else {
            panic!("shell process should have completed");
        };
        let output = process.finish().await.unwrap();

        assert!(status.success());
        assert_eq!(output.stdout, "from stdout");
        assert_eq!(output.stderr, "from stderr");
    }

    #[tokio::test]
    async fn shell_executes_relative_to_workspace_dir() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("worktree");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("marker.txt"), "scoped workspace")
            .await
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            blocked_commands: vec![],
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());

        let pwd = tool.execute(json!({"command": "pwd"})).await.unwrap();
        assert!(pwd.success);
        assert_eq!(
            std::fs::canonicalize(pwd.output.trim()).unwrap(),
            std::fs::canonicalize(&workspace).unwrap()
        );

        let relative_read = tool
            .execute(json!({"command": "cat marker.txt"}))
            .await
            .unwrap();
        assert!(relative_read.success);
        assert_eq!(relative_read.output.trim(), "scoped workspace");
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": "rm -rf /"})).await.unwrap();
        assert!(!result.success);
        let denial: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(denial["type"], "scope_violation");
        assert_eq!(denial["rule"], "blocked_executable");
        assert_eq!(denial["suggestion"]["tool"], "remove");
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly), test_runtime());
        let result = tool.execute(json!({"command": "ls"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("disabled"));
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[test]
    fn captured_shell_output_is_bounded_with_a_truncation_marker() {
        let mut stream = CapturedStream::default();
        let oversized = vec![b'x'; MAX_OUTPUT_BYTES + 1];

        let (accepted, newly_truncated) = stream.push(&oversized);

        assert_eq!(accepted.len(), MAX_OUTPUT_BYTES);
        assert!(newly_truncated);
        assert_eq!(stream.bytes.len(), MAX_OUTPUT_BYTES);
        assert!(
            stream
                .render("\n... [output truncated at 1MB]")
                .ends_with("\n... [output truncated at 1MB]")
        );
    }

    fn test_security_with_env_cmd() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            blocked_commands: vec![],
            ..SecurityPolicy::default()
        })
    }

    /// RAII guard that restores an environment variable to its original state on drop,
    /// ensuring cleanup even if the test panics.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(val) => unsafe { std::env::set_var(self.key, val) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_does_not_leak_api_key() {
        let _g1 = EnvGuard::set("API_KEY", "sk-test-secret-12345");
        let _g2 = EnvGuard::set("ZEROCLAW_API_KEY", "sk-test-secret-67890");

        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool.execute(json!({"command": "env"})).await.unwrap();
        assert!(result.success);
        assert!(
            !result.output.contains("sk-test-secret-12345"),
            "API_KEY leaked to shell command output"
        );
        assert!(
            !result.output.contains("sk-test-secret-67890"),
            "ZEROCLAW_API_KEY leaked to shell command output"
        );
    }

    #[tokio::test]
    async fn shell_preserves_path_and_home() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": "echo $HOME"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            !result.output.trim().is_empty(),
            "HOME should be available in shell"
        );

        let result = tool
            .execute(json!({"command": "echo $PATH"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(
            !result.output.trim().is_empty(),
            "PATH should be available in shell"
        );
    }

    #[tokio::test]
    async fn shell_forwards_env_from_security_policy() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            blocked_commands: vec![],
            workspace_dir: std::env::temp_dir(),
            forwarded_env: vec![
                ("GH_TOKEN".into(), "test-gh-token-xyz".into()),
                ("GITHUB_TOKEN".into(), "test-github-token-xyz".into()),
                ("XAI_API_KEY".into(), "test-xai-key-xyz".into()),
            ],
            ..SecurityPolicy::default()
        });

        let tool = ShellTool::new(security, test_runtime());
        let result = tool.execute(json!({"command": "env"})).await.unwrap();
        assert!(result.success);
        assert!(
            result.output.contains("GH_TOKEN=test-gh-token-xyz"),
            "GH_TOKEN should be forwarded to shell subprocess"
        );
        assert!(
            result.output.contains("GITHUB_TOKEN=test-github-token-xyz"),
            "GITHUB_TOKEN should be forwarded to shell subprocess"
        );
        assert!(
            result.output.contains("XAI_API_KEY=test-xai-key-xyz"),
            "XAI_API_KEY should be forwarded to shell subprocess"
        );
    }

    #[tokio::test]
    async fn shell_allows_workspace_mutations_without_approval() {
        let temp = tempfile::tempdir().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            blocked_commands: vec![],
            workspace_dir: temp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });

        let tool = ShellTool::new(security.clone(), test_runtime());
        let result = tool
            .execute(json!({"command": "touch nenjo_shell_approval_test"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(temp.path().join("nenjo_shell_approval_test").exists());
    }

    #[tokio::test]
    async fn shell_allows_git_push_without_command_approval() {
        async fn git(directory: &std::path::Path, args: &[&str]) {
            let status = tokio::process::Command::new("git")
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(directory)
                .output()
                .await
                .unwrap();
            assert!(
                status.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&status.stderr)
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let remote = temp.path().join("remote.git");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]).await;
        git(&workspace, &["init"]).await;
        git(&workspace, &["config", "user.name", "Nenjo Test"]).await;
        git(&workspace, &["config", "user.email", "nenjo@example.test"]).await;
        tokio::fs::write(workspace.join("tracked.txt"), "initial\n")
            .await
            .unwrap();
        git(&workspace, &["add", "tracked.txt"]).await;
        git(&workspace, &["commit", "-m", "initial"]).await;
        git(
            &workspace,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        )
        .await;
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());

        let result = tool
            .execute(json!({"command": "git push origin HEAD:main"}))
            .await
            .unwrap();

        assert!(result.success, "{:?}", result.error);
        let pushed = tokio::process::Command::new("git")
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(&remote)
            .output()
            .await
            .unwrap();
        assert!(pushed.status.success());
    }
}
