use crate::providers::ModelProviders;
use anyhow::{Context, Result};
use directories::UserDirs;
use nenjo::AgentConfig;
use nenjo_events::Capability;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

use std::path::{Path, PathBuf};
use toml;

// ── Top-level config ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The `.nenjo/` directory (e.g. `~/.nenjo/`) — computed, not serialized.
    #[serde(skip)]
    pub config_dir: PathBuf,
    /// Workspace directory (e.g. `~/.nenjo/workspace/`) — computed, not serialized.
    #[serde(skip)]
    pub workspace_dir: PathBuf,
    /// State directory (e.g. `~/.nenjo/state/`) — computed, not serialized.
    ///
    /// Contains agent memories and resources. This is the single directory
    /// users can back up to preserve all agent-generated state. Always resolved
    /// as an absolute path from `~/.nenjo/` so it remains accessible regardless
    /// of the current working directory (including worktrees).
    #[serde(skip)]
    pub state_dir: PathBuf,
    /// Directory for cached manifest data (`~/.nenjo/manifests/`) — computed, not serialized.
    #[serde(skip)]
    pub manifests_dir: PathBuf,

    /// Base URL for the backend API. Defaults to `https://api.nenjo.ai`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_api_url: Option<String>,
    /// API key attached to every request to the backend.
    pub api_key: String,
    /// NATS server URL for direct backend↔worker communication. Defaults to `tls://nats.nenjo.ai`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nats_url: Option<String>,

    /// Api keys for the llm model providers
    pub model_provider_api_keys: HashMap<ModelProviders, String>,

    #[serde(default)]
    pub autonomy: AutonomyConfig,

    #[serde(default)]
    pub reliability: ReliabilityConfig,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub memory: MemoryConfig,

    #[serde(default)]
    pub browser: BrowserConfig,

    #[serde(default)]
    pub http_request: HttpRequestConfig,

    #[serde(default)]
    pub web_search: WebSearchConfig,

    #[serde(default)]
    pub web_fetch: WebFetchConfig,

    #[serde(default)]
    pub git: GitConfig,

    /// Worker capabilities — which command types this worker handles.
    /// Empty means all capabilities (full runner mode).
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

const DEFAULT_BACKEND_API_URL: &str = "https://api.nenjo.ai";
const DEFAULT_NATS_URL: &str = "tls://nats.nenjo.ai";

fn default_true() -> bool {
    true
}

// ── Browser (friendly-service browsing only) ───────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrowserConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub allowed_domains: Vec<String>,
}

// ── Web search ───────────────────────────────────────────────────

/// Web search tool configuration (`[web_search]` section).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Enable `web_search_tool` for web searches (default: true, uses DuckDuckGo)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Search provider: "duckduckgo" (free, no API key) or "brave" (requires API key)
    #[serde(default = "default_web_search_provider")]
    pub provider: String,
    /// Brave Search API key (required if provider is "brave")
    #[serde(default)]
    pub brave_api_key: Option<String>,
    /// Maximum results per search (1-10)
    #[serde(default = "default_web_search_max_results")]
    pub max_results: usize,
    /// Request timeout in seconds
    #[serde(default = "default_web_search_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_web_search_provider() -> String {
    "duckduckgo".into()
}

fn default_web_search_max_results() -> usize {
    5
}

fn default_web_search_timeout_secs() -> u64 {
    15
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: default_web_search_provider(),
            brave_api_key: None,
            max_results: default_web_search_max_results(),
            timeout_secs: default_web_search_timeout_secs(),
        }
    }
}

// ── Web fetch ────────────────────────────────────────────────────

