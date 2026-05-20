//! Nenjo package manager primitives.
//!
//! `nenjo-nenpm` sits above `nenjo-packages`. The packages crate owns the file
//! format and graph resolver; this crate turns resolved package graphs into
//! install plans that registry, cache, and platform-import layers can execute.

mod dependency;
mod github;
mod install;
mod lockfile;
mod plan;
mod pm;
mod registry;
mod source;

pub use dependency::{DependencyManifest, DependencyOverride, LoadedDependencyManifest};
pub use github::GitHubRawFetcher;
pub use install::{InstallOptions, InstallReport, install};
pub use lockfile::{LockedModule, LockedPackage, NenpmLock};
pub use plan::{InstallPlan, PlannedModule, PlannedPackage};
pub use pm::{
    AddOptions, AddReport, InfoOptions, ListOptions, PackageInfo, PackageSpec, RemoveOptions,
    RemoveReport, add, info, list, remove, update,
};
pub use registry::{
    InMemoryRegistry, PackageRegistry, RegistryIndex, RegistryIndexVersion,
    RegistryPackageResolver, RegistryPackageVersion,
};
pub use source::{
    DefaultPackageSourceFetcher, FetchedPackageSource, PackageSource, PackageSourceFetcher,
};

const DEFAULT_MAX_CONCURRENCY: usize = 8;
