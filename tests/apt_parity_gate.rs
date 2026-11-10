//! Differential APT parity gate: unsupported commands must fail closed.
//! A passing test here is NOT evidence of APT parity; it proves safe refusal.
use std::process::Command;

#[test]
fn unsupported_apt_commands_never_succeed() {
    for command in [
        "install", "reinstall", "remove", "purge", "upgrade",
        "full-upgrade", "dist-upgrade", "autoremove", "autoclean",
        "clean", "depends", "rdepends",
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

#[test]
fn update_is_not_misrepresented_as_apt_parity() {
    let result = Command::new(env!("CARGO_BIN_EXE_rpt"))
        .args(["update", "--help"]).output().expect("launch RPT");
    assert!(result.status.success());
    let help = String::from_utf8_lossy(&result.stdout);
    assert!(help.contains("isolated cache"), "{help}");
    assert!(help.contains("--workers"), "{help}");
}

#[test]
fn update_apt_style_options_are_validated() {
    // Parsing these options is supported; missing source directory must still
    // fail instead of reporting a successful package update.
    for args in [
        vec!["update", "-q", "--workers", "2", "--root", "/nonexistent-rpt-sources"],
        vec!["update", "-qq", "-o", "Acquire::Retries=3", "--root", "/nonexistent-rpt-sources"],
        vec!["update", "--no-progress", "-o", "Acquire::Retries=0", "--root", "/nonexistent-rpt-sources"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_rpt"))
            .args(&args).output().unwrap();
        assert!(!result.status.success(), "{args:?} must fail on missing sources");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!stderr.contains("unexpected argument"), "{args:?}: {stderr}");
    }
    for args in [
        vec!["update", "-o", "APT::Get::Assume-Yes=true"],
        vec!["update", "-o", "Acquire::Retries=-1"],
        vec!["update", "-o", "Acquire::Retries=11", "--root", "/nonexistent-rpt-sources"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_rpt"))
            .args(&args).output().unwrap();
        assert!(!result.status.success(), "unsupported configuration must fail: {args:?}");
    }
}
