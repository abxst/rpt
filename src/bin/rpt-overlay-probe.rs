//! Privileged OverlayFS mount probe, confined to a disposable mount namespace.
//! This is NOT a package installer, chroot, or security sandbox.
use anyhow::{bail, Context, Result};
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<()> {
    let mut args = env::args_os();
    let executable = args.next().context("missing executable")?;
    match args.next().as_deref() {
        Some(arg) if arg == "probe" => {
            let lower = PathBuf::from(args.next().context("missing lower directory")?);
            let base = PathBuf::from(args.next().context("missing staging directory")?);
            if args.next().is_some() { bail!("unexpected argument"); }
            let lower = fs::canonicalize(lower)?;
            let base = fs::canonicalize(base)?;
            if lower == std::path::Path::new("/") { bail!("live root cannot be probed"); }
            // A new mount namespace prevents propagation of probe mounts into
            // the host namespace. It is NOT sufficient to isolate dpkg scripts.
            let status = Command::new("unshare")
                .args(["--mount", "--fork", "--propagation", "private", "--"])
                .arg(executable).arg("--child").arg(lower).arg(base)
                .status().context("starting private mount namespace (requires CAP_SYS_ADMIN)")?;
            if !status.success() { bail!("isolated OverlayFS probe failed: {status}"); }
            println!("OverlayFS isolated mount probe passed; no package installed");
        }
        Some(arg) if arg == "--child" => {
            let lower = PathBuf::from(args.next().context("missing lower")?);
            let base = PathBuf::from(args.next().context("missing base")?);
            if args.next().is_some() { bail!("unexpected argument"); }
            // The child must never trust an unverified inherited mount
            // namespace: only allow a private namespace compared to PID 1.
            let ours = fs::read_link("/proc/self/ns/mnt")?;
            let init = fs::read_link("/proc/1/ns/mnt")?;
            if ours == init { bail!("refusing to mount in the init mount namespace"); }
            let stage = rpt::overlay_stage::OverlayStage::prepare(&lower, &base.join("probe"))?;
            for path in [&stage.lower, &stage.upper, &stage.work] {
                if path.to_string_lossy().chars().any(|c| c == ',' || c == ':' || c == '\\' || c.is_control()) {
                    bail!("unsupported character in OverlayFS mount path");
                }
            }
            let options = format!("lowerdir={},upperdir={},workdir={}",
                stage.lower.display(), stage.upper.display(), stage.work.display());
            let status = Command::new("mount").args(["-t", "overlay", "overlay", "-o"])
                .arg(options).arg(&stage.merged).status().context("mounting OverlayFS")?;
            if !status.success() { bail!("overlay mount failed: {status}"); }
            if !stage.is_mounted()? { bail!("overlay mount not visible in child namespace"); }
            if env::var_os("RPT_OVERLAY_INTEGRATION_FIXTURE").is_some() {
                // The fixture is opt-in and must never operate on a live root.
                // Require the exact known fixture files before any writes.
                for name in ["modified", "deleted", "untouched"] {
                    if !stage.lower.join(name).is_file() {
                        bail!("missing disposable integration fixture: {name}");
                    }
                }
                if fs::read_to_string(stage.lower.join("modified"))? != "original\\n"
                    || fs::read_to_string(stage.lower.join("deleted"))? != "remove-me\\n"
                    || fs::read_to_string(stage.lower.join("untouched"))? != "untouched\\n" {
                    bail!("unexpected fixture contents");
                }
                fs::write(stage.merged.join("modified"), "changed\\n")?;
                fs::remove_file(stage.merged.join("deleted"))?;
                fs::write(stage.merged.join("created"), "new\\n")?;
                fs::create_dir(stage.merged.join("new-directory"))?;
                if fs::read_to_string(stage.merged.join("modified"))? != "changed\\n"
                    || stage.merged.join("deleted").exists()
                    || fs::read_to_string(stage.merged.join("created"))? != "new\\n"
                    || !stage.merged.join("new-directory").is_dir() {
                    bail!("overlay copy-up/whiteout/new-file validation failed");
                }
                if fs::read_to_string(stage.lower.join("modified"))? != "original\\n"
                    || !stage.lower.join("deleted").exists()
                    || stage.lower.join("created").exists() {
                    bail!("lower filesystem was modified");
                }
            }
            // Exit drops the entire mount namespace. No copy-back, no dpkg.
        }
        _ => bail!("usage: rpt-overlay-probe probe <lower-directory> <staging-directory>"),
    }
    Ok(())
}
