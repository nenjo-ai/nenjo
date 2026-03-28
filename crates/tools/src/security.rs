use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// How much autonomy the agent has
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutonomyLevel {
    /// Read-only: can observe but not act
    ReadOnly,
    /// Supervised: acts but requires approval for risky operations
    #[default]
    Supervised,
    /// Full: autonomous execution within policy bounds
    Full,
}

/// Risk score for shell command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRiskLevel {
    Low,
    Medium,
    High,
}

/// Sliding-window action tracker for rate limiting.
#[derive(Debug)]
pub struct ActionTracker {
    /// Timestamps of recent actions (kept within the last hour).
    actions: Mutex<Vec<Instant>>,
}

impl Default for ActionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionTracker {
    pub fn new() -> Self {
        Self {
            actions: Mutex::new(Vec::new()),
        }
    }

    /// Record an action and return the current count within the window.
    pub fn record(&self) -> usize {
        let mut actions = self.actions.lock();
        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        actions.retain(|t| *t > cutoff);
        actions.push(Instant::now());
        actions.len()
    }

    /// Count of actions in the current window without recording.
    pub fn count(&self) -> usize {
        let mut actions = self.actions.lock();
        let cutoff = Instant::now()
            .checked_sub(std::time::Duration::from_secs(3600))
            .unwrap_or_else(Instant::now);
        actions.retain(|t| *t > cutoff);
        actions.len()
    }
}

impl Clone for ActionTracker {
    fn clone(&self) -> Self {
        let actions = self.actions.lock();
        Self {
            actions: Mutex::new(actions.clone()),
        }
    }
}

/// Security policy enforced on all tool executions
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    pub autonomy: AutonomyLevel,
    pub workspace_dir: PathBuf,
    pub workspace_only: bool,
    pub blocked_commands: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub max_actions_per_hour: u32,
    pub max_cost_per_day_cents: u32,
    pub require_approval_for_medium_risk: bool,
    pub block_high_risk_commands: bool,
    pub tracker: ActionTracker,
    /// Extra environment variables forwarded to shell subprocesses.
    /// Used for tool-specific credentials like GITHUB_TOKEN that agents
    /// need at runtime (e.g. for `gh` CLI).  Populated from the worker
    /// process environment during policy construction.
    pub forwarded_env: Vec<(String, String)>,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));
        Self {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: home.join(".nenjo").join("workspace"),
            workspace_only: true,
            blocked_commands: default_blocked_commands(),
            forbidden_paths: vec![
                // System directories (blocked even when workspace_only=false)
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
                // Sensitive dotfiles
                "~/.ssh".into(),
                "~/.gnupg".into(),
                "~/.aws".into(),
                "~/.config".into(),
            ],
            max_actions_per_hour: 1000,
            max_cost_per_day_cents: 500,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            tracker: ActionTracker::new(),
            forwarded_env: collect_forwarded_env(),
        }
    }
}

/// Collect environment variables that should be forwarded to shell subprocesses.
fn collect_forwarded_env() -> Vec<(String, String)> {
    let mut env = Vec::new();

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        env.push(("GITHUB_TOKEN".into(), token.clone()));
        if std::env::var("GH_TOKEN").is_err() {
            env.push(("GH_TOKEN".into(), token));
        }
    }
    if let Ok(token) = std::env::var("GH_TOKEN") {
        if !env.iter().any(|(k, _)| k == "GH_TOKEN") {
            env.push(("GH_TOKEN".into(), token.clone()));
        }
        if !env.iter().any(|(k, _)| k == "GITHUB_TOKEN") {
            env.push(("GITHUB_TOKEN".into(), token));
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let global_config = Path::new(&home).join(".gitconfig");
        if global_config.exists() {
            env.push((
                "GIT_CONFIG_GLOBAL".into(),
                global_config.to_string_lossy().into_owned(),
            ));
        }
    }
    env.push(("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()));

    env
}

/// Default blocked commands for agent shell execution.
fn default_blocked_commands() -> Vec<String> {
    vec![
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
    ]
}

/// Check if a character appears outside of single/double quotes.
fn contains_unquoted(s: &str, ch: char) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' && !in_single {
            escaped = true;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if !in_single && !in_double && c == ch {
            return true;
        }
    }
    false
}

