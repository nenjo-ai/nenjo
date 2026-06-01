use std::collections::{BTreeMap, BTreeSet};

use crate::Result;
use anyhow::anyhow;

use crate::{
    ModuleImport, ModulePackageManifest, PackageDescriptor, PackageKind, ResourceDependency,
    ResourceManifest, validate_source_path, version_satisfies,
};

#[derive(Debug, Clone)]
/// Resolved descriptor, manifest, and content hash for one package resource.
pub struct ResolvedResource {
    /// Repository-relative descriptor path.
    pub path: String,
    /// Repository-relative resource manifest path.
    pub entry_path: String,
    /// SHA-256 hash of the descriptor and resource manifest content.
    pub hash: String,
    /// Resolved resource kind.
    pub kind: PackageKind,
    /// Parsed package descriptor.
    pub descriptor: PackageDescriptor,
    /// Parsed resource manifest envelope.
    pub manifest: ResourceManifest,
}
#[derive(Debug, Clone)]
/// Resolved package module with inferred runtime information.
pub struct ResolvedModule {
    /// Package name that owns this module.
    pub package_name: String,
    /// Package version that owns this module.
    pub package_version: String,
    /// Package-relative module manifest path.
    pub path: String,
    /// Repository-relative module manifest path.
    pub source_path: String,
    /// SHA-256 hash of the module manifest content.
    pub hash: String,
    /// Resource kind inferred from the module manifest schema.
    pub kind: PackageKind,
    /// Parsed module manifest.
    pub manifest: ResourceManifest,
    /// Structured resource imports declared by this module.
    pub imports: Vec<ModuleImport>,
}

impl ResolvedModule {
    /// Return the validated resource name inferred from the module manifest body.
    pub fn name(&self) -> &str {
        self.manifest
            .name()
            .expect("resolved module manifest was validated")
    }

    /// Return the manifest schema string.
    pub fn schema(&self) -> &str {
        &self.manifest.schema
    }

    /// Return the canonical key for this resolved module resource.
    pub fn key(&self) -> String {
        format!("{}#{}", self.path, self.name())
    }
}

#[derive(Debug, Clone)]
/// Resolved package manifest and all included modules.
pub struct ResolvedPackage {
    /// Repository package name.
    pub name: String,
    /// Repository-relative package manifest path.
    pub path: String,
    /// Package version.
    pub version: String,
    /// SHA-256 hash of the package manifest content.
    pub hash: String,
    /// Parsed package manifest.
    pub manifest: ModulePackageManifest,
    /// Resolved modules keyed by package-relative module path.
    pub modules: BTreeMap<String, ResolvedModule>,
}

impl ResolvedPackage {
    /// Return package dependencies as a name-to-version-requirement map.
    pub fn dependencies(&self) -> &BTreeMap<String, String> {
        &self.manifest.dependencies
    }

    /// Validate package-local knowledge pack names derived from `pack_id`.
    pub fn validate_knowledge_pack_names(&self) -> Result<()> {
        let mut seen_resources = BTreeSet::new();
        let mut names = BTreeMap::new();
        for module in self
            .modules
            .values()
            .filter(|module| module.kind == PackageKind::Knowledge)
        {
            if !seen_resources.insert(module.key()) {
                continue;
            }
            let pack_name = knowledge_pack_name(&module.manifest).unwrap_or_else(|| {
                module
                    .name()
                    .trim()
                    .rsplit(['.', '/'])
                    .next()
                    .unwrap_or(module.name())
                    .to_string()
            });
            if let Some(existing) = names.insert(pack_name.clone(), module.source_path.clone()) {
                bail!(
                    "{} declares duplicate knowledge pack name '{}' in {} and {}",
                    self.name,
                    pack_name,
                    existing,
                    module.source_path
                );
            }
        }
        Ok(())
    }
}

fn knowledge_pack_name(manifest: &ResourceManifest) -> Option<String> {
    manifest
        .manifest
        .get("pack_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|pack_id| {
            pack_id
                .trim()
                .rsplit(['.', '/'])
                .next()
                .filter(|name| !name.trim().is_empty())
                .map(str::to_string)
        })
}

#[derive(Debug, Clone)]
/// Dependency graph for a root package and all resolved package dependencies.
pub struct ResolvedPackageGraph {
    /// Package name requested by the installer.
    pub root_package: String,
    /// Resolved packages keyed by package name.
    pub packages: BTreeMap<String, ResolvedPackage>,
}

