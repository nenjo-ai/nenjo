//! Browser automation tool with pluggable backends.
//!
//! By default this uses Vercel's `agent-browser` CLI for automation.
//! Computer-use (OS-level) actions are supported via an optional sidecar endpoint.

use crate::security::SecurityPolicy;
use crate::{Tool, ToolCategory, ToolResult};
use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::net::ToSocketAddrs;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tracing::debug;

/// Computer-use sidecar settings.
#[derive(Debug, Clone)]
pub struct ComputerUseConfig {
    pub endpoint: String,
    pub api_key: Option<String>,
    pub timeout_ms: u64,
    pub allow_remote_endpoint: bool,
    pub window_allowlist: Vec<String>,
    pub max_coordinate_x: Option<i64>,
    pub max_coordinate_y: Option<i64>,
}

impl Default for ComputerUseConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8787/v1/actions".into(),
            api_key: None,
            timeout_ms: 15_000,
            allow_remote_endpoint: false,
            window_allowlist: Vec::new(),
            max_coordinate_x: None,
            max_coordinate_y: None,
        }
    }
}

/// Browser automation tool settings.
#[derive(Debug, Clone)]
pub struct BrowserToolConfig {
    pub allowed_domains: Vec<String>,
    pub session_name: Option<String>,
    pub backend: String,
    pub computer_use: ComputerUseConfig,
}

impl Default for BrowserToolConfig {
    fn default() -> Self {
        Self {
            allowed_domains: Vec::new(),
            session_name: None,
            backend: "agent_browser".into(),
            computer_use: ComputerUseConfig::default(),
        }
    }
}

/// Browser automation tool using pluggable backends.
pub struct BrowserTool {
    security: Arc<SecurityPolicy>,
    allowed_domains: Vec<String>,
    session_name: Option<String>,
    backend: String,
    computer_use: ComputerUseConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserBackendKind {
    AgentBrowser,
    ComputerUse,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedBackend {
    AgentBrowser,
    ComputerUse,
}

impl BrowserBackendKind {
    fn parse(raw: &str) -> anyhow::Result<Self> {
        let key = raw.trim().to_ascii_lowercase().replace('-', "_");
        match key.as_str() {
            "agent_browser" | "agentbrowser" => Ok(Self::AgentBrowser),
            "computer_use" | "computeruse" => Ok(Self::ComputerUse),
            "auto" => Ok(Self::Auto),
            _ => anyhow::bail!(
                "Unsupported browser backend '{raw}'. Use 'agent_browser', 'computer_use', or 'auto'"
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::AgentBrowser => "agent_browser",
            Self::ComputerUse => "computer_use",
            Self::Auto => "auto",
        }
    }
}

/// Response from agent-browser --json commands
#[derive(Debug, Deserialize)]
struct AgentBrowserResponse {
    success: bool,
    data: Option<Value>,
    error: Option<String>,
}

/// Response format from computer-use sidecar.
#[derive(Debug, Deserialize)]
struct ComputerUseResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// Supported browser actions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserAction {
    /// Navigate to a URL
    Open { url: String },
    /// Get accessibility snapshot with refs
    Snapshot {
        #[serde(default)]
        interactive_only: bool,
        #[serde(default)]
        compact: bool,
        #[serde(default)]
        depth: Option<u32>,
    },
    /// Click an element by ref or selector
    Click { selector: String },
    /// Fill a form field
    Fill { selector: String, value: String },
    /// Type text into focused element
    Type { selector: String, text: String },
    /// Get text content of element
    GetText { selector: String },
    /// Get page title
    GetTitle,
    /// Get current URL
    GetUrl,
    /// Take screenshot
    Screenshot {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        full_page: bool,
    },
    /// Wait for element or time
    Wait {
        #[serde(default)]
        selector: Option<String>,
        #[serde(default)]
        ms: Option<u64>,
        #[serde(default)]
        text: Option<String>,
    },
    /// Press a key
    Press { key: String },
    /// Hover over element
    Hover { selector: String },
    /// Scroll page
    Scroll {
        direction: String,
        #[serde(default)]
        pixels: Option<u32>,
    },
    /// Check if element is visible
    IsVisible { selector: String },
    /// Close browser
    Close,
    /// Find element by semantic locator
    Find {
        by: String, // role, text, label, placeholder, testid
        value: String,
        action: String, // click, fill, text, hover
        #[serde(default)]
        fill_value: Option<String>,
    },
}

impl BrowserTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        allowed_domains: Vec<String>,
        session_name: Option<String>,
    ) -> Self {
        Self::new_with_config(
            security,
            BrowserToolConfig {
                allowed_domains,
                session_name,
                ..BrowserToolConfig::default()
            },
        )
    }

    pub fn new_with_config(security: Arc<SecurityPolicy>, config: BrowserToolConfig) -> Self {
        Self {
            security,
            allowed_domains: normalize_domains(config.allowed_domains),
            session_name: config.session_name,
            backend: config.backend,
            computer_use: config.computer_use,
        }
    }

