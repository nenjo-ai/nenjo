use nenjo::manifest::{AbilityManifest, AgentManifest, DomainManifest};

use crate::scope::{PlatformScope, ScopeResource};

#[derive(Debug, Clone)]
/// Scope-based filter used to limit manifest visibility and mutations for a caller.
pub struct ManifestAccessPolicy {
    caller_scopes: Vec<PlatformScope>,
}

impl ManifestAccessPolicy {
    /// Parse a caller's raw platform scope strings into a policy object.
    pub fn new(caller_scopes: Vec<String>) -> Self {
        Self {
            caller_scopes: caller_scopes.into_iter().map(PlatformScope::from).collect(),
        }
    }

    /// Return the normalized scopes attached to the caller.
    pub fn caller_scopes(&self) -> &[PlatformScope] {
        &self.caller_scopes
    }

    /// Return whether the caller has a scope that satisfies `required`.
    pub fn has_scope(&self, required: PlatformScope) -> bool {
        self.caller_scopes
            .iter()
            .any(|scope| scope.allows(&required))
    }

    /// Return whether the caller can read a resource family.
    pub fn can_read_resource(&self, resource: ScopeResource) -> bool {
        self.has_scope(PlatformScope::read(resource))
    }

    /// Return whether the caller can write a resource family.
    pub fn can_write_resource(&self, resource: ScopeResource) -> bool {
        self.has_scope(PlatformScope::write(resource))
    }

    /// Return whether all required normalized scopes are satisfied.
    pub fn allows_all(&self, required_scopes: &[PlatformScope]) -> bool {
        required_scopes
            .iter()
            .all(|required| self.has_scope(required.clone()))
    }

    /// Return whether all required raw scope strings are satisfied.
    pub fn allows_all_strings(&self, required_scopes: &[String]) -> bool {
        required_scopes
            .iter()
            .cloned()
            .map(PlatformScope::from)
            .all(|required| self.has_scope(required))
    }

    /// Return whether the caller may see or operate on this agent.
    pub fn allows_agent(&self, agent: &AgentManifest) -> bool {
        self.allows_all_strings(&agent.platform_scopes)
    }

    /// Return whether the caller may see or operate on this ability.
    pub fn allows_ability(&self, ability: &AbilityManifest) -> bool {
        self.allows_all_strings(&ability.platform_scopes)
    }

    /// Return whether the caller may see or operate on this domain.
    pub fn allows_domain(&self, domain: &DomainManifest) -> bool {
        self.validate_domain_scopes(&domain.platform_scopes)
    }

    /// Validate that an agent update does not request scopes the caller lacks.
    pub fn validate_agent_scopes(&self, requested_scopes: &[String]) -> bool {
        self.allows_all_strings(requested_scopes)
    }

    /// Validate that an ability update does not request scopes the caller lacks.
    pub fn validate_ability_scopes(&self, requested_scopes: &[String]) -> bool {
        self.allows_all_strings(requested_scopes)
    }

    /// Validate that a domain update does not request scopes the caller lacks.
    pub fn validate_domain_scopes(&self, requested_scopes: &[String]) -> bool {
        self.allows_all_strings(requested_scopes)
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::ManifestAccessPolicy;
    use crate::scope::{PlatformScope, ScopeResource};
    use nenjo::manifest::{
        AbilityManifest, AbilityPromptConfig, AgentManifest, DomainManifest, DomainPromptConfig,
        PromptConfig,
    };

    #[test]
    fn write_scope_implies_read() {
        let policy = ManifestAccessPolicy::new(vec!["projects:write".into()]);
        assert!(policy.has_scope(PlatformScope::read(ScopeResource::Projects)));
        assert!(policy.has_scope(PlatformScope::write(ScopeResource::Projects)));
        assert!(!policy.has_scope(PlatformScope::read(ScopeResource::Agents)));
    }

    #[test]
    fn agent_and_ability_scope_checks_use_platform_scopes() {
        let policy = ManifestAccessPolicy::new(vec!["projects:read".into()]);
        let agent = AgentManifest {
            id: Uuid::new_v4(),
            name: "agent".into(),
            description: None,
            prompt_config: PromptConfig::default(),
            color: None,
            model_id: None,
            domain_ids: vec![],
            platform_scopes: vec!["projects:read".into()],
            mcp_server_ids: vec![],
            ability_ids: vec![],
            prompt_locked: false,
            heartbeat: None,
        };
        let ability = AbilityManifest {
            id: Uuid::new_v4(),
            name: "ability".into(),
            tool_name: "ability".into(),
            path: String::new(),
            display_name: None,
            description: None,
            activation_condition: String::new(),
            prompt_config: AbilityPromptConfig::default(),
            platform_scopes: vec!["projects:read".into()],
            mcp_server_ids: vec![],
            source_type: "native".into(),
            read_only: false,
            metadata: serde_json::Value::Null,
        };
        assert!(policy.allows_agent(&agent));
        assert!(policy.allows_ability(&ability));
    }

    #[test]
    fn domains_require_allowed_manifest_scopes() {
        let policy = ManifestAccessPolicy::new(vec!["projects:read".into()]);
        let allowed_domain = DomainManifest {
            id: Uuid::new_v4(),
            name: "domain".into(),
            path: String::new(),
            display_name: "Domain".into(),
            description: None,
            command: "#domain".into(),
            platform_scopes: vec!["projects:read".into()],
            ability_ids: vec![],
            mcp_server_ids: vec![],
            prompt_config: DomainPromptConfig::default(),
        };
        let denied_domain = DomainManifest {
            id: Uuid::new_v4(),
            name: "domain-2".into(),
            path: String::new(),
            display_name: "Domain 2".into(),
            description: None,
            command: "#domain-2".into(),
            platform_scopes: vec!["projects:write".into()],
            ability_ids: vec![],
            mcp_server_ids: vec![],
            prompt_config: DomainPromptConfig::default(),
        };
        assert!(policy.allows_domain(&allowed_domain));
        assert!(policy.validate_domain_scopes(&["projects:read".into()]));
        assert!(!policy.allows_domain(&denied_domain));
        assert!(!policy.validate_domain_scopes(&["projects:write".into()]));
    }
}
