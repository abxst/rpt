//! Read-only, deliberately restricted APT preference evaluator.
//! Supports exact package selectors and exact version/release identifiers only.
//! This is a diagnostic, not APT candidate selection or authorization to install.
use crate::{apt_preferences::PinRecord, package_provenance::VerifiedVersionOrigin};
use anyhow::{bail, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluatedPin {
    pub version: String,
    pub repository_uri: String,
    pub suite: String,
    pub priority: i32,
}

/// APT policy accepts many wildcard, origin, architecture and preference forms.
/// Reject all unsupported forms before evaluating any version, including records
/// for unrelated packages: otherwise a caller could mistake partial policy for
/// a complete and authoritative result.
fn validate_record(record: &PinRecord) -> Result<()> {
    if record.package == "*" || record.package.contains(char::is_whitespace)
        || !record.package.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b)) {
        bail!("unsupported pin Package selector: {}", record.package);
    }
    let (kind, value) = record.pin.split_once(' ').ok_or_else(|| anyhow::anyhow!("invalid Pin"))?;
    let value = value.trim();
    let valid = match kind {
        "version" => !value.is_empty() && !value.contains('*') && !value.contains('?')
            && crate::version::compare(value, value).is_ok(),
        "release" => value.starts_with("a=") && value.len() > 2
            && value[2..].bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        _ => false,
    };
    if !valid { bail!("unsupported pin selector: {}", record.pin); }
    if record.priority == 0 {
        bail!("APT pin priority zero has undefined behavior and is unsupported");
    }
    Ok(())
}

fn matches(record: &PinRecord, name: &str, origin: &VerifiedVersionOrigin) -> bool {
    if record.package != name { return false; }
    let Some((kind, value)) = record.pin.split_once(' ') else { return false };
    let value = value.trim();
    match kind {
        "version" => origin.version == value,
        "release" => origin.release.archive.as_deref() == Some(&value[2..]),
        _ => false,
    }
}

/// Report priority per *verified occurrence*, without collapsing two origins
/// that advertise identical versions. For this restricted diagnostic first
/// matching explicit package rule wins; default repository priority is 500.
/// It deliberately refuses wildcard/general pins rather than approximating APT.
pub fn evaluate(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
) -> Result<Vec<EvaluatedPin>> {
    if package.is_empty() { bail!("empty package name"); }
    for record in records { validate_record(record)?; }
    Ok(origins.iter().map(|origin| {
        let priority = records.iter().find(|r| matches(r, package, origin))
            .map_or(500, |r| r.priority);
        EvaluatedPin {
            version: origin.version.clone(),
            repository_uri: origin.repository_uri.clone(),
            suite: origin.suite.clone(),
            priority,
        }
    }).collect())
}

/// Candidate diagnostic for the restricted policy subset. This is NOT an
/// authoritative apt-cache policy result, nor permission to install/upgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidatePreview {
    pub installed: Option<String>,
    pub candidate: Option<String>,
    /// One entry per unique version; each version receives the highest
    /// applicable repository occurrence priority (plus installed priority 100).
    pub priorities: Vec<(String, i32)>,
}