    /// Check if agent-browser CLI is available
    pub async fn is_agent_browser_available() -> bool {
        Command::new("agent-browser")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Backward-compatible alias.
    pub async fn is_available() -> bool {
        Self::is_agent_browser_available().await
    }

    fn configured_backend(&self) -> anyhow::Result<BrowserBackendKind> {
        BrowserBackendKind::parse(&self.backend)
    }

    fn computer_use_endpoint_url(&self) -> anyhow::Result<reqwest::Url> {
        if self.computer_use.timeout_ms == 0 {
            anyhow::bail!("browser.computer_use.timeout_ms must be > 0");
        }

        let endpoint = self.computer_use.endpoint.trim();
        if endpoint.is_empty() {
            anyhow::bail!("browser.computer_use.endpoint cannot be empty");
        }

        let parsed = reqwest::Url::parse(endpoint).map_err(|_| {
            anyhow::anyhow!(
                "Invalid browser.computer_use.endpoint: '{endpoint}'. Expected http(s) URL"
            )
        })?;

        let scheme = parsed.scheme();
        if scheme != "http" && scheme != "https" {
            anyhow::bail!("browser.computer_use.endpoint must use http:// or https://");
        }

        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("browser.computer_use.endpoint must include host"))?;

        let host_is_private = is_private_host(host);
        if !self.computer_use.allow_remote_endpoint && !host_is_private {
            anyhow::bail!(
                "browser.computer_use.endpoint host '{host}' is public. Set browser.computer_use.allow_remote_endpoint=true to allow it"
            );
        }

        if self.computer_use.allow_remote_endpoint && !host_is_private && scheme != "https" {
            anyhow::bail!(
                "browser.computer_use.endpoint must use https:// when allow_remote_endpoint=true and host is public"
            );
        }

        Ok(parsed)
    }

    fn computer_use_available(&self) -> anyhow::Result<bool> {
        let endpoint = self.computer_use_endpoint_url()?;
        Ok(endpoint_reachable(&endpoint, Duration::from_millis(500)))
    }

    async fn resolve_backend(&self) -> anyhow::Result<ResolvedBackend> {
        let configured = self.configured_backend()?;

        match configured {
            BrowserBackendKind::AgentBrowser => {
                if Self::is_agent_browser_available().await {
                    Ok(ResolvedBackend::AgentBrowser)
                } else {
                    anyhow::bail!(
                        "browser.backend='{}' but agent-browser CLI is unavailable. Install with: npm install -g agent-browser",
                        configured.as_str()
                    )
                }
            }
            BrowserBackendKind::ComputerUse => {
                if !self.computer_use_available()? {
                    anyhow::bail!(
                        "browser.backend='computer_use' but sidecar endpoint is unreachable. Check browser.computer_use.endpoint and sidecar status"
                    );
                }
                Ok(ResolvedBackend::ComputerUse)
            }
            BrowserBackendKind::Auto => {
                if Self::is_agent_browser_available().await {
                    return Ok(ResolvedBackend::AgentBrowser);
                }

                let computer_use_err = match self.computer_use_available() {
                    Ok(true) => return Ok(ResolvedBackend::ComputerUse),
                    Ok(false) => None,
                    Err(err) => Some(err.to_string()),
                };

                if let Some(err) = computer_use_err {
                    anyhow::bail!(
                        "browser.backend='auto' needs agent-browser CLI or valid computer-use sidecar (error: {err})"
                    );
                }

                anyhow::bail!(
                    "browser.backend='auto' needs agent-browser CLI or computer-use sidecar"
                )
            }
        }
    }

    /// Validate URL against allowlist
    fn validate_url(&self, url: &str) -> anyhow::Result<()> {
        let url = url.trim();

        if url.is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        // Block file:// URLs — browser file access bypasses all SSRF and
        // domain-allowlist controls and can exfiltrate arbitrary local files.
        if url.starts_with("file://") {
            anyhow::bail!("file:// URLs are not allowed in browser automation");
        }

        if !url.starts_with("https://") && !url.starts_with("http://") {
            anyhow::bail!("Only http:// and https:// URLs are allowed");
        }

        if self.allowed_domains.is_empty() {
            anyhow::bail!(
                "Browser tool enabled but no allowed_domains configured. \
                Add [browser].allowed_domains in config.toml"
            );
        }

        let host = extract_host(url)?;

        if is_private_host(&host) {
            anyhow::bail!("Blocked local/private host: {host}");
        }

        if !host_matches_allowlist(&host, &self.allowed_domains) {
            anyhow::bail!("Host '{host}' not in browser.allowed_domains");
        }

        Ok(())
    }

