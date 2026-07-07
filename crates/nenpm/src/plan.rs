use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::Result;
use anyhow::{Context, anyhow};
use nenjo_packages::{
    LocalPackageResolver, ModulePackageManifest, PackageKind, PackageModule, ResolvedModule,
    ResolvedPackage, ResolvedPackageFile, ResolvedPackageGraph, ResourceManifest,
};

use crate::lockfile::NenpmLock;
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
        let package_order = topo_order_all(&graph)?;
        Ok(Self {
            graph,
            package_order,
        })
    }

    /// Build an install plan from a lockfile without resolving package registries.
    pub fn from_lockfile(lockfile: &NenpmLock) -> Result<Self> {
        let mut packages = BTreeMap::new();
        let mut package_order = Vec::new();
        for package in &lockfile.packages {
            package_order.push(package.name.clone());
            let mut modules = BTreeMap::new();
            let mut manifest_modules = Vec::new();
            for module in &package.modules {
                manifest_modules.push(PackageModule {
                    path: module.path.clone(),
                    metadata: serde_json::Value::Null,
                });
                let resolved = ResolvedModule {
                    package_name: package.name.clone(),
                    package_version: package.version.clone(),
                    path: module.path.clone(),
                    source_path: module.source_path.clone(),
                    hash: module.hash.clone(),
                    kind: module.kind,
                    manifest: ResourceManifest {
                        schema: module.schema.clone(),
                        slug: None,
                        root_uri: None,
                        selector: None,
                        imports: BTreeMap::new(),
                        manifest: serde_json::json!({ "name": module.name }),
                    },
                    imports: module.imports.clone(),
                    files: module
                        .files
                        .iter()
                        .map(|file| ResolvedPackageFile {
                            path: file.path.clone(),
                            hash: file.hash.clone(),
                        })
                        .collect(),
                };
                modules.insert(resolved.key(), resolved);
            }
            let resolved = ResolvedPackage {
                name: package.name.clone(),
                path: package.manifest_path.clone(),
                version: package.version.clone(),
                hash: package.hash.clone(),
                manifest: ModulePackageManifest {
                    schema: "nenjo.package.v1".to_string(),
                    name: package.name.clone(),
                    version: package.version.clone(),
                    description: None,
                    dependencies: package.dependencies.clone(),
                    arguments: Vec::new(),
                    modules: manifest_modules,
                    metadata: serde_json::Value::Null,
                },
                modules,
            };
            if packages.insert(package.name.clone(), resolved).is_some() {
                bail!("lockfile contains duplicate package {}", package.name);
            }
        }
        Ok(Self {
            graph: ResolvedPackageGraph {
                root_package: package_order.last().cloned().unwrap_or_default(),
                packages,
            },
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

fn topo_order_all(graph: &ResolvedPackageGraph) -> Result<Vec<String>> {
    fn visit(
        name: &str,
        packages: &BTreeMap<String, ResolvedPackage>,
        temp: &mut BTreeSet<String>,
        perm: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) -> Result<()> {
        if perm.contains(name) {
            return Ok(());
        }
        if !temp.insert(name.to_string()) {
            bail!("dependency cycle includes {name}");
        }

        let package = packages
            .get(name)
            .ok_or_else(|| anyhow!("dependency {name} was not resolved"))?;
        for dependency in package.dependencies().keys() {
            visit(dependency, packages, temp, perm, out)?;
        }

        temp.remove(name);
        perm.insert(name.to_string());
        out.push(name.to_string());
        Ok(())
    }

    let mut out = Vec::new();
    let mut temp = BTreeSet::new();
    let mut perm = BTreeSet::new();

    if !graph.root_package.is_empty() {
        visit(
            &graph.root_package,
            &graph.packages,
            &mut temp,
            &mut perm,
            &mut out,
        )?;
    }

    for name in graph.packages.keys() {
        visit(name, &graph.packages, &mut temp, &mut perm, &mut out)?;
    }

    Ok(out)
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
