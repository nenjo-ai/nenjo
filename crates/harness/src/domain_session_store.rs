//! Local persistence for active domain sessions that must survive worker restarts.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

/// Serialized representation of one active domain session on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedDomainSession {
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub agent_id: Uuid,
    pub domain_command: String,
    pub turn_number: u32,
}

/// Filesystem-backed store for persisted domain sessions.
pub struct DomainSessionStore {
    root: PathBuf,
}

impl DomainSessionStore {
    /// Create a new store rooted under the worker workspace.
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            root: workspace_dir.join("domain_sessions"),
        }
    }

    /// Persist or overwrite one active domain session atomically.
    pub fn save(&self, session: &PersistedDomainSession) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path(session.session_id);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(session)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Delete one persisted domain session if it exists.
    pub fn delete(&self, session_id: Uuid) -> Result<()> {
        let path = self.path(session_id);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Load one persisted session by ID, returning `Ok(None)` if it does not exist.
    pub fn load(&self, session_id: Uuid) -> Result<Option<PersistedDomainSession>> {
        let path = self.path(session_id);
        match std::fs::read_to_string(&path) {
            Ok(data) => Ok(Some(serde_json::from_str::<PersistedDomainSession>(&data)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load every readable persisted session, skipping corrupt files with a warning.
    pub fn load_all(&self) -> Result<Vec<PersistedDomainSession>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut sessions = Vec::new();
        let mut entries: Vec<_> = std::fs::read_dir(&self.root)?.flatten().collect();
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(data) => match serde_json::from_str::<PersistedDomainSession>(&data) {
                    Ok(session) => sessions.push(session),
                    Err(error) => {
                        warn!(file = %path.display(), error = %error, "Failed to parse persisted domain session");
                    }
                },
                Err(error) => {
                    warn!(file = %path.display(), error = %error, "Failed to read persisted domain session");
                }
            }
        }
        Ok(sessions)
    }

    fn path(&self, session_id: Uuid) -> PathBuf {
        self.root.join(format!("{session_id}.json"))
    }
}