    /// Execute an agent-browser command
    async fn run_command(&self, args: &[&str]) -> anyhow::Result<AgentBrowserResponse> {
        let mut cmd = Command::new("agent-browser");

        // Add session if configured
        if let Some(ref session) = self.session_name {
            cmd.arg("--session").arg(session);
        }

        // Add --json for machine-readable output
        cmd.args(args).arg("--json");

        debug!("Running: agent-browser {} --json", args.join(" "));

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stderr.is_empty() {
            debug!("agent-browser stderr: {}", stderr);
        }

        // Parse JSON response
        if let Ok(resp) = serde_json::from_str::<AgentBrowserResponse>(&stdout) {
            return Ok(resp);
        }

        // Fallback for non-JSON output
        if output.status.success() {
            Ok(AgentBrowserResponse {
                success: true,
                data: Some(json!({ "output": stdout.trim() })),
                error: None,
            })
        } else {
            Ok(AgentBrowserResponse {
                success: false,
                data: None,
                error: Some(stderr.trim().to_string()),
            })
        }
    }

    fn agent_browser_args(&self, action: BrowserAction) -> anyhow::Result<Vec<String>> {
        match action {
            BrowserAction::Open { url } => {
                self.validate_url(&url)?;
                Ok(vec!["open".into(), url])
            }

            BrowserAction::Snapshot {
                interactive_only,
                compact,
                depth,
            } => {
                let mut args = vec!["snapshot".into()];
                if interactive_only {
                    args.push("-i".into());
                }
                if compact {
                    args.push("-c".into());
                }
                if let Some(d) = depth {
                    args.push("-d".into());
                    args.push(d.to_string());
                }
                Ok(args)
            }

            BrowserAction::Click { selector } => Ok(vec!["click".into(), selector]),

            BrowserAction::Fill { selector, value } => Ok(vec!["fill".into(), selector, value]),

            BrowserAction::Type { selector, text } => Ok(vec!["type".into(), selector, text]),

            BrowserAction::GetText { selector } => Ok(vec!["get".into(), "text".into(), selector]),

            BrowserAction::GetTitle => Ok(vec!["get".into(), "title".into()]),

            BrowserAction::GetUrl => Ok(vec!["get".into(), "url".into()]),

            BrowserAction::Screenshot { path, full_page } => {
                let mut args = vec!["screenshot".into()];
                if let Some(p) = path {
                    args.push(p);
                }
                if full_page {
                    args.push("--full".into());
                }
                Ok(args)
            }

            BrowserAction::Wait { selector, ms, text } => {
                let mut args = vec!["wait".into()];
                if let Some(sel) = selector {
                    args.push(sel);
                } else if let Some(millis) = ms {
                    args.push(millis.to_string());
                } else if let Some(t) = text {
                    args.push("--text".into());
                    args.push(t);
                }
                Ok(args)
            }

            BrowserAction::Press { key } => Ok(vec!["press".into(), key]),

            BrowserAction::Hover { selector } => Ok(vec!["hover".into(), selector]),

            BrowserAction::Scroll { direction, pixels } => {
                let mut args = vec!["scroll".into(), direction];
                if let Some(px) = pixels {
                    args.push(px.to_string());
                }
                Ok(args)
            }

            BrowserAction::IsVisible { selector } => {
                Ok(vec!["is".into(), "visible".into(), selector])
            }

            BrowserAction::Close => Ok(vec!["close".into()]),

            BrowserAction::Find {
                by,
                value,
                action,
                fill_value,
            } => {
                let mut args = vec!["find".into(), by, value, action];
                if let Some(fv) = fill_value {
                    args.push(fv);
                }
                Ok(args)
            }
        }
    }

    /// Execute a browser action via agent-browser CLI.
    async fn execute_agent_browser_action(
        &self,
        action: BrowserAction,
    ) -> anyhow::Result<ToolResult> {
        let args = self.agent_browser_args(action)?;
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let resp = self.run_command(&arg_refs).await?;
        Ok(Self::to_result(resp))
    }

    fn validate_coordinate(&self, key: &str, value: i64, max: Option<i64>) -> anyhow::Result<()> {
        if value < 0 {
            anyhow::bail!("'{key}' must be >= 0")
        }
        if let Some(limit) = max {
            if limit < 0 {
                anyhow::bail!("Configured coordinate limit for '{key}' must be >= 0")
            }
            if value > limit {
                anyhow::bail!("'{key}'={value} exceeds configured limit {limit}")
            }
        }
        Ok(())
    }

