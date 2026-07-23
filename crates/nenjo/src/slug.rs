use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Slug(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlugError {
    #[error("slug cannot be empty")]
    Empty,
    #[error("slug cannot be longer than {max} characters")]
    TooLong { max: usize },
    #[error("slug may contain only lowercase letters, numbers, underscores, and hyphens")]
    InvalidCharacter,
    #[error("slug cannot start or end with a separator")]
    BoundarySeparator,
}

impl Slug {
    /// Matches the platform's persisted `varchar(255)` slug boundary.
    pub const MAX_LEN: usize = 255;

    pub fn parse(value: impl AsRef<str>) -> Result<Self, SlugError> {
        let value = value.as_ref();
        if value.is_empty() {
            return Err(SlugError::Empty);
        }
        if value.len() > Self::MAX_LEN {
            return Err(SlugError::TooLong { max: Self::MAX_LEN });
        }
        if value.starts_with(['_', '-']) || value.ends_with(['_', '-']) {
            return Err(SlugError::BoundarySeparator);
        }
        if !value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-')
        {
            return Err(SlugError::InvalidCharacter);
        }
        Ok(Self(value.to_string()))
    }

    pub fn derive(value: impl AsRef<str>) -> Self {
        Self::derive_with_fallback(value, "slug")
    }

    /// Build a slug from free-form text.
    ///
    /// - lowercase alphanumerics kept
    /// - `_` and `-` kept
    /// - whitespace becomes `-` (kebab-case)
    /// - other characters dropped
    /// - consecutive separators collapsed
    pub fn derive_with_fallback(value: impl AsRef<str>, fallback: &str) -> Self {
        let mut slug = String::new();
        let mut previous_separator = false;
        for ch in value.as_ref().chars() {
            let next = if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch == '_' || ch == '-' {
                Some(ch)
            } else if ch.is_whitespace() {
                Some('-')
            } else {
                None
            };

            let Some(next) = next else {
                continue;
            };
            if next == '_' || next == '-' {
                if previous_separator || slug.is_empty() {
                    continue;
                }
                previous_separator = true;
            } else {
                previous_separator = false;
            }
            slug.push(next);
            if slug.len() == Self::MAX_LEN {
                break;
            }
        }
        while slug.ends_with(['_', '-']) {
            slug.pop();
        }
        if slug.is_empty() {
            slug.push_str(fallback);
        }
        Self(slug)
    }

    pub fn with_suffix(&self, suffix: usize) -> Self {
        self.with_slug_suffix(suffix.to_string())
    }

    pub fn with_slug_suffix(&self, suffix: impl AsRef<str>) -> Self {
        let suffix = Self::derive_with_fallback(suffix, "suffix");
        let suffix = format!("-{}", suffix.as_str());
        let base_len = Self::MAX_LEN.saturating_sub(suffix.len());
        let mut base = self.0.chars().take(base_len).collect::<String>();
        while base.ends_with(['_', '-']) {
            base.pop();
        }
        base.push_str(&suffix);
        Self(base)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

/// Ergonomic conversion into a manifest slug for public SDK APIs.
///
/// `Slug` remains the internal resource identity type, but callers can pass
/// simple string literals such as `"code-reviewer"` at API boundaries.
pub trait IntoSlug {
    fn into_slug(self) -> Slug;
}

impl IntoSlug for Slug {
    fn into_slug(self) -> Slug {
        self
    }
}

impl IntoSlug for &Slug {
    fn into_slug(self) -> Slug {
        self.clone()
    }
}

impl IntoSlug for &str {
    fn into_slug(self) -> Slug {
        Slug::derive(self)
    }
}

impl IntoSlug for String {
    fn into_slug(self) -> Slug {
        Slug::derive(self)
    }
}

impl IntoSlug for &String {
    fn into_slug(self) -> Slug {
        Slug::derive(self)
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Slug {
    type Err = SlugError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl TryFrom<String> for Slug {
    type Error = SlugError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for Slug {
    type Error = SlugError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<Slug> for String {
    fn from(value: Slug) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for Slug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_slug() {
        assert_eq!(Slug::parse("core_pack-1").unwrap().as_str(), "core_pack-1");
        assert!(Slug::parse("a".repeat(Slug::MAX_LEN)).is_ok());
        assert_eq!(
            Slug::parse("a".repeat(Slug::MAX_LEN + 1)),
            Err(SlugError::TooLong { max: Slug::MAX_LEN })
        );
    }

    #[test]
    fn rejects_invalid_slug_shape() {
        assert!(matches!(Slug::parse(""), Err(SlugError::Empty)));
        assert!(matches!(
            Slug::parse("Bad"),
            Err(SlugError::InvalidCharacter)
        ));
        assert!(matches!(
            Slug::parse("_bad"),
            Err(SlugError::BoundarySeparator)
        ));
    }

    #[test]
    fn derives_lossy_slug() {
        assert_eq!(
            Slug::derive_with_fallback("Core Pack!", "fallback").as_str(),
            "core-pack"
        );
        assert_eq!(
            Slug::derive_with_fallback("nenjo-ai", "fallback").as_str(),
            "nenjo-ai"
        );
        assert_eq!(
            Slug::derive_with_fallback("Shared Agent", "fallback").as_str(),
            "shared-agent"
        );
        assert_eq!(
            Slug::derive_with_fallback("!!!", "fallback").as_str(),
            "fallback"
        );
    }

    #[test]
    fn appends_suffix_with_max_length_preserved() {
        let slug = Slug::derive("a".repeat(Slug::MAX_LEN)).with_slug_suffix("ABC 123");
        assert_eq!(slug.as_str().len(), Slug::MAX_LEN);
        assert!(slug.as_str().ends_with("-abc-123"));
    }

    #[test]
    fn into_slug_accepts_owned_and_borrowed_inputs() {
        let parsed = Slug::parse("code-reviewer").unwrap();
        assert_eq!(parsed.clone().into_slug(), parsed);
        assert_eq!((&parsed).into_slug(), parsed);
        assert_eq!("Code Reviewer".into_slug().as_str(), "code-reviewer");
        assert_eq!(
            String::from("demo_project").into_slug().as_str(),
            "demo_project"
        );
    }
}
