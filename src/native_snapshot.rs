//! Verified filesystem checkpoints for *disposable* native rootfs only.
//! Never operates on /, never clears an interrupted transaction journal.
//! This is a full tree COPY, not a crash-atomic snapshot or secure chroot.
use crate::{native_rootfs, sandbox_execution::valid_id, transaction_journal::Journal};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{ffi::OsStrExt, fs::{FileTypeExt, MetadataExt}},
    path::{Path, PathBuf},
    process::Command,
};

const SNAPSHOTS: &str = "native-rootfs-snapshots";
const MAX_RECEIPT: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 250_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeSnapshotReceipt {
    pub schema_version: u32,
    pub id: String,
    pub source_rootfs: String,
    pub snapshot_rootfs: String,
    pub journal_dir: String,
    pub status_sha256: String,
    pub apt_marks_sha256: String,
    pub tree_sha256: String,
    pub created_utc: String,
}

#[derive(Debug, Serialize)]
pub struct NativeRestoreResult {
    pub source_snapshot: String,
    pub new_rootfs: String,
    pub tree_sha256: String,
    pub status_sha256: String,
    pub note: &'static str,
}

fn sha(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

fn hash_bytes(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

/// Directory walk never follows guest symlinks. Include paths, file content,
/// UNIX mode/ownership and special-file device identity. This is a byte-level
/// consistency witness, not proof of preserved ACLs, hardlink topology,
/// xattrs, application state or durability across power loss.
fn tree_hash(root: &Path) -> Result<String> {
    fn walk(root: &Path, dir: &Path, hash: &mut Sha256,
        count: &mut usize, depth: usize) -> Result<()> {
        if depth > 128 { bail!("rootfs tree exceeds supported directory depth"); }
        let mut paths = fs::read_dir(dir)?
            .map(|e| e.map(|v| v.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        paths.sort_by(|a, b| a.file_name().unwrap().as_bytes()
            .cmp(b.file_name().unwrap().as_bytes()));
        for path in paths {
            *count += 1;
            if *count > MAX_ENTRIES { bail!("rootfs snapshot exceeds {MAX_ENTRIES} entries"); }
            if *count % 5_000 == 0 {
                eprintln!("RPT native snapshot: verified {count} filesystem entries...");
            }
            let meta = fs::symlink_metadata(&path)?;
            let kind = meta.file_type();
            hash_bytes(hash, path.strip_prefix(root)?.as_os_str().as_bytes());
            hash.update(meta.mode().to_le_bytes());
            hash.update(meta.uid().to_le_bytes());
            hash.update(meta.gid().to_le_bytes());
            if kind.is_file() {
                hash.update(b"file");
                hash.update(meta.len().to_le_bytes());
                let mut file = File::open(&path)?;
                let mut block = [0u8; 65_536];
                loop {
                    let bytes = file.read(&mut block)?;
                    if bytes == 0 { break; }
                    hash.update(&block[..bytes]);
                }
            } else if kind.is_symlink() {
                hash.update(b"symlink");
                hash_bytes(hash, fs::read_link(&path)?.as_os_str().as_bytes());
            } else if kind.is_dir() {
                hash.update(b"directory");
                walk(root, &path, hash, count, depth + 1)?;
            } else if kind.is_socket() {
                bail!("cannot checkpoint a rootfs containing a Unix socket: {}", path.display());
            } else if kind.is_char_device() || kind.is_block_device() || kind.is_fifo() {
                hash.update(b"special");
                hash.update(meta.rdev().to_le_bytes());
            } else {
                bail!("unsupported filesystem object in disposable guest: {}", path.display());
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    hash.update(b"rpt-native-tree-v1");
    let mut count = 0;
    walk(root, root, &mut hash, &mut count, 0)?;
    Ok(hex::encode(hash.finalize()))
}

fn checked_destination(path: &Path, existing: &[&Path]) -> Result<PathBuf> {
    let basename = path.file_name().and_then(|x| x.to_str())
        .context("destination must have a UTF-8 basename")?;
    if basename.is_empty() || basename.len() > 100
        || basename == "." || basename == ".."
        || !basename.bytes().all(|b| b.is_ascii_alphanumeric()
            || matches!(b, b'_' | b'-' | b'.')) {
        bail!("unsafe native rootfs destination basename");
    }
    let parent = fs::canonicalize(path.parent()
        .context("destination needs an existing parent directory")?)?;
    let target = parent.join(basename);
    if !target.starts_with("/tmp/") && !target.starts_with("/var/tmp/") {
        bail!("native snapshot destinations are restricted to /tmp or /var/tmp");
    }
    for root in existing {
        if target.starts_with(root) || root.starts_with(&target) {
            bail!("native snapshot paths cannot overlap source, snapshot or journal");
        }
    }
    match fs::symlink_metadata(&target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => bail!("destination already exists; never overwrite a guest rootfs"),
    }
    Ok(target)
}

fn copy_rootfs(from: &Path, to: &Path, expected_tree: &str) -> Result<()> {
    fs::create_dir(to).context("exclusively creating new rootfs destination")?;
    // GNU cp preserves ownership, mode, symlinks and most metadata; reflinks
    // are opportunistic, never hardlinks to the mutable source.
    let result = Command::new("cp")
        .args(["-a", "--one-file-system", "--reflink=auto", "--"])
        .arg(from.join(".")).arg(to).status()
        .context("copying disposable rootfs checkpoint")?;
    if !result.success() {
        bail!("copy failed; partial destination retained for manual inspection");
    }
    let checked = native_rootfs::rootfs(to)?;
    if tree_hash(&checked)? != expected_tree {
        bail!("copied rootfs differs from source tree; destination retained for inspection");
    }
    if !dpkg_clean(&checked)? {
        bail!("copied guest has an incomplete dpkg database");
    }
    Ok(())
}

fn dpkg_clean(root: &Path) -> Result<bool> {
    let output = Command::new("dpkg").arg(format!("--root={}", root.display()))
        .arg("--audit").output().context("auditing guest dpkg database")?;
    if !output.status.success() {
        bail!("guest dpkg audit command failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(output.stdout.is_empty())
}

fn snapshot_dir(journal: &Path) -> Result<PathBuf> {
    let path = journal.join(SNAPSHOTS);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&path)?,
        _ => bail!("snapshot receipt directory must be a real directory"),
    }
    Ok(path)
}

/// The journal lock prevents *cooperating RPT* transactions during the copy.
/// External writers are not confined; source is rehashed after copying and
/// any detected concurrent edits fail closed.
pub fn create(root: &Path, destination: &Path, journal: &Path,
    id: &str) -> Result<NativeSnapshotReceipt> {
    if !valid_id(id) { bail!("invalid snapshot identifier"); }
    let root = native_rootfs::rootfs(root)?;
    if !dpkg_clean(&root)? { bail!("guest dpkg audit is not clean; refusing checkpoint"); }
    if let Ok(meta) = fs::symlink_metadata(journal) {
        if !meta.file_type().is_dir() { bail!("journal cannot be a symlink or special file"); }
    }
    fs::create_dir_all(journal)?;
    let journal = fs::canonicalize(journal)?;
    if journal == Path::new("/") || journal.starts_with(&root) {
        bail!("snapshot journal must be outside the source guest");
    }
    let dest = checked_destination(destination, &[&root, &journal])?;
    let _lock = Journal::wait(&journal).context("checkpoint requires a clean, unlocked transaction journal")?;
    if !dpkg_clean(&root)? {
        bail!("guest dpkg changed or became incomplete before checkpoint lock");
    }
    let receipts = snapshot_dir(&journal)?;
    let path = receipts.join(format!("{id}.json"));
    if fs::symlink_metadata(&path).is_ok() {
        bail!("checkpoint receipt already exists; transaction IDs cannot be reused");
    }
    // All evidence must describe the same source while the cooperative
    // transaction lock is held. Inconsistent copies never receive receipts.
    eprintln!("RPT native snapshot: hashing source rootfs {}", root.display());
    let original_tree = tree_hash(&root)?;
    let status_sha = sha(&native_rootfs::status(&root)?);
    let marks_sha = native_rootfs::mark_state_digest(&root)?;
    eprintln!("RPT native snapshot: copying and verifying checkpoint at {}", dest.display());
    copy_rootfs(&root, &dest, &original_tree)?;
    eprintln!("RPT native snapshot: checking source is unchanged");
    if tree_hash(&root)? != original_tree
        || sha(&native_rootfs::status(&root)?) != status_sha
        || native_rootfs::mark_state_digest(&root)? != marks_sha {
        bail!("source rootfs changed during checkpoint; no receipt written");
    }
    let receipt = NativeSnapshotReceipt {
        schema_version: 1,
        id: id.to_owned(),
        source_rootfs: root.display().to_string(),
        snapshot_rootfs: dest.display().to_string(),
        journal_dir: journal.display().to_string(),
        status_sha256: status_sha,
        apt_marks_sha256: marks_sha,
        tree_sha256: original_tree,
        created_utc: chrono::Utc::now().to_rfc3339(),
    };
    // Refuse overwriting even an incomplete receipt. On write failure,
    // preserve the copied rootfs for inspection.
    let mut file = OpenOptions::new().create_new(true).write(true).open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    File::open(&receipts)?.sync_all()?;
    eprintln!("RPT native snapshot: receipt written and synced at {}", path.display());
    Ok(receipt)
}

pub fn verify(path: &Path) -> Result<NativeSnapshotReceipt> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > MAX_RECEIPT {
        bail!("snapshot receipt must be a regular file <=64 KiB");
    }
    let receipt: NativeSnapshotReceipt = serde_json::from_slice(&fs::read(path)?)?;
    if receipt.schema_version != 1 || !valid_id(&receipt.id)
        || receipt.tree_sha256.len() != 64
        || !receipt.tree_sha256.bytes().all(|c| c.is_ascii_hexdigit())
        || receipt.status_sha256.len() != 64
        || !receipt.status_sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid or unsupported native snapshot receipt");
    }
    let parent = fs::canonicalize(path.parent().context("receipt has no parent")?)?;
    let journal = fs::canonicalize(&receipt.journal_dir)?;
    if parent != journal.join(SNAPSHOTS)
        || path.file_name().and_then(|x| x.to_str()) != Some(format!("{}.json", receipt.id).as_str()) {
        bail!("snapshot receipt path is not bound to the recorded journal");
    }
    let snapshot = native_rootfs::rootfs(Path::new(&receipt.snapshot_rootfs))?;
    eprintln!("RPT native snapshot: verifying snapshot tree {}", snapshot.display());
    if snapshot.to_string_lossy() != receipt.snapshot_rootfs {
        bail!("snapshot path identity changed");
    }
    if tree_hash(&snapshot)? != receipt.tree_sha256
        || sha(&native_rootfs::status(&snapshot)?) != receipt.status_sha256
        || native_rootfs::mark_state_digest(&snapshot)? != receipt.apt_marks_sha256 {
        bail!("checkpoint content has changed since its receipt was sealed");
    }
    if !dpkg_clean(&snapshot)? { bail!("snapshot dpkg database is broken"); }
    eprintln!("RPT native snapshot: verified receipt {}", path.display());
    Ok(receipt)
}

/// Restores into a new path. The original (possibly failed) guest, its
/// checkpoint and its unfinished journal are NEVER modified or unlocked.
pub fn restore(receipt_path: &Path, destination: &Path) -> Result<NativeRestoreResult> {
    eprintln!("RPT native snapshot: restore preflight, verifying receipt");
    let receipt = verify(receipt_path)?;
    let snapshot = native_rootfs::rootfs(Path::new(&receipt.snapshot_rootfs))?;
    let journal = fs::canonicalize(&receipt.journal_dir)?;
    let source = Path::new(&receipt.source_rootfs);
    let dest = checked_destination(destination, &[&snapshot, source, &journal])?;
    eprintln!("RPT native snapshot: copying into fresh rootfs {}", dest.display());
    copy_rootfs(&snapshot, &dest, &receipt.tree_sha256)?;
    eprintln!("RPT native snapshot: rechecking source checkpoint integrity");
    if tree_hash(&snapshot)? != receipt.tree_sha256 {
        bail!("checkpoint changed during restore; recovered copy is not trusted");
    }
    eprintln!("RPT native snapshot: restore verified at {}", dest.display());
    Ok(NativeRestoreResult {
        source_snapshot: receipt.snapshot_rootfs,
        new_rootfs: dest.display().to_string(),
        tree_sha256: receipt.tree_sha256,
        status_sha256: receipt.status_sha256,
        note: "Fresh disposable rootfs only. Failed original and journal untouched. No atomic swap, automatic rollback, or authorization to retry an unfinished transaction.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tree_integrity_detects_content_and_symlink_mutation() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("a")).unwrap();
        fs::write(dir.path().join("a/file"), b"original").unwrap();
        std::os::unix::fs::symlink("a/file", dir.path().join("link")).unwrap();
        let first = tree_hash(dir.path()).unwrap();
        assert_eq!(first, tree_hash(dir.path()).unwrap());
        fs::write(dir.path().join("a/file"), b"changed").unwrap();
        assert_ne!(first, tree_hash(dir.path()).unwrap());
        let second = tree_hash(dir.path()).unwrap();
        fs::remove_file(dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink("../outside", dir.path().join("link")).unwrap();
        assert_ne!(second, tree_hash(dir.path()).unwrap());
    }

    #[test]
    fn destination_rejects_overlap_and_existing_paths() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        assert!(checked_destination(&source, &[&source]).is_err());
        assert!(checked_destination(&dir.path().join("new"), &[dir.path()]).is_err());
        assert!(checked_destination(&dir.path().join("fresh"), &[&source]).is_ok());
        assert!(checked_destination(Path::new("/etc/not-a-rootfs"), &[&source]).is_err());
    }
}