    fn read_required_i64(
        &self,
        params: &serde_json::Map<String, Value>,
        key: &str,
    ) -> anyhow::Result<i64> {
        params
            .get(key)
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("Missing or invalid '{key}' parameter"))
    }

    fn validate_computer_use_action(
        &self,
        action: &str,
        params: &serde_json::Map<String, Value>,
    ) -> anyhow::Result<()> {
        match action {
            "open" => {
                let url = params
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Missing 'url' for open action"))?;
                self.validate_url(url)?;
            }
            "mouse_move" | "mouse_click" => {
                let x = self.read_required_i64(params, "x")?;
                let y = self.read_required_i64(params, "y")?;
                self.validate_coordinate("x", x, self.computer_use.max_coordinate_x)?;
                self.validate_coordinate("y", y, self.computer_use.max_coordinate_y)?;
            }
            "mouse_drag" => {
                let from_x = self.read_required_i64(params, "from_x")?;
                let from_y = self.read_required_i64(params, "from_y")?;
                let to_x = self.read_required_i64(params, "to_x")?;
                let to_y = self.read_required_i64(params, "to_y")?;
                self.validate_coordinate("from_x", from_x, self.computer_use.max_coordinate_x)?;
                self.validate_coordinate("to_x", to_x, self.computer_use.max_coordinate_x)?;
                self.validate_coordinate("from_y", from_y, self.computer_use.max_coordinate_y)?;
                self.validate_coordinate("to_y", to_y, self.computer_use.max_coordinate_y)?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn execute_computer_use_action(
        &self,
        action: &str,
        args: &Value,
    ) -> anyhow::Result<ToolResult> {
        let endpoint = self.computer_use_endpoint_url()?;

        let mut params = args
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("browser args must be a JSON object"))?;
        params.remove("action");

        self.validate_computer_use_action(action, &params)?;

        let payload = json!({
            "action": action,
            "params": params,
            "policy": {
                "allowed_domains": self.allowed_domains,
                "window_allowlist": self.computer_use.window_allowlist,
                "max_coordinate_x": self.computer_use.max_coordinate_x,
                "max_coordinate_y": self.computer_use.max_coordinate_y,
            },
            "metadata": {
                "session_name": self.session_name,
                "source": "nenjo.browser",
                "version": env!("CARGO_PKG_VERSION"),
            }
        });

        let client = reqwest::Client::new();
        let mut request = client
            .post(endpoint)
            .timeout(Duration::from_millis(self.computer_use.timeout_ms))
            .json(&payload);

        if let Some(api_key) = self.computer_use.api_key.as_deref() {
            let token = api_key.trim();
            if !token.is_empty() {
                request = request.bearer_auth(token);
            }
        }

        let response = request.send().await.with_context(|| {
            format!(
                "Failed to call computer-use sidecar at {}",
                self.computer_use.endpoint
            )
        })?;

        let status = response.status();
        let body = response
            .text()
            .await
            .context("Failed to read computer-use sidecar response body")?;

        if let Ok(parsed) = serde_json::from_str::<ComputerUseResponse>(&body) {
            if status.is_success() && parsed.success.unwrap_or(true) {
                let output = parsed
                    .data
                    .map(|data| serde_json::to_string_pretty(&data).unwrap_or_default())
                    .unwrap_or_else(|| {
                        serde_json::to_string_pretty(&json!({
                            "backend": "computer_use",
                            "action": action,
                            "ok": true,
                        }))
                        .unwrap_or_default()
                    });

                return Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                });
            }

            let error = parsed.error.or_else(|| {
                if status.is_success() && parsed.success == Some(false) {
                    Some("computer-use sidecar returned success=false".to_string())
                } else {
                    Some(format!(
                        "computer-use sidecar request failed with status {status}"
                    ))
                }
            });

            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error,
            });
        }

        if status.is_success() {
            return Ok(ToolResult {
                success: true,
                output: body,
                error: None,
            });
        }

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!(
                "computer-use sidecar request failed with status {status}: {}",
                body.trim()
            )),
        })
    }

    async fn execute_action(
        &self,
        action: BrowserAction,
        backend: ResolvedBackend,
    ) -> anyhow::Result<ToolResult> {
        match backend {
            ResolvedBackend::AgentBrowser => self.execute_agent_browser_action(action).await,
            ResolvedBackend::ComputerUse => anyhow::bail!(
                "Internal error: computer_use backend must be handled before BrowserAction parsing"
            ),
        }
    }

    fn to_result(resp: AgentBrowserResponse) -> ToolResult {
        if resp.success {
            let output = resp
                .data
                .map(|d| serde_json::to_string_pretty(&d).unwrap_or_default())
                .unwrap_or_default();
            ToolResult {
                success: true,
                output,
                error: None,
            }
        } else {
            ToolResult {
                success: false,
                output: String::new(),
                error: resp.error,
            }
        }
    }
}