/// Web fetch tool configuration (`[web_fetch]` section).
///
/// Fetches web pages and converts HTML to plain text for LLM consumption.
/// Domain filtering: `allowed_domains` controls which hosts are reachable (use `["*"]`
/// for all public hosts). `blocked_domains` takes priority over `allowed_domains`.
/// If `allowed_domains` is empty, all requests are rejected (deny-by-default).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebFetchConfig {
    /// Enable `web_fetch` tool for fetching web page content
    #[serde(default)]
    pub enabled: bool,
    /// Allowed domains for web fetch (exact or subdomain match; `["*"]` = all public hosts)
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Blocked domains (exact or subdomain match; always takes priority over allowed_domains)
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    /// Maximum response size in bytes (default: 500KB, plain text is much smaller than raw HTML)
    #[serde(default = "default_web_fetch_max_response_size")]
    pub max_response_size: usize,
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_web_fetch_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_web_fetch_max_response_size() -> usize {
    500_000 // 500KB
}

fn default_web_fetch_timeout_secs() -> u64 {
    30
}

impl Default for WebFetchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_domains: vec!["*".into()],
            blocked_domains: vec![],
            max_response_size: default_web_fetch_max_response_size(),
            timeout_secs: default_web_fetch_timeout_secs(),
        }
    }
}

// ── HTTP request tool ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HttpRequestConfig {
    /// Enable `http_request` tool for API interactions
    #[serde(default)]
    pub enabled: bool,
    /// Allowed domains for HTTP requests (exact or subdomain match)
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Maximum response size in bytes (default: 1MB)
    #[serde(default = "default_http_max_response_size")]
    pub max_response_size: usize,
    /// Request timeout in seconds (default: 30)
    #[serde(default = "default_http_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_http_max_response_size() -> usize {
    1_000_000 // 1MB
}

fn default_http_timeout_secs() -> u64 {
    30
}

// ── Memory ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    #[serde(default = "default_memory_backend")]
    pub backend: String,
}

fn default_memory_backend() -> String {
    "markdown".into()
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            backend: default_memory_backend(),
        }
    }
}

// ── Autonomy / Security ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutonomyConfig {
    pub workspace_only: bool,
    pub blocked_commands: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_actions_per_hour: u32,
    pub max_cost_per_day_cents: u32,

    /// Require explicit approval for medium-risk shell commands.
    #[serde(default = "default_true")]
    pub require_approval_for_medium_risk: bool,

    /// Block high-risk shell commands even if not on the blocklist.
    #[serde(default = "default_true")]
    pub block_high_risk_commands: bool,
}

impl Default for AutonomyConfig {
    fn default() -> Self {
        Self {
            workspace_only: true,
            blocked_commands: vec![
                // Destructive / filesystem
                "rm".into(),
                "mkfs".into(),
                "dd".into(),
                // System power
                "shutdown".into(),
                "reboot".into(),
                "halt".into(),
                "poweroff".into(),
                // Privilege escalation
                "sudo".into(),
                "su".into(),
                // Permissions / user management
                "chown".into(),
                "chmod".into(),
                "useradd".into(),
                "userdel".into(),
                "usermod".into(),
                "passwd".into(),
                // Mount / unmount
                "mount".into(),
                "umount".into(),
                // Firewall
                "iptables".into(),
                "ufw".into(),
                "firewall-cmd".into(),
                // Network / data exfiltration
                "curl".into(),
                "wget".into(),
                "nc".into(),
                "ncat".into(),
                "netcat".into(),
                "scp".into(),
                "ssh".into(),
                "ftp".into(),
                "telnet".into(),
                // Process / service management
                "killall".into(),
                "kill".into(),
                "pkill".into(),
                "crontab".into(),
                "at".into(),
                "systemctl".into(),
                "service".into(),
            ],
            forbidden_paths: vec![
                "/etc".into(),
                "/root".into(),
                "/home".into(),
                "/usr".into(),
                "/bin".into(),
                "/sbin".into(),
                "/lib".into(),
                "/opt".into(),
                "/boot".into(),
                "/dev".into(),
                "/proc".into(),
                "/sys".into(),
                "/var".into(),
                "/tmp".into(),
                "~/.ssh".into(),
                "~/.gnupg".into(),
                "~/.aws".into(),
                "~/.config".into(),
            ],
            max_actions_per_hour: 1000,
            max_cost_per_day_cents: 500,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
        }
    }
}

