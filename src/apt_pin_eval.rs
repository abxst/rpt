//! Read-only, deliberately restricted APT preference evaluator.
//! Supports exact package selectors and Package: * Release general pins.
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

/// Supported APT Release selectors use exact, comma-separated conjunctive
/// matches. In particular, "o=" is the signed Release Origin, not the host.
fn parse_release_predicates(value: &str) -> Result<Vec<(&str, &str)>> {
    let mut result = Vec::new();
    for clause in value.split(',') {
        let (key, expected) = clause.trim().split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid APT release predicate"))?;
        let key = key.trim();
        let expected = expected.trim();
        if !matches!(key, "a" | "n" | "o" | "l" | "c")
            || expected.is_empty()
            || !expected.bytes().all(|b| b.is_ascii_alphanumeric() || b".+_-".contains(&b))
            || result.iter().any(|(seen, _)| *seen == key) {
            bail!("unsupported or duplicate APT release predicate: {clause}");
        }
        result.push((key, expected));
    }
    if result.is_empty() { bail!("empty APT release selector"); }
    Ok(result)
}

fn release_matches(value: &str, origin: &VerifiedVersionOrigin) -> bool {
    // Validated before evaluating. This non-fallible match is only called from
    // evaluate() after validate_record() has rejected unsupported expressions.
    let Ok(clauses) = parse_release_predicates(value) else { return false };
    clauses.into_iter().all(|(key, value)| match key {
        "a" => origin.release.archive.as_deref() == Some(value),
        "n" => origin.release.codename.as_deref() == Some(value),
        "o" => origin.release.origin.as_deref() == Some(value),
        "l" => origin.release.label.as_deref() == Some(value),
        "c" => origin.component == value,
        _ => false,
    })
}

/// APT policy accepts many wildcard, origin, architecture and preference forms.
/// Reject all unsupported forms before evaluating any version, including records
/// for unrelated packages: otherwise a caller could mistake partial policy for
/// a complete and authoritative result.
fn validate_record(record: &PinRecord) -> Result<()> {
    if record.package.contains(char::is_whitespace)
        || (record.package != "*" && !record.package.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))) {
        bail!("unsupported pin Package selector: {}", record.package);
    }
    let (kind, value) = record.pin.split_once(' ').ok_or_else(|| anyhow::anyhow!("invalid Pin"))?;
    let value = value.trim();
    let valid = match kind {
        "version" => !value.is_empty() && !value.contains('*') && !value.contains('?')
            && crate::version::compare(value, value).is_ok(),
        "release" => parse_release_predicates(value).is_ok(),
        _ => false,
    };
    if !valid { bail!("unsupported pin selector: {}", record.pin); }
    if record.package == "*" && kind != "release" {
        bail!("general Package: * currently supports only Release pins");
    }
    if record.priority == 0 {
        bail!("APT pin priority zero has undefined behavior and is unsupported");
    }
    Ok(())
}

fn matches(record: &PinRecord, name: &str, origin: &VerifiedVersionOrigin) -> bool {
    if record.package != name && record.package != "*" { return false; }
    let Some((kind, value)) = record.pin.split_once(' ') else { return false };
    let value = value.trim();
    match kind {
        "version" => origin.version == value,
        "release" => release_matches(value, origin),
        _ => false,
    }
}

/// Report priority per *verified occurrence*, without collapsing two origins
/// advertising identical versions. Explicit package records have precedence
/// over general Package: * Release records, regardless of file order.
/// First matching record within each class wins; fallback is priority 500.
/// This diagnostic does not implement wildcard package patterns.
pub fn evaluate(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
) -> Result<Vec<EvaluatedPin>> {
    evaluate_with_target(package, origins, records, None)
}

/// For an exact target release, APT assigns priority 990 to matching signed
/// archives/codenames. Specific package pins override the target release;
/// general pins do not. Unmatched repositories retain priority 500.
pub fn evaluate_with_target(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
    target_release: Option<&str>,
) -> Result<Vec<EvaluatedPin>> {
    evaluate_with_phasing(package, origins, records, target_release, None)
}

