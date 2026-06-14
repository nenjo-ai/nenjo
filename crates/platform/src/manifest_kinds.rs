//! Shared manifest resource and sensitive-content classification.

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
    /// Library knowledge document resource.
    Document,
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
    /// Return the event/resource classification for this manifest resource kind.
    pub const fn resource_type(self) -> ResourceType {
        match self {
            Self::Agent => ResourceType::Agent,
            Self::Ability => ResourceType::Ability,
            Self::Domain => ResourceType::Domain,
            Self::ContextBlock => ResourceType::ContextBlock,
            Self::Document => ResourceType::Document,
            Self::Project => ResourceType::Project,
            Self::Routine => ResourceType::Routine,
            Self::Model => ResourceType::Model,
            Self::Council => ResourceType::Council,
        }
    }
}

/// Sensitive content envelopes attached to platform resources or runtime commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitiveContentKind {
    /// Agent developer/base prompt content.
    AgentPrompt,
    /// Ability developer prompt content.
    AbilityPrompt,
    /// Domain prompt content.
    DomainPrompt,
    /// Context block template content.
    ContextBlockContent,
    /// Library knowledge document body.
    DocumentContent,
    /// Project task description and acceptance criteria.
    TaskContent,
    /// Project settings sensitive envelope.
    ProjectSettings,
    /// Agent heartbeat instruction envelope.
    HeartbeatInstructions,
    /// Cron routine task envelope.
    RoutineCronTask,
}

impl SensitiveContentKind {
    /// Return the resource classification this sensitive content is attached to, if any.
    pub const fn resource_type(self) -> Option<ResourceType> {
        match self {
            Self::AgentPrompt => Some(ResourceType::Agent),
            Self::AbilityPrompt => Some(ResourceType::Ability),
            Self::DomainPrompt => Some(ResourceType::Domain),
            Self::ContextBlockContent => Some(ResourceType::ContextBlock),
            Self::DocumentContent => Some(ResourceType::Document),
            Self::ProjectSettings => Some(ResourceType::Project),
            Self::HeartbeatInstructions => Some(ResourceType::Agent),
            Self::RoutineCronTask => Some(ResourceType::Routine),
            Self::TaskContent => None,
        }
    }

    /// Return the platform encrypted payload object type for this sensitive content.
    pub const fn encrypted_object_type(self) -> &'static str {
        match self {
            Self::AgentPrompt => "manifest.agent.prompt",
            Self::AbilityPrompt => "manifest.ability.prompt",
            Self::DomainPrompt => "manifest.domain.prompt",
            Self::ContextBlockContent => "manifest.context_block.content",
            Self::DocumentContent => "manifest.document.content",
            Self::TaskContent => "task_content",
            Self::ProjectSettings => "project.settings",
            Self::HeartbeatInstructions => "agent.heartbeat.instructions",
            Self::RoutineCronTask => "routine.cron_task",
        }
    }

    /// Return the content key scope used for this sensitive content.
    pub const fn encrypted_scope(self) -> ContentScope {
        match self {
            Self::AgentPrompt
            | Self::AbilityPrompt
            | Self::DomainPrompt
            | Self::ContextBlockContent
            | Self::DocumentContent
            | Self::TaskContent
            | Self::ProjectSettings
            | Self::HeartbeatInstructions
            | Self::RoutineCronTask => ContentScope::Org,
        }
    }

    /// Parse an encrypted payload `object_type` back into a sensitive content kind.
    pub fn from_encrypted_object_type(object_type: &str) -> Option<Self> {
        match object_type {
            "manifest.agent.prompt" => Some(Self::AgentPrompt),
            "manifest.ability.prompt" => Some(Self::AbilityPrompt),
            "manifest.domain.prompt" => Some(Self::DomainPrompt),
            "manifest.context_block.content" => Some(Self::ContextBlockContent),
            "manifest.document.content" => Some(Self::DocumentContent),
            "task_content" => Some(Self::TaskContent),
            "project.settings" => Some(Self::ProjectSettings),
            "agent.heartbeat.instructions" => Some(Self::HeartbeatInstructions),
            "routine.cron_task" => Some(Self::RoutineCronTask),
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
    use super::{ContentScope, ManifestKind, SensitiveContentKind};
    use nenjo_events::ResourceType;

    #[test]
    fn manifest_kinds_have_resource_types() {
        for (kind, resource_type) in [
            (ManifestKind::Agent, ResourceType::Agent),
            (ManifestKind::Ability, ResourceType::Ability),
            (ManifestKind::Domain, ResourceType::Domain),
            (ManifestKind::ContextBlock, ResourceType::ContextBlock),
            (ManifestKind::Document, ResourceType::Document),
            (ManifestKind::Project, ResourceType::Project),
            (ManifestKind::Routine, ResourceType::Routine),
            (ManifestKind::Model, ResourceType::Model),
            (ManifestKind::Council, ResourceType::Council),
        ] {
            assert_eq!(kind.resource_type(), resource_type);
        }
    }

    #[test]
    fn sensitive_content_kinds_have_stable_object_types_and_org_scope() {
        for (kind, resource_type) in [
            (SensitiveContentKind::AgentPrompt, ResourceType::Agent),
            (SensitiveContentKind::AbilityPrompt, ResourceType::Ability),
            (SensitiveContentKind::DomainPrompt, ResourceType::Domain),
            (
                SensitiveContentKind::ContextBlockContent,
                ResourceType::ContextBlock,
            ),
            (
                SensitiveContentKind::DocumentContent,
                ResourceType::Document,
            ),
            (SensitiveContentKind::ProjectSettings, ResourceType::Project),
            (
                SensitiveContentKind::HeartbeatInstructions,
                ResourceType::Agent,
            ),
            (SensitiveContentKind::RoutineCronTask, ResourceType::Routine),
        ] {
            let object_type = kind.encrypted_object_type();
            assert_eq!(kind.encrypted_scope(), ContentScope::Org);
            assert_eq!(
                SensitiveContentKind::from_encrypted_object_type(object_type),
                Some(kind)
            );
            assert!(kind.matches_resource_type(resource_type));
        }
    }

    #[test]
    fn task_content_is_org_scoped_without_manifest_resource_type() {
        assert_eq!(
            SensitiveContentKind::TaskContent.encrypted_object_type(),
            "task_content"
        );
        assert_eq!(
            SensitiveContentKind::TaskContent.encrypted_scope(),
            ContentScope::Org
        );
        assert_eq!(SensitiveContentKind::TaskContent.resource_type(), None);
    }
}
