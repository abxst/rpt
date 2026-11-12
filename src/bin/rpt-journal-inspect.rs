//! Read-only transaction journal diagnostics. Never acquires a writer lock
//! and never changes transaction state or performs recovery.
use anyhow::{bail, Context, Result};
use rpt::transaction_journal::{JournalRecord, Phase};
use std::{env, fs, path::PathBuf, process::ExitCode};

fn inspect(dir: &std::path::Path, json: bool) -> Result<bool> {
    let mut records = Vec::new();
    let mut unsafe_state = false;
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "tmp") {
            bail!("incomplete journal write: {}", path.display());
        }
        if !path.extension().is_some_and(|ext| ext == "json") { continue; }
        if !entry.file_type()?.is_file() { bail!("non-regular journal record: {}", path.display()); }
        let record: JournalRecord = serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("invalid journal {}", path.display()))?;
        if !record.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || path.file_stem().and_then(|s| s.to_str()) != Some(record.id.as_str()) {
            bail!("journal identifier mismatch: {}", path.display());
        }
        unsafe_state |= matches!(record.phase, Phase::Prepared | Phase::Running | Phase::Interrupted);
        records.push(record);
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
    } else {
        for record in &records {
            println!("{}\t{:?}\t{}", record.id, record.phase, record.error.as_deref().unwrap_or(""));
        }
    }
    Ok(unsafe_state)
}

fn main() -> ExitCode {
    let mut args = env::args_os();
    let _ = args.next();
    let dir = match args.next() {
        Some(dir) => PathBuf::from(dir),
        None => {
            eprintln!("usage: rpt-journal-inspect <journal-directory> [--json]");
            return ExitCode::from(2);
        }
    };
    let json = match args.next() {
        None => false,
        Some(value) if value == "--json" => true,
        _ => {
            eprintln!("usage: rpt-journal-inspect <journal-directory> [--json]");
            return ExitCode::from(2);
        }
    };
    if args.next().is_some() {
        eprintln!("unexpected argument");
        return ExitCode::from(2);
    }
    match inspect(&dir, json) {
        Ok(true) => {
            eprintln!("rpt: unfinished transaction detected; manual inspection required");
            ExitCode::FAILURE
        }
        Ok(false) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rpt: journal inspection failed: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_unfinished_and_rejects_mismatched_ids() {
        let dir = env::temp_dir().join(format!("rpt-inspect-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&dir).unwrap();
        assert!(!inspect(&dir, false).unwrap());
        let record = JournalRecord { id: "tx".into(), phase: Phase::Running, error: None };
        fs::write(dir.join("tx.json"), serde_json::to_vec(&record).unwrap()).unwrap();
        assert!(inspect(&dir, false).unwrap());
        fs::rename(dir.join("tx.json"), dir.join("other.json")).unwrap();
        assert!(inspect(&dir, false).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
