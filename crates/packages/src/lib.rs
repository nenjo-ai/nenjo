//! Package catalog and manifest primitives for Nenjo package registries.
//!
//! `nenjo-packages` handles the registry-facing package format: catalog files,
//! package descriptors, resource manifests, dependency graphs, GitHub fetching,
//! lockfile records, and small validation helpers. It intentionally keeps the
//! format-level logic independent from platform persistence so workers and
//! platform services can share the same package parsing rules.

macro_rules! bail {
    ($($arg:tt)*) => {
        return Err(crate::PackageError::Message(format!($($arg)*)))
    };
}

mod claude_plugin;
mod command_content;
mod error;
mod github;
mod identity;
mod local;
mod module;
mod package;
mod reader;
mod resolved;
mod schema;
pub mod validation;

pub use claude_plugin::{
    ClaudeMarketplaceManifest, ClaudeMarketplacePlugin, ClaudePluginCommand,
    ClaudePluginDependency, ClaudePluginHook, ClaudePluginManifest, ClaudePluginMcpServer,
    ClaudePluginResource, ClaudePluginSkill, ClaudePluginUnsupportedComponent,
    claude_command_resource_manifest, claude_hook_resource_manifest,
    claude_mcp_server_resource_manifest, claude_plugin_resource_manifest, claude_plugin_resources,
    claude_skill_resource_manifest, detect_unsupported_claude_plugin_components,
    parse_claude_marketplace_manifest, parse_claude_plugin_command,
    parse_claude_plugin_dependencies, parse_claude_plugin_hooks, parse_claude_plugin_manifest,
    parse_claude_plugin_mcp_servers, parse_claude_plugin_skill,
};
pub use error::{PackageError, Result};
pub use github::{GitHubFetcher, GitHubSource};
pub use identity::{
    PackageLock, PackageLockResource, PackageResourceInstanceKey, PackageResourceLogicalKey,
    package_entry_path, package_module_source_path, parse_json_or_yaml, parse_json_or_yaml_as,
    sha256_hex, validate_package_name, validate_package_slug, validate_source_path,
    version_satisfies,
};
pub(crate) use identity::{validate_relative_module_import_path, validate_resource_name};
pub use local::LocalPackageResolver;
pub use module::{
    ModuleBundle, ModuleImport, ModuleIndexManifest, PackageMediaRequirement, ResourceManifest,
};
pub(crate) use module::{
    complete_package_resource_manifest, module_file_schema, module_reference_is_directory,
    normalize_module_reference, parse_module_file,
};
pub use package::{
    ModulePackageManifest, PackageCatalog, PackageDescriptor, PackageEntry, PackageModule,
    PackageRegistryManifest, PackageRegistryReference, PackageRegistrySource, ResourceDependency,
};
pub use reader::{
    PackageFileReader, resolve_module_package_graph_from_reader,
    resolve_module_package_manifest_from_reader,
};
pub use resolved::{
    ResolvedModule, ResolvedPackage, ResolvedPackageFile, ResolvedPackageGraph, ResolvedResource,
    ResolvedResourceGraph,
};
pub use schema::{
    ManifestSchemaVersion, PackageAdapter, PackageFileSchema, PackageKind, ResourceSchema,
};
#[cfg(test)]
mod tests;