/// Split a shell command on unquoted separators (`|`, `||`, `&&`, `;`, `\n`).
fn split_on_unquoted_separators(command: &str) -> Vec<String> {
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let chars: Vec<char> = command.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];

        if escaped {
            current.push(c);
            escaped = false;
            i += 1;
            continue;
        }

        if c == '\\' && !in_single {
            current.push(c);
            escaped = true;
            i += 1;
            continue;
        }

        if c == '\'' && !in_double {
            in_single = !in_single;
            current.push(c);
            i += 1;
            continue;
        }

        if c == '"' && !in_single {
            in_double = !in_double;
            current.push(c);
            i += 1;
            continue;
        }

        if !in_single && !in_double {
            // Check two-char operators first
            if i + 1 < len {
                let two = &command[i..][..chars[i].len_utf8() + chars[i + 1].len_utf8()];
                if two == "&&" || two == "||" {
                    if !current.trim().is_empty() {
                        segments.push(current.clone());
                    }
                    current.clear();
                    i += 2;
                    continue;
                }
            }
            // Single-char separators
            if c == '|' || c == ';' || c == '\n' {
                if !current.trim().is_empty() {
                    segments.push(current.clone());
                }
                current.clear();
                i += 1;
                continue;
            }
        }

        current.push(c);
        i += 1;
    }

    if !current.trim().is_empty() {
        segments.push(current);
    }
    segments
}

/// Skip leading environment variable assignments (e.g. `FOO=bar cmd args`).
fn skip_env_assignments(s: &str) -> &str {
    let mut rest = s;
    loop {
        let Some(word) = rest.split_whitespace().next() else {
            return rest;
        };
        if word.contains('=')
            && word
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            rest = rest[word.len()..].trim_start();
        } else {
            return rest;
        }
    }
}

impl SecurityPolicy {
    /// Classify command risk. Any high-risk segment marks the whole command high.
    pub fn command_risk_level(&self, command: &str) -> CommandRiskLevel {
        let segments = split_on_unquoted_separators(command);
        let mut saw_medium = false;

        for segment in segments.iter().map(|s| s.trim()) {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }

            let cmd_part = skip_env_assignments(segment);
            let mut words = cmd_part.split_whitespace();
            let Some(base_raw) = words.next() else {
                continue;
            };

            let base = base_raw
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();

            let args: Vec<String> = words.map(|w| w.to_ascii_lowercase()).collect();
            let joined_segment = cmd_part.to_ascii_lowercase();

            // High-risk commands
            if matches!(
                base.as_str(),
                "rm" | "mkfs"
                    | "dd"
                    | "shutdown"
                    | "reboot"
                    | "halt"
                    | "poweroff"
                    | "sudo"
                    | "su"
                    | "chown"
                    | "chmod"
                    | "useradd"
                    | "userdel"
                    | "usermod"
                    | "passwd"
                    | "mount"
                    | "umount"
                    | "iptables"
                    | "ufw"
                    | "firewall-cmd"
                    | "curl"
                    | "wget"
                    | "nc"
                    | "ncat"
                    | "netcat"
                    | "scp"
                    | "ssh"
                    | "ftp"
                    | "telnet"
            ) {
                return CommandRiskLevel::High;
            }

            if joined_segment.contains("rm -rf /")
                || joined_segment.contains("rm -fr /")
                || joined_segment.contains(":(){:|:&};:")
            {
                return CommandRiskLevel::High;
            }

            // Medium-risk commands
            let medium = match base.as_str() {
                "git" => args.first().is_some_and(|verb| {
                    matches!(
                        verb.as_str(),
                        "commit"
                            | "push"
                            | "reset"
                            | "clean"
                            | "rebase"
                            | "merge"
                            | "cherry-pick"
                            | "revert"
                            | "branch"
                            | "checkout"
                            | "switch"
                            | "tag"
                    )
                }),
                "npm" | "pnpm" | "yarn" => args.first().is_some_and(|verb| {
                    matches!(
                        verb.as_str(),
                        "install" | "add" | "remove" | "uninstall" | "update" | "publish"
                    )
                }),
                "cargo" => args.first().is_some_and(|verb| {
                    matches!(
                        verb.as_str(),
                        "add" | "remove" | "install" | "clean" | "publish"
                    )
                }),
                "pip" | "pip3" | "uv" => args
                    .first()
                    .is_some_and(|verb| matches!(verb.as_str(), "install" | "uninstall")),
                "go" => args.first().is_some_and(|verb| {
                    matches!(verb.as_str(), "install" | "get" | "mod" | "clean")
                }),
                "gh" => args.first().is_some_and(|verb| {
                    matches!(verb.as_str(), "pr" | "issue" | "release" | "repo")
                }),
                "make" | "cmake" => true,
                "touch" | "mkdir" | "mv" | "cp" | "ln" => true,
                _ => false,
            };

            saw_medium |= medium;
        }

