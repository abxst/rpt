//! Differential APT parity gate: unsupported commands must fail closed.
//! A passing test here is NOT evidence of APT parity; it proves safe refusal.
use std::process::Command;

#[test]
fn unsupported_apt_commands_never_succeed() {
    for command in [
        "update", "install", "reinstall", "remove", "purge", "upgrade",
        "full-upgrade", "dist-upgrade", "autoremove", "autoclean",
        "clean", "search", "show", "list", "depends", "rdepends",
        "policy", "download", "satisfy", "edit-sources",
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_rpt"))
            .arg(command).output().expect("launch RPT");
        assert!(!result.status.success(), "unsupported apt command {command} must fail");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stderr.contains("unrecognized subcommand"), "{command}: {stderr}");
    }
}

#[test]
fn unsupported_apt_arguments_never_succeed() {
    for args in [
        vec!["-y", "install", "bash"],
        vec!["-qq", "update"],
        vec!["-o", "APT::Get::Assume-Yes=true", "update"],
        vec!["--fix-broken", "install"],
        vec!["--no-install-recommends", "install", "curl"],
        vec!["--simulate", "upgrade"],
        vec!["--allow-downgrades", "install", "bash"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_rpt"))
            .args(&args).output().expect("launch RPT");
        assert!(!result.status.success(), "unsupported apt args {args:?} must fail");
    }
}
