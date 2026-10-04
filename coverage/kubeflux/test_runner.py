"""Opt-in behavioral checks for the corpus runner, separate from Cargo tests."""

import json
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest

import run


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.case = self.root / "example"
        (self.case / "repo").mkdir(parents=True)
        (self.case / "repo" / "pod.yaml").write_text("image: demo:1.0.0\n")
        self.spec = {
            "id": "example",
            "category": "runner",
            "description": "Exercise the CLI boundary in a disposable repository.",
            "audit": [],
            "source": [],
            "expectation": "pass",
            "todo": "",
            "command": "apply",
            "responses": {},
            "checks": {
                "exit_code": 20,
                "json": [{"at": "/summary/applied_count", "equals": 1}],
            },
        }
        self.binary = self.root / "cli"

    def write_case(self, program):
        (self.case / "case.json").write_text(json.dumps(self.spec))
        self.binary.write_text("#!/usr/bin/env python3\n" + program)
        self.binary.chmod(0o755)

    def test_checks_real_cli_output_and_exact_written_files(self):
        (self.case / "expected").mkdir()
        (self.case / "expected" / "pod.yaml").write_text("image: demo:2.0.0\n")
        self.write_case(
            "import sys\nfrom pathlib import Path\n"
            "(Path(sys.argv[2]) / 'pod.yaml').write_text('image: demo:2.0.0\\n')\n"
            "print('{\"summary\": {\"applied_count\": 1}}')\n"
            "sys.exit(20)\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "PASS", result)
        self.assertEqual(result["observed"]["summary"]["applied_count"], 1)
        self.assertEqual(
            (self.case / "repo" / "pod.yaml").read_text(), "image: demo:1.0.0\n"
        )

    def test_known_gap_is_todo_until_the_desired_checks_pass(self):
        self.spec.update(expectation="todo", todo="U7: embedded patch images are missed")
        self.write_case("print('{\"summary\": {\"applied_count\": 0}}')\n")
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "TODO", result)
        self.assertEqual(result["todo"], self.spec["todo"])
        self.assertTrue(result["mismatches"])
        self.write_case(
            "import sys\nprint('{\"summary\": {\"applied_count\": 1}}')\nsys.exit(20)\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "XPASS", result)

    def test_runtime_and_protocol_errors_cannot_hide_as_todos(self):
        self.spec.update(expectation="todo", todo="U7: unsupported declaration")
        for code, output in [(2, '{}'), (101, '{}'), (0, 'not JSON')]:
            with self.subTest(code=code, output=output):
                self.write_case(f"import sys\nprint({output!r})\nsys.exit({code})\n")
                result = run.run_case(self.case, self.binary)
                self.assertEqual(result["status"], "ERROR", result)
                self.assertTrue(result["error"])

    def test_missing_desired_field_is_reported_as_a_capability_gap(self):
        self.spec.update(expectation="todo", todo="U7: declaration not discovered")
        self.spec["checks"] = {
            "exit_code": 0,
            "json": [{"at": "/unchecked_version_declarations/0/current_value", "equals": "demo:1.0.0"}],
        }
        self.write_case("print('{\"unchecked_version_declarations\": []}')\n")
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "TODO", result)
        self.assertIn("missing", result["mismatches"][0])

    def test_partial_array_checks_match_fields_on_the_same_declaration(self):
        self.spec["checks"] = {"exit_code": 0, "json": [{
            "at": "/planned", "contains": {"path": "pod.yaml", "latest_version": "2.0.0"}
        }]}
        self.write_case("print('{\"planned\": [{\"path\": \"pod.yaml\", \"latest_version\": \"2.0.0\", \"id\": \"opaque\"}]}')\n")
        self.assertEqual(run.run_case(self.case, self.binary)["status"], "PASS")
        self.write_case("print('{\"planned\": [{\"path\": \"pod.yaml\", \"latest_version\": \"1.0.0\"}, {\"path\": \"other.yaml\", \"latest_version\": \"2.0.0\"}]}')\n")
        self.assertEqual(run.run_case(self.case, self.binary)["status"], "FAIL")

    def test_local_responses_and_placeholders_exercise_the_http_boundary(self):
        self.spec["responses"] = {"/v2/demo/app/tags/list": {"json": {"tags": ["1.0.0", "2.0.0"]}}}
        self.spec["checks"] = {"exit_code": 0, "json": [{"at": "/tags", "equals": ["1.0.0", "2.0.0"]}]}
        (self.case / "repo" / "origin.txt").write_text("{{server}}")
        self.write_case(
            "import sys, urllib.request\nfrom pathlib import Path\n"
            "origin = (Path(sys.argv[2]) / 'origin.txt').read_text()\n"
            "print(urllib.request.urlopen(origin + '/v2/demo/app/tags/list?n=1000').read().decode())\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "PASS", result)
        self.assertEqual(result["requests"], ["/v2/demo/app/tags/list?n=1000"])

    def test_command_reports_todos_without_failing_unless_strict(self):
        self.spec.update(expectation="todo", todo="U7: unsupported declaration")
        self.write_case("print('{\"summary\": {\"applied_count\": 0}}')\n")
        args = [sys.executable, run.__file__, "--cases", str(self.root), "--binary", str(self.binary), "--json"]
        normal = subprocess.run(args, capture_output=True, text=True, check=False)
        self.assertEqual(normal.returncode, 0, normal.stderr)
        self.assertEqual(json.loads(normal.stdout)["summary"]["TODO"], 1)
        strict = subprocess.run(args + ["--strict"], capture_output=True, text=True, check=False)
        self.assertEqual(strict.returncode, 1, strict.stderr)
        self.assertEqual(json.loads(strict.stdout)["results"][0]["status"], "TODO")

    def test_unexpected_mutations_are_failures_even_for_known_gaps(self):
        self.spec.update(expectation="todo", todo="U7: unsupported declaration")
        self.write_case(
            "import sys\nfrom pathlib import Path\n"
            "(Path(sys.argv[2]) / 'pod.yaml').write_text('unrelated: damaged\\n')\n"
            "print('{\"summary\": {\"applied_count\": 0}}')\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "FAIL", result)
        self.assertTrue(any("unexpected edit" in message for message in result["mismatches"]))

    def test_a_known_wrong_update_keeps_its_desired_result_and_exact_failure_evidence(self):
        self.spec.update(expectation="todo", todo="U6: wrong release selected")
        (self.case / "expected").mkdir()
        (self.case / "expected" / "pod.yaml").write_text("image: demo:2.0.0\n")
        (self.case / "known_actual").mkdir()
        (self.case / "known_actual" / "pod.yaml").write_text("image: demo:2026092811\n")
        for tag, status in [("2026092811", "TODO"), ("corrupted", "FAIL")]:
            self.write_case(
                "import sys\nfrom pathlib import Path\n"
                f"(Path(sys.argv[2]) / 'pod.yaml').write_text('image: demo:{tag}\\n')\n"
                "print('{\"summary\": {\"applied_count\": 1}}')\nsys.exit(20)\n"
            )
            result = run.run_case(self.case, self.binary)
            self.assertEqual(result["status"], status, result)
            self.assertIn("file differs: pod.yaml", result["mismatches"])

    def test_invalid_case_definitions_are_errors_not_empty_successes(self):
        original = json.loads(json.dumps(self.spec))
        for invalid in [
            {"command": "aply"},
            {"expectation": "tood"},
            {"id": "different-case"},
            {"checks": {"exit_code": 0, "json": []}},
        ]:
            with self.subTest(invalid=invalid):
                self.spec = {**original, **invalid}
                self.write_case("print('{\"summary\": {\"applied_count\": 1}}')\n")
                self.assertEqual(run.run_case(self.case, self.binary)["status"], "ERROR")

    def test_only_the_documented_structured_runtime_error_is_a_known_gap(self):
        self.spec.update(expectation="todo", todo="U15: valid list cannot be edited")
        self.spec["known_failure"] = {"exit_code": 2, "error": "runtime_error", "message_contains": "missing YAML path spec.initContainers[0].image"}
        for message, status in [
            ("missing YAML path spec.initContainers[0].image; target changed after planning", "TODO"),
            ("disk write failed", "ERROR"),
        ]:
            error = {"error": "runtime_error", "message": message, "exit_code": 2}
            self.write_case(f"import sys\nprint({json.dumps(error)!r}, file=sys.stderr)\nsys.exit(2)\n")
            result = run.run_case(self.case, self.binary)
            self.assertEqual(result["status"], status, result)

    def test_incomplete_check_can_require_a_nonzero_outcome_without_pinning_a_code(self):
        self.spec.update(expectation="todo", todo="U13: total resolution failure exits zero")
        self.spec["checks"] = {"exit_code_not": 0, "json": [{"at": "/summary/checked_count", "equals": 0}]}
        self.write_case("print('{\"summary\": {\"checked_count\": 0}}')\n")
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "TODO", result)
        self.assertTrue(any("exit code" in item for item in result["mismatches"]))

    def test_unexpected_file_deletion_cannot_be_hidden_as_a_todo(self):
        self.spec.update(expectation="todo", todo="U7: unsupported declaration")
        self.write_case(
            "import sys\nfrom pathlib import Path\n"
            "(Path(sys.argv[2]) / 'pod.yaml').unlink()\n"
            "print('{\"summary\": {\"applied_count\": 0}}')\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "FAIL", result)

    def test_known_runtime_failure_cannot_hide_partial_application(self):
        self.spec.update(expectation="todo", todo="U15: preflight failure")
        self.spec["known_failure"] = {"exit_code": 2, "error": "runtime_error", "message_contains": "missing YAML path"}
        (self.case / "expected").mkdir()
        (self.case / "expected" / "pod.yaml").write_text("image: demo:2.0.0\n")
        self.write_case(
            "import sys\nfrom pathlib import Path\n"
            "(Path(sys.argv[2]) / 'pod.yaml').write_text('image: demo:2.0.0\\n')\n"
            "print('{\"error\": \"runtime_error\", \"message\": \"missing YAML path\", \"exit_code\": 2}', file=sys.stderr)\n"
            "sys.exit(2)\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "FAIL", result)

    def test_nonzero_completeness_outcomes_can_pass_when_they_keep_a_valid_report(self):
        self.spec.update(expectation="todo", todo="U13: total failure needs a nonzero outcome")
        self.spec["checks"] = {"exit_code_not": 0, "json": [{"at": "/summary/checked_count", "equals": 0}]}
        report = {"mode": "plan", "summary": {"checked_count": 0}, "planned": [], "skipped": []}
        self.write_case(f"import sys\nprint({json.dumps(report)!r})\nsys.exit(1)\n")
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "XPASS", result)

    def test_unexpected_http_methods_are_runner_errors_even_if_the_cli_recovers(self):
        (self.case / "repo" / "origin.txt").write_text("{{server}}")
        self.write_case(
            "import sys, urllib.request, urllib.error\nfrom pathlib import Path\n"
            "origin = (Path(sys.argv[2]) / 'origin.txt').read_text()\n"
            "try:\n    urllib.request.urlopen(urllib.request.Request(origin + '/upload', data=b'x'))\n"
            "except urllib.error.HTTPError:\n    pass\n"
            "print('{\"summary\": {\"applied_count\": 1}}')\nsys.exit(20)\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "ERROR", result)

    def test_replacing_a_file_with_a_symlink_cannot_pass_on_matching_bytes(self):
        (self.case / "repo" / "copy.txt").write_text("image: demo:1.0.0\n")
        self.write_case(
            "import sys\nfrom pathlib import Path\n"
            "path = Path(sys.argv[2]) / 'pod.yaml'\n"
            "path.unlink()\npath.symlink_to('copy.txt')\n"
            "print('{\"summary\": {\"applied_count\": 1}}')\nsys.exit(20)\n"
        )
        result = run.run_case(self.case, self.binary)
        self.assertEqual(result["status"], "ERROR", result)


if __name__ == "__main__":
    unittest.main()
