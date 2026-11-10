//! Differential oracle: compare independent version ordering to dpkg.
//! Only exercises read-only dpkg --compare-versions, never package mutation.
use rpt::version::compare;
use std::{cmp::Ordering, process::Command};

#[test]
fn versions_match_dpkg_oracle() {
    if Command::new("dpkg").arg("--version").output().is_err() {
        eprintln!("dpkg not available: oracle test skipped");
        return;
    }
    let versions = [
        "0", "1", "1.0", "1.0-1", "1.0-2", "1.0-0", "1.0~rc1",
        "1.0~~", "1.0~", "1.0+git", "1.0a", "1.0b", "1.0.1",
        "1.001", "1.1", "1.9", "1.10", "1:1.0", "2:1.0",
        "0001:1.0", "0:1.0", "1.0ubuntu1", "1.0-1ubuntu1",
        "1.0-1ubuntu2", "1.0-1~build1", "1.0-1+build1",
    ];
    for left in versions {
        for right in versions {
            let ours = compare(left, right).unwrap();
            let is_less = Command::new("dpkg")
                .args(["--compare-versions", left, "lt", right]).status().unwrap().success();
            let is_greater = Command::new("dpkg")
                .args(["--compare-versions", left, "gt", right]).status().unwrap().success();
            let expected = if is_less { Ordering::Less }
                else if is_greater { Ordering::Greater }
                else { Ordering::Equal };
            assert_eq!(ours, expected, "{left} vs {right}");
        }
    }
}
