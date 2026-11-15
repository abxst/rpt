#!/usr/bin/env python3
"""M12 release evidence checker: integrity/completeness only, never authorization.

Records are self-reported; SHA-256 detects accidental changes, NOT fabrication.
This tool does not verify GitHub attestations or prove VM identity.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys

GATES = ("A", "B", "C", "D", "E", "F", "G", "H", "I")
TARGETS = tuple(
    f"{distro}-{release}-{arch}"
    for distro, release in (
        ("ubuntu", "22.04"), ("ubuntu", "24.04"), ("ubuntu", "26.04"),
        ("debian", "12"), ("debian", "13"),
    )
    for arch in ("amd64", "arm64")
)
HASH = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,120}$")
MAX_BYTES = 8 * 1024 * 1024


def unique_dict(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def read_manifest(path):
    if path.is_symlink() or not path.is_file():
        raise ValueError("manifest must be a regular non-symlink file")
    if path.stat().st_size > MAX_BYTES:
        raise ValueError("manifest is oversized")
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_dict)


def witness(directory, object_, used):
    if not isinstance(object_, dict) or set(object_) != {"file", "sha256"}:
        raise ValueError("witness needs exactly file and sha256")
    filename, checksum = object_["file"], object_["sha256"]
    if not isinstance(filename, str) or not NAME.fullmatch(filename) or filename in {".", ".."}:
        raise ValueError("unsafe or noncanonical evidence file")
    if not isinstance(checksum, str) or not HASH.fullmatch(checksum):
        raise ValueError(f"invalid SHA-256 for {filename}")
    if filename in used:
        raise ValueError(f"reused witness filename: {filename}")
    used.add(filename)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(directory / filename, flags)
    try:
        meta = os.fstat(fd)
        if not stat.S_ISREG(meta.st_mode) or not 0 < meta.st_size <= MAX_BYTES:
            raise ValueError(f"empty, oversized, or nonregular witness: {filename}")
        h = hashlib.sha256()
        with os.fdopen(os.dup(fd), "rb") as stream:
            while block := stream.read(64 * 1024):
                h.update(block)
        if h.hexdigest() != checksum:
            raise ValueError(f"evidence SHA-256 mismatch: {filename}")
    finally:
        os.close(fd)


def qualify(manifest, directory, expected_commit):
    if not isinstance(manifest, dict) or set(manifest) != {
        "schema_version", "source_commit", "gates", "matrix"
    } or manifest["schema_version"] != 1:
        raise ValueError("unsupported or incomplete qualification manifest schema")
    if manifest["source_commit"] != expected_commit:
        raise ValueError("qualification evidence is not bound to expected source commit")
    gates, matrix = manifest["gates"], manifest["matrix"]
    if not isinstance(gates, dict) or not set(gates).issubset(GATES):
        raise ValueError("unknown parity gate")
    if not isinstance(matrix, list):
        raise ValueError("VM acceptance matrix must be an array")
    used = set()
    passed = []
    for gate in GATES:
        item = gates.get(gate)
        if item is None:
            continue
        if not isinstance(item, dict) or item.get("status") not in ("pass", "blocked"):
            raise ValueError(f"invalid status for gate {gate}")
        if item["status"] == "blocked":
            if set(item) != {"status"}:
                raise ValueError(f"blocked gate {gate} cannot claim evidence")
            continue
        if set(item) != {"status", "positive", "negative"}:
            raise ValueError(f"pass gate {gate} lacks positive and negative evidence")
        witness(directory, item["positive"], used)
        witness(directory, item["negative"], used)
        passed.append(gate)
    seen = set()
    matrix_passed = []
    for entry in matrix:
        if not isinstance(entry, dict) or set(entry) != {
            "target", "status", "isolation", "host_audit"
        }:
            raise ValueError("invalid VM acceptance entry")
        target = entry["target"]
        if not isinstance(target, str) or target not in TARGETS or target in seen:
            raise ValueError(f"duplicate or unsupported VM target: {target}")
        seen.add(target)
        if entry["isolation"] != "vm" or entry["status"] not in ("pass", "blocked"):
            raise ValueError(f"invalid VM isolation/status for {target}")
        if entry["status"] == "pass":
            witness(directory, entry["host_audit"], used)
            matrix_passed.append(target)
        elif entry["host_audit"] is not None:
            raise ValueError(f"blocked VM target {target} cannot claim host audit")
    missing_gates = [x for x in GATES if x not in passed]
    missing_targets = [x for x in TARGETS if x not in matrix_passed]
    return report(expected_commit, missing_gates, missing_targets)


def report(commit, missing_gates, missing_targets):
    complete = not missing_gates and not missing_targets
    return {
        "schema_version": 1,
        "source_commit": commit,
        "qualification": "EVIDENCE_COMPLETE_REVIEW_REQUIRED" if complete else "BLOCKED",
        "missing_parity_gates": missing_gates,
        "missing_vm_targets": missing_targets,
        "manual_security_review_required": True,
        "host_execution_allowed": False,
        "note": "Evidence is self-reported, not cryptographically authenticated or a security certification. Complete evidence needs manual independent review; host dpkg execution remains disabled.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, help="explicit evidence manifest")
    parser.add_argument("--expected-commit", help="full 40-hex commit SHA")
    parser.add_argument("--require-complete", action="store_true",
                        help="fail closed unless every acceptance gate and VM target has evidence")
    args = parser.parse_args()
    if args.expected_commit is not None and not COMMIT.fullmatch(args.expected_commit):
        parser.error("--expected-commit must be 40 lowercase hexadecimal characters")
    try:
        if args.manifest is None:
            result = report(args.expected_commit, list(GATES), list(TARGETS))
        else:
            if not args.expected_commit:
                parser.error("--expected-commit required when providing --manifest")
            manifest = read_manifest(args.manifest)
            result = qualify(manifest, args.manifest.parent, args.expected_commit)
    except (OSError, ValueError, UnicodeError, json.JSONDecodeError) as exc:
        print(f"rpt: invalid qualification evidence: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True, indent=2))
    if args.require_complete and result["qualification"] == "BLOCKED":
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
