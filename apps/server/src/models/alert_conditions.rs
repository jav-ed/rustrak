//! Conditions an alert rule can carry beyond its type and cooldown.

use serde::Deserialize;

use crate::error::{AppError, AppResult, FieldErrorCode};

/// Event severity, ordered from least to most severe (Sentry's five levels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueLevel {
    Debug,
    Info,
    Warning,
    Error,
    Fatal,
}

impl IssueLevel {
    /// Ranks the level stored on an issue. An absent or unrecognized level is
    /// Sentry's default, `error`, so an SDK-specific level can never cause an
    /// alert to be withheld.
    pub fn of_issue(level: Option<&str>) -> Self {
        match level {
            Some("debug") => Self::Debug,
            Some("info") => Self::Info,
            Some("warning") => Self::Warning,
            Some("fatal") => Self::Fatal,
            _ => Self::Error,
        }
    }
}

/// The conditions an alert rule understands. Writes are validated strictly by
/// [`AlertConditions::validate`]; reading is lenient (see
/// [`AlertConditions::from_stored`]) because rules saved before conditions had
/// any effect can hold arbitrary JSON.
#[derive(Debug, Default, Deserialize)]
pub struct AlertConditions {
    /// Only issues at this level or above send the alert. Absent or `null`:
    /// every issue does.
    #[serde(default)]
    pub min_level: Option<IssueLevel>,
}

impl AlertConditions {
    fn invalid(message: impl Into<String>) -> AppError {
        AppError::Validation(message.into()).with_field("conditions", FieldErrorCode::Invalid)
    }

    /// Rejects what a rule could never act on, so a typo such as `minlevel`
    /// cannot leave a rule silently unfiltered.
    pub fn validate(value: &serde_json::Value) -> AppResult<()> {
        let Some(map) = value.as_object() else {
            return Err(Self::invalid("conditions must be a JSON object"));
        };
        if let Some(key) = map.keys().find(|key| key.as_str() != "min_level") {
            return Err(Self::invalid(format!(
                "unknown condition '{key}'; supported conditions: min_level"
            )));
        }
        serde_json::from_value::<Self>(value.clone()).map_err(|_| {
            Self::invalid("min_level must be one of debug, info, warning, error, fatal")
        })?;
        Ok(())
    }

    /// Reads the conditions saved on a rule. A stored value this version cannot
    /// read is logged and treated as no condition: a rule written before
    /// conditions had meaning must keep alerting, and failing to alert is worse
    /// than alerting too often.
    pub fn from_stored(value: &serde_json::Value, rule_id: i32) -> Self {
        serde_json::from_value(value.clone()).unwrap_or_else(|error| {
            log::warn!("Alert rule {rule_id} has unreadable conditions, ignoring them: {error}");
            Self::default()
        })
    }

    /// Whether an issue at `level` may send the alert.
    pub fn admits(&self, level: Option<&str>) -> bool {
        self.min_level
            .is_none_or(|minimum| IssueLevel::of_issue(level) >= minimum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn levels_are_ordered_from_debug_to_fatal() {
        assert!(IssueLevel::Debug < IssueLevel::Info);
        assert!(IssueLevel::Info < IssueLevel::Warning);
        assert!(IssueLevel::Warning < IssueLevel::Error);
        assert!(IssueLevel::Error < IssueLevel::Fatal);
    }

    #[test]
    fn missing_or_unrecognized_level_counts_as_error() {
        assert_eq!(IssueLevel::of_issue(None), IssueLevel::Error);
        assert_eq!(IssueLevel::of_issue(Some("critical")), IssueLevel::Error);
    }

    #[test]
    fn accepts_empty_and_supported_conditions() {
        for value in [
            json!({}),
            json!({"min_level": null}),
            json!({"min_level": "error"}),
        ] {
            assert!(AlertConditions::validate(&value).is_ok(), "{value}");
        }
    }

    #[test]
    fn rejects_conditions_a_rule_cannot_act_on() {
        for value in [
            json!([]),
            json!("error"),
            json!({"minlevel": "error"}),
            json!({"min_level": "high"}),
            json!({"min_level": 3}),
            json!({"min_level": "error", "extra": true}),
        ] {
            assert!(AlertConditions::validate(&value).is_err(), "{value}");
        }
    }

    #[test]
    fn admits_issues_at_or_above_the_minimum_level() {
        let conditions = AlertConditions::from_stored(&json!({"min_level": "error"}), 1);
        assert!(!conditions.admits(Some("debug")));
        assert!(!conditions.admits(Some("info")));
        assert!(!conditions.admits(Some("warning")));
        assert!(conditions.admits(Some("error")));
        assert!(conditions.admits(Some("fatal")));
        assert!(conditions.admits(None));
        assert!(conditions.admits(Some("critical")));
    }

    #[test]
    fn a_rule_without_a_minimum_admits_every_level() {
        let conditions = AlertConditions::from_stored(&json!({}), 1);
        assert!(conditions.admits(Some("debug")));
    }

    #[test]
    fn unreadable_stored_conditions_never_withhold_an_alert() {
        // Saved before conditions had any effect.
        let conditions = AlertConditions::from_stored(&json!({"min_level": "high"}), 1);
        assert!(conditions.admits(Some("debug")));
        let conditions = AlertConditions::from_stored(&json!("legacy"), 1);
        assert!(conditions.admits(Some("debug")));
    }
}
