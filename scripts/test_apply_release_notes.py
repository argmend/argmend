#!/usr/bin/env python3
"""Tests for scripts/apply-release-notes.sh."""

from __future__ import annotations

import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "apply-release-notes.sh"
ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"


def run_script(
    env: dict[str, str], cwd: Path | None = None
) -> subprocess.CompletedProcess[str]:
    merged = os.environ.copy()
    merged.pop("GH_TOKEN", None)
    merged.pop("GITHUB_TOKEN", None)
    merged.pop("RELEASE_NOTES", None)
    merged.pop("RELEASE_NOTES_TAG", None)
    merged.update(env)
    return subprocess.run(
        ["bash", str(SCRIPT)],
        capture_output=True,
        text=True,
        env=merged,
        cwd=cwd,
        check=False,
    )


def _on_block(text: str) -> str:
    start = text.index("\non:")
    rest = text[start + 1 :]
    end = rest.index("\njobs:")
    lines = []
    for line in rest[:end].splitlines():
        stripped = line.split("#", 1)[0].rstrip()
        if stripped:
            lines.append(stripped)
    return "\n".join(lines)


class ApplyReleaseNotesTests(unittest.TestCase):
    def test_rejects_bad_tag(self) -> None:
        r = run_script({"TAG": "patchloom-v0.1.2", "GH_REPO": "argmend/argmend"})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("vX.Y.Z", r.stderr)

    def test_missing_source_is_noop(self) -> None:
        r = run_script(
            {"TAG": "v0.3.0", "GH_REPO": "argmend/argmend", "DRY_RUN": "1"}
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("leaving auto notes", r.stdout)

    def test_notes_file_dry_run(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            notes = Path(td) / "RELEASE_NOTES.md"
            notes.write_text("argmend 0.3.0 notes\n", encoding="utf-8")
            r = run_script(
                {
                    "TAG": "v0.3.0",
                    "GH_REPO": "argmend/argmend",
                    "DRY_RUN": "1",
                    "NOTES_FILE": str(notes),
                }
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("DRY_RUN: would apply file:", r.stdout)
            self.assertIn("BYTES: 20", r.stdout)
            self.assertNotIn("would delete branch", r.stdout)

    def test_empty_file_is_noop(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            notes = Path(td) / "RELEASE_NOTES.md"
            notes.write_text("", encoding="utf-8")
            r = run_script(
                {
                    "TAG": "v0.3.0",
                    "GH_REPO": "argmend/argmend",
                    "DRY_RUN": "1",
                    "NOTES_FILE": str(notes),
                }
            )
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("leaving auto notes", r.stdout)

    def test_variable_requires_matching_tag(self) -> None:
        r = run_script(
            {
                "TAG": "v0.3.0",
                "GH_REPO": "argmend/argmend",
                "DRY_RUN": "1",
                "RELEASE_NOTES": "stale notes",
                "RELEASE_NOTES_TAG": "v0.2.8",
            }
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("leaving auto notes", r.stdout)

    def test_variable_applies_when_tag_matches(self) -> None:
        r = run_script(
            {
                "TAG": "v0.3.0",
                "GH_REPO": "argmend/argmend",
                "DRY_RUN": "1",
                "RELEASE_NOTES": "from var",
                "RELEASE_NOTES_TAG": "0.3.0",
            }
        )
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("Actions variable", r.stdout)
        self.assertIn("DRY_RUN: would apply variable to v0.3.0", r.stdout)
        self.assertNotIn("would delete branch", r.stdout)

    def test_branch_fetch_dry_run_deletes_branch(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            fake_bin = Path(td) / "bin"
            fake_bin.mkdir()
            gh = fake_bin / "gh"
            gh.write_text(
                "#!/bin/bash\n"
                'if [ "$1" = api ] && [ "$2" != -X ]; then\n'
                "  printf '%s\\n' 'from branch'\n"
                "  exit 0\n"
                "fi\n"
                "exit 1\n",
                encoding="utf-8",
            )
            gh.chmod(gh.stat().st_mode | stat.S_IEXEC)
            env_path = f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}"
            r = run_script(
                {
                    "TAG": "v0.3.0",
                    "GH_REPO": "argmend/argmend",
                    "DRY_RUN": "1",
                    "GH_TOKEN": "test",
                    "PATH": env_path,
                }
            )
            self.assertEqual(r.returncode, 0, r.stderr + r.stdout)
            self.assertIn("release-note-0.3.0", r.stdout)
            self.assertIn(
                "DRY_RUN: would delete branch release-note-0.3.0", r.stdout
            )

    def test_release_view_fail_skips_edit(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            td_path = Path(td)
            notes = td_path / "RELEASE_NOTES.md"
            notes.write_text("curated notes\n", encoding="utf-8")
            fake_bin = td_path / "bin"
            fake_bin.mkdir()
            log = td_path / "gh.log"
            gh = fake_bin / "gh"
            gh.write_text(
                "#!/bin/bash\n"
                f"printf '%s\\n' \"$*\" >> '{log}'\n"
                'if [ "$1" = release ] && [ "$2" = view ]; then\n'
                "  exit 1\n"
                "fi\n"
                "exit 0\n",
                encoding="utf-8",
            )
            gh.chmod(gh.stat().st_mode | stat.S_IEXEC)
            env_path = f"{fake_bin}{os.pathsep}{os.environ.get('PATH', '')}"
            r = run_script(
                {
                    "TAG": "v0.3.0",
                    "GH_REPO": "argmend/argmend",
                    "NOTES_FILE": str(notes),
                    "GH_TOKEN": "test",
                    "PATH": env_path,
                }
            )
            self.assertEqual(r.returncode, 1, r.stderr + r.stdout)
            self.assertIn("does not exist", r.stderr)
            logged = log.read_text(encoding="utf-8") if log.exists() else ""
            self.assertIn("release view", logged)
            self.assertNotIn("release edit", logged)

    def test_workflow_is_dispatch_only_and_release_calls_it(self) -> None:
        text = (WORKFLOWS / "apply-release-notes.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertNotIn("pull_request:", on_block)
        self.assertNotIn("push:", on_block)
        rel = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
        self.assertIn("publisher/scripts/apply-release-notes.sh", rel)
        self.assertIn("continue-on-error: true", rel)

    def test_rust_cache_workspace_only_on_publish_jobs(self) -> None:
        action = (
            ROOT / ".github" / "actions" / "setup-rust" / "action.yml"
        ).read_text(encoding="utf-8")
        self.assertIn('default: "."', action)
        self.assertIn("workspaces: ${{ inputs.workspace }}", action)
        for name in ("publish-crates.yml", "release.yml"):
            text = (WORKFLOWS / name).read_text(encoding="utf-8")
            self.assertIn("workspace: crate", text)
        for name in ("ci.yml", "coverage.yml", "fuzz.yml"):
            text = (WORKFLOWS / name).read_text(encoding="utf-8")
            self.assertNotIn("workspace:", text)


if __name__ == "__main__":
    unittest.main()
