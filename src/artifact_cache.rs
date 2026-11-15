//! Explicitly owned RPT artifact-cache lifecycle, separate from APT's
//! /var/cache/apt archives. No host dpkg/APT operation is performed.
//! Mutating clean is opt-in and requires an exact, private directory marker.
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const MARKER: &str = "rpt-artifact-cache-v1\n";
const MARKER_FILE: &str = ".rpt-artifact-cache";
const LOCK_FILE: &str = ".rpt-artifact-lock";
const MAX_ENTRIES: usize = 50_000;
const MAX_DEPTH: usize = 32;

#[derive(Debug, Serialize)]
pub struct CacheLifecycleReport {
    pub schema_version: u32,
    pub root: String,
    pub operation: &'static str,
    pub initialized: bool,
    pub applied: bool,
    pub candidate_files: usize,
    pub candidate_bytes: u64,
    pub host_package_execution_allowed: bool,
    pub warning: &'static str,
}

fn safe_root(path: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("artifact cache root cannot be a symlink");
    }
    let root = fs::canonicalize(path)?;
    let protected = [
        "/", "/etc", "/usr", "/var/lib", "/var/cache/apt",
        "/var/cache/apt/archives", "/bin", "/sbin", "/boot",
        "/dev", "/proc", "/sys", "/run", "/lib", "/lib64",
        "/var/tmp/apt", "/tmp/apt",
    ];
    if root.components().count() < 3
        || protected.iter().any(|value| {
            let protected = Path::new(value);
            root == protected || (protected != Path::new("/") && root.starts_with(protected))
        }) {
        bail!("refusing system or high-level artifact cache directory: {}", root.display());
    }
    if !root.is_dir() {
        bail!("artifact cache root must be a directory");
    }
    Ok(root)
}

fn real_file(path: &Path) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.nlink() != 1 {
        bail!("artifact cache control or candidate must be a single-linked regular file: {}", path.display());
    }
    Ok(meta)
}

fn validate_marker(root: &Path) -> Result<()> {
    let marker = root.join(MARKER_FILE);
    if real_file(&marker)?.len() as usize != MARKER.len()
        || fs::read(&marker)? != MARKER.as_bytes() {
        bail!("missing or invalid exact RPT artifact cache ownership marker");
    }
    Ok(())
}

fn lock(root: &Path, shared: bool) -> Result<File> {
    validate_marker(root)?;
    let path = root.join(LOCK_FILE);
    real_file(&path)?;
    let file = OpenOptions::new().read(true).write(true).open(&path)
        .context("opening RPT artifact cache lock")?;
    if shared {
        FileExt::lock_shared(&file)?;
    } else {
        FileExt::lock_exclusive(&file)?;
    }
    validate_marker(root)?;
    Ok(file)
}

/// Acquires a cooperative reader lease during an entire download batch
/// when the output is an initialized managed artifact cache. Ordinary
/// unmarked destinations preserve their previous behavior.
pub fn download_lease(path: &Path) -> Result<Option<File>> {
    let marker = path.join(MARKER_FILE);
    match fs::symlink_metadata(&marker) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("inspecting RPT cache ownership marker"),
        Ok(_) => {
            let root = safe_root(path)?;
            Ok(Some(lock(&root, true)?))
        }
    }
}

/// Marks a NEW/EMPTY directory as an RPT-owned cache. Never takes over
/// an existing populated directory or the system's real APT cache.
pub fn initialize(path: &Path) -> Result<CacheLifecycleReport> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => fs::create_dir(path)
            .context("create RPT cache root only when its parent already exists")?,
        Err(error) => return Err(error).context("inspecting cache root"),
        Ok(meta) if !meta.file_type().is_dir() => bail!("RPT cache root must be a real directory"),
        Ok(_) => {}
    }
    let root = safe_root(path)?;
    if fs::read_dir(&root)?.next().is_some() {
        bail!("RPT artifact cache initialization requires a completely empty directory");
    }
    let marker = root.join(MARKER_FILE);
    let mut file = OpenOptions::new().write(true).create_new(true).open(&marker)?;
    file.write_all(MARKER.as_bytes())?;
    file.sync_all()?;
    let lock = root.join(LOCK_FILE);
    let lock_file = OpenOptions::new().read(true).write(true).create_new(true).open(lock)?;
    lock_file.sync_all()?;
    File::open(&root)?.sync_all()?;
    Ok(CacheLifecycleReport {
        schema_version: 1, root: root.display().to_string(),
        operation: "initialize", initialized: true, applied: true,
        candidate_files: 0, candidate_bytes: 0,
        host_package_execution_allowed: false,
        warning: "Only an explicitly marked isolated RPT cache may be cleaned; host APT archives are never managed.",
    })
}

#[derive(Debug)]
struct Candidate {
    path: PathBuf,
    dev: u64,
    ino: u64,
    len: u64,
}