// ── Reliability / supervision ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityConfig {
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_backoff_ms")]
    pub backoff_ms: u64,
    #[serde(default)]
    pub fallback_providers: Vec<String>,
    #[serde(default)]
    pub model_fallbacks: std::collections::HashMap<String, Vec<String>>,
}

fn default_max_retries() -> u32 {
    2
}

fn default_backoff_ms() -> u64 {
    500
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            backoff_ms: default_backoff_ms(),
            fallback_providers: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
        }
    }
}

// ── Security Config ─────────────────────────────────────────────────

/// Security configuration for sandboxing and audit logging
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SecurityConfig {
    /// Sandbox configuration
    #[serde(default)]
    pub sandbox: SandboxConfig,

    /// Audit logging configuration
    #[serde(default)]
    pub audit: AuditConfig,
}

/// Sandbox configuration for OS-level isolation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Enable sandboxing (None = auto-detect, Some = explicit)
    #[serde(default)]
    pub enabled: Option<bool>,

    /// Sandbox backend to use
    #[serde(default)]
    pub backend: SandboxBackend,

    /// Custom Firejail arguments (when backend = firejail)
    #[serde(default)]
    pub firejail_args: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: None, // Auto-detect
            backend: SandboxBackend::Auto,
            firejail_args: Vec::new(),
        }
    }
}

/// Sandbox backend selection
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxBackend {
    /// Auto-detect best available (default)
    #[default]
    Auto,
    /// Landlock (Linux kernel LSM, native)
    Landlock,
    /// Firejail (user-space sandbox)
    Firejail,
    /// Bubblewrap (user namespaces)
    Bubblewrap,
    /// Docker container isolation
    Docker,
    /// No sandboxing (application-layer only)
    None,
}

/// Audit logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    /// Enable audit logging
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,

    /// Path to audit log file (relative to nenjo dir)
    #[serde(default = "default_audit_log_path")]
    pub log_path: String,

    /// Maximum log size in MB before rotation
    #[serde(default = "default_audit_max_size_mb")]
    pub max_size_mb: u32,

    /// Sign events with HMAC for tamper evidence
    #[serde(default)]
    pub sign_events: bool,
}

fn default_audit_enabled() -> bool {
    true
}

fn default_audit_log_path() -> String {
    "audit.log".to_string()
}

fn default_audit_max_size_mb() -> u32 {
    100
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            log_path: default_audit_log_path(),
            max_size_mb: default_audit_max_size_mb(),
            sign_events: false,
        }
    }
}

// ── Skills config ─────────────────────────────────────────────────

// ── Git configuration ───────────────────────────────────────────

/// Git identity and signing configuration (`[git]` section).
///
/// ```toml
/// [git]
/// user_name = "neni-nenjo"
/// user_email = "gitops@boonlabs.co"
/// signing_key = "~/.ssh/id_ed25519.pub"
/// ```
///
/// The signing key path supports `~` expansion. When set, all agent commits
/// are signed with SSH and will show as "Verified" on GitHub (provided the
/// public key is added to the GitHub account's SSH signing keys).
///
/// Environment variable overrides: `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`,
/// `GIT_SIGNING_KEY`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitConfig {
    /// Git author/committer name (e.g. "neni-nenjo")
    #[serde(default)]
    pub user_name: Option<String>,
    /// Git author/committer email (e.g. "gitops@boonlabs.co")
    #[serde(default)]
    pub user_email: Option<String>,
    /// Path to SSH signing key (e.g. "~/.ssh/id_ed25519.pub").
    /// When set, commits are signed with SSH (`gpg.format = ssh`).
    #[serde(default)]
    pub signing_key: Option<String>,
}

// ── Git credential helper setup ──────────────────────────────────

