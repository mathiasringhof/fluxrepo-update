"""Opt-in checks that malformed examples cannot manufacture coverage results."""

import json
from pathlib import Path
import tempfile
import unittest

import run


class ValidationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.case = self.root / "example"
        (self.case / "repo").mkdir(parents=True)
        (self.case / "repo" / "pod.yaml").write_text("image: demo:1.0.0\n")
        self.binary = self.root / "cli"
        self.launched = self.root / "launched"
        self.spec = {
            "id": "example",
            "category": "runner",
            "description": "A minimal case with a real CLI result.",
            "audit": [],
            "source": [],
            "expectation": "pass",
            "todo": "",
            "command": "apply",
            "responses": {},
            "checks": {
                "exit_code": 0,
                "json": [{"at": "/summary/applied_count", "equals": 0}],
            },
        }

    def execute(self, spec=None, payload=None):
        (self.case / "case.json").write_text(json.dumps(spec or self.spec))
        report = {"mode": "apply", "summary": {
            "discovered_count": 1, "checked_count": 1, "unchecked_count": 0,
            "planned_count": 0, "applied_count": 0, "skipped_count": 0, "changed_file_count": 0,
        }, "planned": [], "skipped": []}
        output = json.dumps(report | (payload or {}))
        self.binary.write_text(
            "#!/usr/bin/env python3\nfrom pathlib import Path\n"
            f"Path({str(self.launched)!r}).touch()\nprint({output!r})\n"
        )
        self.binary.chmod(0o755)
        return run.run_case(self.case, self.binary)

    def assert_invalid(self, spec):
        self.launched.unlink(missing_ok=True)
        result = self.execute(spec)
        self.assertEqual(result["status"], "ERROR", result)
        self.assertFalse(self.launched.exists(), "invalid cases must not run the CLI")

    def test_metadata_requires_strings_and_lists_of_strings(self):
        for field, value in [
            ("category", 42),
            ("description", ["text"]),
            ("todo", 42),
            ("audit", "U1"),
            ("source", ["pod.yaml", 42]),
        ]:
            with self.subTest(field=field, value=value):
                self.assert_invalid(self.spec | {field: value})

    def test_output_checks_require_typed_exit_codes_and_object_selectors(self):
        for checks in [
            {"exit_code": False, "json": [{"at": "/summary/applied_count", "equals": 0}]},
            {"exit_code": 0.0, "json": [{"at": "/summary/applied_count", "equals": 0}]},
            {"exit_code": 256, "json": [{"at": "/summary/applied_count", "equals": 0}]},
            {"exit_code_not": -1, "json": [{"at": "/summary/applied_count", "equals": 0}]},
            {"exit_code": 0, "json": [{"at": "/planned", "contains": []}]},
            {"exit_code": 0, "json": [{"at": "/planned", "contains": {}}]},
            {"exit_code": 0, "json": ["not a check object"]},
        ]:
            with self.subTest(checks=checks):
                self.assert_invalid(self.spec | {"checks": checks})

    def test_invariants_require_the_same_valid_selectors_as_desired_checks(self):
        for invariants in [None, "not a list", [{"at": "/planned", "contains": {}}]]:
            with self.subTest(invariants=invariants):
                checks = self.spec["checks"] | {"invariants": invariants}
                self.assert_invalid(self.spec | {"checks": checks})

    def test_malformed_http_responses_are_rejected_before_running(self):
        invalid_responses = [
            [],
            {"/tags": {"text": 7}},
            {"/tags": {"headers": []}},
            {"/tags": {"headers": {"X-Tag": 7}}},
            {"/tags": {"headers": {"X-Tag": "bad\nheader"}}},
            {"/tags": {"headers": {"X-Tag": "\u2603"}}},
            {"/tags": {"status": True}},
            {"/tags": {"status": "200"}},
            {"/tags": {"status": 600}},
            {"/tags": {"status": 99}},
        ]
        for responses in invalid_responses:
            with self.subTest(responses=responses):
                self.assert_invalid(self.spec | {"responses": responses})

    def test_known_failures_require_a_precise_typed_runtime_error(self):
        known = {"exit_code": 2, "error": "runtime_error", "message_contains": "missing YAML path"}
        spec = self.spec | {"expectation": "todo", "todo": "A documented preflight failure."}
        for invalid in [
            known | {"exit_code": 2.0},
            known | {"message_contains": 42},
            known | {"message_contains": " "},
            [known],
        ]:
            with self.subTest(known_failure=invalid):
                self.assert_invalid(spec | {"known_failure": invalid})

    def test_partial_declaration_checks_do_not_equate_booleans_with_counts(self):
        for expected, actual in [
            ({"count": 1}, {"count": True}),
            ({"enabled": False}, {"enabled": 0}),
            ({"counts": [1]}, {"counts": [True]}),
        ]:
            with self.subTest(expected=expected, actual=actual):
                spec = self.spec | {"checks": {"exit_code": 0, "json": [
                    {"at": "/planned", "contains": expected}
                ]}}
                result = self.execute(spec, {"planned": [actual]})
                self.assertEqual(result["status"], "FAIL", result)
                result = self.execute(spec, {"planned": [expected | {"extra": "allowed"}]})
                self.assertEqual(result["status"], "PASS", result)

    def test_exact_checks_distinguish_booleans_and_preserve_complete_structure(self):
        wanted = {"count": 1, "flags": [False]}
        spec = self.spec | {"checks": {"exit_code": 0, "json": [
            {"at": "/metadata", "equals": wanted}
        ]}}
        for actual in [
            {"count": True, "flags": [False]},
            {"count": 1, "flags": [0]},
            wanted | {"extra": "not allowed"},
        ]:
            with self.subTest(actual=actual):
                result = self.execute(spec, {"metadata": actual})
                self.assertEqual(result["status"], "FAIL", result)
        self.assertEqual(self.execute(spec, {"metadata": wanted})["status"], "PASS")

    def test_array_pointers_cannot_select_with_noncanonical_indices(self):
        payload = {"planned": [{"version": "first"}, {"version": "last"}]}
        for index in ["-1", "01", "+1", " 1", "\u0661", "-", "2"]:
            with self.subTest(index=index):
                spec = self.spec | {"checks": {"exit_code": 0, "json": [
                    {"at": f"/planned/{index}/version", "equals": "last"}
                ]}}
                self.assertEqual(self.execute(spec, payload)["status"], "FAIL")
        spec = self.spec | {"checks": {"exit_code": 0, "json": [
            {"at": "/planned/1/version", "equals": "last"},
            {"at": "/keys/-1", "equals": "literal"},
            {"at": "/a~1b/~0", "equals": "escaped"},
        ]}}
        payload.update({"keys": {"-1": "literal"}, "a/b": {"~": "escaped"}})
        self.assertEqual(self.execute(spec, payload)["status"], "PASS")

    def test_malformed_pointer_escapes_are_case_errors(self):
        for pointer in ["/summary/~2count", "/summary/count~", "/summary/~~0"]:
            with self.subTest(pointer=pointer):
                checks = {"exit_code": 0, "json": [{"at": pointer, "equals": 0}]}
                self.assert_invalid(self.spec | {"checks": checks})

    def test_overlays_only_replace_inputs_and_known_actual_requires_a_todo(self):
        for folder, filename, expectation in [
            ("expected", "new.yaml", "pass"),
            ("known_actual", "pod.yaml", "pass"),
            ("known_actual", "new.yaml", "todo"),
        ]:
            with self.subTest(folder=folder, filename=filename, expectation=expectation):
                directory = self.case / folder
                directory.mkdir()
                path = directory / filename
                path.write_text("image: demo:2.0.0\n")
                spec = self.spec | {"expectation": expectation, "todo": "A known update gap."}
                try:
                    self.assert_invalid(spec)
                finally:
                    path.unlink()
                    directory.rmdir()


if __name__ == "__main__":
    unittest.main()
