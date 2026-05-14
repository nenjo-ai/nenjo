//! Worker capability types.
//!
//! A [`Capability`] represents a category of commands that a worker can handle.
//! Workers declare their capabilities on connect; the backend uses them to route
//! commands to the correct NATS subject and the auth callout uses them to scope
//! subscribe permissions.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapabilityLane {
    Work,
    Broadcast,
}

/// A capability that a worker can handle.
///
/// Each capability maps to a set of [`Command`](crate::Command) variants.
/// Workers subscribe to queue, targeted, and broadcast local subjects for the
/// capabilities they support inside their org NATS account.
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

    pub const WORK_LANE: &[Capability] = &[Capability::Chat, Capability::Task, Capability::Cron];

    pub const BROADCAST_LANE: &[Capability] =
        &[Capability::Manifest, Capability::Repo, Capability::Ping];

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

    /// The NATS command lane used for this capability.
    pub fn lane(&self) -> CapabilityLane {
        match self {
            Capability::Chat | Capability::Task | Capability::Cron => CapabilityLane::Work,
            Capability::Manifest | Capability::Repo | Capability::Ping => CapabilityLane::Broadcast,
        }
    }

    pub fn is_work_lane(&self) -> bool {
        self.lane() == CapabilityLane::Work
    }

    pub fn is_broadcast_lane(&self) -> bool {
        self.lane() == CapabilityLane::Broadcast
    }

    /// Normalize capability subscriptions for a worker.
    ///
    /// Empty means all capabilities. Ping is always included so the backend can
    /// health-check every worker regardless of its configured work lane.
    pub fn effective_worker_subscriptions(capabilities: &[Capability]) -> Vec<Capability> {
        let mut capabilities = if capabilities.is_empty() {
            Capability::ALL.to_vec()
        } else {
            capabilities.to_vec()
        };

        if !capabilities.contains(&Capability::Ping) {
            capabilities.push(Capability::Ping);
        }

        Capability::ALL
            .iter()
            .copied()
            .filter(|capability| capabilities.contains(capability))
            .collect()
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
    fn from_str_error() {
        assert!("bogus".parse::<Capability>().is_err());
    }

    #[test]
    fn lane_split_is_disjoint() {
        assert_eq!(Capability::Chat.lane(), CapabilityLane::Work);
        assert_eq!(Capability::Task.lane(), CapabilityLane::Work);
        assert_eq!(Capability::Cron.lane(), CapabilityLane::Work);
        assert_eq!(Capability::Manifest.lane(), CapabilityLane::Broadcast);
        assert_eq!(Capability::Repo.lane(), CapabilityLane::Broadcast);
        assert_eq!(Capability::Ping.lane(), CapabilityLane::Broadcast);

        assert_eq!(
            Capability::WORK_LANE
                .iter()
                .chain(Capability::BROADCAST_LANE.iter())
                .copied()
                .collect::<Vec<_>>(),
            Capability::ALL
        );
    }

    #[test]
    fn effective_worker_subscriptions_empty_means_all() {
        assert_eq!(
            Capability::effective_worker_subscriptions(&[]),
            Capability::ALL.to_vec()
        );
    }

    #[test]
    fn effective_worker_subscriptions_adds_ping() {
        assert_eq!(
            Capability::effective_worker_subscriptions(&[Capability::Chat]),
            vec![Capability::Chat, Capability::Ping]
        );
    }
}
