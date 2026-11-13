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
    // APT_CONFIG can redirect or extend the configuration we would otherwise inspect.
    // Do not claim a default-priority candidate without evaluating that configuration.
    if std::env::var_os("APT_CONFIG").is_some() {
        bail!("APT_CONFIG override detected; candidate preview is unsupported");
    }
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
    ensure_supported_apt_configuration(apt_root)?;
    Ok(())
}

/// Reject config indirection and target-release options while allowing the
/// separate read-only preference parser to inspect pin records.
pub fn ensure_supported_apt_configuration(apt_root: &Path) -> Result<()> {
    if std::env::var_os("APT_CONFIG").is_some() {
        bail!("APT_CONFIG override detected; pin inspection is unsupported");
    }
    let conf = apt_root.join("apt.conf");
    if conf.is_file() && declares_unsupported_policy_config(&conf)? {
        bail!("APT policy configuration unsupported in {}; candidate preview is unsupported", conf.display());
    }
    let conf_dir = apt_root.join("apt.conf.d");
    if conf_dir.exists() {
        for ent in fs::read_dir(&conf_dir).with_context(|| format!("reading {}", conf_dir.display()))? {
            let path = ent?.path();
            if path.is_file() && declares_unsupported_policy_config(&path)? {
                bail!("APT policy configuration unsupported in {}; candidate preview is unsupported", path.display());
            }
        }
    }
    Ok(())
}

