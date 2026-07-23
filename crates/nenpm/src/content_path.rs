use std::fmt;

use nenjo_packages::GitHubRepositoryRef;

use crate::source::{PackageSource, package_source_github_repository};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackageCoordinateError {
    #[error("package {coordinate} segment '{value}' is empty after normalization")]
    EmptySegment {
        coordinate: &'static str,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageCoordinates {
    source: Vec<CanonicalSegment>,
    package: CanonicalSegment,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CanonicalSegment(String);

impl CanonicalSegment {
    fn parse(value: &str, coordinate: &'static str) -> Result<Self, PackageCoordinateError> {
        let normalized = normalize_segment(value);
        if normalized.is_empty() {
            return Err(PackageCoordinateError::EmptySegment {
                coordinate,
                value: value.to_string(),
            });
        }
        Ok(Self(normalized))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl PackageCoordinates {
    pub fn new(
        package_name: &str,
        package_repository: Option<&GitHubRepositoryRef>,
        package_source: Option<&PackageSource>,
    ) -> Result<Self, PackageCoordinateError> {
        let repository = package_repository
            .cloned()
            .or_else(|| package_source.and_then(package_source_github_repository));
        let package_parts = package_name
            .trim_start_matches('@')
            .split('/')
            .filter(|segment| !segment.trim().is_empty())
            .collect::<Vec<_>>();
        let package = CanonicalSegment::parse(
            package_parts.last().copied().unwrap_or(package_name),
            "name",
        )?;

        let source = if let Some(repository) = repository {
            vec![
                CanonicalSegment::parse(repository.owner(), "repository owner")?,
                CanonicalSegment::parse(repository.repository(), "repository name")?,
            ]
        } else if let Some(PackageSource::Local {
            scope: Some(scope), ..
        }) = package_source
        {
            let segments = scope
                .split(['.', '/'])
                .filter(|segment| !segment.trim().is_empty())
                .map(|segment| CanonicalSegment::parse(segment, "scope"))
                .collect::<Result<Vec<_>, _>>()?;
            if segments.is_empty() {
                return Err(PackageCoordinateError::EmptySegment {
                    coordinate: "scope",
                    value: scope.clone(),
                });
            }
            segments
        } else {
            if package_parts.len() == 1 {
                vec![package.clone()]
            } else {
                package_parts[..package_parts.len() - 1]
                    .iter()
                    .map(|segment| CanonicalSegment::parse(segment, "scope"))
                    .collect::<Result<Vec<_>, _>>()?
            }
        };

        Ok(Self { source, package })
    }

    pub fn selector_with_leaf(&self, leaf: &str) -> Result<String, PackageCoordinateError> {
        let leaf = CanonicalSegment::parse(leaf, "selector leaf")?;
        Ok(self
            .source
            .iter()
            .chain(std::iter::once(&self.package))
            .chain(std::iter::once(&leaf))
            .map(CanonicalSegment::as_str)
            .collect::<Vec<_>>()
            .join("."))
    }
}

/// Canonical runtime storage path for package-owned content.
///
/// Package content is grouped as `pkg/<source>/<package>/<version>/<module...>`.
/// The package precedes the version so the hierarchy remains readable while
/// the versioned subtree still permits multiple installed versions to coexist.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackageContentPath(String);

impl PackageContentPath {
    /// Build a canonical content path from package distribution coordinates.
    pub fn new(
        package_name: &str,
        package_version: &str,
        package_repository: Option<&GitHubRepositoryRef>,
        package_source: Option<&PackageSource>,
        module_path: &str,
    ) -> Result<Self, PackageCoordinateError> {
        let coordinates =
            PackageCoordinates::new(package_name, package_repository, package_source)?;
        let version = CanonicalSegment::parse(package_version, "version")?;
        let mut segments = vec!["pkg".to_string()];
        segments.extend(
            coordinates
                .source
                .iter()
                .map(|segment| segment.as_str().to_string()),
        );
        // Scope and package are distinct coordinates even when an unscoped
        // local package uses its own name as the fallback scope.
        segments.push(coordinates.package.as_str().to_string());
        segments.push(format!("v{}", version.as_str()));

        if let Some((directory, _)) = module_path.rsplit_once('/') {
            let mut module_segments = directory
                .split('/')
                .filter(|segment| !segment.trim().is_empty())
                .map(|segment| CanonicalSegment::parse(segment, "module path"))
                .collect::<Result<Vec<_>, _>>()?;
            // Lockfiles may store module paths relative to the registry root.
            // The package segment has already been emitted above.
            if module_segments
                .first()
                .is_some_and(|segment| segment == &coordinates.package)
            {
                module_segments.remove(0);
            }
            segments.extend(
                module_segments
                    .iter()
                    .map(|segment| segment.as_str().to_string()),
            );
        }

        Ok(Self(segments.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl AsRef<str> for PackageContentPath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for PackageContentPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn normalize_segment(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('@')
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn github_content_path_is_source_package_version_module() {
        let repository = GitHubRepositoryRef::parse("@nenjo-ai/packages").unwrap();
        let path = PackageContentPath::new(
            "@nenjo-ai/context",
            "1.0.4",
            Some(&repository),
            None,
            "memory/remembrance.yml",
        )
        .unwrap();

        assert_eq!(path.as_str(), "pkg/nenjo_ai/packages/context/v1_0_4/memory");
    }

    #[test]
    fn local_scope_and_registry_relative_module_are_normalized() {
        let source = PackageSource::Local {
            root: PathBuf::from("/packages"),
            manifest_path: "packages.yaml".to_string(),
            scope: Some("@nenjo".to_string()),
        };
        let path = PackageContentPath::new(
            "@nenjo/nenji",
            "0.2.0",
            None,
            Some(&source),
            "nenji/abilities/design/agent.yml",
        )
        .unwrap();

        assert_eq!(path.as_str(), "pkg/nenjo/nenji/v0_2_0/abilities/design");
    }

    #[test]
    fn punctuation_only_coordinates_are_rejected() {
        let error = PackageContentPath::new(
            "@nenjo/context",
            "---",
            None,
            None,
            "memory/remembrance.yml",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            PackageCoordinateError::EmptySegment {
                coordinate: "version",
                ..
            }
        ));
    }

    #[test]
    fn punctuation_only_module_directory_is_rejected() {
        let error =
            PackageContentPath::new("@nenjo/context", "1.0.0", None, None, "---/remembrance.yml")
                .unwrap_err();

        assert!(matches!(
            error,
            PackageCoordinateError::EmptySegment {
                coordinate: "module path",
                ..
            }
        ));
    }

    #[test]
    fn explicit_repository_coordinates_override_a_local_mirror_scope() {
        let repository = GitHubRepositoryRef::parse("@nenjo-ai/packages").unwrap();
        let local_mirror = PackageSource::Local {
            root: PathBuf::from("/tmp/package-mirror"),
            manifest_path: "packages.yaml".into(),
            scope: Some("local.mirror".into()),
        };
        let coordinates =
            PackageCoordinates::new("@nenjo-ai/context", Some(&repository), Some(&local_mirror))
                .unwrap();

        assert_eq!(
            coordinates.selector_with_leaf("guides").unwrap(),
            "nenjo_ai.packages.context.guides"
        );
        assert_eq!(
            PackageContentPath::new(
                "@nenjo-ai/context",
                "1.0.0",
                Some(&repository),
                Some(&local_mirror),
                "context/guides/intro.yml",
            )
            .unwrap()
            .as_str(),
            "pkg/nenjo_ai/packages/context/v1_0_0/guides"
        );
    }

    #[test]
    fn punctuation_only_package_and_scope_are_rejected() {
        let package_error = PackageCoordinates::new("---", None, None).unwrap_err();
        assert!(matches!(
            package_error,
            PackageCoordinateError::EmptySegment {
                coordinate: "name",
                ..
            }
        ));

        let source = PackageSource::Local {
            root: PathBuf::from("/packages"),
            manifest_path: "packages.yaml".into(),
            scope: Some("---".into()),
        };
        let scope_error = PackageCoordinates::new("context", None, Some(&source)).unwrap_err();
        assert!(matches!(
            scope_error,
            PackageCoordinateError::EmptySegment {
                coordinate: "scope",
                ..
            }
        ));
    }
}
