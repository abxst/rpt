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

/// Explicit category markers in an error chain take precedence; download is
/// the default for errors originating in the transport layer.
pub fn classify(error: &anyhow::Error, fallback: ErrorCategory) -> ErrorCategory {
    error.chain()
        .find_map(|cause| cause.downcast_ref::<ErrorCategory>().copied())
        .unwrap_or(fallback)
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
    fn falls_back_to_download_for_transport_errors() {
        let error = anyhow::anyhow!("HTTP 404");
        assert_eq!(classify(&error, ErrorCategory::Download), ErrorCategory::Download);
    }
}
