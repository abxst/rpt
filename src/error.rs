//! Stable error domains for CLI diagnostics.
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Download,
    System,
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl std::error::Error for ErrorCategory {}

impl ErrorCategory {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::System => "system",
        }
    }
}

/// Format the causal chain without duplicating internal category markers.
/// The category is already displayed by the CLI as `[download]` or `[system]`.
pub fn describe(error: &anyhow::Error) -> String {
    let category = error.downcast_ref::<ErrorCategory>();
    error.chain()
        .enumerate()
        .filter(|(index, cause)| {
            !(*index == 0 && category.is_some_and(|kind| cause.to_string() == kind.label()))
        })
        .map(|(_, cause)| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

/// Explicit category markers in an error chain take precedence; download is
/// the default for errors originating in the transport layer.
pub fn classify(error: &anyhow::Error, fallback: ErrorCategory) -> ErrorCategory {
    error.downcast_ref::<ErrorCategory>().copied().unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn classifies_wrapped_system_errors() {
        let error = std::fs::read("/rpt-does-not-exist")
            .context(ErrorCategory::System)
            .unwrap_err();
        assert_eq!(classify(&error, ErrorCategory::Download), ErrorCategory::System);
    }

    #[test]
    fn skips_category_markers_without_losing_context() {
        let error = anyhow::anyhow!("file not found")
            .context("reading manifest")
            .context(ErrorCategory::System);
        assert_eq!(describe(&error), "reading manifest: file not found");
        assert_eq!(classify(&error, ErrorCategory::Download), ErrorCategory::System);
    }

    #[test]
    fn preserves_regular_error_chains() {
        let error = anyhow::anyhow!("404 Not Found").context("HTTP request");
        assert_eq!(describe(&error), "HTTP request: 404 Not Found");
    }

    #[test]
    fn falls_back_to_download_for_transport_errors() {
        let error = anyhow::anyhow!("HTTP 404");
        assert_eq!(classify(&error, ErrorCategory::Download), ErrorCategory::Download);
    }
}
