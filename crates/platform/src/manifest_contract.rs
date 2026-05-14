//! Shared manifest resource classification used by worker and platform surfaces.

use nenjo_events::{EncryptedPayload, ResourceType};

/// Scope used when selecting between user-private and org-shared encrypted content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentScope {
    /// User-private account content encrypted with an ACK.
    User,
    /// Org-shared content encrypted with an OCK.
    Org,
}

impl ContentScope {
    /// Infer the content scope from an encrypted payload's declared scope marker.
    pub fn from_payload(payload: &EncryptedPayload) -> Self {
        if payload.encryption_scope.as_deref() == Some("org") {
            Self::Org
        } else {
            Self::User
        }
    }

    /// Return the serialized `encryption_scope` value used on payloads.
    pub const fn encryption_scope_value(self) -> Option<&'static str> {
        match self {
            Self::User => None,
            Self::Org => Some("org"),
        }
    }
}

/// Canonical manifest resource kinds used across the platform and worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestKind {
    /// Agent manifest resource.
    Agent,
    /// Ability manifest resource.
    Ability,
    /// Domain manifest resource.
    Domain,
    /// Context block manifest resource.
    ContextBlock,
    /// Library knowledge item content resource.
    ProjectDocument,
    /// Project task content resource.
    Task,
    /// Project manifest resource.
    Project,
    /// Routine manifest resource.
    Routine,
    /// Model manifest resource.
    Model,
    /// Council manifest resource.
    Council,
}

impl ManifestKind {
    /// Return the event/resource classification when this kind participates in manifest events.
    pub const fn resource_type(self) -> Option<ResourceType> {
        match self {
            Self::Agent => Some(ResourceType::Agent),
            Self::Ability => Some(ResourceType::Ability),
            Self::Domain => Some(ResourceType::Domain),
            Self::ContextBlock => Some(ResourceType::ContextBlock),
            Self::ProjectDocument => Some(ResourceType::Document),
            Self::Project => Some(ResourceType::Project),
            Self::Routine => Some(ResourceType::Routine),
            Self::Model => Some(ResourceType::Model),
            Self::Council => Some(ResourceType::Council),
            Self::Task => None,
        }
    }

    /// Return the encrypted payload object type, if this manifest kind carries encrypted content.
    pub const fn encrypted_object_type(self) -> Option<&'static str> {
        match self {
            Self::Agent => Some("manifest.agent.prompt"),
            Self::Ability => Some("manifest.ability.prompt"),
            Self::Domain => Some("manifest.domain.prompt"),
            Self::ContextBlock => Some("manifest.context_block.content"),
            Self::ProjectDocument => Some("manifest.document.content"),
            Self::Task => Some("task_content"),
            Self::Project | Self::Routine | Self::Model | Self::Council => None,
        }
    }

    /// Return the encrypted payload scope, if this manifest kind carries encrypted content.
    pub const fn encrypted_scope(self) -> Option<ContentScope> {
        match self {
            Self::Agent
            | Self::Ability
            | Self::Domain
            | Self::ContextBlock
            | Self::ProjectDocument
            | Self::Task => Some(ContentScope::Org),
            Self::Project | Self::Routine | Self::Model | Self::Council => None,
        }
    }

    /// Parse an encrypted payload `object_type` back into a canonical manifest kind.
    pub fn from_encrypted_object_type(object_type: &str) -> Option<Self> {
        match object_type {
            "manifest.agent.prompt" => Some(Self::Agent),
            "manifest.ability.prompt" => Some(Self::Ability),
            "manifest.domain.prompt" => Some(Self::Domain),
            "manifest.context_block.content" => Some(Self::ContextBlock),
            "manifest.document.content" => Some(Self::ProjectDocument),
            "task_content" => Some(Self::Task),
            _ => None,
        }
    }

    /// Return true when this encrypted object type is valid for the given event resource type.
    pub fn matches_resource_type(self, resource_type: ResourceType) -> bool {
        self.resource_type() == Some(resource_type)
    }
}

#[cfg(test)]
mod tests {
    use super::{ContentScope, ManifestKind};
    use nenjo_events::ResourceType;

    #[test]
    fn encrypted_manifest_kinds_have_stable_object_types_and_org_scope() {
        for (kind, resource_type) in [
            (ManifestKind::Agent, ResourceType::Agent),
            (ManifestKind::Ability, ResourceType::Ability),
            (ManifestKind::Domain, ResourceType::Domain),
            (ManifestKind::ContextBlock, ResourceType::ContextBlock),
            (ManifestKind::ProjectDocument, ResourceType::Document),
        ] {
            let object_type = kind.encrypted_object_type().expect("encrypted object type");
            assert_eq!(kind.encrypted_scope(), Some(ContentScope::Org));
            assert_eq!(
                ManifestKind::from_encrypted_object_type(object_type),
                Some(kind)
            );
            assert!(kind.matches_resource_type(resource_type));
        }
    }

    #[test]
    fn task_content_is_org_scoped_without_manifest_resource_type() {
        assert_eq!(
            ManifestKind::Task.encrypted_object_type(),
            Some("task_content")
        );
        assert_eq!(
            ManifestKind::Task.encrypted_scope(),
            Some(ContentScope::Org)
        );
        assert_eq!(ManifestKind::Task.resource_type(), None);
    }
}