fn browser_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["open", "snapshot", "click", "fill", "type", "get_text",
                         "get_title", "get_url", "screenshot", "wait", "press",
                         "hover", "scroll", "is_visible", "close", "find",
                         "mouse_move", "mouse_click", "mouse_drag", "key_type",
                         "key_press", "screen_capture"],
                "description": "Browser action to perform (OS-level actions require backend=computer_use)"
            },
            "url": {
                "type": "string",
                "description": "URL to navigate to (for 'open' action)"
            },
            "selector": {
                "type": "string",
                "description": "Element selector: @ref (e.g. @e1), CSS (#id, .class), or text=..."
            },
            "value": {
                "type": "string",
                "description": "Value to fill or type"
            },
            "text": {
                "type": "string",
                "description": "Text to type or wait for"
            },
            "key": {
                "type": "string",
                "description": "Key to press (Enter, Tab, Escape, etc.)"
            },
            "x": {
                "type": "integer",
                "description": "Screen X coordinate (computer_use: mouse_move/mouse_click)"
            },
            "y": {
                "type": "integer",
                "description": "Screen Y coordinate (computer_use: mouse_move/mouse_click)"
            },
            "from_x": {
                "type": "integer",
                "description": "Drag source X coordinate (computer_use: mouse_drag)"
            },
            "from_y": {
                "type": "integer",
                "description": "Drag source Y coordinate (computer_use: mouse_drag)"
            },
            "to_x": {
                "type": "integer",
                "description": "Drag target X coordinate (computer_use: mouse_drag)"
            },
            "to_y": {
                "type": "integer",
                "description": "Drag target Y coordinate (computer_use: mouse_drag)"
            },
            "button": {
                "type": "string",
                "enum": ["left", "right", "middle"],
                "description": "Mouse button for computer_use mouse_click"
            },
            "direction": {
                "type": "string",
                "enum": ["up", "down", "left", "right"],
                "description": "Scroll direction"
            },
            "pixels": {
                "type": "integer",
                "description": "Pixels to scroll"
            },
            "interactive_only": {
                "type": "boolean",
                "description": "For snapshot: only show interactive elements"
            },
            "compact": {
                "type": "boolean",
                "description": "For snapshot: remove empty structural elements"
            },
            "depth": {
                "type": "integer",
                "description": "For snapshot: limit tree depth"
            },
            "full_page": {
                "type": "boolean",
                "description": "For screenshot: capture full page"
            },
            "path": {
                "type": "string",
                "description": "File path for screenshot"
            },
            "ms": {
                "type": "integer",
                "description": "Milliseconds to wait"
            },
            "by": {
                "type": "string",
                "enum": ["role", "text", "label", "placeholder", "testid"],
                "description": "For find: semantic locator type"
            },
            "find_action": {
                "type": "string",
                "enum": ["click", "fill", "text", "hover", "check"],
                "description": "For find: action to perform on found element"
            },
            "fill_value": {
                "type": "string",
                "description": "For find with fill action: value to fill"
            }
        },
        "required": ["action"]
    })
}

fn browser_action_unavailable(action_str: &str, backend: ResolvedBackend) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(format!(
            "Action '{action_str}' is unavailable for backend '{}'",
            match backend {
                ResolvedBackend::AgentBrowser => "agent_browser",
                ResolvedBackend::ComputerUse => "computer_use",
            }
        )),
    }
}

fn required_browser_str<'a>(args: &'a Value, key: &str, context: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Missing '{key}' for {context}"))
}

fn parse_browser_action(
    action_str: &str,
    args: &Value,
    backend: ResolvedBackend,
) -> anyhow::Result<Result<BrowserAction, ToolResult>> {
    let action = match action_str {
        "open" => BrowserAction::Open {
            url: required_browser_str(args, "url", "open action")?.into(),
        },
        "snapshot" => BrowserAction::Snapshot {
            interactive_only: args
                .get("interactive_only")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            compact: args.get("compact").and_then(Value::as_bool).unwrap_or(true),
            depth: args
                .get("depth")
                .and_then(Value::as_u64)
                .map(|d| u32::try_from(d).unwrap_or(u32::MAX)),
        },
        "click" => BrowserAction::Click {
            selector: required_browser_str(args, "selector", "click")?.into(),
        },
        "fill" => BrowserAction::Fill {
            selector: required_browser_str(args, "selector", "fill")?.into(),
            value: required_browser_str(args, "value", "fill")?.into(),
        },
        "type" => BrowserAction::Type {
            selector: required_browser_str(args, "selector", "type")?.into(),
            text: required_browser_str(args, "text", "type")?.into(),
        },
        "get_text" => BrowserAction::GetText {
            selector: required_browser_str(args, "selector", "get_text")?.into(),
        },
        "get_title" => BrowserAction::GetTitle,
        "get_url" => BrowserAction::GetUrl,
        "screenshot" => BrowserAction::Screenshot {
            path: args.get("path").and_then(Value::as_str).map(String::from),
            full_page: args
                .get("full_page")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "wait" => BrowserAction::Wait {
            selector: args
                .get("selector")
                .and_then(Value::as_str)
                .map(String::from),
            ms: args.get("ms").and_then(Value::as_u64),
            text: args.get("text").and_then(Value::as_str).map(String::from),
        },
        "press" => BrowserAction::Press {
            key: required_browser_str(args, "key", "press")?.into(),
        },
        "hover" => BrowserAction::Hover {
            selector: required_browser_str(args, "selector", "hover")?.into(),
        },
        "scroll" => BrowserAction::Scroll {
            direction: required_browser_str(args, "direction", "scroll")?.into(),
            pixels: args
                .get("pixels")
                .and_then(Value::as_u64)
                .map(|p| u32::try_from(p).unwrap_or(u32::MAX)),
        },
        "is_visible" => BrowserAction::IsVisible {
            selector: required_browser_str(args, "selector", "is_visible")?.into(),
        },
        "close" => BrowserAction::Close,
        "find" => BrowserAction::Find {
            by: required_browser_str(args, "by", "find")?.into(),
            value: required_browser_str(args, "value", "find")?.into(),
            action: required_browser_str(args, "find_action", "find")?.into(),
            fill_value: args
                .get("fill_value")
                .and_then(Value::as_str)
                .map(String::from),
        },
        _ => return Ok(Err(browser_action_unavailable(action_str, backend))),
    };

    Ok(Ok(action))
}

#[async_trait]
impl Tool for BrowserTool {
    fn category(&self) -> ToolCategory {
        ToolCategory::ReadWrite
    }

    fn name(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        concat!(
            "Web/browser automation with pluggable backends (agent-browser, rust-native, computer_use). ",
            "Supports DOM actions plus optional OS-level actions (mouse_move, mouse_click, mouse_drag, ",
            "key_type, key_press, screen_capture) through a computer-use sidecar. Use 'snapshot' to map ",
            "interactive elements to refs (@e1, @e2). Enforces browser.allowed_domains for open actions."
        )
    }

    fn parameters_schema(&self) -> Value {
        browser_parameters_schema()
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        // Security checks
        if !self.security.can_act() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let backend = match self.resolve_backend().await {
            Ok(selected) => selected,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(error.to_string()),
                });
            }
        };

        // Parse action from args
        let action_str = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'action' parameter"))?;

        if !is_supported_browser_action(action_str) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown action: {action_str}")),
            });
        }

        if backend == ResolvedBackend::ComputerUse {
            return self.execute_computer_use_action(action_str, &args).await;
        }

        let action = match parse_browser_action(action_str, &args, backend)? {
            Ok(action) => action,
            Err(result) => return Ok(result),
        };

        self.execute_action(action, backend).await
    }
}

