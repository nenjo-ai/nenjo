use std::fmt;

/// A single routine graph validation issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineValidationIssue {
    pub message: String,
    pub step: Option<String>,
    pub edge: Option<String>,
}

impl RoutineValidationIssue {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            step: None,
            edge: None,
        }
    }

    pub fn step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }

    pub fn edge(mut self, edge: impl Into<String>) -> Self {
        self.edge = Some(edge.into());
        self
    }
}

/// Validation failure for a routine graph. The first issue is formatted first
/// so callers that expose a single message keep deterministic behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineValidationError {
    pub issues: Vec<RoutineValidationIssue>,
}

impl RoutineValidationError {
    pub fn single(issue: RoutineValidationIssue) -> Self {
        Self {
            issues: vec![issue],
        }
    }
}

impl fmt::Display for RoutineValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(first) = self.issues.first() else {
            return f.write_str("Routine graph validation failed");
        };
        f.write_str(&first.message)
    }
}

impl std::error::Error for RoutineValidationError {}