/// Inspect an intentionally narrow APT configuration subset for read-only
/// candidate diagnostics. Unknown active statements fail closed: they can
/// change policy semantics, including hidden config/includes.
pub fn read_default_release(apt_root: &Path) -> Result<Option<String>> {
    if std::env::var_os("APT_CONFIG").is_some() {
        bail!("APT_CONFIG override detected; target release inspection unsupported");
    }
    let mut paths = vec![apt_root.join("apt.conf")];
    let dir = apt_root.join("apt.conf.d");
    if dir.exists() {
        let mut entries = fs::read_dir(&dir)?.map(|e| e.map(|v| v.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort();
        paths.extend(entries);
    }
    let mut target: Option<String> = None;
    for path in paths {
        if !path.exists() { continue; }
        if !path.is_file() { bail!("unsupported APT configuration entry: {}", path.display()); }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        for line in data.lines() {
            let line = line.trim();
            if line.to_ascii_lowercase().starts_with("#include") {
                bail!("APT configuration includes are unsupported: {}", path.display());
            }
            if line.is_empty() || line.starts_with("//") || line.starts_with('#') { continue; }
            let value = line.strip_prefix("APT::Default-Release ")
                .and_then(|v| v.strip_suffix(';'))
                .map(str::trim)
                .and_then(|v| v.strip_prefix('"'))
                .and_then(|v| v.strip_suffix('"'))
                .filter(|v| !v.is_empty() && v.len() <= 128
                    && v.bytes().all(|b| b.is_ascii_alphanumeric() || b".+_-".contains(&b)));
            let Some(value) = value else {
                bail!("unsupported APT policy config statement in {}: {}", path.display(), line);
            };
            if target.is_some() {
                bail!("multiple APT::Default-Release declarations are unsupported");
            }
            target = Some(value.to_owned());
        }
    }
    Ok(target)
}

fn active_lines(path: &Path) -> Result<bool> {
    Ok(fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?
        .lines().any(|line| {
            let trim = line.trim();
            !trim.is_empty() && !trim.starts_with('#')
        }))
}

/// Fail closed for APT settings that can change the default candidate or hide
/// policy inputs elsewhere. Full apt.conf parsing and include resolution are
/// intentionally outside the current read-only policy preview.
fn declares_unsupported_policy_config(path: &Path) -> Result<bool> {
    let config = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut in_block_comment = false;
    for raw_line in config.lines() {
        let mut body = String::new();
        let mut remaining = raw_line;
        loop {
            if in_block_comment {
                if let Some(end) = remaining.find("*/") {
                    remaining = &remaining[end + 2..];
                    in_block_comment = false;
                } else {
                    break;
                }
            } else {
                let block = remaining.find("/*");
                let line_comment = remaining.find("//");
                match (block, line_comment) {
                    (_, Some(line)) if block.is_none_or(|start| line < start) => {
                        body.push_str(&remaining[..line]);
                        break;
                    }
                    (Some(start), _) => {
                        body.push_str(&remaining[..start]);
                        remaining = &remaining[start + 2..];
                        in_block_comment = true;
                    }
                    (None, _) => {
                        body.push_str(remaining);
                        break;
                    }
                }
            }
        }
        let body = body.trim().to_ascii_lowercase();
        if body.starts_with("#include")
            || body.contains("default-release")
            || body.contains("target-release")
            || body.contains("dir::etc")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Default-priority preview; installed versions newer than repository versions
/// cannot be downgraded at priority 500 (APT needs priority >=1000 for that).
pub fn choose_default_candidate(
    versions: &[String], installed: Option<&str>,
) -> Result<Option<String>> {
    let mut selected: Option<&str> = None;
    for version_str in versions {
        version::compare(version_str, version_str)?;
        let is_newer = match selected {
            Some(current) => version::compare(version_str, current)? == Ordering::Greater,
            None => true,
        };
        if is_newer {
            selected = Some(version_str);
        }
    }
    if let Some(value) = installed {
        version::compare(value, value)?;
        let installed_is_newer = match selected {
            Some(current) => version::compare(value, current)? == Ordering::Greater,
            None => true,
        };
        if installed_is_newer {
            // No default-priority repository can cause a downgrade.
            return Ok(Some(value.to_owned()));
        }
    }
    Ok(selected.map(str::to_owned))
}

/// Select a candidate from already authenticated, externally assigned APT
/// priorities. This is a pure policy primitive, not a preferences parser.
pub fn choose_priority_candidate(
    available: &[(String, i32)], installed: Option<&str>,
) -> Result<Option<String>> {
    let mut selected: Option<(&str, i32)> = None;
    for (candidate, priority) in available {
        version::compare(candidate, candidate)?;
        if *priority <= 0 { continue; }
        let replace = match selected {
            None => true,
            Some((current, current_priority)) => {
                *priority > current_priority
                    || (*priority == current_priority
                        && version::compare(candidate, current)? == Ordering::Greater)
            }
        };
        if replace { selected = Some((candidate, *priority)); }
    }
    if let Some(value) = installed {
        version::compare(value, value)?;
        // The installed version participates at priority 100. Equal-version
        // repository entries do not constitute a downgrade.
        let replace = match selected {
            None => true,
            Some((current, priority)) => {
                let order = version::compare(value, current)?;
                (order == Ordering::Greater && priority < 1000)
                    || (order == Ordering::Equal && priority < 100)
                    || (order == Ordering::Less && priority < 100)
            }
        };
        if replace { return Ok(Some(value.to_owned())); }
    }
    Ok(selected.map(|(candidate, _)| candidate.to_owned()))
}

/// Read-only candidate diagnostic for the explicitly supported exact-version
/// preferences subset. All available versions are assumed to have the caller's
/// trusted baseline priority; repository provenance is not inferred here.
pub fn choose_exact_pin_candidate(
    package: &str,
    available: &[String],
    installed: Option<&str>,
    records: &[crate::apt_preferences::PinRecord],
    baseline_priority: i32,
) -> Result<Option<String>> {
    if baseline_priority <= 0 {
        bail!("baseline repository priority must be positive");
    }
    let mut prioritized = Vec::with_capacity(available.len());
    for candidate in available {
        let pinned = crate::apt_preferences::exact_version_priority(
            records, package, candidate,
        )?;
        prioritized.push((candidate.clone(), pinned.unwrap_or(baseline_priority)));
    }
    // Even with no repository candidates, unsupported preference semantics
    // must not silently become a successful installed-only preview.
    if available.is_empty() {
        crate::apt_preferences::exact_version_priority(records, package, "")?;
    }
    choose_priority_candidate(&prioritized, installed)
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
        assert!(choose_default_candidate(&["1.0".to_string(), "invalid".to_string()], None).is_err());
        assert!(choose_default_candidate(&["1.0".to_string()], Some("invalid")).is_err());
    }
    #[test]
    fn priority_candidate_prefers_pin_over_newer_version() {
        let versions = vec![("3.0".into(), 500), ("2.0".into(), 990)];
        assert_eq!(choose_priority_candidate(&versions, None).unwrap().as_deref(), Some("2.0"));
    }
    #[test]
    fn priority_candidate_respects_downgrade_threshold() {
        let low = vec![("1.0".into(), 999)];
        assert_eq!(choose_priority_candidate(&low, Some("2.0")).unwrap().as_deref(), Some("2.0"));
        let boundary = vec![("1.0".into(), 1000)];
        assert_eq!(choose_priority_candidate(&boundary, Some("2.0")).unwrap().as_deref(), Some("1.0"));
        let high = vec![("1.0".into(), 1001)];
        assert_eq!(choose_priority_candidate(&high, Some("2.0")).unwrap().as_deref(), Some("1.0"));
    }
    #[test]
    fn priority_candidate_breaks_equal_pins_by_debian_version() {
        let versions = vec![("1.9".into(), 500), ("2.0~rc1".into(), 500),
            ("2.0".into(), 500)];
        assert_eq!(choose_priority_candidate(&versions, None).unwrap().as_deref(), Some("2.0"));
    }
    #[test]
    fn priority_candidate_keeps_installed_version_without_available_versions() {
        assert_eq!(choose_priority_candidate(&[], Some("3.0")).unwrap().as_deref(), Some("3.0"));
    }
    #[test]
    fn priority_candidate_upgrades_at_normal_repository_priority() {
        assert_eq!(choose_priority_candidate(&[("2.0".into(), 500)], Some("1.0"))
            .unwrap().as_deref(), Some("2.0"));
    }
    #[test]
    fn priority_candidate_ignores_nonpositive_pins_and_rejects_bad_versions() {
        assert_eq!(choose_priority_candidate(&[("2.0".into(), -1)], None).unwrap(), None);
        assert!(choose_priority_candidate(&[("bad version".into(), 500)], None).is_err());
    }
    #[test]
    fn candidate_priority_matrix_covers_ties_and_installed_state() {
        type CandidateCase<'a> = (&'a [(&'a str, i32)], Option<&'a str>, Option<&'a str>);
        let cases: &[CandidateCase<'_>] = &[
            (&[("1.0", 500), ("2.0", 500)], None, Some("2.0")),
            (&[("1.0", 990), ("2.0", 500)], None, Some("1.0")),
            (&[("1.0", -1), ("2.0", 0)], None, None),
            (&[("1.0", 1000)], Some("2.0"), Some("1.0")),
            (&[("1.0", 1001)], Some("2.0"), Some("1.0")),
            (&[("2.0", 500)], Some("1.0"), Some("2.0")),
            (&[("1.0", 500)], Some("2.0"), Some("2.0")),
            (&[("2.0~rc1", 500), ("2.0", 500)], None, Some("2.0")),
            (&[], Some("1.0"), Some("1.0")),
            (&[], None, None),
        ];
        for (available, installed, expected) in cases {
            let owned: Vec<(String, i32)> = available.iter()
                .map(|(version, priority)| ((*version).to_owned(), *priority))
                .collect();
            let actual = choose_priority_candidate(&owned, *installed).unwrap();
            assert_eq!(actual.as_deref(), *expected, "available={available:?} installed={installed:?}");
        }
    }
    #[test]
    fn exact_pin_candidate_prefers_explicit_priority() {
        let records = crate::apt_preferences::parse(
            "Package: app\nPin: version 1.0\nPin-Priority: 990\n",
        ).unwrap();
        let versions = vec!["1.0".into(), "2.0".into()];
        assert_eq!(choose_exact_pin_candidate("app", &versions, None, &records, 500)
            .unwrap().as_deref(), Some("1.0"));
        assert_eq!(choose_exact_pin_candidate("other", &versions, None, &records, 500)
            .unwrap().as_deref(), Some("2.0"));
    }
    #[test]
    fn exact_pin_candidate_respects_downgrade_boundary() {
        let versions = vec!["1.0".into()];
        for (priority, expected) in [(999, "2.0"), (1000, "1.0"), (1001, "1.0")] {
            let records = crate::apt_preferences::parse(&format!(
                "Package: app\nPin: version 1.0\nPin-Priority: {priority}\n"
            )).unwrap();
            assert_eq!(
                choose_exact_pin_candidate("app", &versions, Some("2.0"), &records, 500)
                    .unwrap().as_deref(),
                Some(expected),
                "pin priority {priority}"
            );
        }
    }
    #[test]
    fn exact_pin_candidate_excludes_negative_pins() {
        let records = crate::apt_preferences::parse(
            "Package: app\nPin: version 2.0\nPin-Priority: -1\n",
        ).unwrap();
        let versions = vec!["1.0".into(), "2.0".into()];
        assert_eq!(choose_exact_pin_candidate("app", &versions, None, &records, 500)
            .unwrap().as_deref(), Some("1.0"));
    }
    #[test]
    fn exact_pin_candidate_rejects_unsupported_rules_even_without_versions() {
        let records = crate::apt_preferences::parse(
            "Package: *\nPin: release a=noble\nPin-Priority: 990\n",
        ).unwrap();
        assert!(choose_exact_pin_candidate("app", &[], Some("1.0"), &records, 500).is_err());
        assert!(choose_exact_pin_candidate("app", &["2.0".into()], None, &records, 500).is_err());
        assert!(choose_exact_pin_candidate("app", &["2.0".into()], None, &[], 0).is_err());
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
    fn refuses_indirect_policy_config() {
        let root = std::env::temp_dir().join(format!("rpt-policy-indirect-{}", std::process::id()));
        fs::create_dir_all(root.join("apt.conf.d")).unwrap();
        for config in [
            "#include \"/tmp/custom-apt.conf\";",
            "#includedir \"/tmp/apt-settings\";",
            "Dir::Etc::preferences \"custom-pins\";",
            "/* comment */ APT::Default-Release \"noble\";",
        ] {
            fs::write(root.join("apt.conf.d/99custom"), config).unwrap();
            assert!(ensure_no_custom_policy(&root).is_err(), "must reject: {config}");
        }
        // Comments alone must not block an otherwise default-policy preview.
        fs::write(root.join("apt.conf.d/99custom"), "/* APT::Default-Release \"noble\"; */\n// #include \"/tmp/custom\"\n").unwrap();
        assert!(ensure_no_custom_policy(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn reads_exact_target_release_and_rejects_config_indirection() {
        let root = std::env::temp_dir().join(format!("rpt-target-exact-{}", std::process::id()));
        fs::create_dir_all(root.join("apt.conf.d")).unwrap();
        fs::write(root.join("apt.conf.d/50target"), "// policy\nAPT::Default-Release \"noble\";\n").unwrap();
        assert_eq!(read_default_release(&root).unwrap().as_deref(), Some("noble"));
        fs::write(root.join("apt.conf.d/60other"), "APT::Default-Release \"noble-updates\";").unwrap();
        assert!(read_default_release(&root).is_err());
        fs::remove_file(root.join("apt.conf.d/60other")).unwrap();
        for invalid in [
            "#include \"/tmp/other\";",
            "APT::Default-Release \"noble*\";",
            "Dir::Etc::preferences \"somewhere\";",
            "APT::Default-Release \"noble\"; APT::Something \"else\";",
        ] {
            fs::write(root.join("apt.conf.d/50target"), invalid).unwrap();
            assert!(read_default_release(&root).is_err(), "{invalid}");
        }
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