fn walk(root: &Path, current: &Path, root_device: u64, depth: usize,
    entries: &mut usize, candidates: &mut Vec<Candidate>) -> Result<()> {
    if depth > MAX_DEPTH { bail!("RPT artifact cache depth limit exceeded"); }
    let meta = fs::symlink_metadata(current)?;
    if meta.dev() != root_device || !meta.file_type().is_dir() {
        bail!("RPT artifact cache contains a mount or unsupported directory: {}", current.display());
    }
    for entry in fs::read_dir(current)? {
        *entries += 1;
        if *entries > MAX_ENTRIES { bail!("RPT artifact cache scan entry limit exceeded"); }
        let entry = entry?;
        let path = entry.path();
        if !path.starts_with(root) { bail!("cache path escaped selected root"); }
        let meta = fs::symlink_metadata(&path)?;
        if meta.dev() != root_device || meta.file_type().is_symlink() {
            bail!("RPT artifact cache contains external/mounted or symlinked entry: {}", path.display());
        }
        if meta.is_dir() {
            walk(root, &path, root_device, depth + 1, entries, candidates)?;
        } else if meta.is_file() {
            if meta.nlink() != 1 {
                bail!("artifact cache contains external hardlink alias: {}", path.display());
            }
            let name = path.file_name().and_then(|item| item.to_str())
                .context("RPT cache filename is not valid UTF-8")?;
            let candidate = name.ends_with(".deb")
                || (name.ends_with(".part") && name.contains(".rpt-"));
            if candidate {
                candidates.push(Candidate {
                    path, dev: meta.dev(), ino: meta.ino(), len: meta.len(),
                });
            }
        } else {
            bail!("RPT artifact cache contains unsupported special file: {}", path.display());
        }
    }
    Ok(())
}

/// Preview or explicitly remove only .deb archives and .rpt-*.part leftovers
/// within the marked cache. Other files and nested directory structure remain.
pub fn clean(path: &Path, apply: bool) -> Result<CacheLifecycleReport> {
    let root = safe_root(path)?;
    let _lock = lock(&root, false)?;
    let device = fs::symlink_metadata(&root)?.dev();
    let mut entries = 0;
    let mut candidates = Vec::new();
    walk(&root, &root, device, 0, &mut entries, &mut candidates)?;
    let mut candidate_bytes = 0u64;
    for candidate in &candidates {
        candidate_bytes = candidate_bytes.checked_add(candidate.len)
            .context("RPT artifact candidate byte count overflow")?;
    }
    // Full scan and hardlink/symlink checks finish before any mutation.
    if apply {
        for candidate in &candidates {
            let meta = real_file(&candidate.path)?;
            if meta.dev() != candidate.dev || meta.ino() != candidate.ino
                || meta.len() != candidate.len {
                bail!("artifact changed since clean preflight; refusing stale deletion");
            }
            fs::remove_file(&candidate.path)?;
        }
        File::open(&root)?.sync_all()?;
    }
    Ok(CacheLifecycleReport {
        schema_version: 1, root: root.display().to_string(),
        operation: "clean", initialized: true, applied: apply,
        candidate_files: candidates.len(), candidate_bytes,
        host_package_execution_allowed: false,
        warning: "Exclusive cooperative cache lock only; no dpkg, no host APT cache changes. External non-cooperating filesystem writers remain unsupported.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn only_explicit_marker_allows_dry_run_and_clean() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("cache");
        assert!(clean(&root, false).is_err());
        assert!(initialize(&root).unwrap().initialized);
        assert!(initialize(&root).is_err());
        fs::create_dir(root.join("pool")).unwrap();
        fs::write(root.join("pool").join("pkg.deb"), b"pkg").unwrap();
        fs::write(root.join("pool").join("pkg.deb.rpt-42.part"), b"x").unwrap();
        fs::write(root.join("README"), b"keep").unwrap();
        let preview = clean(&root, false).unwrap();
        assert_eq!(preview.candidate_files, 2);
        assert_eq!(preview.candidate_bytes, 4);
        assert!(!preview.applied);
        assert!(root.join("pool/pkg.deb").exists());
        let result = clean(&root, true).unwrap();
        assert_eq!(result.candidate_files, 2);
        assert!(!result.host_package_execution_allowed);
        assert!(!root.join("pool/pkg.deb").exists());
        assert!(!root.join("pool/pkg.deb.rpt-42.part").exists());
        assert_eq!(fs::read(root.join("README")).unwrap(), b"keep");
        assert!(clean(&root, true).unwrap().candidate_files == 0);
    }

    #[test]
    fn hostile_symlink_blocks_entire_cleanup_before_deleting_anything() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("cache");
        initialize(&root).unwrap();
        let external = dir.path().join("host-archive.deb");
        fs::write(&external, b"host").unwrap();
        fs::write(root.join("first.deb"), b"artifact").unwrap();
        std::os::unix::fs::symlink(&external, root.join("host-symlink.deb")).unwrap();
        assert!(clean(&root, true).is_err());
        assert!(root.join("first.deb").exists());
        assert_eq!(fs::read(&external).unwrap(), b"host");
        fs::remove_file(root.join("host-symlink.deb")).unwrap();
        fs::hard_link(&external, root.join("host-hardlink.deb")).unwrap();
        assert!(clean(&root, true).is_err());
        assert!(root.join("first.deb").exists());
        assert_eq!(fs::read(&external).unwrap(), b"host");
    }

    #[test]
    fn invalid_owner_marker_and_protected_root_are_rejected() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("do-not-adopt.deb"), b"x").unwrap();
        assert!(initialize(&root).is_err());
        assert!(clean(&root, true).is_err());
        assert!(initialize(Path::new("/var/lib/dpkg")).is_err());
        assert!(clean(Path::new("/var/cache/apt"), true).is_err());
    }
}