impl ResolvedPackageGraph {
    /// Return dependency-first package install order with the root package last.
    pub fn topo_order(&self) -> Result<Vec<String>> {
        fn visit(
            name: &str,
            graph: &BTreeMap<String, ResolvedPackage>,
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
            let package = graph
                .get(name)
                .ok_or_else(|| anyhow!("dependency {name} was not resolved"))?;
            for dependency in package.dependencies().keys() {
                visit(dependency, graph, temp, perm, out)?;
            }
            temp.remove(name);
            perm.insert(name.to_string());
            out.push(name.to_string());
            Ok(())
        }

        let mut out = Vec::new();
        visit(
            &self.root_package,
            &self.packages,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut out,
        )?;
        if let Some(pos) = out.iter().position(|name| name == &self.root_package) {
            let root = out.remove(pos);
            out.push(root);
        }
        Ok(out)
    }

    /// Validate package dependency version requirements against resolved versions.
    pub fn validate_versions(&self) -> Result<()> {
        for (name, package) in &self.packages {
            package.validate_knowledge_pack_names()?;
            for (dependency, required) in package.dependencies() {
                let resolved = self
                    .packages
                    .get(dependency)
                    .ok_or_else(|| anyhow!("{name} depends on unresolved {dependency}"))?;
                if !version_satisfies(&resolved.version, required) {
                    bail!(
                        "{name} requires {dependency} version {required}, got {}",
                        resolved.version
                    );
                }
            }
        }
        Ok(())
    }
}

impl ResolvedResource {
    /// Return the validated resource name.
    pub fn name(&self) -> &str {
        self.manifest
            .name()
            .expect("resolved resource manifest was validated")
    }

    /// Return the optional resource slug.
    pub fn slug(&self) -> Option<&str> {
        self.manifest.slug()
    }

    /// Return the optional source root URI.
    pub fn root_uri(&self) -> Option<&str> {
        self.manifest.root_uri()
    }

    /// Return the source selector, falling back to the root URI.
    pub fn selector(&self) -> Option<&str> {
        self.manifest.selector()
    }

    /// Return the package descriptor version.
    pub fn version(&self) -> Option<&str> {
        self.descriptor.version.as_deref()
    }

    /// Return the package dependencies declared by the descriptor.
    pub fn dependencies(&self) -> &[ResourceDependency] {
        &self.descriptor.depends_on
    }
}

#[derive(Debug, Clone)]
/// Dependency graph for a root package resource and all resolved dependencies.
pub struct ResolvedResourceGraph {
    /// Repository-relative descriptor path requested by the installer.
    pub root_path: String,
    /// Resolved resources keyed by descriptor path.
    pub resources: BTreeMap<String, ResolvedResource>,
}

impl ResolvedResourceGraph {
    /// Return dependency-first install order with the root resource last.
    pub fn topo_order(&self) -> Result<Vec<String>> {
        fn visit(
            path: &str,
            graph: &BTreeMap<String, ResolvedResource>,
            temp: &mut BTreeSet<String>,
            perm: &mut BTreeSet<String>,
            out: &mut Vec<String>,
        ) -> Result<()> {
            if perm.contains(path) {
                return Ok(());
            }
            if !temp.insert(path.to_string()) {
                bail!("dependency cycle includes {path}");
            }
            let resource = graph
                .get(path)
                .ok_or_else(|| anyhow!("dependency {path} was not resolved"))?;
            for dep in resource.dependencies() {
                visit(&validate_source_path(&dep.path)?, graph, temp, perm, out)?;
            }
            temp.remove(path);
            perm.insert(path.to_string());
            out.push(path.to_string());
            Ok(())
        }

        let mut out = Vec::new();
        visit(
            &self.root_path,
            &self.resources,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut out,
        )?;
        if let Some(pos) = out.iter().position(|path| path == &self.root_path) {
            let root = out.remove(pos);
            out.push(root);
        }
        Ok(out)
    }

    /// Validate all dependency version requirements against resolved package versions.
    pub fn validate_versions(&self) -> Result<()> {
        for (path, resource) in &self.resources {
            for dep in resource.dependencies() {
                let Some(required) = dep.version.as_deref() else {
                    continue;
                };
                let dep_path = validate_source_path(&dep.path)?;
                let resolved = self
                    .resources
                    .get(&dep_path)
                    .ok_or_else(|| anyhow!("{path} depends on unresolved {dep_path}"))?;
                let actual = resolved.version().unwrap_or("0.0.0");
                if !version_satisfies(actual, required) {
                    bail!("{path} requires {dep_path} version {required}, got {actual}");
                }
            }
        }
        Ok(())
    }
}
