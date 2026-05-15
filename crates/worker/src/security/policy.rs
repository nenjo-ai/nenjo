//! Re-exports worker tool security types and provides the
//! worker-specific config mapping.

pub use crate::tools::security::{ActionTracker, AutonomyLevel, CommandRiskLevel, SecurityPolicy};

use std::path::Path;

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

/// Build a [`SecurityPolicy`] from worker config sections.
///
/// This is a standalone function to keep config mapping outside the generic
/// worker tool policy type.
/// Callers should use this instead of `SecurityPolicy::from_config`.
pub fn security_policy_from_config(
    autonomy_config: &crate::config::AutonomyConfig,
    workspace_dir: &Path,
) -> SecurityPolicy {
    SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        workspace_dir: workspace_dir.to_path_buf(),
        workspace_only: autonomy_config.workspace_only,
        allowed_runtime_roots: SecurityPolicy::with_workspace_dir(workspace_dir.to_path_buf())
            .allowed_runtime_roots,
        blocked_commands: autonomy_config.blocked_commands.clone(),
        forbidden_paths: autonomy_config.forbidden_paths.clone(),
        max_actions_per_hour: autonomy_config.max_actions_per_hour,
        max_cost_per_day_cents: autonomy_config.max_cost_per_day_cents,
        require_approval_for_medium_risk: autonomy_config.require_approval_for_medium_risk,
        block_high_risk_commands: autonomy_config.block_high_risk_commands,
        tracker: ActionTracker::new(),
        forwarded_env: collect_forwarded_env(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_config_maps_all_fields() {
        let autonomy_config = crate::config::AutonomyConfig {
            workspace_only: false,
            blocked_commands: vec!["docker".into()],
            forbidden_paths: vec!["/secret".into()],
            max_actions_per_hour: 100,
            max_cost_per_day_cents: 1000,
            require_approval_for_medium_risk: false,
            block_high_risk_commands: false,
        };
        let workspace = PathBuf::from("/tmp/test-workspace");
        let policy = security_policy_from_config(&autonomy_config, &workspace);

        assert_eq!(policy.autonomy, AutonomyLevel::Full);
        assert!(!policy.workspace_only);
        assert_eq!(policy.blocked_commands, vec!["docker"]);
        assert_eq!(policy.forbidden_paths, vec!["/secret"]);
        assert_eq!(policy.max_actions_per_hour, 100);
        assert_eq!(policy.max_cost_per_day_cents, 1000);
        assert!(!policy.require_approval_for_medium_risk);
        assert!(!policy.block_high_risk_commands);
        assert_eq!(policy.workspace_dir, PathBuf::from("/tmp/test-workspace"));
    }

    #[test]
    fn from_config_creates_fresh_tracker() {
        let autonomy_config = crate::config::AutonomyConfig {
            workspace_only: false,
            blocked_commands: vec![],
            forbidden_paths: vec![],
            max_actions_per_hour: 10,
            max_cost_per_day_cents: 100,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
        };
        let workspace = PathBuf::from("/tmp/test");
        let policy = security_policy_from_config(&autonomy_config, &workspace);
        assert_eq!(policy.tracker.count(), 0);
        assert!(!policy.is_rate_limited());
    }
}
