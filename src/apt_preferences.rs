//! Strict, read-only parser for a subset of APT preferences records.
//! Not a pinning evaluator: callers MUST NOT use these records to authorize installs
//! or select a candidate without complete repository provenance and APT semantics.
use anyhow::{bail, Context, Result};
use std::{fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinRecord {
    pub package: String,
    pub pin: String,
    pub priority: i32,
}

/// Parse explicit three-field records. Unsupported fields/semantics fail closed.
/// APT accepts more forms than this strict subset; this is not apt preferences parity.
pub fn parse(input: &str) -> Result<Vec<PinRecord>> {
    let mut records = Vec::new();
    let mut fields: Vec<(String, String)> = Vec::new();
    for (i, raw) in input.lines().chain(std::iter::once("")).enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.len() > 64 * 1024 { bail!("preferences line {} exceeds 64 KiB", i + 1); }
        if line.trim().is_empty() {
            if fields.is_empty() { continue; }
            let mut package = None;
            let mut pin = None;
            let mut priority = None;
            for (key, value) in fields.drain(..) {
                match key.to_ascii_lowercase().as_str() {
                    "package" if package.is_none() => package = Some(value),
                    "pin" if pin.is_none() => pin = Some(value),
                    "pin-priority" if priority.is_none() => priority = Some(value),
                    _ => bail!("duplicate or unsupported APT preferences field: {key}"),
                }
            }
            let package = package.context("APT preferences record missing Package")?;
            let pin = pin.context("APT preferences record missing Pin")?;
            let priority: i32 = priority.context("APT preferences record missing Pin-Priority")?
                .parse().context("invalid Pin-Priority integer")?;
            if !package.split_whitespace().all(valid_package_selector) || package.is_empty() {
                bail!("unsupported Package selector: {package}");
            }
            if !valid_pin(&pin) { bail!("unsupported Pin selector: {pin}"); }
            records.push(PinRecord { package, pin, priority });
            if records.len() > 100_000 { bail!("too many APT preference records"); }
        } else if line.trim_start().starts_with('#') {
            continue;
        } else if line.starts_with(' ') || line.starts_with('\t') {
            bail!("APT preferences continuation lines are not supported");
        } else {
            let (key, value) = line.split_once(':')
                .context("APT preferences field missing colon")?;
            let value = value.trim();
            if key.trim().is_empty() || value.is_empty() {
                bail!("empty APT preferences field");
            }
            fields.push((key.trim().to_owned(), value.to_owned()));
        }
    }
    Ok(records)
}

fn valid_package_selector(value: &str) -> bool {
    value == "*" || (!value.is_empty() && value.bytes().all(|c| {
        c.is_ascii_alphanumeric() || b"+.-_:".contains(&c)
    }))
}
fn valid_pin(value: &str) -> bool {
    let Some((kind, argument)) = value.split_once(' ') else { return false; };
    let argument = argument.trim();
    if argument.is_empty() || argument.contains('\n') || argument.contains('\r') {
        return false;
    }
    match kind.to_ascii_lowercase().as_str() {
        "version" => !argument.chars().any(char::is_whitespace),
        // Preserve origin/release expressions without pretending to evaluate them.
        "release" | "origin" => true,
        _ => false,
    }
}

/// Evaluate only explicit package names and exact version pins. Repository
/// release/origin selectors and glob patterns require provenance-aware policy
/// evaluation and deliberately fail closed here.
pub fn exact_version_priority(
    records: &[PinRecord], package: &str, version: &str,
) -> Result<Option<i32>> {
    let mut selected = None;
    for record in records {
        for selector in record.package.split_whitespace() {
            if selector == "*" || selector.contains(['*', '?', '[', ']']) {
                bail!("wildcard Package selectors require full APT policy evaluation");
            }
        }
        let Some((kind, pattern)) = record.pin.split_once(' ') else {
            bail!("invalid APT pin selector");
        };
        if !kind.eq_ignore_ascii_case("version")
            || pattern.contains(['*', '?', '[', ']'])
            || pattern.chars().any(char::is_whitespace)
        {
            bail!("only exact version pins are supported by this evaluator");
        }
        if record.package.split_whitespace().any(|name| name == package)
            && pattern == version
        {
            if selected.replace(record.priority).is_some() {
                bail!("multiple matching exact version pins require full APT policy evaluation");
            }
        }
    }
    Ok(selected)
}