        if saw_medium {
            CommandRiskLevel::Medium
        } else {
            CommandRiskLevel::Low
        }
    }

    /// Validate full command execution policy (allowlist + risk gate).
    pub fn validate_command_execution(
        &self,
        command: &str,
        approved: bool,
    ) -> Result<CommandRiskLevel, String> {
        if !self.is_command_allowed(command) {
            return Err(format!("Command not allowed by security policy: {command}"));
        }

        let risk = self.command_risk_level(command);

        if risk == CommandRiskLevel::High {
            if self.block_high_risk_commands {
                return Err("Command blocked: high-risk command is disallowed by policy".into());
            }
            if self.autonomy == AutonomyLevel::Supervised && !approved {
                return Err(
                    "Command requires explicit approval (approved=true): high-risk operation"
                        .into(),
                );
            }
        }

        if risk == CommandRiskLevel::Medium
            && self.autonomy == AutonomyLevel::Supervised
            && self.require_approval_for_medium_risk
            && !approved
        {
            return Err(
                "Command requires explicit approval (approved=true): medium-risk operation".into(),
            );
        }

        Ok(risk)
    }

    /// Check if a path (absolute, `~/`-prefixed, or relative) resolves within
    /// the workspace directory.
    fn is_within_workspace(&self, arg: &str) -> bool {
        let expanded = if let Some(stripped) = arg.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                PathBuf::from(home).join(stripped)
            } else {
                return false;
            }
        } else {
            PathBuf::from(arg)
        };

        match (expanded.canonicalize(), self.workspace_dir.canonicalize()) {
            (Ok(resolved), Ok(workspace)) => resolved.starts_with(&workspace),
            _ => expanded.starts_with(&self.workspace_dir),
        }
    }

    pub fn is_command_allowed(&self, command: &str) -> bool {
        if self.autonomy == AutonomyLevel::ReadOnly {
            return false;
        }

        // Block subshell/expansion operators
        {
            let mut outside_single_quotes = String::new();
            let mut in_single = false;
            for c in command.chars() {
                if c == '\'' {
                    in_single = !in_single;
                    continue;
                }
                if !in_single {
                    outside_single_quotes.push(c);
                }
            }
            if outside_single_quotes.contains('`')
                || outside_single_quotes.contains("$(")
                || outside_single_quotes.contains("${")
            {
                return false;
            }
        }

        // Block output redirections
        if contains_unquoted(command, '>') {
            return false;
        }

        let segments = split_on_unquoted_separators(command);

        for segment in segments.iter().map(|s| s.trim()) {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }

            let cmd_part = skip_env_assignments(segment);

            let base_cmd = cmd_part
                .split_whitespace()
                .next()
                .unwrap_or("")
                .rsplit('/')
                .next()
                .unwrap_or("");

            if base_cmd.is_empty() {
                continue;
            }

            if self
                .blocked_commands
                .iter()
                .any(|blocked| blocked == base_cmd)
            {
                return false;
            }

            // When workspace_only is set, scan arguments for path escapes.
            if self.workspace_only {
                for arg in cmd_part.split_whitespace().skip(1) {
                    if arg.starts_with('-') {
                        continue;
                    }
                    if Path::new(arg)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                    {
                        return false;
                    }
                    if (arg.starts_with('/') || arg.starts_with("~/"))
                        && !self.is_within_workspace(arg)
                    {
                        return false;
                    }
                }
            }
        }

        segments.iter().any(|s| {
            let s = skip_env_assignments(s.trim());
            s.split_whitespace().next().is_some_and(|w| !w.is_empty())
        })
    }

    /// Check if a file path is allowed (no path traversal, within workspace)
    pub fn is_path_allowed(&self, path: &str) -> bool {
        if path.contains('\0') {
            return false;
        }

        if Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return false;
        }

        let lower = path.to_lowercase();
        if lower.contains("..%2f") || lower.contains("%2f..") {
            return false;
        }

        let expanded = if let Some(stripped) = path.strip_prefix("~/") {
            if let Some(home) = std::env::var("HOME").ok().map(PathBuf::from) {
                home.join(stripped).to_string_lossy().to_string()
            } else {
                path.to_string()
            }
        } else {
            path.to_string()
        };

        if self.workspace_only
            && Path::new(&expanded).is_absolute()
            && !self.is_within_workspace(path)
        {
            return false;
        }

        let expanded_path = Path::new(&expanded);
        for forbidden in &self.forbidden_paths {
            let forbidden_expanded = if let Some(stripped) = forbidden.strip_prefix("~/") {
                if let Some(home) = std::env::var("HOME").ok().map(PathBuf::from) {
                    home.join(stripped).to_string_lossy().to_string()
                } else {
                    forbidden.clone()
                }
            } else {
                forbidden.clone()
            };
            let forbidden_path = Path::new(&forbidden_expanded);
            if expanded_path.starts_with(forbidden_path) {
                return false;
            }
        }

        true
    }

    /// Validate that a resolved path is still inside the workspace.
    pub fn is_resolved_path_allowed(&self, resolved: &Path) -> bool {
        let workspace_root = self
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_dir.clone());
        resolved.starts_with(workspace_root)
    }

    /// Check if autonomy level permits any action at all
    pub fn can_act(&self) -> bool {
        self.autonomy != AutonomyLevel::ReadOnly
    }

    /// Record an action and check if the rate limit has been exceeded.
    pub fn record_action(&self) -> bool {
        let count = self.tracker.record();
        count <= self.max_actions_per_hour as usize
    }

    /// Check if the rate limit would be exceeded without recording.
    pub fn is_rate_limited(&self) -> bool {
        self.tracker.count() >= self.max_actions_per_hour as usize
    }

    /// Return a human-readable error message when a resolved path escapes the workspace.
    pub fn resolved_path_violation_message(&self, resolved: &Path) -> String {
        format!(
            "Path escapes workspace: resolved to '{}' which is outside '{}'",
            resolved.display(),
            self.workspace_dir.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_policy() -> SecurityPolicy {
        SecurityPolicy::default()
    }

    fn readonly_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        }
    }

    fn full_policy() -> SecurityPolicy {
        SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            ..SecurityPolicy::default()
        }
    }

    #[test]
    fn autonomy_default_is_supervised() {
        assert_eq!(AutonomyLevel::default(), AutonomyLevel::Supervised);
    }

    #[test]
    fn autonomy_serde_roundtrip() {
        let json = serde_json::to_string(&AutonomyLevel::Full).unwrap();
        assert_eq!(json, "\"full\"");
        let parsed: AutonomyLevel = serde_json::from_str("\"readonly\"").unwrap();
        assert_eq!(parsed, AutonomyLevel::ReadOnly);
    }

    #[test]
    fn can_act_readonly_false() {
        assert!(!readonly_policy().can_act());
    }

    #[test]
    fn can_act_supervised_true() {
        assert!(default_policy().can_act());
    }

    #[test]
    fn can_act_full_true() {
        assert!(full_policy().can_act());
    }

    #[test]
    fn allowed_commands_basic() {
        let p = default_policy();
        assert!(p.is_command_allowed("ls"));
        assert!(p.is_command_allowed("git status"));
        assert!(p.is_command_allowed("cargo build --release"));
    }

    #[test]
    fn blocked_commands_basic() {
        let p = default_policy();
        assert!(!p.is_command_allowed("rm -rf /"));
        assert!(!p.is_command_allowed("sudo apt install"));
        assert!(!p.is_command_allowed("curl http://evil.com"));
    }

    #[test]
    fn readonly_blocks_all_commands() {
        let p = readonly_policy();
        assert!(!p.is_command_allowed("ls"));
    }

    #[test]
    fn action_tracker_records_actions() {
        let tracker = ActionTracker::new();
        assert_eq!(tracker.record(), 1);
        assert_eq!(tracker.record(), 2);
        assert_eq!(tracker.count(), 2);
    }

    #[test]
    fn record_action_blocks_over_limit() {
        let p = SecurityPolicy {
            max_actions_per_hour: 3,
            ..SecurityPolicy::default()
        };
        assert!(p.record_action());
        assert!(p.record_action());
        assert!(p.record_action());
        assert!(!p.record_action());
    }

    #[test]
    fn relative_paths_allowed() {
        let p = default_policy();
        assert!(p.is_path_allowed("file.txt"));
        assert!(p.is_path_allowed("src/main.rs"));
    }

    #[test]
    fn path_traversal_blocked() {
        let p = default_policy();
        assert!(!p.is_path_allowed("../etc/passwd"));
    }

    #[test]
    fn absolute_paths_blocked_when_workspace_only() {
        let p = default_policy();
        assert!(!p.is_path_allowed("/etc/passwd"));
    }

    #[test]
    fn path_with_null_byte_blocked() {
        let p = default_policy();
        assert!(!p.is_path_allowed("file\0.txt"));
    }

    #[test]
    fn resolved_path_must_be_in_workspace() {
        let p = SecurityPolicy {
            workspace_dir: PathBuf::from("/home/user/project"),
            ..SecurityPolicy::default()
        };
        assert!(p.is_resolved_path_allowed(Path::new("/home/user/project/src/main.rs")));
        assert!(!p.is_resolved_path_allowed(Path::new("/etc/passwd")));
    }

    #[test]
    fn default_policy_has_sane_values() {
        let p = SecurityPolicy::default();
        assert_eq!(p.autonomy, AutonomyLevel::Supervised);
        assert!(p.workspace_only);
        assert!(!p.blocked_commands.is_empty());
    }
}