/// Write an isolated git config for all worker git operations.
///
/// Sets `GIT_CONFIG_GLOBAL` in the process environment so every git command
/// run by the worker (and any lambda subprocess that inherits the env) uses
/// `~/.nenjo/gitconfig` instead of `~/.gitconfig`.  This prevents system
/// credential helpers (e.g. osxkeychain) from interfering.
///
/// The only required user configuration is the `GITHUB_TOKEN` environment
/// variable — no `gh` CLI installation or manual auth steps needed.
///
/// **Step A** — Write `~/.nenjo/git-credential-helper.sh`.
/// **Step B** — Write `~/.nenjo/gitconfig` pointing at the helper.
/// **Step C** — Set `GIT_CONFIG_GLOBAL=~/.nenjo/gitconfig` in the process env.
fn setup_git_credential_helper(home: &Path, git_config: &GitConfig) {
    // If GITHUB_TOKEN is not set, skip the entire git setup so the system's
    // native git credentials (e.g. osxkeychain, credential-manager) are used.
    let has_github_token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .is_some();

    if !has_github_token {
        tracing::debug!("GITHUB_TOKEN not set — using system git credentials");

        // Still set identity env vars if configured, so they survive env_clear()
        // in the tools. The gitconfig and credential helper are skipped.
        let git_name = git_config.user_name.as_deref().unwrap_or("");
        let git_email = git_config.user_email.as_deref().unwrap_or("");
        #[allow(unsafe_code)]
        unsafe {
            if !git_name.is_empty() {
                std::env::set_var("GIT_AUTHOR_NAME", git_name);
                std::env::set_var("GIT_COMMITTER_NAME", git_name);
            }
            if !git_email.is_empty() {
                std::env::set_var("GIT_AUTHOR_EMAIL", git_email);
                std::env::set_var("GIT_COMMITTER_EMAIL", git_email);
            }
        }
        return;
    }

    let nenjo_dir = home.join(".nenjo");
    let nenjo_gitconfig = nenjo_dir.join("gitconfig");
    let helper_script = nenjo_dir.join("git-credential-helper.sh");

    // Step A: write the credential helper script
    //
    // Git calls credential helpers with a single argument ("get", "store", or
    // "erase"). We only need to handle "get" — output the token as a
    // username/password pair and exit 0 for all other invocations.
    let script = r#"#!/bin/sh
# Nenjo git credential helper — reads GITHUB_TOKEN from the environment.
# Git calls this script with one argument: get | store | erase
case "$1" in
  get)
    echo "username=x-access-token"
    echo "password=${GITHUB_TOKEN}"
    ;;
