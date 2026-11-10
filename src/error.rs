//! Stable error domains for CLI diagnostics.
use std::fmt;
use serde::Serialize;
use std::io;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat { Text, Json }

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    DownloadHttp,
    DownloadTimeout,
    DownloadNetwork,
    DownloadChecksum,
    DownloadIntegrity,
    DownloadSignature,
    DownloadIndex,
    SystemIo,
    SystemConfig,
    SystemWorker,
}

impl ErrorCode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::DownloadHttp => "DOWNLOAD_HTTP",
            Self::DownloadTimeout => "DOWNLOAD_TIMEOUT",
            Self::DownloadNetwork => "DOWNLOAD_NETWORK",
            Self::DownloadChecksum => "DOWNLOAD_CHECKSUM",
            Self::DownloadIntegrity => "DOWNLOAD_INTEGRITY",
            Self::DownloadSignature => "DOWNLOAD_SIGNATURE",
            Self::DownloadIndex => "DOWNLOAD_INDEX",
            Self::SystemIo => "SYSTEM_IO",
            Self::SystemConfig => "SYSTEM_CONFIG",
            Self::SystemWorker => "SYSTEM_WORKER",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}
impl std::error::Error for ErrorCode {}

pub fn code(error: &anyhow::Error, fallback: ErrorCategory) -> ErrorCode {
    if let Some(explicit) = error.downcast_ref::<ErrorCode>() {
        return *explicit;
    }
    let category = classify(error, fallback);
    if category == ErrorCategory::System {
        let description = describe(error);
        if description.starts_with("reading manifest")
            || description.starts_with("parsing manifest")
            || description.contains("invalid package")
            || description.contains("unsafe filename")
            || description.contains("workers must")
            || description.contains("retries must")
            || description.contains("HTTP(S) URLs")
            || description.contains("invalid SHA-256")
        {
            // Missing manifest is an IO error; malformed manifest is configuration.
            if description.starts_with("reading manifest") {
                return ErrorCode::SystemIo;
            }
            return ErrorCode::SystemConfig;
        }
        if error.chain().any(|cause| cause.is::<io::Error>()) {
            return ErrorCode::SystemIo;
        }
        return ErrorCode::SystemWorker;
    }
    if error.chain().any(|cause| cause.to_string().contains("SHA-256 mismatch")) {
        return ErrorCode::DownloadChecksum;
    }
    if let Some(req) = error.chain().find_map(|cause| cause.downcast_ref::<reqwest::Error>()) {
        if req.is_status() { return ErrorCode::DownloadHttp; }
        if req.is_timeout() { return ErrorCode::DownloadTimeout; }
        return ErrorCode::DownloadNetwork;
    }
    ErrorCode::DownloadIntegrity
}

pub fn emit(
    format: LogFormat, event: &str, category: ErrorCategory,
    code: ErrorCode, package: Option<&str>, message: &str,
) {
    match format {
        LogFormat::Text => {
            if let Some(package) = package {
                eprintln!("rpt: {event} [{category}] {} package {package}: {message}", code.label());
            } else {
                eprintln!("rpt: {event} [{category}] {}: {message}", code.label());
            }
        }
        LogFormat::Json => {
            let entry = serde_json::json!({
                "event": event, "category": category.label(),
                "code": code.label(), "package": package,
                "message": message
            });
            eprintln!("{entry}");
        }
    }
}

/// Format the causal chain without duplicating internal category markers.
/// The category is already displayed by the CLI as `[download]` or `[system]`.
pub fn describe(error: &anyhow::Error) -> String {
    let category = error.downcast_ref::<ErrorCategory>();
    let explicit_code = error.downcast_ref::<ErrorCode>();
    error.chain()
        .enumerate()
        .filter(|(index, cause)| {
            !(*index == 0 && category.is_some_and(|kind| cause.to_string() == kind.label()))
                && !explicit_code.is_some_and(|kind| cause.to_string() == kind.label())
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
    #[test]
    fn code_maps_errors() {
        let missing = std::fs::read("/rpt-nonexistent-329080")
            .context("reading manifest").context(ErrorCategory::System).unwrap_err();
        assert_eq!(code(&missing, ErrorCategory::System), ErrorCode::SystemIo);
        let malformed = anyhow::anyhow!("invalid package name")
            .context(ErrorCategory::System);
        assert_eq!(code(&malformed, ErrorCategory::Download), ErrorCode::SystemConfig);
        let checksum = anyhow::anyhow!("SHA-256 mismatch: bash");
        assert_eq!(code(&checksum, ErrorCategory::Download), ErrorCode::DownloadChecksum);
    }

}
