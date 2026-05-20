use std::path::Path;

use anyhow::{Context, Result};
use nenjo_packages::{LocalPackageResolver, PackageKind, ResolvedPackageGraph};

use crate::registry::{PackageRegistry, RegistryPackageResolver};
use crate::source::DefaultPackageSourceFetcher;

/// Package install plan produced from a resolved package graph.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub(crate) graph: ResolvedPackageGraph,
    pub(crate) package_order: Vec<String>,
}

impl InstallPlan {
    /// Resolve a local package repository into an install plan.
    pub fn from_local_repository(root: impl AsRef<Path>, package: &str) -> Result<Self> {
        let resolver = LocalPackageResolver::new(root.as_ref());
        let graph = resolver
            .resolve_package_graph(package)
            .with_context(|| format!("failed to resolve local package {package}"))?;
        Self::from_graph(graph)
    }

    /// Build an install plan from an already-resolved package graph.
    pub fn from_graph(graph: ResolvedPackageGraph) -> Result<Self> {
        let package_order = if graph.root_package.is_empty() {
            Vec::new()
        } else {
            graph.topo_order()?
        };
        Ok(Self {
            graph,
            package_order,
        })
    }

    /// Resolve a local-source registry package into an install plan.
    pub fn from_registry_local_sources<R>(
        resolver: &RegistryPackageResolver<R>,
        package: &str,
        requirement: &str,
    ) -> Result<Self>
    where
        R: PackageRegistry,
    {
        let graph = resolver.resolve_local_sources(package, requirement)?;
        Self::from_graph(graph)
    }

    /// Resolve a registry package through the default source fetcher.
    pub fn from_registry<R>(
        resolver: &RegistryPackageResolver<R>,
        package: &str,
        requirement: &str,
    ) -> Result<Self>
    where
        R: PackageRegistry,
    {
        let graph = resolver.resolve_with_fetcher(
            package,
            requirement,
            &DefaultPackageSourceFetcher::new(),
        )?;
        Self::from_graph(graph)
    }

    /// Return dependency-first packages in install order.
    pub fn packages(&self) -> impl Iterator<Item = PlannedPackage<'_>> {
        self.package_order.iter().map(|name| {
            let package = &self.graph.packages[name];
            PlannedPackage {
                name,
                version: &package.version,
                modules: package
                    .modules
                    .iter()
                    .filter(|(key, module)| *key == &module.key())
                    .map(|(_, module)| PlannedModule {
                        path: &module.path,
                        source_path: &module.source_path,
                        schema: module.schema(),
                        kind: module.kind,
                        name: module.name(),
                    })
                    .collect(),
            }
        })
    }

    /// Return the underlying resolved graph.
    pub fn graph(&self) -> &ResolvedPackageGraph {
        &self.graph
    }
}

/// Package entry in an install plan.
#[derive(Debug, Clone)]
pub struct PlannedPackage<'a> {
    /// Package name.
    pub name: &'a str,
    /// Resolved package version.
    pub version: &'a str,
    /// Modules installed by this package.
    pub modules: Vec<PlannedModule<'a>>,
}

/// Module entry in an install plan.
#[derive(Debug, Clone, Copy)]
pub struct PlannedModule<'a> {
    /// Package-relative module path.
    pub path: &'a str,
    /// Repository-relative module path.
    pub source_path: &'a str,
    /// Module manifest schema.
    pub schema: &'a str,
    /// Inferred module kind.
    pub kind: PackageKind,
    /// Inferred runtime resource name.
    pub name: &'a str,
}