/// APT-style priority ordering for a restricted, fully validated set of pins.
/// Higher priority wins, then newer Debian version. Downgrades are prohibited
/// unless the candidate's effective priority is >= 1000. Version 0 priority
/// is rejected, as its behavior is not defined by APT policy.
pub fn preview_candidate(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
    installed: Option<&str>,
) -> Result<CandidatePreview> {
    use std::{cmp::Ordering, collections::BTreeMap};
    let evaluated = evaluate(package, origins, records)?;
    let mut values = BTreeMap::<String, i32>::new();
    for entry in evaluated {
        values.entry(entry.version).and_modify(|p| *p = (*p).max(entry.priority))
            .or_insert(entry.priority);
    }
    if let Some(current) = installed {
        crate::version::compare(current, current)?;
        values.entry(current.to_owned()).and_modify(|p| *p = (*p).max(100))
            .or_insert(100);
    }
    let mut ordered = values.into_iter().collect::<Vec<_>>();
    // Fail fast on invalid versions before sorting; comparator errors must not
    // be silently converted to equality.
    for (version, _) in &ordered { crate::version::compare(version, version)?; }
    ordered.sort_by(|(a, _), (b, _)| crate::version::compare(b, a).unwrap_or(Ordering::Equal));
    let mut best: Option<(&str, i32)> = None;
    for (version, priority) in &ordered {
        if *priority <= 0 { continue; }
        if let Some(current) = installed {
            if crate::version::compare(version, current)? == Ordering::Less && *priority < 1000 {
                continue;
            }
        }
        if best.is_none_or(|(chosen, p)| *priority > p
            || (*priority == p && crate::version::compare(version, chosen)
                .is_ok_and(|order| order == Ordering::Greater))) {
            best = Some((version, *priority));
        }
    }
    Ok(CandidatePreview {
        installed: installed.map(str::to_owned),
        candidate: best.map(|(version, _)| version.to_owned()),
        priorities: ordered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_provenance::ReleaseIdentity;

    fn origin(version: &str, archive: &str) -> VerifiedVersionOrigin {
        VerifiedVersionOrigin {
            version: version.into(), architecture: "amd64".into(),
            repository_uri: "https://example.org".into(), suite: archive.into(),
            index_path: "main/binary-amd64/Packages.xz".into(), component: "main".into(),
            release: ReleaseIdentity {
                archive: Some(archive.into()), origin: Some("Example".into()),
                ..Default::default()
            },
        }
    }
    #[test]
    fn version_and_archive_rules_are_separate_per_occurrence() {
        let versions = [origin("2.0", "stable"), origin("2.0", "testing"), origin("1.0", "stable")];
        let pins = [
            PinRecord { package: "bash".into(), pin: "release a=testing".into(), priority: 750 },
            PinRecord { package: "bash".into(), pin: "version 1.0".into(), priority: 900 },
        ];
        let result = evaluate("bash", &versions, &pins).unwrap();
        assert_eq!(result.iter().map(|r| r.priority).collect::<Vec<_>>(), [500, 750, 900]);
    }
    #[test]
    fn refuses_unsupported_or_ambiguous_pins() {
        for package in ["*", "bash:amd64", "bash curl"] {
            assert!(evaluate("bash", &[origin("1.0", "stable")], &[PinRecord {
                package: package.into(), pin: "version 1.0".into(), priority: 500,
            }]).is_err());
        }
        for pin in ["origin example.org", "release n=stable", "version 1.*", "release a=stable,o=Example"] {
            assert!(evaluate("bash", &[], &[PinRecord {
                package: "bash".into(), pin: pin.into(), priority: 500,
            }]).is_err());
        }
    }
    #[test]
    fn candidate_priorities_no_downgrade_and_explicit_downgrade() {
        let origins = [origin("1.0", "stable"), origin("2.0", "testing"), origin("3.0", "testing")];
        let no_pin = preview_candidate("bash", &origins, &[], Some("2.0")).unwrap();
        assert_eq!(no_pin.candidate.as_deref(), Some("3.0"));
        let stable = [PinRecord { package: "bash".into(), pin: "release a=stable".into(), priority: 999 }];
        assert_eq!(preview_candidate("bash", &origins, &stable, Some("2.0")).unwrap().candidate.as_deref(), Some("3.0"));
        let forced = [PinRecord { package: "bash".into(), pin: "release a=stable".into(), priority: 1001 }];
        assert_eq!(preview_candidate("bash", &origins, &forced, Some("2.0")).unwrap().candidate.as_deref(), Some("1.0"));
        assert_eq!(preview_candidate("bash", &[], &[], Some("2.0")).unwrap().candidate.as_deref(), Some("2.0"));
        assert!(preview_candidate("bash", &[], &[], None).unwrap().candidate.is_none());
    }
    #[test]
    fn highest_priority_occurrence_wins_and_negative_pins_are_skipped() {
        let origins = [origin("1.0", "stable"), origin("1.0", "testing"), origin("2.0", "stable")];
        let pins = [
            PinRecord { package: "bash".into(), pin: "release a=testing".into(), priority: 850 },
            PinRecord { package: "bash".into(), pin: "version 2.0".into(), priority: -1 },
        ];
        let result = preview_candidate("bash", &origins, &pins, None).unwrap();
        assert_eq!(result.candidate.as_deref(), Some("1.0"));
        assert_eq!(result.priorities, vec![("2.0".into(), -1), ("1.0".into(), 850)]);
        assert_eq!(preview_candidate("bash", &origins, &pins, Some("2.0")).unwrap().candidate.as_deref(), Some("1.0"));
    }
    #[test]
    fn first_matching_rule_wins_and_unsupported_priorities_refused() {
        let pins = [
            PinRecord { package: "bash".into(), pin: "version 1.0".into(), priority: 700 },
            PinRecord { package: "bash".into(), pin: "release a=stable".into(), priority: 600 },
        ];
        assert_eq!(evaluate("bash", &[origin("1.0", "stable")], &pins).unwrap()[0].priority, 700);
        let bad = [PinRecord { priority: 0, ..pins[0].clone() }];
        assert!(evaluate("bash", &[], &bad).is_err());
        let allowed = [PinRecord { priority: 1001, ..pins[0].clone() }];
        assert_eq!(evaluate("bash", &[origin("1.0", "stable")], &allowed).unwrap()[0].priority, 1001);
    }
}
