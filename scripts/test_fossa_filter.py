import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

FILTER = Path(__file__).resolve().parent / "fossa-filter.py"
spec = importlib.util.spec_from_file_location("fossa_filter", FILTER)
fossa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fossa)


class FilterTest(unittest.TestCase):
    def test_extract_package(self):
        self.assertEqual(fossa.extract_package("cargo+r-efi$5.3.0"), "r-efi")

    def test_known_lgpl_type_crate_is_dropped(self):
        data = [
            {
                "revisionId": "cargo+r-efi$5.3.0",
                "type": "policy_conflict",
                "licenseId": "LGPL-2.1-or-later",
            }
        ]
        self.assertEqual(fossa.remaining(data), [])

    def test_unknown_issue_remains(self):
        data = [
            {
                "revisionId": "cargo+some-crate$1.0.0",
                "type": "policy_conflict",
                "licenseId": "GPL-3.0-only",
            }
        ]
        self.assertEqual(len(fossa.remaining(data)), 1)

    def test_cli_exits_zero_for_known_only(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            json.dump(
                [
                    {
                        "revisionId": "cargo+cc$1.2.0",
                        "licenseId": "Apache-2.0 WITH LLVM-exception",
                    }
                ],
                handle,
            )
            path = handle.name
        result = subprocess.run(
            [sys.executable, str(Path(__file__).parent / "fossa-filter.py"), path],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