/// Inventory both canonical APT preferences locations in deterministic order.
/// Do not follow arbitrary names in preferences.d: only files with accepted
/// APT-style names are read. Any unsupported name with active content fails
/// closed instead of silently allowing a false candidate policy.
pub fn load(apt_root: &Path) -> Result<Vec<PinRecord>> {
    let mut out = Vec::new();
    let main = apt_root.join("preferences");
    if main.is_file() {
        out.extend(parse(&fs::read_to_string(&main).with_context(|| format!("reading {}", main.display()))?)?);
    }
    let dir = apt_root.join("preferences.d");
    if dir.exists() {
        let mut paths = fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .map(|entry| entry.map(|value| value.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        for path in paths {
            if !path.is_file() { continue; }
            let name = path.file_name().and_then(|x| x.to_str()).context("invalid preferences filename")?;
            if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
                bail!("unsupported preferences filename: {name}");
            }
            out.extend(parse(&fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?)?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ordered_multi_record_preferences() {
        let records = parse("# comment\nPackage: bash\nPin: version 5.2*\nPin-Priority: 1001\n\nPackage: *\nPin: release a=noble\nPin-Priority: -1\n").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].package, "bash");
        assert_eq!(records[0].pin, "version 5.2*");
        assert_eq!(records[0].priority, 1001);
        assert_eq!(records[1].priority, -1);
    }
    #[test]
    fn refuses_ambiguous_or_incomplete_records() {
        for value in [
            "Package: bash\nPin: version 1\n",
            "Package: bash\nPin: version 1\nPin-Priority: 1.5\n",
            "Package: bash\nPackage: curl\nPin: version 1\nPin-Priority: 500\n",
            "Package: bash\nPin: nonsense foo\nPin-Priority: 500\n",
            "Package: bash\nPin: version 1\nPin-Priority: 500\nExplanation: x\n",
            "Package: bash\nPin: version 1\n Pin-Priority: 500\n",
        ] {
            assert!(parse(value).is_err(), "should refuse: {value}");
        }
    }
    #[test]
    fn evaluates_only_exact_package_and_version_pins() {
        let records = parse("Package: bash curl\nPin: version 2.0\nPin-Priority: 1001\n").unwrap();
        assert_eq!(exact_version_priority(&records, "bash", "2.0").unwrap(), Some(1001));
        assert_eq!(exact_version_priority(&records, "curl", "1.0").unwrap(), None);
        assert_eq!(exact_version_priority(&records, "other", "2.0").unwrap(), None);
    }
    #[test]
    fn exact_pin_evaluator_rejects_unsupported_and_ambiguous_rules() {
        for input in [
            "Package: *\nPin: version 2.0\nPin-Priority: 500\n",
            "Package: bash\nPin: version 2.*\nPin-Priority: 500\n",
            "Package: bash\nPin: release a=noble\nPin-Priority: 990\n",
        ] {
            let records = parse(input).unwrap();
            assert!(exact_version_priority(&records, "bash", "2.0").is_err());
        }
        let records = parse("Package: bash\nPin: version 2.0\nPin-Priority: 500\n\nPackage: bash\nPin: version 2.0\nPin-Priority: 600\n").unwrap();
        assert!(exact_version_priority(&records, "bash", "2.0").is_err());
    }
    #[test]
    fn ignores_empty_and_comment_only_files() {
        assert!(parse("# comment\n\n #another comment\n").unwrap().is_empty());
    }
}
