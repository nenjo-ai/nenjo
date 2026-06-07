//! Nenjo package manager primitives.
//!
//! `nenjo-nenpm` sits above `nenjo-packages`. The packages crate owns the file
//! format and graph resolver; this crate turns resolved package graphs into
//! install plans that registry, cache, and platform-import layers can execute.

macro_rules! bail {
    ($($arg:tt)*) => {
        return Err(crate::NenpmError::Message(format!($($arg)*)))
    };
}

mod dependency;
mod error;
mod github;
mod install;
mod lockfile;
mod plan;
mod pm;
mod registry;
mod source;
mod validate;

pub use dependency::{
    DependencyManifest, DependencyOverride, LoadedDependencyManifest, RegistryReference,
};
pub use error::{NenpmError, Result};
pub use github::GitHubRawFetcher;
pub use install::{InstallOptions, InstallReport, MaterializationReport, UpgradePolicy, install};
pub use lockfile::{
    LockedModule, LockedPackage, NenpmLock, PackageInstallIndex, PackageInstallIndexEntry,
    package_install_path, package_install_path_in_packages_dir, package_instance_key,
};
pub use plan::{InstallPlan, PlannedModule, PlannedPackage};
pub use pm::{
    AddOptions, AddReport, AddTarget, CleanOptions, CleanReport, InfoOptions, InitOptions,
    InitReport, ListOptions, PackageInfo, PackageInfoModule, PackageInfoVersion, PackageSpec,
    RemoveOptions, RemoveReport, add, clean, info, init, list, remove, update,
};
pub use registry::{
    InMemoryRegistry, PackageRegistry, RegistryIndex, RegistryIndexVersion,
    RegistryPackageResolver, RegistryPackageVersion,
};
pub use source::{
    DefaultPackageSourceFetcher, FetchMode, FetchedPackageSource, PackageSource,
    PackageSourceFetcher, package_source_scope,
};
pub use validate::{
    PrepareOptions, PrepareReport, PreparedModule, PreparedPackage, PreparedRegistry,
    ValidateOptions, ValidateReport, prepare, validate,
};
