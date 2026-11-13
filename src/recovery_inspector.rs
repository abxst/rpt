//! Read-only transaction recovery inspection. Never clears recovery gates.
use crate::transaction_journal::{JournalRecord, Phase};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{fs, path::Path};

#[derive(Debug, Serialize)]
pub struct RecoveryReport {
    pub records: Vec<JournalRecord>,
    pub warnings: Vec<String>,
    pub requires_manual_recovery: bool,
}

pub fn inspect(dir: &Path) -> Result<RecoveryReport> {
    let mut report = RecoveryReport {
        records: Vec::new(),
        warnings: Vec::new(),
        requires_manual_recovery: false,
    };
    if !dir.exists() {
        bail!("journal directory does not exist: {}", dir.display());
    }
    if !fs::symlink_metadata(dir)?.file_type().is_dir() {
        bail!("journal path is not a real directory: {}", dir.display());
    }
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.extension().is_some_and(|ext| ext == "tmp") {
            report.warnings.push(format!("incomplete journal write: {name}"));
            report.requires_manual_recovery = true;
            continue;
        }
        if !path.extension().is_some_and(|ext| ext == "json") {
            continue;
        }
        if !entry.file_type()?.is_file() {
            report.warnings.push(format!("non-regular journal record: {name}"));
            report.requires_manual_recovery = true;
            continue;
        }
        let record = fs::read(&path)
            .with_context(|| format!("reading journal {}", path.display()))
            .and_then(|bytes| serde_json::from_slice::<JournalRecord>(&bytes)
                .with_context(|| format!("parsing journal {}", path.display())));
        match record {
            Ok(record) => {
                if path.file_stem().and_then(|stem| stem.to_str()) != Some(record.id.as_str()) {
                    report.warnings.push(format!("journal filename/id mismatch: {name}"));
                    report.requires_manual_recovery = true;
                }
                if matches!(record.phase, Phase::Prepared | Phase::Running | Phase::Interrupted) {
                    report.requires_manual_recovery = true;
                }
                report.records.push(record);
            }
            Err(error) => {
                report.warnings.push(format!("invalid journal {name}: {error:#}"));
                report.requires_manual_recovery = true;
            }
        }
    }
    report.records.sort_by(|a, b| a.id.cmp(&b.id));
    report.warnings.sort();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_unfinished_and_corrupt_without_modification() {
        let dir = std::env::temp_dir().join(format!("rpt-inspect-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tx.json");
        let original = br#"{"id":"tx","phase":"interrupted","error":"SIGTERM"}"#;
        fs::write(&path, original).unwrap();
        fs::write(dir.join("bad.json"), "{").unwrap();
        fs::write(dir.join(".pending.tmp"), "partial").unwrap();
        let report = inspect(&dir).unwrap();
        assert!(report.requires_manual_recovery);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].phase, Phase::Interrupted);
        assert_eq!(report.warnings.len(), 2);
        assert_eq!(fs::read(path).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }
}
