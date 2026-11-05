#!/usr/bin/env python3
"""Decision table for scripts/report-scheduled-failure.py."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import unittest
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "report-scheduled-failure.py"


def load_reporter():
    spec = importlib.util.spec_from_file_location("report_scheduled_failure", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load report-scheduled-failure.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ReportScheduledFailureTests(unittest.TestCase):
    def test_missing_args_exit_2(self) -> None:
        proc = subprocess.run(
            [sys.executable, str(SCRIPT)],
            capture_output=True,
            text=True,
            check=False,
            env={"PATH": os.environ.get("PATH", "")},
        )
        self.assertEqual(proc.returncode, 2, proc.stderr)
        self.assertIn("--prefix", proc.stderr)

    def test_missing_job_results_exit_2(self) -> None:
        proc = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--prefix",
                "CodeQL red",
                "--run-id",
                "11",
                "--repo",
                "argmend/argmend",
                "--dry-run",
            ],
            capture_output=True,
            text=True,
            check=False,
            env={"PATH": os.environ.get("PATH", "")},
        )
        self.assertEqual(proc.returncode, 2, proc.stderr)
        self.assertIn("JOB_RESULTS", proc.stderr)

    def test_decide(self) -> None:
        mod = load_reporter()
        today = date(2026, 9, 29)
        yesterday = date(2026, 9, 28).isoformat()
        red = {"codeql": "failure"}
        green = {"codeql": "success"}
        issue = {
            "signature": "codeql",
            "first_failed_on": today.isoformat(),
            "run_id": "10",
        }

        self.assertEqual(
            mod.decide(
                today=today,
                results={"codeql": "cancelled"},
                issue=issue,
                run_id="11",
            ),
            {"action": "noop"},
        )
        self.assertEqual(
            mod.decide(today=today, results=green, issue=None, run_id="11"),
            {"action": "noop"},
        )
        self.assertEqual(
            mod.decide(today=today, results=green, issue=issue, run_id="11")["action"],
            "close",
        )
        created = mod.decide(today=today, results=red, issue=None, run_id="11")
        self.assertEqual(created["action"], "create")
        self.assertEqual(created["day_count"], 1)
        self.assertEqual(created["first_failed_on"], today.isoformat())
        self.assertEqual(
            mod.issue_title("CodeQL red", "codeql", 1),
            "CodeQL red: codeql (1 consecutive day)",
        )
        self.assertEqual(
            mod.issue_title("CodeQL red", "codeql", 5),
            "CodeQL red: codeql (5 consecutive days)",
        )
        self.assertEqual(
            mod.decide(today=today, results=red, issue=issue, run_id="10")["action"],
            "noop",
        )
        updated = mod.decide(today=today, results=red, issue=issue, run_id="12")
        self.assertEqual(updated["action"], "update")
        self.assertEqual(updated["day_count"], 1)
        self.assertEqual(updated["first_failed_on"], today.isoformat())
        next_day = mod.decide(
            today=today,
            results=red,
            issue={
                "signature": "codeql",
                "first_failed_on": yesterday,
                "run_id": "9",
            },
            run_id="13",
        )
        self.assertEqual(next_day["action"], "replace")
        self.assertEqual(next_day["day_count"], (today - date(2026, 9, 28)).days + 1)
        self.assertEqual(next_day["first_failed_on"], yesterday)
        changed = mod.decide(
            today=today,
            results={"scorecard": "failure"},
            issue=issue,
            run_id="14",
        )
        self.assertEqual(changed["action"], "replace")
        self.assertEqual(changed["day_count"], 1)
        self.assertEqual(changed["first_failed_on"], today.isoformat())
        unmarked = [{"number": 4, "title": "CodeQL red: old", "body": "no marker"}]
        green_issue, _ordered = mod.prepare_matches(unmarked, results=green, today=today)
        self.assertIsNotNone(green_issue)
        assert green_issue is not None
        self.assertEqual(
            mod.decide(today=today, results=green, issue=green_issue, run_id="15")["action"],
            "close",
        )
        red_issue, _ordered = mod.prepare_matches(unmarked, results=red, today=today)
        self.assertIsNone(red_issue)

        skipped_green = {"codeql": "success", "dependency-review": "skipped"}
        self.assertEqual(mod.outcome(skipped_green), "green")
        self.assertEqual(
            mod.decide(today=today, results=skipped_green, issue=issue, run_id="16")["action"],
            "close",
        )
        self.assertEqual(mod.outcome({"codeql": "skipped"}), "noop")
        self.assertEqual(
            mod.outcome({"codeql": "failure", "dependency-review": "skipped"}),
            "red",
        )
        self.assertEqual(
            mod.decide(
                today=today,
                results={"codeql": "cancelled", "dependency-review": "success"},
                issue=issue,
                run_id="17",
            ),
            {"action": "noop"},
        )

        body = mod.render_body(
            run_url="https://example.test/run/1",
            signature="codeql",
            day_count_value=2,
            first_failed_on=yesterday,
            run_id="13",
        )
        self.assertNotIn("\u2014", body)
        self.assertNotIn("boring", body)
        self.assertNotIn("honest", body)
        self.assertEqual(
            mod.parse_state(body),
            {
                "signature": "codeql",
                "first_failed_on": yesterday,
                "run_id": "13",
            },
        )
        self.assertEqual(mod.normalize_assignee("  "), "SebTardif")
        self.assertEqual(mod.normalize_assignee("Ada"), "Ada")
        self.assertIn("https://example.test/run/1", mod.close_comment("https://example.test/run/1"))
        created_labels = mod.labels_to_create({"ready"}, ["nightly-failure", "ready"])
        self.assertEqual(
            created_labels,
            [("nightly-failure", "B60205", "Scheduled CI failure")],
        )
        self.assertEqual(mod.labels_to_create({"nightly-failure", "ready"}, ["nightly-failure", "ready"]), [])

        dry = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--prefix",
                "CodeQL red",
                "--run-id",
                "11",
                "--repo",
                "argmend/argmend",
                "--today",
                today.isoformat(),
                "--dry-run",
                "--assignee",
                "   ",
            ],
            capture_output=True,
            text=True,
            check=False,
            env={
                "PATH": os.environ.get("PATH", ""),
                "JOB_RESULTS": "codeql=failure",
            },
        )
        self.assertEqual(dry.returncode, 0, dry.stderr)
        payload = json.loads(dry.stdout.strip().splitlines()[-2])
        self.assertEqual(payload["action"], "create")
        self.assertEqual(payload["day_count"], 1)


if __name__ == "__main__":
    unittest.main()
