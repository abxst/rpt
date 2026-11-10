//! Read-only inventory of APT source definitions.
//! This module does not yet interpret all APT configuration or Deb822 semantics.
use anyhow::{Context, Result};
use std::{fs, path::Path};

pub fn list_sources(root: &Path) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let legacy = root.join("sources.list");
    if legacy.is_file() {
        collect_legacy(&legacy, &mut result)?;
    }
    let dir = root.join("sources.list.d");
    if dir.is_dir() {
        let mut paths = fs::read_dir(&dir)?
            .map(|e| e.map(|x| x.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        for path in paths {
            match path.extension().and_then(|s| s.to_str()) {
                Some("list") => collect_legacy(&path, &mut result)?,
                Some("sources") => collect_deb822(&path, &mut result)?,
                _ => {}
            }
        }
    }
    Ok(result)
}
fn collect_legacy(path: &Path, out: &mut Vec<String>) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            out.push(line.to_owned());
        }
    }
    Ok(())
}
fn collect_deb822(path: &Path, out: &mut Vec<String>) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    // Preserve complete stanza text, including Signed-By and other fields.
    for stanza in text.split("\n\n") {
        let stanza = stanza.trim();
        if !stanza.is_empty() && !stanza.starts_with('#') {
            out.push(stanza.to_owned());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nonexistent_source_dir_is_empty() {
        let root = std::env::temp_dir().join("rpt-nonexistent-test-directory-987421");
        assert!(list_sources(&root).unwrap().is_empty());
    }
}