// ── Helper functions ─────────────────────────────────────────────

fn is_supported_browser_action(action: &str) -> bool {
    matches!(
        action,
        "open"
            | "snapshot"
            | "click"
            | "fill"
            | "type"
            | "get_text"
            | "get_title"
            | "get_url"
            | "screenshot"
            | "wait"
            | "press"
            | "hover"
            | "scroll"
            | "is_visible"
            | "close"
            | "find"
            | "mouse_move"
            | "mouse_click"
            | "mouse_drag"
            | "key_type"
            | "key_press"
            | "screen_capture"
    )
}

fn normalize_domains(domains: Vec<String>) -> Vec<String> {
    domains
        .into_iter()
        .map(|d| d.trim().to_lowercase())
        .filter(|d| !d.is_empty())
        .collect()
}

fn endpoint_reachable(endpoint: &reqwest::Url, timeout: Duration) -> bool {
    let host = match endpoint.host_str() {
        Some(host) if !host.is_empty() => host,
        _ => return false,
    };

    let port = match endpoint.port_or_known_default() {
        Some(port) => port,
        None => return false,
    };

    let mut addrs = match (host, port).to_socket_addrs() {
        Ok(addrs) => addrs,
        Err(_) => return false,
    };

    let addr = match addrs.next() {
        Some(addr) => addr,
        None => return false,
    };

    std::net::TcpStream::connect_timeout(&addr, timeout).is_ok()
}

fn extract_host(url_str: &str) -> anyhow::Result<String> {
    // Simple host extraction without url crate
    let url = url_str.trim();
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("file://"))
        .unwrap_or(url);

    // Extract host — handle bracketed IPv6 addresses like [::1]:8080
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);

    let host = if authority.starts_with('[') {
        // IPv6: take everything up to and including the closing ']'
        authority.find(']').map_or(authority, |i| &authority[..=i])
    } else {
        // IPv4 or hostname: take everything before the port separator
        authority.split(':').next().unwrap_or(authority)
    };

    if host.is_empty() {
        anyhow::bail!("Invalid URL: no host");
    }

    Ok(host.to_lowercase())
}

fn is_private_host(host: &str) -> bool {
    // Strip brackets from IPv6 addresses like [::1]
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);

    if bare == "localhost" || bare.ends_with(".localhost") {
        return true;
    }

    // .local TLD (mDNS)
    if bare
        .rsplit('.')
        .next()
        .is_some_and(|label| label == "local")
    {
        return true;
    }

    // Parse as IP address to catch all representations (decimal, hex, octal, mapped)
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => is_non_global_v4(v4),
            std::net::IpAddr::V6(v6) => is_non_global_v6(v6),
        };
    }

    false
}

