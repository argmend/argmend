#!/usr/bin/env python3
"""Open, update, or close an assigned issue for a scheduled CI failure.

The decision function is pure. The CLI calls gh with list arguments.
A skipped neighbor does not keep a green run from closing the issue.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from datetime import date, datetime, timezone

LABEL_DEFAULTS: dict[str, tuple[str, str]] = {
    "nightly-failure": ("B60205", "Scheduled CI failure"),
    "ready": ("0E8A16", "Ready for a maintainer"),
}


def outcome(results: dict[str, str]) -> str:
    vals = {status for status in results.values() if status != "skipped"}
    if not vals:
        return "noop"
    if "failure" in vals:
        return "red"
    if "cancelled" in vals:
        return "noop"
    if vals == {"success"}:
        return "green"
    return "noop"


def failure_signature(results: dict[str, str]) -> str:
    names = sorted(name for name, status in results.items() if status == "failure")
    return ",".join(names)


def day_count(today: date, first_failed_on: str) -> int:
    delta = (today - date.fromisoformat(first_failed_on)).days
    if delta < 0:
        delta = 0
    return delta + 1


def decide(
    *,
    today: date,
    results: dict[str, str],
    issue: dict[str, str] | None,
    run_id: str,
) -> dict[str, object]:
    kind = outcome(results)
    if kind == "noop":
        return {"action": "noop"}
    if kind == "green":
        if issue is None:
            return {"action": "noop"}
        return {"action": "close"}
    signature = failure_signature(results)
    if issue is not None and issue.get("run_id") == run_id:
        return {"action": "noop"}
    if issue is None:
        return {
            "action": "create",
            "signature": signature,
            "first_failed_on": today.isoformat(),
            "day_count": 1,
            "run_id": run_id,
        }
    first = str(issue["first_failed_on"])
    count = day_count(today, first)
    if issue.get("signature") == signature and first == today.isoformat():
        action = "update"
    else:
        action = "replace"
    return {
        "action": action,
        "signature": signature,
        "first_failed_on": first,
        "day_count": count,
        "run_id": run_id,
    }


def parse_state(body: str) -> dict[str, str] | None:
    marker = "<!-- nightly-failure-state:"
    start = body.find(marker)
    if start < 0:
        return None
    json_start = body.find("{", start)
    json_end = body.find("-->", json_start)
    if json_start < 0 or json_end < 0:
        return None
    try:
        data = json.loads(body[json_start:json_end].strip())
    except json.JSONDecodeError:
        return None
    if not isinstance(data, dict):
        return None
    state: dict[str, str] = {}
    for key in ("signature", "first_failed_on", "run_id"):
        value = data.get(key)
        if not isinstance(value, str) or not value:
            return None
        state[key] = value
    return state


def render_body(
    *,
    run_url: str,
    signature: str,
    day_count_value: int,
    first_failed_on: str,
    run_id: str,
) -> str:
    state = json.dumps(
        {
            "signature": signature,
            "first_failed_on": first_failed_on,
            "run_id": run_id,
        },
        separators=(",", ":"),
    )
    return "\n".join(
        [
            "Scheduled run failed.",
            "",
            f"Run: {run_url}",
            f"Signature: {signature}",
            f"Consecutive days: {day_count_value}",
            f"First failed on: {first_failed_on}",
            "",
            f"<!-- nightly-failure-state: {state} -->",
            "",
        ]
    )


def issue_title(prefix: str, signature: str, day_count_value: int) -> str:
    unit = "day" if day_count_value == 1 else "days"
    return f"{prefix}: {signature} ({day_count_value} consecutive {unit})"


def close_comment(run_url: str) -> str:
    if run_url:
        return f"Scheduled jobs passed. Closing this failure. Run: {run_url}"
    return "Scheduled jobs passed. Closing this failure."


def normalize_assignee(value: str) -> str:
    cleaned = value.strip()
    if cleaned:
        return cleaned
    return "SebTardif"


def labels_to_create(
    existing: set[str], requested: list[str]
) -> list[tuple[str, str, str]]:
    pending: list[tuple[str, str, str]] = []
    for name in requested:
        if name in existing:
            continue
        spec = LABEL_DEFAULTS.get(name)
        if spec is None:
            continue
        color, description = spec
        pending.append((name, color, description))
    return pending


def prepare_matches(
    matches: list[dict[str, object]],
    *,
    results: dict[str, str],
    today: date,
) -> tuple[dict[str, str] | None, list[dict[str, object]]]:
    """Pick the stateful issue and still close prefix matches on green.

    A body with no state marker cannot be updated, but a fully green run
    must close every open issue for this prefix.
    """
    issue: dict[str, str] | None = None
    ordered = list(matches)
    for item in matches:
        parsed = parse_state(str(item.get("body", "")))
        if parsed is not None:
            issue = parsed
            ordered = [item] + [other for other in matches if other is not item]
            break
    if issue is None and matches and outcome(results) == "green":
        issue = {
            "signature": "unparsed",
            "first_failed_on": today.isoformat(),
            "run_id": "unparsed",
        }
    return issue, ordered


def parse_results(raw: str) -> dict[str, str]:
    parsed: dict[str, str] = {}
    for part in raw.split():
        if "=" not in part:
            continue
        name, status = part.split("=", 1)
        if name and status:
            parsed[name] = status
    return parsed


def _run_gh(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, check=False, text=True, capture_output=True)


def _fail_gh(proc: subprocess.CompletedProcess[str], what: str) -> int:
    print(f"FAIL: gh {what} exited {proc.returncode}", file=sys.stderr)
    if proc.stderr:
        print(proc.stderr, file=sys.stderr)
    if proc.stdout:
        print(proc.stdout, file=sys.stderr)
    return 1


def _list_issues(repo: str, label: str) -> tuple[list[dict[str, object]], int]:
    proc = _run_gh(
        [
            "gh",
            "issue",
            "list",
            "--repo",
            repo,
            "--label",
            label,
            "--state",
            "open",
            "--limit",
            "50",
            "--json",
            "number,title,body",
        ]
    )
    if proc.returncode != 0:
        return [], _fail_gh(proc, "issue list")
    data = json.loads(proc.stdout or "[]")
    if not isinstance(data, list):
        print("FAIL: gh issue list returned unexpected JSON", file=sys.stderr)
        return [], 1
    return data, 0


def ensure_labels(repo: str, labels: list[str]) -> int:
    print("DO: list labels")
    proc = _run_gh(
        [
            "gh",
            "label",
            "list",
            "--repo",
            repo,
            "--limit",
            "100",
            "--json",
            "name",
        ]
    )
    if proc.returncode != 0:
        return _fail_gh(proc, "label list")
    data = json.loads(proc.stdout or "[]")
    if not isinstance(data, list):
        print("FAIL: gh label list returned unexpected JSON", file=sys.stderr)
        return 1
    existing: set[str] = set()
    for item in data:
        if isinstance(item, dict) and isinstance(item.get("name"), str):
            existing.add(str(item["name"]))
    for name, color, description in labels_to_create(existing, labels):
        print(f"DO: create label {name}")
        proc = _run_gh(
            [
                "gh",
                "label",
                "create",
                name,
                "--repo",
                repo,
                "--color",
                color,
                "--description",
                description,
            ]
        )
        if proc.returncode != 0:
            return _fail_gh(proc, "label create")
    return 0


def _write_body(path: str, body: str) -> None:
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(body)


def apply_decision(
    *,
    repo: str,
    prefix: str,
    labels: list[str],
    assignee: str,
    run_url: str,
    decision: dict[str, object],
    matches: list[dict[str, object]],
) -> int:
    action = str(decision["action"])
    print(f"OK: decision {action}")
    if action == "noop":
        print(json.dumps({"action": "noop"}))
        print("DONE: no issue change")
        return 0
    if action == "close":
        comment = close_comment(run_url)
        for item in matches:
            number = str(item["number"])
            print(f"DO: close issue {number}")
            proc = _run_gh(
                [
                    "gh",
                    "issue",
                    "close",
                    number,
                    "--repo",
                    repo,
                    "--reason",
                    "completed",
                    "--comment",
                    comment,
                ]
            )
            if proc.returncode != 0:
                return _fail_gh(proc, "issue close")
        print(json.dumps({"action": "close", "count": len(matches)}))
        print("DONE: closed matching issues")
        return 0

    signature = str(decision["signature"])
    first = str(decision["first_failed_on"])
    count = int(decision["day_count"])
    run_id = str(decision["run_id"])
    title = issue_title(prefix, signature, count)
    body = render_body(
        run_url=run_url,
        signature=signature,
        day_count_value=count,
        first_failed_on=first,
        run_id=run_id,
    )
    body_path = os.environ.get("REPORT_BODY_PATH", "")
    if not body_path:
        body_path = os.path.join(
            os.environ.get("RUNNER_TEMP", "/tmp"),
            "nightly-failure-body.md",
        )
    _write_body(body_path, body)

    if action == "update":
        number = str(matches[0]["number"])
        print(f"DO: update issue {number}")
        proc = _run_gh(
            [
                "gh",
                "issue",
                "edit",
                number,
                "--repo",
                repo,
                "--title",
                title,
                "--body-file",
                body_path,
            ]
        )
        if proc.returncode != 0:
            return _fail_gh(proc, "issue edit")
        print(json.dumps({"action": "update", "number": int(number)}))
        print("DONE: updated the open failure issue")
        return 0

    if action in {"create", "replace"}:
        code = ensure_labels(repo, labels)
        if code != 0:
            return code

    if action == "replace":
        for item in matches:
            number = str(item["number"])
            print(f"DO: close replaced issue {number}")
            proc = _run_gh(
                [
                    "gh",
                    "issue",
                    "close",
                    number,
                    "--repo",
                    repo,
                    "--reason",
                    "completed",
                    "--comment",
                    "Replaced by a new failure issue for this run.",
                ]
            )
            if proc.returncode != 0:
                return _fail_gh(proc, "issue close")

    print(f"DO: create issue {title}")
    cmd = [
        "gh",
        "issue",
        "create",
        "--repo",
        repo,
        "--title",
        title,
        "--body-file",
        body_path,
        "--assignee",
        normalize_assignee(assignee),
    ]
    for label in labels:
        cmd.extend(["--label", label])
    proc = _run_gh(cmd)
    if proc.returncode != 0:
        return _fail_gh(proc, "issue create")
    url = (proc.stdout or "").strip()
    print(f"OK: created {url}")
    print(json.dumps({"action": action, "title": title}))
    print("DONE: recorded the scheduled failure")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix")
    parser.add_argument("--run-id")
    parser.add_argument("--run-url", default="")
    parser.add_argument("--repo", default=os.environ.get("GH_REPO", ""))
    parser.add_argument("--assignee", default=os.environ.get("ASSIGNEE", "SebTardif"))
    parser.add_argument("--label", action="append", default=[])
    parser.add_argument("--today", default="")
    parser.add_argument("--issue-json", default="")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)

    print("PLAN: report scheduled failure")
    if not args.prefix or not args.run_id or not args.repo:
        print("FAIL: --prefix, --run-id, and --repo are required", file=sys.stderr)
        return 2
    raw_results = os.environ.get("JOB_RESULTS", "")
    if not raw_results.strip():
        print("FAIL: JOB_RESULTS is required", file=sys.stderr)
        return 2
    results = parse_results(raw_results)
    if not results:
        print("FAIL: JOB_RESULTS had no job=status pairs", file=sys.stderr)
        return 2
    if args.today:
        today = date.fromisoformat(args.today)
    else:
        today = datetime.now(timezone.utc).date()
    labels = args.label or ["nightly-failure", "ready"]
    assignee = normalize_assignee(args.assignee or "")

    if args.dry_run:
        issue = json.loads(args.issue_json) if args.issue_json else None
        decision = decide(today=today, results=results, issue=issue, run_id=args.run_id)
        print(f"OK: decision {decision['action']}")
        print(json.dumps(decision))
        print("DONE: dry run")
        return 0

    print("DO: list open issues")
    found, code = _list_issues(args.repo, labels[0])
    if code != 0:
        return code
    prefix_token = args.prefix + ":"
    matches = [
        item
        for item in found
        if str(item.get("title", "")).startswith(prefix_token)
    ]
    issue, matches = prepare_matches(matches, results=results, today=today)
    decision = decide(today=today, results=results, issue=issue, run_id=args.run_id)
    return apply_decision(
        repo=args.repo,
        prefix=args.prefix,
        labels=labels,
        assignee=assignee,
        run_url=args.run_url,
        decision=decision,
        matches=matches,
    )


if __name__ == "__main__":
    sys.exit(main())
