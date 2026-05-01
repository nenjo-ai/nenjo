use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Operation component of a platform scope string.
pub enum ScopeAction {
    /// Read-only access.
    Read,
    /// Write access, which also implies read access for the same resource.
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Resource family component of a platform scope string.
pub enum ScopeResource {
    /// Agent manifests.
    Agents,
    /// Ability manifests.
    Abilities,
    /// Domain manifests.
    Domains,
    /// Project manifests and project-scoped data.
    Projects,
    /// Routine manifests.
    Routines,
    /// Model manifests.
    Models,
    /// Council manifests.
    Councils,
    /// Context block manifests.
    ContextBlocks,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Parsed platform scope with explicit handling for unknown strings.
pub enum PlatformScope {
    /// Scope recognized by the platform parser.
    Known {
        /// Resource family covered by the scope.
        resource: ScopeResource,
        /// Operation allowed for the resource family.
        action: ScopeAction,
    },
    /// Scope string preserved verbatim when this crate does not know its shape.
    Unknown(String),
}

impl PlatformScope {
    /// Build a read scope for a resource family.
    pub fn read(resource: ScopeResource) -> Self {
        Self::Known {
            resource,
            action: ScopeAction::Read,
        }
    }

    /// Build a write scope for a resource family.
    pub fn write(resource: ScopeResource) -> Self {
        Self::Known {
            resource,
            action: ScopeAction::Write,
        }
    }

    /// Parse a raw scope string into a normalized scope value.
    pub fn parse(scope: &str) -> Self {
        match scope {
            "agents:read" => Self::read(ScopeResource::Agents),
            "agents:write" => Self::write(ScopeResource::Agents),
            "abilities:read" => Self::read(ScopeResource::Abilities),
            "abilities:write" => Self::write(ScopeResource::Abilities),
            "domains:read" => Self::read(ScopeResource::Domains),
            "domains:write" => Self::write(ScopeResource::Domains),
            "projects:read" => Self::read(ScopeResource::Projects),
            "projects:write" => Self::write(ScopeResource::Projects),
            "routines:read" => Self::read(ScopeResource::Routines),
            "routines:write" => Self::write(ScopeResource::Routines),
            "models:read" => Self::read(ScopeResource::Models),
            "models:write" => Self::write(ScopeResource::Models),
            "councils:read" => Self::read(ScopeResource::Councils),
            "councils:write" => Self::write(ScopeResource::Councils),
            "context_blocks:read" => Self::read(ScopeResource::ContextBlocks),
            "context_blocks:write" => Self::write(ScopeResource::ContextBlocks),
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Return whether `self` satisfies the required scope.
    ///
    /// Write scopes imply read scopes for the same resource family.
    pub fn allows(&self, required: &Self) -> bool {
        match (self, required) {
            (
                Self::Known {
                    resource: lhs_resource,
                    action: ScopeAction::Write,
                },
                Self::Known {
                    resource: rhs_resource,
                    action: ScopeAction::Read,
                },
            ) if lhs_resource == rhs_resource => true,
            (Self::Known { .. }, Self::Known { .. }) => self == required,
            (Self::Unknown(lhs), Self::Unknown(rhs)) => lhs == rhs,
            _ => false,
        }
    }
}

impl From<&str> for PlatformScope {
    fn from(value: &str) -> Self {
        Self::parse(value)
    }
}

impl From<String> for PlatformScope {
    fn from(value: String) -> Self {
        Self::parse(&value)
    }
}

impl fmt::Display for PlatformScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known { resource, action } => write!(
                f,
                "{}:{}",
                match resource {
                    ScopeResource::Agents => "agents",
                    ScopeResource::Abilities => "abilities",
                    ScopeResource::Domains => "domains",
                    ScopeResource::Projects => "projects",
                    ScopeResource::Routines => "routines",
                    ScopeResource::Models => "models",
                    ScopeResource::Councils => "councils",
                    ScopeResource::ContextBlocks => "context_blocks",
                },
                match action {
                    ScopeAction::Read => "read",
                    ScopeAction::Write => "write",
                }
            ),
            Self::Unknown(scope) => f.write_str(scope),
        }
    }
}
