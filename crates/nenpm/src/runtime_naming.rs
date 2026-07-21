use nenjo::Slug;
use nenjo_packages::GitHubRepositoryRef;

use crate::{PackageSource, package_source_github_repository, package_source_scope};

/// Return the storage namespace for a package's runtime resources.
///
/// GitHub-backed packages use the full repository as their registry namespace.
/// Resource slugs are unique within a registry, so the package name is not
/// repeated in runtime identity.
pub fn package_runtime_scope(package_name: &str, package_source: Option<&PackageSource>) -> Slug {
    let repository = package_source.and_then(package_source_github_repository);
    package_runtime_scope_with_repository(package_name, repository.as_ref(), package_source)
}

/// Return the storage namespace using an already resolved canonical repository.
///
/// The explicit repository wins over the fetch source so local mirrors and
/// overrides retain the upstream package identity.
pub fn package_runtime_scope_with_repository(
    package_name: &str,
    repository: Option<&GitHubRepositoryRef>,
    package_source: Option<&PackageSource>,
) -> Slug {
    let package_leaf = package_name
        .trim()
        .trim_start_matches('@')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("package");
    if let Some(repository) = repository {
        let repository_name = repository.as_str().trim_start_matches('@');
        return Slug::derive_with_fallback(repository_name.replace('/', "-"), "local-registry");
    }

    let package_scope = package_name
        .trim()
        .strip_prefix('@')
        .and_then(|name| name.split_once('/'))
        .map(|(scope, _)| scope);
    let source_scope = package_source
        .and_then(package_source_scope)
        .map(|scope| scope.trim_start_matches('@').to_string());
    match source_scope.or_else(|| package_scope.map(str::to_string)) {
        Some(scope) => Slug::derive_with_fallback(scope, "local-registry"),
        None => Slug::derive_with_fallback(package_leaf, "local-package"),
    }
}

/// Derive a stable, registry-scoped runtime slug for a package resource.
pub fn package_runtime_slug(
    package_name: &str,
    package_source: Option<&PackageSource>,
    local_name: &str,
) -> Slug {
    package_runtime_slug_with_repository(package_name, None, package_source, local_name)
}

/// Derive a stable runtime slug from a canonical repository when available.
pub fn package_runtime_slug_with_repository(
    package_name: &str,
    repository: Option<&GitHubRepositoryRef>,
    package_source: Option<&PackageSource>,
    local_name: &str,
) -> Slug {
    let derived_repository = package_source.and_then(package_source_github_repository);
    let repository = repository.or(derived_repository.as_ref());
    let scope = package_runtime_scope_with_repository(package_name, repository, package_source);
    let local = Slug::derive_with_fallback(local_name, "resource");
    if local == scope || local.as_str().starts_with(&format!("{}-", scope.as_str())) {
        local
    } else {
        Slug::derive_with_fallback(format!("{}-{}", scope.as_str(), local.as_str()), "resource")
    }
}

/// Derive a repository-scoped runtime slug for a versioned package resource.
///
/// This is used by name-keyed resources whose installed versions may coexist.
/// Stable logical references remain versionless; only the runtime storage name
/// receives the normalized `v1_2_0` suffix.
pub fn package_runtime_versioned_slug(
    package_name: &str,
    package_source: Option<&PackageSource>,
    local_name: &str,
    version: Option<&str>,
) -> Slug {
    package_runtime_versioned_slug_with_repository(
        package_name,
        None,
        package_source,
        local_name,
        version,
    )
}

/// Derive a versioned runtime slug using an explicit canonical repository.
pub fn package_runtime_versioned_slug_with_repository(
    package_name: &str,
    repository: Option<&GitHubRepositoryRef>,
    package_source: Option<&PackageSource>,
    local_name: &str,
    version: Option<&str>,
) -> Slug {
    let stable =
        package_runtime_slug_with_repository(package_name, repository, package_source, local_name);
    match version.and_then(package_version_slug_label) {
        Some(version) => stable.with_slug_suffix(version),
        None => stable,
    }
}

fn package_version_slug_label(version: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut previous_separator = false;
    for character in version.trim().chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator && !normalized.is_empty() {
            normalized.push('_');
            previous_separator = true;
        }
    }
    while normalized.ends_with('_') {
        normalized.pop();
    }
    (!normalized.is_empty()).then(|| format!("v{normalized}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn github_repository_qualifies_runtime_slug_without_package_name() {
        let source = PackageSource::Git {
            url: "https://github.com/upstream/packages.git".to_string(),
            reference: "main".to_string(),
            manifest_path: "packages.yaml".to_string(),
        };

        assert_eq!(
            package_runtime_scope("@registry-scope/agent", Some(&source)).as_str(),
            "upstream-packages"
        );
        assert_eq!(
            package_runtime_slug("@registry-scope/agent", Some(&source), "Shop Manager").as_str(),
            "upstream-packages-shop-manager"
        );
    }

    #[test]
    fn local_source_scope_is_fallback_for_unscoped_packages() {
        let source = PackageSource::Local {
            root: PathBuf::from("/packages"),
            manifest_path: "packages.yaml".to_string(),
            scope: Some("@acme".to_string()),
        };

        assert_eq!(
            package_runtime_slug("agent", Some(&source), "Reviewer").as_str(),
            "acme-reviewer"
        );
        assert_eq!(
            package_runtime_slug("agent", None, "Reviewer").as_str(),
            "agent-reviewer"
        );
    }

    #[test]
    fn canonical_repository_wins_over_local_mirror_scope() {
        let source = PackageSource::Local {
            root: PathBuf::from("/packages"),
            manifest_path: "packages.yaml".to_string(),
            scope: Some("@mirror".to_string()),
        };
        let repository = GitHubRepositoryRef::parse("@nenjo-ai/packages").unwrap();

        assert_eq!(
            package_runtime_slug_with_repository(
                "@nenjo-ai/nenji",
                Some(&repository),
                Some(&source),
                "manage-tasks",
            )
            .as_str(),
            "nenjo-ai-packages-manage-tasks"
        );
    }

    #[test]
    fn versioned_runtime_slug_keeps_repository_scope_and_normalizes_version() {
        let repository = GitHubRepositoryRef::parse("@nenjo-ai/packages").unwrap();

        assert_eq!(
            package_runtime_versioned_slug_with_repository(
                "@nenjo-ai/nenji",
                Some(&repository),
                None,
                "Run Task",
                Some("1.2.0"),
            )
            .as_str(),
            "nenjo-ai-packages-run-task-v1_2_0"
        );
    }

    #[test]
    fn long_scopes_preserve_readable_resource_identity() {
        let repository = GitHubRepositoryRef::parse(
            "@organization-with-a-long-name/repository-with-a-long-name",
        )
        .unwrap();

        let first = package_runtime_slug_with_repository(
            "@registry/package-with-a-long-name",
            Some(&repository),
            None,
            "first-resource-with-a-long-local-name",
        );
        let second = package_runtime_slug_with_repository(
            "@registry/package-with-a-long-name",
            Some(&repository),
            None,
            "second-resource-with-a-long-local-name",
        );

        assert_ne!(first, second);
        assert_eq!(
            first.as_str(),
            "organization-with-a-long-name-repository-with-a-long-name-first-resource-with-a-long-local-name"
        );
        assert!(second.as_str().contains("second-resource"));
        assert!(first.as_str().len() <= Slug::MAX_LEN);
        assert!(second.as_str().len() <= Slug::MAX_LEN);
    }
}
