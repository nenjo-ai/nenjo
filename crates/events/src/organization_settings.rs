//! Organization-wide runtime settings distributed to every worker.

use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Complete replacement snapshot of organization settings used at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationSettings {
    pub timezone: Tz,
}

impl Default for OrganizationSettings {
    fn default() -> Self {
        Self {
            timezone: chrono_tz::UTC,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn organization_settings_round_trip_with_an_iana_timezone() {
        let settings: OrganizationSettings =
            serde_json::from_str(r#"{"timezone":"America/Chicago"}"#).unwrap();

        assert_eq!(settings.timezone, chrono_tz::America::Chicago);
        assert_eq!(
            serde_json::to_value(settings).unwrap()["timezone"],
            "America/Chicago"
        );
    }

    #[test]
    fn organization_settings_reject_an_invalid_timezone() {
        assert!(serde_json::from_str::<OrganizationSettings>(r#"{"timezone":"local"}"#).is_err());
    }
}