/// Returns `true` for any IPv4 address that is not globally routable.
fn is_non_global_v4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, _, _] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        // Shared address space (100.64/10)
        || (a == 100 && (64..=127).contains(&b))
        // Reserved (240.0.0.0/4)
        || a >= 240
        // Documentation (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24)
        || (a == 192 && b == 0)
        || (a == 198 && b == 51)
        || (a == 203 && b == 0)
        // Benchmarking (198.18.0.0/15)
        || (a == 198 && (18..=19).contains(&b))
}

/// Returns `true` for any IPv6 address that is not globally routable.
fn is_non_global_v6(v6: std::net::Ipv6Addr) -> bool {
    let segs = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        // Unique-local (fc00::/7) — IPv6 equivalent of RFC 1918
        || (segs[0] & 0xfe00) == 0xfc00
        // Link-local (fe80::/10)
        || (segs[0] & 0xffc0) == 0xfe80
        // IPv4-mapped addresses
        || v6.to_ipv4_mapped().is_some_and(is_non_global_v4)
}

fn host_matches_allowlist(host: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|pattern| {
        if pattern == "*" {
            return true;
        }
        if pattern.starts_with("*.") {
            // Wildcard subdomain match
            let suffix = &pattern[1..]; // ".example.com"
            host.ends_with(suffix) || host == &pattern[2..]
        } else {
            // Exact match or subdomain
            host == pattern || host.ends_with(&format!(".{pattern}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_domains_works() {
        let domains = vec![
            "  Example.COM  ".into(),
            "docs.example.com".into(),
            String::new(),
        ];
        let normalized = normalize_domains(domains);
        assert_eq!(normalized, vec!["example.com", "docs.example.com"]);
    }

    #[test]
    fn extract_host_works() {
        assert_eq!(
            extract_host("https://example.com/path").unwrap(),
            "example.com"
        );
        assert_eq!(
            extract_host("https://Sub.Example.COM:8080/").unwrap(),
            "sub.example.com"
        );
    }

    #[test]
    fn extract_host_handles_ipv6() {
        // IPv6 with brackets (required for URLs with ports)
        assert_eq!(extract_host("https://[::1]/path").unwrap(), "[::1]");
        // IPv6 with brackets and port
        assert_eq!(
            extract_host("https://[2001:db8::1]:8080/path").unwrap(),
            "[2001:db8::1]"
        );
        // IPv6 with brackets, trailing slash
        assert_eq!(extract_host("https://[fe80::1]/").unwrap(), "[fe80::1]");
    }

    #[test]
    fn is_private_host_detects_local() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("app.localhost"));
        assert!(is_private_host("printer.local"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("192.168.1.1"));
        assert!(is_private_host("10.0.0.1"));
        assert!(!is_private_host("example.com"));
        assert!(!is_private_host("google.com"));
    }

    #[test]
    fn is_private_host_blocks_multicast_and_reserved() {
        assert!(is_private_host("224.0.0.1")); // multicast
        assert!(is_private_host("255.255.255.255")); // broadcast
        assert!(is_private_host("100.64.0.1")); // shared address space
        assert!(is_private_host("240.0.0.1")); // reserved
        assert!(is_private_host("192.0.2.1")); // documentation
        assert!(is_private_host("198.51.100.1")); // documentation
        assert!(is_private_host("203.0.113.1")); // documentation
        assert!(is_private_host("198.18.0.1")); // benchmarking
    }

    #[test]
    fn is_private_host_catches_ipv6() {
        assert!(is_private_host("::1"));
        assert!(is_private_host("[::1]"));
        assert!(is_private_host("0.0.0.0"));
    }

    #[test]
    fn is_private_host_catches_mapped_ipv4() {
        // IPv4-mapped IPv6 addresses
        assert!(is_private_host("::ffff:127.0.0.1"));
        assert!(is_private_host("::ffff:10.0.0.1"));
        assert!(is_private_host("::ffff:192.168.1.1"));
    }

    #[test]
    fn is_private_host_catches_ipv6_private_ranges() {
        // Unique-local (fc00::/7)
        assert!(is_private_host("fd00::1"));
        assert!(is_private_host("fc00::1"));
        // Link-local (fe80::/10)
        assert!(is_private_host("fe80::1"));
        // Public IPv6 should pass
        assert!(!is_private_host("2001:db8::1"));
    }

    #[test]
    fn validate_url_blocks_ipv6_ssrf() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["*".into()], None);
        assert!(tool.validate_url("https://[::1]/").is_err());
        assert!(tool.validate_url("https://[::ffff:127.0.0.1]/").is_err());
        assert!(
            tool.validate_url("https://[::ffff:10.0.0.1]:8080/")
                .is_err()
        );
    }

    #[test]
    fn host_matches_allowlist_exact() {
        let allowed = vec!["example.com".into()];
        assert!(host_matches_allowlist("example.com", &allowed));
        assert!(host_matches_allowlist("sub.example.com", &allowed));
        assert!(!host_matches_allowlist("notexample.com", &allowed));
    }

    #[test]
    fn host_matches_allowlist_wildcard() {
        let allowed = vec!["*.example.com".into()];
        assert!(host_matches_allowlist("sub.example.com", &allowed));
        assert!(host_matches_allowlist("example.com", &allowed));
        assert!(!host_matches_allowlist("other.com", &allowed));
    }

    #[test]
    fn host_matches_allowlist_star() {
        let allowed = vec!["*".into()];
        assert!(host_matches_allowlist("anything.com", &allowed));
        assert!(host_matches_allowlist("example.org", &allowed));
    }

    #[test]
    fn browser_backend_parser_accepts_supported_values() {
        assert_eq!(
            BrowserBackendKind::parse("agent_browser").unwrap(),
            BrowserBackendKind::AgentBrowser
        );
        assert_eq!(
            BrowserBackendKind::parse("computer_use").unwrap(),
            BrowserBackendKind::ComputerUse
        );
        assert_eq!(
            BrowserBackendKind::parse("auto").unwrap(),
            BrowserBackendKind::Auto
        );
    }

    #[test]
    fn browser_backend_parser_rejects_unknown_values() {
        assert!(BrowserBackendKind::parse("playwright").is_err());
    }

    #[test]
    fn browser_tool_default_backend_is_agent_browser() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None);
        assert_eq!(
            tool.configured_backend().unwrap(),
            BrowserBackendKind::AgentBrowser
        );
    }

    #[test]
    fn browser_tool_accepts_auto_backend_config() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_config(
            security,
            BrowserToolConfig {
                allowed_domains: vec!["example.com".into()],
                backend: "auto".into(),
                ..BrowserToolConfig::default()
            },
        );
        assert_eq!(tool.configured_backend().unwrap(), BrowserBackendKind::Auto);
    }

    #[test]
    fn browser_tool_accepts_computer_use_backend_config() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_config(
            security,
            BrowserToolConfig {
                allowed_domains: vec!["example.com".into()],
                backend: "computer_use".into(),
                ..BrowserToolConfig::default()
            },
        );
        assert_eq!(
            tool.configured_backend().unwrap(),
            BrowserBackendKind::ComputerUse
        );
    }

    #[test]
    fn computer_use_endpoint_rejects_public_http_by_default() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_config(
            security,
            BrowserToolConfig {
                allowed_domains: vec!["example.com".into()],
                backend: "computer_use".into(),
                computer_use: ComputerUseConfig {
                    endpoint: "http://computer-use.example.com/v1/actions".into(),
                    ..ComputerUseConfig::default()
                },
                ..BrowserToolConfig::default()
            },
        );

        assert!(tool.computer_use_endpoint_url().is_err());
    }

    #[test]
    fn computer_use_endpoint_requires_https_for_public_remote() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_config(
            security,
            BrowserToolConfig {
                allowed_domains: vec!["example.com".into()],
                backend: "computer_use".into(),
                computer_use: ComputerUseConfig {
                    endpoint: "https://computer-use.example.com/v1/actions".into(),
                    allow_remote_endpoint: true,
                    ..ComputerUseConfig::default()
                },
                ..BrowserToolConfig::default()
            },
        );

        assert!(tool.computer_use_endpoint_url().is_ok());
    }

    #[test]
    fn computer_use_coordinate_validation_applies_limits() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new_with_config(
            security,
            BrowserToolConfig {
                allowed_domains: vec!["example.com".into()],
                backend: "computer_use".into(),
                computer_use: ComputerUseConfig {
                    max_coordinate_x: Some(100),
                    max_coordinate_y: Some(100),
                    ..ComputerUseConfig::default()
                },
                ..BrowserToolConfig::default()
            },
        );

        assert!(
            tool.validate_coordinate("x", 50, tool.computer_use.max_coordinate_x)
                .is_ok()
        );
        assert!(
            tool.validate_coordinate("x", 101, tool.computer_use.max_coordinate_x)
                .is_err()
        );
        assert!(
            tool.validate_coordinate("y", -1, tool.computer_use.max_coordinate_y)
                .is_err()
        );
    }

    #[test]
    fn browser_tool_name() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None);
        assert_eq!(tool.name(), "browser");
    }

    #[test]
    fn browser_tool_validates_url() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec!["example.com".into()], None);

        // Valid
        assert!(tool.validate_url("https://example.com").is_ok());
        assert!(tool.validate_url("https://sub.example.com/path").is_ok());

        // Invalid - not in allowlist
        assert!(tool.validate_url("https://other.com").is_err());

        // Invalid - private host
        assert!(tool.validate_url("https://localhost").is_err());
        assert!(tool.validate_url("https://127.0.0.1").is_err());

        // Invalid - not https
        assert!(tool.validate_url("ftp://example.com").is_err());

        // file:// URLs blocked (local file exfiltration risk)
        assert!(tool.validate_url("file:///tmp/test.html").is_err());
    }

    #[test]
    fn browser_tool_empty_allowlist_blocks() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = BrowserTool::new(security, vec![], None);
        assert!(tool.validate_url("https://example.com").is_err());
    }
}
