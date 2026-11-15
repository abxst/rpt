#!/usr/bin/env python3
"""Validate the *shape* of M15 VM power-cut evidence, never production approval.

This deliberately cannot authenticate a VM, its cloud image, its origin or
the semantic truth of self-reported results. Only the live QEMU test runner
may supply an evidence report. No fabricated report can unlock host apt.
"""
import json
from pathlib import Path
import re
import sys

SHA256 = re.compile(r"^[0-9a-f]{64}$")
EXPECTED = {
    "schema_version", "case", "guest", "vm_acceleration",
    "image_sha256", "baseline_sha256", "failed_disk_sha256",
    "cut_signal", "failed_dpkg_audit_nonempty", "failed_full_upgrade",
    "restore_to_fresh_disk", "restored_version", "restored_dpkg_audit_empty",
    "baseline_disk_unchanged", "failed_disk_unchanged_by_restore",
    "host_dpkg_unchanged", "rpt_guest_transaction_executed", "production_ready",
}


def no_duplicates(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate evidence key {key}")
        value[key] = item
    return value


def validate(value):
    if type(value) is not dict or set(value) != EXPECTED:
        raise ValueError("incomplete or unknown M15 evidence fields")
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise ValueError("unsupported M15 evidence schema")
    for key in ("image_sha256", "baseline_sha256", "failed_disk_sha256"):
        if type(value[key]) is not str or not SHA256.fullmatch(value[key]):
            raise ValueError(f"invalid {key} hash")
    constants = {
        "case": "qemu-sigkill-dpkg-postinst",
        "guest": "ubuntu-24.04-amd64",
        "cut_signal": "SIGKILL-QEMU",
        "restored_version": "1.0",
    }
    for key, expected in constants.items():
        if type(value[key]) is not str or value[key] != expected:
            raise ValueError(f"invalid M15 {key}")
    if type(value["vm_acceleration"]) is not str or value["vm_acceleration"] not in ("kvm", "tcg"):
        raise ValueError("invalid QEMU accelerator")
    required_true = (
        "failed_dpkg_audit_nonempty", "restore_to_fresh_disk",
        "restored_dpkg_audit_empty", "baseline_disk_unchanged",
        "failed_disk_unchanged_by_restore", "host_dpkg_unchanged",
    )
    for key in required_true:
        if value[key] is not True:
            raise ValueError(f"missing confirmed M15 success: {key}")
    for key in ("failed_full_upgrade", "rpt_guest_transaction_executed", "production_ready"):
        if value[key] is not False:
            raise ValueError(f"M15 cannot claim completion/authorization: {key}")
    return {
        "m15_qemu_dpkg_powercut_acceptance": "SELF_REPORTED_PASS_MANUAL_REVIEW_REQUIRED",
        "guest": value["guest"],
        "source_image_sha256": value["image_sha256"],
        "host_execution_allowed": False,
        "apt_parity_verified": False,
        "m15_rpt_powercut_transaction_complete": False,
        "full_production_qualification": "BLOCKED",
        "note": "Actual QEMU SIGKILL testing is independent of RPT dpkg transaction; evidence must be authenticated by CI before human review.",
    }


def main():
    if len(sys.argv) != 2:
        print("usage: validate-powercut-evidence.py <result.json>", file=sys.stderr)
        return 2
    path = Path(sys.argv[1])
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 16 * 1024:
            raise ValueError("evidence must be a regular file <=16 KiB")
        data = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=no_duplicates)
        result = validate(data)
    except (ValueError, OSError, UnicodeError) as error:
        print(f"invalid M15 evidence: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
