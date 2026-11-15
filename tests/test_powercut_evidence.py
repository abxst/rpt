#!/usr/bin/env python3
"""Synthetic negative contracts only; does not constitute VM power-cut evidence."""
import importlib.util
from pathlib import Path
import unittest

PATH = Path(__file__).resolve().parents[1] / "scripts" / "validate-powercut-evidence.py"
SPEC = importlib.util.spec_from_file_location("rpt_powercut_validator", PATH)
MOD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MOD)


def example():
    return {
        "schema_version": 1,
        "case": "qemu-sigkill-dpkg-postinst",
        "guest": "ubuntu-24.04-amd64",
        "vm_acceleration": "kvm",
        "image_sha256": "a" * 64,
        "baseline_sha256": "b" * 64,
        "failed_disk_sha256": "c" * 64,
        "cut_signal": "SIGKILL-QEMU",
        "failed_dpkg_audit_nonempty": True,
        "failed_full_upgrade": False,
        "restore_to_fresh_disk": True,
        "restored_version": "1.0",
        "restored_dpkg_audit_empty": True,
        "baseline_disk_unchanged": True,
        "failed_disk_unchanged_by_restore": True,
        "host_dpkg_unchanged": True,
        "rpt_guest_transaction_executed": False,
        "production_ready": False,
    }


class TestQemuEvidenceContract(unittest.TestCase):
    def test_complete_synthetic_shape_requires_manual_review(self):
        result = MOD.validate(example())
        self.assertEqual(result["full_production_qualification"], "BLOCKED")
        self.assertFalse(result["host_execution_allowed"])
        self.assertFalse(result["m15_rpt_powercut_transaction_complete"])

    def test_corruption_missing_field_and_type_confusion_are_rejected(self):
        for field, bad in (
            ("baseline_sha256", "not-a-hash"),
            ("cut_signal", "SIGTERM-dpkg"),
            ("vm_acceleration", "skipped"),
            ("failed_dpkg_audit_nonempty", False),
            ("restore_to_fresh_disk", "true"),
            ("host_dpkg_unchanged", 1),
            ("production_ready", True),
            ("rpt_guest_transaction_executed", True),
            ("failed_full_upgrade", True),
            ("schema_version", True),
        ):
            with self.subTest(field=field):
                data = example()
                data[field] = bad
                with self.assertRaises(ValueError):
                    MOD.validate(data)
        data = example()
        data.pop("restored_dpkg_audit_empty")
        with self.assertRaises(ValueError):
            MOD.validate(data)
        data = example()
        data["invented_field"] = True
        with self.assertRaises(ValueError):
            MOD.validate(data)

    def test_duplicate_json_evidence_fields_are_rejected(self):
        with self.assertRaises(ValueError):
            MOD.no_duplicates([("status", "pass"), ("status", "fake")])
        with self.assertRaises(ValueError):
            MOD.validate(None)


if __name__ == "__main__":
    unittest.main()