/// Diagnostic using an explicit machine ID; absent ID preserves fail-closed
/// behavior for incomplete phasing. No machine ID is read implicitly.
pub fn evaluate_with_phasing(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
    target_release: Option<&str>, machine_id: Option<&str>,
) -> Result<Vec<EvaluatedPin>> {
    if package.is_empty() { bail!("empty package name"); }
    if let Some(target) = target_release {
        if target.is_empty() || target.len() > 128 || !target.bytes().all(|b|
            b.is_ascii_alphanumeric() || b".+_-".contains(&b)) {
            bail!("unsupported target release selector");
        }
    }
    for record in records { validate_record(record)?; }
    // APT phases at the binary-version level, not per repository URI. Require
    // all occurrences of the same binary version to agree on source metadata
    // and rollout percentage. Conflicts cannot safely be guessed.
    let mut phase_by_version = std::collections::BTreeMap::new();
    for entry in origins {
        let identity = (
            entry.source_package.as_str(),
            entry.source_version.as_str(),
            entry.phased_update_percentage,
        );
        if let Some(previous) = phase_by_version.insert(entry.version.as_str(), identity) {
            if previous != identity {
                bail!("inconsistent signed source/phasing metadata for version {}", entry.version);
            }
        }
        if let Some(percent) = entry.phased_update_percentage {
            if percent < 100 && machine_id.is_none() {
                bail!("Phased-Update-Percentage {percent} for {} {} requires explicit --machine-id; refusing incomplete policy preview", package, entry.version);
            }
        }
    }
    origins.iter().map(|origin| -> Result<EvaluatedPin> {
        let specific = records.iter().find(|r| r.package == package && matches(r, package, origin));
        let general = records.iter().find(|r| r.package == "*" && matches(r, package, origin));
        let is_target = target_release.is_some_and(|target| {
            origin.release.archive.as_deref() == Some(target)
                || origin.release.codename.as_deref() == Some(target)
        });
        let priority = if let Some(rule) = specific { rule.priority }
            else if is_target { 990 }
            else { general.map_or(500, |r| r.priority) };
        let priority = if let Some(percent) = origin.phased_update_percentage {
            if percent < 100 {
                let machine = machine_id.expect("validated phased machine ID");
                if !crate::apt_phasing::eligible(
                    &origin.source_package, &origin.source_version, machine, percent,
                )? {
                    priority.min(1)
                } else { priority }
            } else { priority }
        } else { priority };
        Ok(EvaluatedPin {
            version: origin.version.clone(),
            repository_uri: origin.repository_uri.clone(),
            suite: origin.suite.clone(),
            priority,
        })
    }).collect::<Result<Vec<_>>>()
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
/// unless the candidate's effective priority is > 1000. Version 0 priority
/// is rejected, as its behavior is not defined by APT policy.
pub fn preview_candidate(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
    installed: Option<&str>,
) -> Result<CandidatePreview> {
    preview_candidate_with_target(package, origins, records, installed, None)
}

pub fn preview_candidate_with_target(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
    installed: Option<&str>, target_release: Option<&str>,
) -> Result<CandidatePreview> {
    preview_candidate_with_phasing(package, origins, records, installed, target_release, None)
}

pub fn preview_candidate_with_phasing(
    package: &str, origins: &[VerifiedVersionOrigin], records: &[PinRecord],
    installed: Option<&str>, target_release: Option<&str>, machine_id: Option<&str>,
) -> Result<CandidatePreview> {
    use std::{cmp::Ordering, collections::BTreeMap};
    let evaluated = evaluate_with_phasing(package, origins, records, target_release, machine_id)?;
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
            if crate::version::compare(version, current)? == Ordering::Less && *priority <= 1000 {
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
            phased_update_percentage: None,
            source_package: "bash".into(),
            source_version: version.into(),
            release: ReleaseIdentity {
                archive: Some(archive.into()), origin: Some("Example".into()),
                ..Default::default()
            },
        }
    }
    #[test]
    fn machine_specific_phase_ceils_held_back_priority_to_one() {
        let mut entry = origin("2.0", "stable");
        entry.source_package = "foo".into();
        entry.source_version = "1.0".into();
        entry.phased_update_percentage = Some(40);
        // C++ reference draw for source foo / 1.0 on machine 0000... is 52.
        let machine = "00000000000000000000000000000000";
        assert_eq!(evaluate_with_phasing("bash", &[entry.clone()], &[], None, Some(machine))
            .unwrap()[0].priority, 1);
        // Once the rollout reaches 53, this machine joins.
        entry.phased_update_percentage = Some(53);
        assert_eq!(evaluate_with_phasing("bash", &[entry.clone()], &[], None, Some(machine))
            .unwrap()[0].priority, 500);
        let candidate = preview_candidate_with_phasing(
            "bash", &[entry], &[], Some("1.0"), None, Some(machine),
        ).unwrap();
        assert_eq!(candidate.candidate.as_deref(), Some("2.0"));
    }
    #[test]
    fn conflicting_phase_metadata_for_same_version_is_rejected() {
        let mut first = origin("2.0", "stable");
        first.phased_update_percentage = Some(20);
        let mut second = origin("2.0", "testing");
        second.phased_update_percentage = Some(40);
        assert!(evaluate_with_phasing("bash", &[first, second], &[], None,
            Some("00000000000000000000000000000000")).is_err());
    }
    #[test]
    fn phased_candidates_fail_closed_until_machine_specific_policy_supported() {
        let mut candidate = origin("2.0", "stable");
        candidate.phased_update_percentage = Some(25);
        assert!(evaluate("bash", &[candidate.clone()], &[]).is_err());
        assert!(preview_candidate_with_target("bash", &[candidate.clone()], &[], Some("1.0"), Some("stable")).is_err());
        candidate.phased_update_percentage = Some(0);
        assert!(evaluate("bash", &[candidate.clone()], &[]).is_err());
        candidate.phased_update_percentage = Some(100);
        assert_eq!(evaluate("bash", &[candidate.clone()], &[]).unwrap()[0].priority, 500);
        candidate.phased_update_percentage = None;
        assert_eq!(evaluate("bash", &[candidate], &[]).unwrap()[0].priority, 500);
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
        for package in ["ba*", "bash:amd64", "bash curl"] {
            assert!(evaluate("bash", &[origin("1.0", "stable")], &[PinRecord {
                package: package.into(), pin: "version 1.0".into(), priority: 500,
            }]).is_err());
        }
        for pin in ["origin example.org", "release x=stable", "version 1.*", "release a=stable,o=Example,o=Other", "release a=stable,*", "release a="] {
            assert!(evaluate("bash", &[], &[PinRecord {
                package: "bash".into(), pin: pin.into(), priority: 500,
            }]).is_err());
        }
    }
    #[test]
    fn conjunctive_signed_release_selectors_and_component() {
        let mut first = origin("1.0", "noble");
        first.release.codename = Some("noble".into());
        first.release.label = Some("Ubuntu".into());
        first.release.origin = Some("Ubuntu".into());
        let mut second = first.clone();
        second.component = "universe".into();
        second.index_path = "universe/binary-amd64/Packages.xz".into();
        let records = [
            PinRecord { package: "bash".into(),
                pin: "release o=Ubuntu, n=noble, c=main, l=Ubuntu, a=noble".into(),
                priority: 901 },
        ];
        let result = evaluate("bash", &[first, second], &records).unwrap();
        assert_eq!(result.iter().map(|p| p.priority).collect::<Vec<_>>(), [901, 500]);
        assert!(parse_release_predicates("o=Ubuntu,o=Other").is_err());
        assert!(parse_release_predicates("o=Ubuntu,*").is_err());
        assert!(parse_release_predicates("a=").is_err());
    }
    #[test]
    fn single_release_keys_match_signed_values() {
        let mut item = origin("1.0", "noble-updates");
        item.release.codename = Some("noble".into());
        item.release.label = Some("Ubuntu".into());
        item.release.origin = Some("Ubuntu".into());
        for pin in ["release n=noble", "release o=Ubuntu", "release l=Ubuntu",
                    "release a=noble-updates", "release c=main"] {
            let records = [PinRecord { package: "bash".into(), pin: pin.into(), priority: 723 }];
            assert_eq!(evaluate("bash", &[item.clone()], &records).unwrap()[0].priority, 723, "{pin}");
        }
    }
    #[test]
    fn specific_precedes_general_independent_of_file_order() {
        let origins = [origin("1.0", "stable"), origin("2.0", "testing")];
        let general = PinRecord { package: "*".into(), pin: "release a=stable".into(), priority: 800 };
        let specific = PinRecord { package: "bash".into(), pin: "release a=stable".into(), priority: 650 };
        let records = [general.clone(), specific.clone()];
        let result = evaluate("bash", &origins, &records).unwrap();
        assert_eq!(result.iter().map(|r| r.priority).collect::<Vec<_>>(), [650, 500]);
        let reversed = [specific, general];
        assert_eq!(evaluate("bash", &origins, &reversed).unwrap(), result);
        let another = evaluate("curl", &origins, &records).unwrap();
        assert_eq!(another.iter().map(|r| r.priority).collect::<Vec<_>>(), [800, 500]);
    }
    #[test]
    fn first_general_match_wins_not_highest_pin() {
        let items = [origin("1.0", "stable")];
        let records = [
            PinRecord { package: "*".into(), pin: "release a=stable".into(), priority: 400 },
            PinRecord { package: "*".into(), pin: "release n=stable".into(), priority: 950 },
        ];
        assert_eq!(evaluate("bash", &items, &records).unwrap()[0].priority, 400);
    }
    #[test]
    fn rejects_unsupported_general_version_pinning() {
        let records = [PinRecord { package: "*".into(), pin: "version 1.0".into(), priority: 700 }];
        assert!(evaluate("bash", &[], &records).is_err());
    }
    #[test]
    fn target_release_overrides_general_pins_but_not_specific_pins() {
        let mut stable = origin("1.0", "noble");
        stable.release.codename = Some("noble".into());
        let mut updates = origin("2.0", "noble-updates");
        updates.release.codename = Some("noble".into());
        let other = origin("3.0", "oracular");
        let records = [
            PinRecord { package: "*".into(), pin: "release n=noble".into(), priority: 700 },
            PinRecord { package: "bash".into(), pin: "version 1.0".into(), priority: 1001 },
        ];
        let all = [stable, updates, other];
        let scored = evaluate_with_target("bash", &all, &records, Some("noble")).unwrap();
        assert_eq!(scored.iter().map(|v| v.priority).collect::<Vec<_>>(), [1001, 990, 500]);
        assert_eq!(preview_candidate_with_target("bash", &all, &records, Some("2.0"), Some("noble"))
            .unwrap().candidate.as_deref(), Some("1.0"));
        assert_eq!(evaluate_with_target("curl", &all, &records, Some("noble")).unwrap()
            .iter().map(|v| v.priority).collect::<Vec<_>>(), [990, 990, 500]);
        assert_eq!(evaluate_with_target("curl", &all, &records, None).unwrap()
            .iter().map(|v| v.priority).collect::<Vec<_>>(), [700, 700, 500]);
        assert!(evaluate_with_target("bash", &all, &[], Some("noble*")).is_err());
    }
    #[test]
    fn candidate_priorities_no_downgrade_and_explicit_downgrade() {
        let origins = [origin("1.0", "stable"), origin("2.0", "testing"), origin("3.0", "testing")];
        let no_pin = preview_candidate("bash", &origins, &[], Some("2.0")).unwrap();
        assert_eq!(no_pin.candidate.as_deref(), Some("3.0"));
        let stable = [PinRecord { package: "bash".into(), pin: "release a=stable".into(), priority: 999 }];
        assert_eq!(preview_candidate("bash", &origins, &stable, Some("2.0")).unwrap().candidate.as_deref(), Some("3.0"));
        let threshold = [PinRecord { package: "bash".into(), pin: "release a=stable".into(), priority: 1000 }];
        assert_eq!(preview_candidate("bash", &origins, &threshold, Some("2.0")).unwrap().candidate.as_deref(), Some("3.0"));
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
        assert_eq!(preview_candidate("bash", &origins, &pins, Some("2.0")).unwrap().candidate.as_deref(), Some("2.0"));
        assert_eq!(preview_candidate("bash", &origins, &pins, Some("0.5")).unwrap().candidate.as_deref(), Some("1.0"));
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
