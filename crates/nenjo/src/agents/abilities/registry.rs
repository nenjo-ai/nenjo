//! Immutable lookup of the abilities assigned to one agent.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

use crate::manifest::AbilityManifest;

/// Own manifests in assignment order and index their exact, model-facing names.
#[derive(Debug, Clone)]
pub(super) struct AbilityRegistry {
    entries: Vec<AbilityManifest>,
    by_id: BTreeMap<String, usize>,
}

impl AbilityRegistry {
    /// Reject duplicate names before either broker tool can expose the registry.
    pub(super) fn new(abilities: &[AbilityManifest]) -> Result<Self> {
        let mut entries: Vec<AbilityManifest> = Vec::with_capacity(abilities.len());
        let mut by_id: BTreeMap<String, usize> = BTreeMap::new();
        for ability in abilities {
            let ability_id = ability.name.clone();
            if let Some(existing) = by_id.get(&ability_id) {
                let existing = &entries[*existing];
                bail!(
                    "duplicate ability_id '{ability_id}' for abilities '{}' and '{}'",
                    existing.name,
                    ability.name
                );
            }
            by_id.insert(ability_id, entries.len());
            entries.push(ability.clone());
        }
        Ok(Self { entries, by_id })
    }

    /// Resolve only assigned names; slugs and paths are not alternate identities.
    pub(super) fn get(&self, ability_id: &str) -> Option<&AbilityManifest> {
        self.by_id
            .get(ability_id)
            .and_then(|index| self.entries.get(*index))
    }

    /// Iterate in manifest assignment order without exposing mutable registry state.
    pub(super) fn iter(&self) -> impl Iterator<Item = &AbilityManifest> {
        self.entries.iter()
    }
}
