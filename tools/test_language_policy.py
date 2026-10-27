#!/usr/bin/env python3
"""Verify the repository's Rust-only runtime / Python-validation-only policy.

This script is a development/CI check, never a production dependency.
Existing legacy exceptions are listed explicitly until issue #635 is resolved.
"""
from __future__ import annotations

import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Legacy production/operations implementation files. Remove entries as the
# equivalent Rust code replaces them, not before regression testing.
LEGACY_NON_RUST = frozenset({
    "tools/maintenance_supervisor.py",
    "scripts/prune_merged_pr_branches.py",
    "tools/ServerGui.cs",
})

# Development-only reference oracles; not linked into the server.
JAVA_ORACLES = frozenset({
    "tools/GenLayerAddIslandOracle.java",
    "tools/GenLayerRiverMixOracle.java",
    "tools/GenLayerRiverOracle.java",
})

VERIFICATION_PREFIXES = (
    "test_", "verify_", "check_", "validate_", "audit_", "repro_", "fixture_",
)
VERIFICATION_SUFFIXES = ("_test.py", "_acceptance.py", "_benchmark.py")


def is_permitted_non_rust_source(path: str) -> bool:
    """Allow Rust itself elsewhere; classify tracked Python/C#/Java source."""
    if path in LEGACY_NON_RUST or path in JAVA_ORACLES:
        return True
    p = Path(path)
    if p.suffix == ".py":
        return (
            p.parent.as_posix() == "tools"
            and (
                p.name.startswith(VERIFICATION_PREFIXES)
                or p.name.endswith(VERIFICATION_SUFFIXES)
            )
        )
    return False


def tracked_source_files() -> list[str]:
    listing = subprocess.check_output(
        ["git", "ls-files", "-z"], cwd=ROOT
    ).decode("utf-8")
    return [
        path for path in listing.split("\0")
        if path.endswith((".py", ".cs", ".java"))
    ]


class LanguagePolicyTest(unittest.TestCase):
    def test_only_validation_helpers_are_permitted(self) -> None:
        noncompliant = [
            path for path in tracked_source_files()
            if not is_permitted_non_rust_source(path)
        ]
        self.assertEqual(
            noncompliant, [],
            "Python is verification-only; runtime/operations code must be Rust. "
            "Noncompliant tracked source files: " + ", ".join(noncompliant),
        )

    def test_path_rules(self) -> None:
        for allowed in (
            "tools/test_language_policy.py",
            "tools/e42_acceptance.py",
            "tools/tnt_stress_benchmark.py",
            "tools/verify_backup_restore.py",
            "tools/check_world_format.py",
        ):
            with self.subTest(allowed=allowed):
                self.assertTrue(is_permitted_non_rust_source(allowed))
        for prohibited in (
            "apps/vanilla-server/server.py",
            "crates/world/src/world.py",
            "tools/world_runtime.py",
            "tools/ServerGui2.cs",
            "scripts/deploy.py",
            "scripts/new_supervisor.py",
        ):
            with self.subTest(prohibited=prohibited):
                self.assertFalse(is_permitted_non_rust_source(prohibited))


if __name__ == "__main__":
    unittest.main()
