//! Worker capability types.
//!
//! A [`Capability`] represents a category of commands that a worker can handle.
//! Workers declare their capabilities on connect; the backend uses them to route
//! commands to the correct NATS subject and the auth callout uses them to scope
//! subscribe permissions.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A capability that a worker can handle.
///
/// Each capability maps to a set of [`Command`](crate::Command) variants.
/// Workers subscribe to `agent.requests.<user_id>.<capability>` for each
/// capability they support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Chat commands: message, domain enter/exit, cancel, session delete.
    Chat,
    /// Task execution: execute, cancel, pause, resume.
    Task,
    /// Cron scheduling: enable, disable, trigger.
    Cron,
    /// Manifest updates: resource created/updated/deleted.
    Manifest,
    /// Repository operations: sync, unsync.
    Repo,
    /// Health check ping — non-exclusive, all workers respond.
    Ping,
}

impl Capability {
    /// All defined capabilities.
    pub const ALL: &[Capability] = &[
        Capability::Chat,
        Capability::Task,
        Capability::Cron,
        Capability::Manifest,
        Capability::Repo,
        Capability::Ping,
    ];

    /// The NATS subject segment for this capability.
    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::Chat => "chat",
            Capability::Task => "task",
            Capability::Cron => "cron",
            Capability::Manifest => "manifest",
            Capability::Repo => "repo",
            Capability::Ping => "ping",
        }
    }

    /// Map a command `type` tag (e.g. `"chat.message"`) to its capability.
    ///
    /// Returns `None` for unrecognized command types.
    pub fn from_command_type(type_tag: &str) -> Option<Capability> {
        let prefix = type_tag.split('.').next()?;
        match prefix {
            "chat" => Some(Capability::Chat),
            "task" | "execution" => Some(Capability::Task),
            "cron" => Some(Capability::Cron),
            "manifest" => Some(Capability::Manifest),
            "repo" => Some(Capability::Repo),
            "worker" => Some(Capability::Ping),
            _ => None,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Capability {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "chat" => Ok(Capability::Chat),
            "task" => Ok(Capability::Task),
            "cron" => Ok(Capability::Cron),
            "manifest" => Ok(Capability::Manifest),
            "repo" => Ok(Capability::Repo),
            "ping" => Ok(Capability::Ping),
            _ => Err(format!("unknown capability: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_display_roundtrip() {
        for cap in Capability::ALL {
            let s = cap.to_string();
            let parsed: Capability = s.parse().unwrap();
            assert_eq!(*cap, parsed);
        }
    }

    #[test]
    fn capability_serde_roundtrip() {
        for cap in Capability::ALL {
            let json = serde_json::to_string(cap).unwrap();
            let parsed: Capability = serde_json::from_str(&json).unwrap();
            assert_eq!(*cap, parsed);
        }
    }

    #[test]
    fn from_command_type_mapping() {
        assert_eq!(
            Capability::from_command_type("chat.message"),
            Some(Capability::Chat)
        );
        assert_eq!(
            Capability::from_command_type("chat.domain_enter"),
            Some(Capability::Chat)
        );
        assert_eq!(
            Capability::from_command_type("chat.cancel"),
            Some(Capability::Chat)
        );
        assert_eq!(
            Capability::from_command_type("task.execute"),
            Some(Capability::Task)
        );
        assert_eq!(
            Capability::from_command_type("execution.cancel"),
            Some(Capability::Task)
        );
        assert_eq!(
            Capability::from_command_type("execution.pause"),
            Some(Capability::Task)
        );
        assert_eq!(
            Capability::from_command_type("cron.enable"),
            Some(Capability::Cron)
        );
        assert_eq!(
            Capability::from_command_type("cron.trigger"),
            Some(Capability::Cron)
        );
        assert_eq!(
            Capability::from_command_type("manifest.changed"),
            Some(Capability::Manifest)
        );
        assert_eq!(
            Capability::from_command_type("repo.sync"),
            Some(Capability::Repo)
        );
        assert_eq!(Capability::from_command_type("unknown.thing"), None);
    }

    #[test]
    fn from_str_error() {
        assert!("bogus".parse::<Capability>().is_err());
    }
}
