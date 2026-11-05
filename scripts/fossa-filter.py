#!/usr/bin/env python3
"""Exit 0 when every FOSSA issue is a documented false positive."""

import json
import sys

# License ids FOSSA reports for crates whose real license is fine.
KNOWN_FALSE_POSITIVES = {
    "r-efi": {"LGPL-2.1-or-later", "LGPL-2.1-only", "LGPL-2.1"},
    "cc": {"Apache-2.0 WITH LLVM-exception"},
    "compiler_builtins": {"Apache-2.0 WITH LLVM-exception"},
}


def extract_package(revision_id):
    """Return the crate name from a FOSSA revision id."""
    body = revision_id.split("+", 1)[-1]
    return body.rsplit("$", 1)[0]


def issue_list(data):
    if isinstance(data, list):
        return data
    if isinstance(data, dict):
        for key in ("issues", "results"):
            if isinstance(data.get(key), list):
                return data[key]
    return []


def license_id(issue):
    for key in ("licenseId", "license", "id"):
        value = issue.get(key)
        if isinstance(value, str) and value:
            return value
    return ""


def is_known(issue):
    package = extract_package(issue.get("revisionId") or "")
    allowed = KNOWN_FALSE_POSITIVES.get(package)
    if not allowed:
        return False
    found = license_id(issue)
    if not found:
        return True
    return any(item in found for item in allowed)


def remaining(data):
    return [issue for issue in issue_list(data) if not is_known(issue)]


def main():
    path = sys.argv[1]
    raw = open(path, encoding="utf-8").read()
    try:
        data = json.loads(raw) if raw.strip() else []
    except json.JSONDecodeError:
        print("FOSSA JSON was not parseable")
        sys.exit(1)
    left = remaining(data)
    if left:
        for issue in left:
            print(
                issue.get("revisionId"),
                issue.get("type"),
                license_id(issue),
            )
        sys.exit(1)
    print("FOSSA issues are empty or documented false positives")


if __name__ == "__main__":
    main()
