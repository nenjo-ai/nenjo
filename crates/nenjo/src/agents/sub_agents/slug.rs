use std::fmt;
use std::str::FromStr;

use super::error::SubAgentError;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct SubAgentSlug(String);

impl SubAgentSlug {
    pub(crate) fn parse(raw: impl Into<String>) -> Result<Self, SubAgentError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(SubAgentError::InvalidSlug("slug cannot be empty".into()));
        }
        if raw.len() > 64 {
            return Err(SubAgentError::InvalidSlug(
                "slug cannot be longer than 64 characters".into(),
            ));
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(SubAgentError::InvalidSlug(
                "slug may contain only lowercase letters, numbers, underscores, and hyphens".into(),
            ));
        }
        Ok(Self(raw))
    }

    pub(crate) fn derive(agent_name: &str) -> Self {
        let mut slug = String::new();
        let mut previous_separator = false;
        for ch in agent_name.chars() {
            let next = if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch == '_' || ch == '-' || ch.is_whitespace() {
                Some('_')
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
            if slug.len() == 64 {
                break;
            }
        }
        while slug.ends_with('_') || slug.ends_with('-') {
            slug.pop();
        }
        if slug.is_empty() {
            slug.push_str("sub_agent");
        }
        Self(slug)
    }

    pub(crate) fn with_suffix(&self, suffix: usize) -> Self {
        let suffix = format!("_{suffix}");
        let base_len = 64usize.saturating_sub(suffix.len());
        let mut base = self.0.chars().take(base_len).collect::<String>();
        while base.ends_with('_') || base.ends_with('-') {
            base.pop();
        }
        base.push_str(&suffix);
        Self(base)
    }
}

impl fmt::Display for SubAgentSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SubAgentSlug {
    type Err = SubAgentError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::SubAgentSlug;

    #[test]
    fn validates_slug_shape() {
        assert!(SubAgentSlug::parse("security_review-2").is_ok());
        assert!(SubAgentSlug::parse("Security").is_err());
        assert!(SubAgentSlug::parse("bad space").is_err());
        assert!(SubAgentSlug::parse("a".repeat(65)).is_err());
    }

    #[test]
    fn derives_slug_from_agent_name() {
        assert_eq!(
            SubAgentSlug::derive("Security Reviewer").to_string(),
            "security_reviewer"
        );
        assert_eq!(SubAgentSlug::derive("!!!").to_string(), "sub_agent");
    }
}
