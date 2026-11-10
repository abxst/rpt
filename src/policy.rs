//! Conservative read-only APT policy baseline. NOT apt-cache policy parity.
//! Only models default repository priority 500 and installed priority 100.
use crate::{cache_query, version};
use anyhow::{bail, Context, Result};
use std::{cmp::Ordering, fs, path::Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyPreview {
    pub installed: Option<String>,
    pub candidate: Option<String>,
    pub versions: Vec<String>,
}

/// Reject conventional preference files and target-release settings instead of
/// knowingly giving an inaccurate result for a customized APT policy.
pub fn ensure_no_custom_policy(apt_root: &Path) -> Result<()> {
    let prefs = apt_root.join("preferences");
    if prefs.exists() && prefs.is_file() && active_lines(&prefs)? {
        bail!("APT pinning detected in {}; candidate preview is unsupported", prefs.display());
    }
    let prefs_dir = apt_root.join("preferences.d");
    if prefs_dir.exists() {
        for ent in fs::read_dir(&prefs_dir).with_context(|| format!("reading {}", prefs_dir.display()))? {
            let path = ent?.path();
            if path.is_file() && active_lines(&path)? {
                bail!("APT pinning detected in {}; candidate preview is unsupported", path.display());
            }
        }
    }
    let conf = apt_root.join("apt.conf");
    if conf.is_file() && declares_target_release(&conf)? {
        bail!("APT target release configured in {}; candidate preview is unsupported", conf.display());
    }
    let conf_dir = apt_root.join("apt.conf.d");
    if conf_dir.exists() {
        for ent in fs::read_dir(&conf_dir).with_context(|| format!("reading {}", conf_dir.display()))? {
            let path = ent?.path();
            if path.is_file() && declares_target_release(&path)? {
                bail!("APT target release configured in {}; candidate preview is unsupported", path.display());
            }
        }
    }
    Ok(())
}

fn active_lines(path: &Path) -> Result<bool> {
    Ok(fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?
        .lines().any(|line| {
            let trim = line.trim();
            !trim.is_empty() && !trim.starts_with('#')
        }))
}

fn declares_target_release(path: &Path) -> Result<bool> {
    let config = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(config.lines().any(|line| {
        let body = line.split("//").next().unwrap_or("").trim().to_ascii_lowercase();
        !body.is_empty() && (body.contains("default-release") || body.contains("target-release"))
    }))
}

/// Default-priority preview; installed versions newer than repository versions
/// cannot be downgraded at priority 500 (APT needs priority >=1000 for that).
pub fn choose_default_candidate(
    versions: &[String], installed: Option<&str>,
) -> Result<Option<String>> {
    let mut selected: Option<&str> = None;
    for version_str in versions {
        version::compare(version_str, version_str)?;
        if selected.is_none_or(|current| version::compare(version_str, current).unwrap_or(Ordering::Less) == Ordering::Greater) {
            selected = Some(version_str);
        }
    }
    if let Some(value) = installed {
        version::compare(value, value)?;
        if selected.is_none_or(|current| version::compare(value, current).unwrap_or(Ordering::Less) == Ordering::Greater) {
            // No default-priority repository can cause a downgrade.
            return Ok(Some(value.to_owned()));
        }
    }
    Ok(selected.map(str::to_owned))
}

pub fn preview(
    cache: &Path, status_file: &Path, apt_root: &Path, package: &str, arch: &str,
) -> Result<PolicyPreview> {
    ensure_no_custom_policy(apt_root)?;
    let versions = cache_query::cache_versions(cache, package, arch)?;
    let installed = cache_query::installed_version(status_file, package, arch)?.map(|p| p.version);
    let candidate = choose_default_candidate(&versions, installed.as_deref())?;
    Ok(PolicyPreview { installed, candidate, versions })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_priority_and_no_downgrade() {
        let available = vec!["2.0".to_owned(), "1.0".to_owned()];
        assert_eq!(choose_default_candidate(&available, Some("1.0")).unwrap().as_deref(), Some("2.0"));
        assert_eq!(choose_default_candidate(&available, Some("3.0")).unwrap().as_deref(), Some("3.0"));
        assert_eq!(choose_default_candidate(&[], Some("3.0")).unwrap().as_deref(), Some("3.0"));
        assert_eq!(choose_default_candidate(&available, None).unwrap().as_deref(), Some("2.0"));
        assert!(choose_default_candidate(&["invalid".to_string()], None).is_err());
    }
    #[test]
    fn refuses_pin_preferences() {
        let root = std::env::temp_dir().join(format!("rpt-policy-pin-{}", std::process::id()));
        fs::create_dir_all(root.join("preferences.d")).unwrap();
        fs::write(root.join("preferences.d/custom"), "Package: bash\nPin: release a=noble\nPin-Priority: 999\n").unwrap();
        assert!(ensure_no_custom_policy(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn refuses_target_release() {
        let root = std::env::temp_dir().join(format!("rpt-policy-target-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("apt.conf"), "APT::Default-Release \"noble\";").unwrap();
        assert!(ensure_no_custom_policy(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
