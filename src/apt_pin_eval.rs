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
    if !(-1_000..=1_000).contains(&record.priority) {
        bail!("pin priority outside supported diagnostic range: {}", record.priority);
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
    fn first_matching_rule_wins_and_unsupported_priorities_refused() {
        let pins = [
            PinRecord { package: "bash".into(), pin: "version 1.0".into(), priority: 700 },
            PinRecord { package: "bash".into(), pin: "release a=stable".into(), priority: 600 },
        ];
        assert_eq!(evaluate("bash", &[origin("1.0", "stable")], &pins).unwrap()[0].priority, 700);
        let bad = [PinRecord { priority: 1001, ..pins[0].clone() }];
        assert!(evaluate("bash", &[], &bad).is_err());
    }
}
