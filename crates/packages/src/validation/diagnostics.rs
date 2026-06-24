use crate::PackageKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageValidationSeverity {
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageValidationDiagnostic {
    pub severity: PackageValidationSeverity,
    pub package: String,
    pub source_path: String,
    pub kind: Option<PackageKind>,
    pub field_path: Option<String>,
    pub message: String,
}

impl PackageValidationDiagnostic {
    pub(crate) fn error(
        package: impl Into<String>,
        source_path: impl Into<String>,
        kind: Option<PackageKind>,
        field_path: impl Into<Option<String>>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: PackageValidationSeverity::Error,
            package: package.into(),
            source_path: source_path.into(),
            kind,
            field_path: field_path.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageRuntimeValidationReport {
    pub diagnostics: Vec<PackageValidationDiagnostic>,
}

impl PackageRuntimeValidationReport {
    pub fn is_valid(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn error_summary(&self) -> String {
        self.diagnostics
            .iter()
            .map(|diagnostic| {
                let field = diagnostic
                    .field_path
                    .as_deref()
                    .map(|field| format!(" {field}"))
                    .unwrap_or_default();
                format!(
                    "{}:{}{}: {}",
                    diagnostic.package, diagnostic.source_path, field, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}
