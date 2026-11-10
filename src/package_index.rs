//! Debian Packages index parser. Reads local uncompressed indexes only.
//! The caller must first authenticate repository Release metadata and hashes.
use anyhow::{bail, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub fields: BTreeMap<String, String>,
}

impl Record {
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
}

/// Parse Debian RFC822-style control stanzas and continuation lines.
/// A single-space dot is the conventional empty description paragraph.
pub fn parse(input: &str) -> Result<Vec<Record>> {
    let mut records = Vec::new();
    let mut fields = BTreeMap::new();
    let mut last_key: Option<String> = None;
    for (line_num, raw) in input.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            if !fields.is_empty() {
                records.push(Record { fields: std::mem::take(&mut fields) });
            }
            last_key = None;
            continue;
        }
        if (line.starts_with(' ') || line.starts_with('\t')) {
            let key = last_key.as_ref().ok_or_else(|| anyhow::anyhow!(
                "orphan continuation at line {}", line_num + 1
            ))?;
            let value = fields.get_mut(key).expect("continuation key tracked");
            value.push('\n');
            value.push_str(&line[1..]);
            continue;
        }
        let (key, value) = line.split_once(':').ok_or_else(|| anyhow::anyhow!(
            "missing field separator at line {}", line_num + 1
        ))?;
        if key.is_empty() || !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
            bail!("invalid field name at line {}", line_num + 1);
        }
        if fields.contains_key(key) {
            bail!("duplicate field {key} at line {}", line_num + 1);
        }
        fields.insert(key.to_owned(), value.trim_start().to_owned());
        last_key = Some(key.to_owned());
    }
    if !fields.is_empty() { records.push(Record { fields }); }
    Ok(records)
}

#[derive(Debug, Clone)]
pub struct PackageEntry {
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub filename: String,
    pub size: u64,
    pub sha256: String,
    pub depends: Option<String>,
}

pub fn entries(input: &str) -> Result<Vec<PackageEntry>> {
    parse(input)?.into_iter().map(|record| {
        let get = |key: &str| -> Result<String> {
            Ok(record.field(key).ok_or_else(|| anyhow::anyhow!("missing {key}"))?.to_owned())
        };
        let name = get("Package")?;
        let version = get("Version")?;
        let architecture = get("Architecture")?;
        let filename = get("Filename")?;
        let size = get("Size")?.parse::<u64>()?;
        let sha256 = get("SHA256")?;
        if sha256.len() != 64 || hex::decode(&sha256).is_err() {
            bail!("invalid SHA256 for {name}");
        }
        if filename.starts_with('/') || filename.split('/').any(|x| x == ".." || x.is_empty()) {
            bail!("unsafe Filename for {name}");
        }
        Ok(PackageEntry {
            name, version, architecture, filename, size, sha256,
            depends: record.field("Depends").map(str::to_owned)
        })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_and_records() {
        let input = "Package: test\nVersion: 1.0-1\nArchitecture: amd64\nFilename: pool/t/test.deb\nSize: 4\nSHA256: abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd\nDepends: libc6 (>= 2.39),\n coreutils\nDescription: example\n .\n second paragraph\n\n";
        let items = entries(input).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].depends.as_deref(), Some("libc6 (>= 2.39),\ncoreutils"));
        assert_eq!(parse(input).unwrap()[0].field("Description"), Some("example\n.\nsecond paragraph"));
    }
    #[test]
    fn rejects_unsafe_filenames_and_orphan_lines() {
        assert!(parse(" orphan\n").is_err());
        assert!(parse("Package: x\nPackage: y\n").is_err());
        assert!(parse("broken\n").is_err());
    }
}