esac
"#;

    if let Err(e) = fs::write(&helper_script, script) {
        tracing::warn!(error = %e, path = %helper_script.display(), "Failed to write git credential helper script");
    } else {
        // Make executable (unix only — Windows is unsupported for now)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = fs::set_permissions(&helper_script, fs::Permissions::from_mode(0o755)) {
                tracing::warn!(error = %e, "Failed to chmod git credential helper script");
            }
        }
        tracing::debug!(path = %helper_script.display(), "Git credential helper script written");
    }

    // Step B: write ~/.nenjo/gitconfig
    // Priority: env var > config.toml > empty
    let git_name = std::env::var("GIT_AUTHOR_NAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git_config.user_name.clone())
        .unwrap_or_default();
    let git_email = std::env::var("GIT_AUTHOR_EMAIL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git_config.user_email.clone())
        .unwrap_or_default();
    let signing_key = std::env::var("GIT_SIGNING_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git_config.signing_key.clone())
        .map(|k| {
            // Expand ~ to home directory
            if let Some(rest) = k.strip_prefix("~/") {
                home.join(rest).to_string_lossy().to_string()
            } else {
                k
            }
        });
    let helper_path = helper_script.to_string_lossy();

    // Include the user's own gitconfig so signing, aliases, and other
    // personal settings carry through. The nenjo-specific sections below
    // (credential helper, user identity) take precedence over included values
    // because they appear after the [include].
    //
    // Skip the include if the user's gitconfig references the nenjo gitconfig
    // (e.g. a stale `includeIf` from a previous version), which would cause
    // a circular include loop (fatal: exceeded maximum include depth).
    let user_gitconfig = home.join(".gitconfig");
    let include_user_gitconfig = user_gitconfig.exists()
        && !fs::read_to_string(&user_gitconfig)
            .unwrap_or_default()
            .contains(".nenjo/gitconfig");

    let user_gitconfig_path = user_gitconfig.to_string_lossy();

    let mut contents = if include_user_gitconfig {
        format!(
            r#"[include]
    path = {user_gitconfig_path}
[credential "https://github.com"]
    helper = {helper_path}
[user]
    name = {git_name}
    email = {git_email}
"#
        )
    } else {
        if user_gitconfig.exists() {
            tracing::warn!(
                "Skipping include of ~/.gitconfig — references .nenjo/gitconfig (circular include)"
            );
        }
        format!(
            r#"[credential "https://github.com"]
    helper = {helper_path}
[user]
    name = {git_name}
    email = {git_email}
"#
        )
    };

    if let Some(ref key) = signing_key {
        contents.push_str(&format!(
            r#"    signingkey = {key}
[commit]
    gpgsign = true
[tag]
    gpgsign = true
[gpg]
    format = ssh
"#
        ));
        tracing::info!(signing_key = %key, "Git commit signing enabled (SSH)");
    }

    if let Err(e) = fs::write(&nenjo_gitconfig, &contents) {
        tracing::warn!(error = %e, path = %nenjo_gitconfig.display(), "Failed to write ~/.nenjo/gitconfig");
    }

    // Step C: set GIT_CONFIG_GLOBAL so all git commands in this process (and
    // any subprocess that inherits the environment, including lambda scripts)
    // use our isolated config instead of ~/.gitconfig.
    //
    // Also set GIT_CONFIG_NOSYSTEM=1 so git ignores the system-level gitconfig
    // (e.g. /etc/gitconfig or the Xcode CLT git config on macOS), which often
    // sets `credential.helper = osxkeychain`. Without this, osxkeychain is
    // called first and may return stale credentials, overriding our helper.
    //
    // SAFETY: called once at startup before any threads are spawned.
    // Also set GIT_AUTHOR_*/GIT_COMMITTER_* env vars so they survive
    // env_clear() in the git and shell tools (they're on the safe list).
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("GIT_CONFIG_GLOBAL", &nenjo_gitconfig);
        std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
        if !git_name.is_empty() {
            std::env::set_var("GIT_AUTHOR_NAME", &git_name);
            std::env::set_var("GIT_COMMITTER_NAME", &git_name);
        }
        if !git_email.is_empty() {
            std::env::set_var("GIT_AUTHOR_EMAIL", &git_email);
            std::env::set_var("GIT_COMMITTER_EMAIL", &git_email);
        }
    }
}

// ── Config impl ──────────────────────────────────────────────────

impl Default for Config {
    fn default() -> Self {
        let home =
            UserDirs::new().map_or_else(|| PathBuf::from("."), |u| u.home_dir().to_path_buf());
        let nenjo_dir = home.join(".nenjo");

        Self {
            config_dir: nenjo_dir.clone(),
            workspace_dir: nenjo_dir.join("workspace"),
            state_dir: nenjo_dir.join("state"),
            manifests_dir: nenjo_dir.join("manifests"),
            model_provider_api_keys: HashMap::new(),
            api_key: String::new(),
            backend_api_url: None,
            nats_url: None,
            autonomy: AutonomyConfig::default(),
            reliability: ReliabilityConfig::default(),
            agent: AgentConfig::default(),
            memory: MemoryConfig::default(),
            browser: BrowserConfig::default(),
            http_request: HttpRequestConfig::default(),
            web_search: WebSearchConfig::default(),
            web_fetch: WebFetchConfig::default(),
            git: GitConfig::default(),
            capabilities: Vec::new(),
        }
    }
}

impl Config {
    /// Resolved backend API URL (falls back to production default).
    pub fn backend_api_url(&self) -> &str {
        self.backend_api_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_BACKEND_API_URL)
    }

    /// Resolved NATS URL (falls back to production default).
    pub fn nats_url(&self) -> &str {
        self.nats_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_NATS_URL)
    }

    pub fn load_or_init(nenjo_dir_override: Option<&str>) -> Result<Self> {
        let home = UserDirs::new()
            .map(|u| u.home_dir().to_path_buf())
            .context("Could not find home directory")?;
        let nenjo_dir = match nenjo_dir_override {
            Some(dir) => PathBuf::from(dir),
            None => home.join(".nenjo"),
        };
        let config_path = nenjo_dir.join("config.toml");

        fs::create_dir_all(&nenjo_dir).context("Failed to create nenjo directory")?;
        fs::create_dir_all(nenjo_dir.join("workspace"))
            .context("Failed to create workspace directory")?;
        fs::create_dir_all(nenjo_dir.join("state")).context("Failed to create state directory")?;
        fs::create_dir_all(nenjo_dir.join("manifests"))
            .context("Failed to create manifests directory")?;

        let mut config = if config_path.exists() {
            let contents =
                fs::read_to_string(&config_path).context("Failed to read config file")?;
            let mut config: Config =
                toml::from_str(&contents).context("Failed to parse config file")?;
            // Set computed paths that are skipped during serialization
            config.config_dir = nenjo_dir.clone();
            config.workspace_dir = nenjo_dir.join("workspace");
            config.state_dir = nenjo_dir.join("state");
            config.manifests_dir = nenjo_dir.join("manifests");
            config
        } else {
            let config = Config {
                config_dir: nenjo_dir.clone(),
                workspace_dir: nenjo_dir.join("workspace"),
                state_dir: nenjo_dir.join("state"),
                manifests_dir: nenjo_dir.join("manifests"),
                ..Config::default()
            };
            config.save()?;
            config
        };

        config.apply_env_overrides();
        config.validate()?;

        // Configure git credentials, identity, and signing.
        // Must happen after config is loaded so [git] values are available.
        setup_git_credential_helper(&home, &config.git);

        Ok(config)
    }

    /// Apply environment variable overrides to config.
    ///
    /// Environment variables take precedence over values from config.toml.
    /// For model provider API keys, each provider has one or more candidate
    /// env vars (e.g. `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`). The first
    /// non-empty match wins and is inserted into `model_provider_api_keys`,
    /// overriding any value from the config file.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(val) = std::env::var("NENJO_API_URL") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                self.backend_api_url = Some(val);
            }
        }

        if let Ok(val) = std::env::var("NENJO_API_KEY") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                self.api_key = val;
            }
        }

        if let Ok(val) = std::env::var("NATS_URL") {
            let val = val.trim().to_string();
            if !val.is_empty() {
                self.nats_url = Some(val);
            }
        }

        // ── Model provider API key overrides ─────────────────
        let provider_vars = crate::providers::provider_env_vars();
        for (provider, env_var_candidates) in &provider_vars {
            for env_var in env_var_candidates {
                if let Ok(val) = std::env::var(env_var) {
                    let val = val.trim().to_string();
                    if !val.is_empty() {
                        self.model_provider_api_keys.insert(provider.clone(), val);
                        break; // first non-empty candidate wins
                    }
                }
            }
        }
    }

    /// Write the config to `{config_dir}/config.toml`.
    pub fn save(&self) -> Result<()> {
        if self.config_dir.as_os_str().is_empty() {
            return Ok(()); // no config dir set (e.g. tests)
        }
        let path = self.config_dir.join("config.toml");
        let toml = toml::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&path, toml)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }

    /// Validate that required fields are present.
    pub fn validate(&self) -> Result<()> {
        if self.api_key.is_empty() {
            anyhow::bail!(
                "NENJO_API_KEY is required. Set it via --api-key, NENJO_API_KEY env var, or api_key in ~/.nenjo/config.toml"
            );
        }
        Ok(())
    }

    pub fn can_run_routine(&self) -> bool {
        !self.model_provider_api_keys.is_empty()
    }
}
