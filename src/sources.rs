//! Bounded read-only inventory of APT source definitions. Strict source
//! semantics are enforced by update::parse_sources before any signed update.
use anyhow::{bail, Context, Result};
use std::{fs, io::ErrorKind, path::Path};

const MAX_FILE_SIZE: u64 = 1024 * 1024;
const MAX_SOURCE_FILES: usize = 256;

fn read_regular(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspecting source {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_SIZE {
        bail!("APT source must be a real regular file <=1 MiB: {}", path.display());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading source {}", path.display()))?;
    if text.len() as u64 > MAX_FILE_SIZE {
        bail!("APT source grew past 1 MiB while reading");
    }
    Ok(text)
}

pub fn list_sources(root: &Path) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let legacy = root.join("sources.list");
    match fs::symlink_metadata(&legacy) {
        Ok(_) => collect_legacy(&legacy, &mut result)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspecting {}", legacy.display())),
    }
    let dir = root.join("sources.list.d");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() => {
            let mut paths = fs::read_dir(&dir)?
                .map(|e| e.map(|x| x.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            if paths.len() > MAX_SOURCE_FILES {
                bail!("APT sources.list.d has too many entries");
            }
            paths.sort();
            for path in paths {
                match path.extension().and_then(|s| s.to_str()) {
                    Some("list") => collect_legacy(&path, &mut result)?,
                    Some("sources") => collect_deb822(&path, &mut result)?,
                    _ => {}
                }
            }
        }
        Ok(_) => bail!("APT sources.list.d must be a real directory"),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspecting {}", dir.display())),
    }
    Ok(result)
}
fn collect_legacy(path: &Path, out: &mut Vec<String>) -> Result<()> {
    let text = read_regular(path)?;
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            out.push(line.to_owned());
        }
    }
    Ok(())
}
fn collect_deb822(path: &Path, out: &mut Vec<String>) -> Result<()> {
    let text = read_regular(path)?;
    let mut stanza = Vec::new();
    let flush = |stanza: &mut Vec<&str>, out: &mut Vec<String>| {
        if !stanza.is_empty() {
            out.push(stanza.join("\n"));
            stanza.clear();
        }
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut stanza, out);
        } else if !line.trim_start().starts_with('#') {
            stanza.push(line);
        }
    }
    flush(&mut stanza, out);
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
    #[test]
    fn blank_lines_and_comments_preserve_multiple_deb822_stanzas() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sources.list.d")).unwrap();
        fs::write(dir.path().join("sources.list.d/repos.sources"),
            "# comment\nTypes: deb\nURIs: https://example.org\n\n  \n# another\nTypes: deb\nURIs: https://mirror.example.org\n").unwrap();
        let entries = list_sources(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].contains("example.org"));
        assert!(entries[1].contains("mirror.example.org"));
    }
    #[test]
    fn symlinked_source_or_directory_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("external.list");
        fs::write(&external, "deb [signed-by=/tmp/key.gpg] https://example.org noble main").unwrap();
        std::os::unix::fs::symlink(&external, dir.path().join("sources.list")).unwrap();
        assert!(list_sources(dir.path()).is_err());
        fs::remove_file(dir.path().join("sources.list")).unwrap();
        let real = dir.path().join("sources-real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("sources.list.d")).unwrap();
        assert!(list_sources(dir.path()).is_err());
    }
}
