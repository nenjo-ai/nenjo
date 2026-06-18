use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{PackageError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageAdapter {
    /// Native Nenjo package catalog and descriptor files.
    NenjoPackages,
    /// Claude marketplace style packages.
    ClaudeMarketplace,
    /// Codex plugin directories.
    CodexPlugin,
}

impl PackageAdapter {
    /// Parse a stable adapter identifier.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "nenjo_packages" => Ok(Self::NenjoPackages),
            "claude_marketplace" => Ok(Self::ClaudeMarketplace),
            "codex_plugin" => Ok(Self::CodexPlugin),
            other => Err(PackageError::Message(format!(
                "unsupported package adapter '{other}'"
            ))),
        }
    }

    /// Return the stable adapter identifier used in serialized metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NenjoPackages => "nenjo_packages",
            Self::ClaudeMarketplace => "claude_marketplace",
            Self::CodexPlugin => "codex_plugin",
        }
    }
}

impl FromStr for PackageAdapter {
    type Err = PackageError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Kind of resource a package installs.
pub enum PackageKind {
    /// Model provider configuration manifest.
    Model,
    /// Agent manifest.
    Agent,
    /// Ability/tool manifest.
    Ability,
    /// Domain manifest.
    Domain,
    /// Context block manifest.
    ContextBlock,
    /// Knowledge source or knowledge reference manifest.
    Knowledge,
    /// Codex-style skill manifest.
    Skill,
    /// Plugin manifest.
    Plugin,
    /// User-facing command manifest.
    Command,
    /// Runtime hook manifest.
    Hook,
    /// Native Nenjo typed script tool manifest.
    ScriptTool,
    /// MCP server manifest.
    McpServer,
    /// Routine manifest.
    Routine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Supported manifest schema versions.
pub enum ManifestSchemaVersion {
    /// Initial package and resource schema version.
    V1,
}

impl ManifestSchemaVersion {
    /// Parse a schema version suffix such as `v1`.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "v1" => Ok(Self::V1),
            other => Err(PackageError::invalid_schema(
                other,
                "unsupported manifest schema version",
            )),
        }
    }

    /// Return the schema version suffix used in manifest schema strings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
        }
    }
}

impl FromStr for ManifestSchemaVersion {
    type Err = PackageError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Parsed `nenjo.<resource>.<version>` schema for a resource manifest.
pub struct ResourceSchema {
    /// Resource kind declared by the schema.
    pub kind: PackageKind,
    /// Schema version declared by the schema.
    pub version: ManifestSchemaVersion,
}

impl ResourceSchema {
    /// Parse a resource schema such as `nenjo.agent.v1`.
    pub fn parse(schema: &str) -> Result<Self> {
        let Some(rest) = schema.strip_prefix("nenjo.") else {
            return Err(PackageError::invalid_schema(
                schema,
                "resource schema must start with 'nenjo.'",
            ));
        };
        let Some((kind, version)) = rest.rsplit_once('.') else {
            return Err(PackageError::invalid_schema(
                schema,
                "resource schema must include a version suffix",
            ));
        };
        let version = ManifestSchemaVersion::parse(version).map_err(|error| {
            error.context(format!(
                "resource schema '{schema}' has unsupported version"
            ))
        })?;
        let kind = PackageKind::parse_kind(kind)?;
        Ok(Self { kind, version })
    }
}

impl FromStr for ResourceSchema {
    type Err = PackageError;

    fn from_str(schema: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(schema)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Parsed schema for top-level package files.
pub enum PackageFileSchema {
    /// Catalog file schema such as `nenjo.packages.v1`.
    Catalog(ManifestSchemaVersion),
    /// Package descriptor schema such as `nenjo.package.v1`.
    Descriptor(ManifestSchemaVersion),
}

impl PackageFileSchema {
    /// Parse and validate a catalog schema string.
    pub fn parse_catalog(schema: &str) -> Result<Self> {
        parse_package_file_schema(schema, "packages").map(Self::Catalog)
    }

    /// Parse and validate a package descriptor schema string.
    pub fn parse_descriptor(schema: &str) -> Result<Self> {
        parse_package_file_schema(schema, "package").map(Self::Descriptor)
    }

    /// Return the package file schema version.
    pub fn version(self) -> ManifestSchemaVersion {
        match self {
            Self::Catalog(version) | Self::Descriptor(version) => version,
        }
    }
}

pub(crate) fn parse_package_file_schema(
    schema: &str,
    expected_kind: &str,
) -> Result<ManifestSchemaVersion> {
    let Some(rest) = schema.strip_prefix("nenjo.") else {
        return Err(PackageError::invalid_schema(
            schema,
            "package schema must start with 'nenjo.'",
        ));
    };
    let Some((kind, version)) = rest.rsplit_once('.') else {
        return Err(PackageError::invalid_schema(
            schema,
            "package schema must include a version suffix",
        ));
    };
    if kind != expected_kind {
        return Err(PackageError::invalid_schema(
            schema,
            format!("expected schema 'nenjo.{expected_kind}.*'"),
        ));
    }
    ManifestSchemaVersion::parse(version).map_err(|error| {
        error.context(format!("package schema '{schema}' has unsupported version"))
    })
}

impl PackageKind {
    /// Parse the resource kind from a full resource schema string.
    pub fn parse_schema(schema: &str) -> Result<Self> {
        Ok(ResourceSchema::parse(schema)?.kind)
    }

    fn parse_kind(kind: &str) -> Result<Self> {
        match kind {
            "model" => Ok(Self::Model),
            "agent" => Ok(Self::Agent),
            "ability" => Ok(Self::Ability),
            "domain" => Ok(Self::Domain),
            "context_block" => Ok(Self::ContextBlock),
            "knowledge" | "knowledge_ref" => Ok(Self::Knowledge),
            "skill" => Ok(Self::Skill),
            "plugin" => Ok(Self::Plugin),
            "command" => Ok(Self::Command),
            "hook" => Ok(Self::Hook),
            "script_tool" => Ok(Self::ScriptTool),
            "mcp_server" => Ok(Self::McpServer),
            "routine" => Ok(Self::Routine),
            other => Err(PackageError::invalid_schema(
                other,
                "unsupported package resource schema",
            )),
        }
    }

    /// Return the stable package kind identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Agent => "agent",
            Self::Ability => "ability",
            Self::Domain => "domain",
            Self::ContextBlock => "context_block",
            Self::Knowledge => "knowledge",
            Self::Skill => "skill",
            Self::Plugin => "plugin",
            Self::Command => "command",
            Self::Hook => "hook",
            Self::ScriptTool => "script_tool",
            Self::McpServer => "mcp_server",
            Self::Routine => "routine",
        }
    }
}

impl FromStr for PackageKind {
    type Err = PackageError;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse_kind(value)
    }
}
