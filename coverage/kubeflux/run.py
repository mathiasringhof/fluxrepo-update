#!/usr/bin/env python3
"""Run isolated, synthetic repositories through fluxrepo-update's public CLI."""

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
from urllib.parse import urlsplit


def expand(value, replacements):
    if isinstance(value, str):
        for key, replacement in replacements.items():
            value = value.replace(key, replacement)
    elif isinstance(value, list):
        value = [expand(item, replacements) for item in value]
    elif isinstance(value, dict):
        value = {key: expand(item, replacements) for key, item in value.items()}
    return value


class FixtureServer:
    def __init__(self, responses):
        self.requests = []
        self.errors = []
        self.tls_directory = Path(__file__).resolve().parent / "tls"
        self.tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        # These static credentials belong only to the loopback corpus fixture.
        self.tls_context.load_cert_chain(
            self.tls_directory / "github-api-cert.pem", self.tls_directory / "github-api-key.pem"
        )
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def send_error(self, code, message=None, explain=None):
                if code == 501:
                    owner.errors.append(f"unexpected HTTP method: {self.command} {self.path}")
                super().send_error(code, message, explain)

            def do_CONNECT(self):
                if self.path != "api.github.com:443" or isinstance(self.connection, ssl.SSLSocket):
                    owner.errors.append(f"external HTTPS request: {self.path}")
                    self.send_error(502)
                    return
                self.send_response(200, "Connection Established")
                self.end_headers()
                self.wfile.flush()
                self.close_connection = True
                try:
                    with owner.tls_context.wrap_socket(self.connection, server_side=True) as connection:
                        Handler(connection, self.client_address, self.server)
                except ssl.SSLError as error:
                    owner.errors.append(f"fixture TLS handshake failed: {error}")

            def do_GET(self):
                owner.requests.append(self.path)
                response = owner.responses.get(self.path) or owner.responses.get(urlsplit(self.path).path)
                if isinstance(self.connection, ssl.SSLSocket) and self.headers.get("Host") not in (
                    "api.github.com", "api.github.com:443"
                ):
                    owner.errors.append(f"unexpected HTTPS host: {self.headers.get('Host')}")
                    self.send_error(502)
                    return
                if self.path.startswith(("http://", "https://")) or response is None:
                    owner.errors.append(f"unexpected HTTP request: {self.path}")
                    self.send_error(502)
                    return
                content = json.dumps(response["json"]) if "json" in response else response.get("text", "")
                body = content.encode()
                self.send_response(response.get("status", 200))
                self.send_header("Content-Length", str(len(body)))
                for key, value in response.get("headers", {}).items():
                    self.send_header(key, value)
                self.end_headers()
                self.wfile.write(body)

        class Server(ThreadingHTTPServer):
            def handle_error(self, _request, _client_address):
                owner.errors.append(f"fixture HTTP handler failed: {sys.exc_info()[1]}")

        self.server = Server(("127.0.0.1", 0), Handler)
        self.origin = f"http://127.0.0.1:{self.server.server_port}"
        self.replacements = {"{{server}}": self.origin, "{{registry}}": self.origin.removeprefix("http://")}
        self.responses = expand(responses, self.replacements)
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.01), daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def environment(self):
        environment = os.environ.copy()
        for key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"):
            environment[key] = environment[key.lower()] = self.origin
        environment["NO_PROXY"] = environment["no_proxy"] = "127.0.0.1,localhost"
        # Reqwest's default native-tls/OpenSSL backend keeps certificate verification.
        environment["SSL_CERT_FILE"] = str(self.tls_directory / "github-api-ca.pem")
        return environment


def files_in(root):
    files = {}
    for path in root.rglob("*"):
        if path.is_symlink():
            raise ValueError(f"symlinks are not supported in corpus files: {path}")
        if path.is_file():
            files[path.relative_to(root).as_posix()] = path.read_bytes()
        elif not path.is_dir():
            raise ValueError(f"unsupported filesystem entry: {path}")
    return files


def at_pointer(value, pointer):
    if (not isinstance(pointer, str) or (pointer and not pointer.startswith("/"))
            or any(not escape or escape[0] not in "01" for escape in pointer.split("~")[1:])):
        raise ValueError(f"invalid JSON pointer: {pointer!r}")
    if not pointer:
        return value
    for part in pointer[1:].split("/"):
        part = part.replace("~1", "/").replace("~0", "~")
        if isinstance(value, list):
            if (not part.isascii() or not part.isdecimal()
                    or (len(part) > 1 and part.startswith("0"))):
                raise IndexError(f"invalid JSON array index: {part!r}")
            value = value[int(part)]
        else:
            value = value[part]
    return value


def matches(actual, expected, partial=True):
    if isinstance(expected, dict):
        return isinstance(actual, dict) and (partial or actual.keys() == expected.keys()) and all(
            key in actual and matches(actual[key], value, partial=partial)
            for key, value in expected.items()
        )
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) == len(expected) and all(
            matches(item, wanted, partial=False) for item, wanted in zip(actual, expected)
        )
    if isinstance(actual, bool) or isinstance(expected, bool):
        return type(actual) is type(expected) and actual == expected
    return actual == expected


def validate_case(spec, case_dir):
    if not isinstance(spec, dict):
        raise ValueError("case definition must be an object")
    for field in ("id", "category", "description", "command", "expectation"):
        if not isinstance(spec.get(field), str) or not spec[field].strip():
            raise ValueError(f"{field} must be a nonempty string")
    if not isinstance(spec.get("todo", ""), str):
        raise ValueError("todo must be a string")
    for field in ("audit", "source"):
        values = spec.get(field, [])
        if not isinstance(values, list) or any(
            not isinstance(value, str) or not value.strip() for value in values
        ):
            raise ValueError(f"{field} must be a list of nonempty strings")
    if spec["id"] != case_dir.name:
        raise ValueError("case id must match its directory")
    if spec["command"] not in ("inventory", "plan", "apply"):
        raise ValueError("command must be inventory, plan, or apply")
    if spec["expectation"] not in ("pass", "todo"):
        raise ValueError("expectation must be pass or todo")
    if not spec["category"] or not spec["description"]:
        raise ValueError("category and description are required")
    if spec["expectation"] == "todo" and not spec.get("todo", "").strip():
        raise ValueError("a TODO needs its unsupported capability explained")
    checks = spec["checks"]
    if not isinstance(checks, dict):
        raise ValueError("checks must be an object")
    if ("exit_code" in checks) == ("exit_code_not" in checks):
        raise ValueError("choose exactly one exit_code or exit_code_not check")
    exit_code = checks.get("exit_code", checks.get("exit_code_not"))
    if type(exit_code) is not int or not 0 <= exit_code <= 255:
        raise ValueError("exit code checks require an integer from 0 to 255")
    invariants = checks.get("invariants", [])
    if not isinstance(checks["json"], list) or not isinstance(invariants, list):
        raise ValueError("json and invariants must be lists of structured output checks")
    if not checks["json"] and not invariants:
        raise ValueError("at least one structured output check is required")
    for check in checks["json"] + invariants:
        if not isinstance(check, dict):
            raise ValueError("each JSON check must be an object")
        if ("equals" in check) == ("contains" in check):
            raise ValueError("choose exactly one equals or contains operator")
        if "contains" in check and (
            not isinstance(check["contains"], dict) or not check["contains"]
        ):
            raise ValueError("contains must select a nonempty object")
        pointer = check["at"]
        if (not isinstance(pointer, str) or (pointer and not pointer.startswith("/"))
                or any(not escape or escape[0] not in "01" for escape in pointer.split("~")[1:])):
            raise ValueError(f"invalid JSON pointer: {pointer!r}")
    responses = spec.get("responses", {})
    if not isinstance(responses, dict):
        raise ValueError("responses must be an object")
    for path, response in responses.items():
        if not isinstance(path, str) or not path.startswith("/") or not isinstance(response, dict):
            raise ValueError("HTTP responses require a path and response object")
        if "text" in response and "json" in response:
            raise ValueError("HTTP response must choose text or json")
        if "text" in response and not isinstance(response["text"], str):
            raise ValueError("HTTP response text must be a string")
        status = response.get("status", 200)
        if type(status) is not int or not 100 <= status <= 599:
            raise ValueError("HTTP response status must be an integer from 100 to 599")
        headers = response.get("headers", {})
        if not isinstance(headers, dict):
            raise ValueError("HTTP response headers must be an object")
        for name, value in headers.items():
            if (not isinstance(name, str) or not name or not name.isascii()
                    or any(not (char.isalnum() or char in "!#$%&'*+-.^_`|~") for char in name)
                    or not isinstance(value, str) or "\r" in value or "\n" in value):
                raise ValueError("HTTP headers require valid names and single-line string values")
            value.encode("latin-1")
    if "known_failure" in spec:
        known = spec["known_failure"]
        if (not isinstance(known, dict) or spec["expectation"] != "todo"
                or type(known.get("exit_code")) is not int or known["exit_code"] != 2
                or known.get("error") != "runtime_error"
                or not isinstance(known.get("message_contains"), str)
                or not known["message_contains"].strip()):
            raise ValueError("known_failure requires a TODO and a specific structured runtime error")
    if not (case_dir / "repo").is_dir():
        raise ValueError("case needs an input repo directory")
    input_paths = files_in(case_dir / "repo").keys()
    for name in ("expected", "known_actual"):
        directory = case_dir / name
        if not directory.exists():
            continue
        if not directory.is_dir():
            raise ValueError(f"{name} must be an overlay directory")
        if name == "known_actual" and spec["expectation"] != "todo":
            raise ValueError("known_actual is only allowed for TODO cases")
        extra_paths = files_in(directory).keys() - input_paths
        if extra_paths:
            raise ValueError(f"{name} may only replace input files: {', '.join(sorted(extra_paths))}")


def run_case(case_dir, binary):
    try:
        return execute_case(case_dir, binary)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, subprocess.TimeoutExpired) as error:
        return {"id": case_dir.name, "status": "ERROR", "error": str(error)}


def validate_report(report, command):
    if not isinstance(report, dict) or "error" in report:
        raise ValueError("CLI did not produce a JSON report object")
    if command == "inventory":
        counts = report
        count_fields = ("discovered_count",)
        arrays = ("chart_targets", "image_bindings", "unchecked_version_declarations")
    else:
        if report.get("mode") not in ("plan", "apply") or not isinstance(report.get("summary"), dict):
            raise ValueError("CLI report has an invalid mode or summary")
        counts = report["summary"]
        count_fields = ("discovered_count", "checked_count", "unchecked_count",
                        "planned_count", "applied_count", "skipped_count", "changed_file_count")
        arrays = ("planned", "skipped")
    if any(type(counts.get(field)) is not int or counts[field] < 0 for field in count_fields):
        raise ValueError("CLI report is missing nonnegative integer counts")
    if any(not isinstance(report.get(field), list)
           or any(not isinstance(item, dict) for item in report[field]) for field in arrays):
        raise ValueError("CLI report is missing declaration arrays")


def check_json(report, checks):
    mismatches = []
    for check in checks:
        try:
            actual = at_pointer(report, check["at"])
        except (KeyError, IndexError, TypeError):
            mismatches.append(f"{check['at']}: missing; expected {check.get('equals', check.get('contains'))!r}")
            continue
        if "equals" in check:
            if not matches(actual, check["equals"], partial=False):
                mismatches.append(f"{check['at']}: expected {check['equals']!r}, got {actual!r}")
        elif not isinstance(actual, list) or not any(matches(item, check["contains"]) for item in actual):
            mismatches.append(f"{check['at']}: no declaration matches {check['contains']!r}; got {actual!r}")
    return mismatches


def execute_case(case_dir, binary):
    spec = json.loads((case_dir / "case.json").read_text())
    validate_case(spec, case_dir)
    files_in(case_dir / "repo")
    with FixtureServer(spec.get("responses", {})) as server, tempfile.TemporaryDirectory(prefix="fluxrepo-corpus-") as temporary:
        spec = expand(spec, server.replacements)
        repo = Path(temporary) / "repo"
        shutil.copytree(case_dir / "repo", repo)
        for relative, content in files_in(repo).items():
            text = expand(content.decode(), server.replacements)
            (repo / relative).write_bytes(text.encode())
        original = files_in(repo)
        expected = original.copy()
        expected.update({path: expand(data.decode(), server.replacements).encode() for path, data in files_in(case_dir / "expected").items()})
        known_actual = {path: expand(data.decode(), server.replacements).encode() for path, data in files_in(case_dir / "known_actual").items()} if spec["expectation"] == "todo" else {}
        command = [str(binary), "inventory" if spec["command"] == "inventory" else "update-helm", str(repo), "--json"]
        if spec["command"] != "inventory":
            command.append("--non-interactive")
        if spec["command"] == "apply":
            command.append("--write")
        process = subprocess.run(command, capture_output=True, text=True, timeout=30, check=False, env=server.environment())
        if server.errors:
            raise RuntimeError("; ".join(server.errors))
        mismatches = []
        known_runtime_failure = False
        observed = json.loads(process.stdout) if process.stdout.strip() else None
        new_outcome_code = (
            "exit_code_not" in spec["checks"] and 0 < process.returncode < 126
            and not process.stderr.strip() and isinstance(observed, dict)
            and isinstance(observed.get("summary"), dict)
            and isinstance(observed.get("planned"), list)
            and isinstance(observed.get("skipped"), list)
            and observed.get("mode") in ("plan", "apply")
        )
        if process.returncode not in (0, 10, 20) and not new_outcome_code:
            error = json.loads(process.stderr)
            known = spec.get("known_failure", {})
            if not (spec["expectation"] == "todo" and known
                    and isinstance(error, dict)
                    and process.returncode == known["exit_code"]
                    and error.get("error") == known["error"]
                    and known["message_contains"] in error.get("message", "")):
                raise RuntimeError(f"CLI exited {process.returncode}: {process.stderr or process.stdout}")
            observed = {"runtime_error": error}
            known_runtime_failure = True
            mismatches.append(f"known updater failure: {error['message']}")
        if not known_runtime_failure:
            validate_report(observed, spec["command"])
        checks = spec["checks"]
        if "exit_code" in checks and process.returncode != checks["exit_code"]:
            mismatches.append(f"exit code: expected {checks['exit_code']}, got {process.returncode}")
        if "exit_code_not" in checks and process.returncode == checks["exit_code_not"]:
            mismatches.append(f"exit code: expected a value other than {checks['exit_code_not']}, got {process.returncode}")
        mismatches.extend(check_json(observed, checks["json"]))
        invariant_mismatches = check_json(observed, checks.get("invariants", []))
        mismatches.extend(f"invariant {message}" for message in invariant_mismatches)
        actual_files = files_in(repo)
        unexpected_edits = False
        for path in sorted(expected.keys() | actual_files.keys()):
            if expected.get(path) != actual_files.get(path):
                mismatches.append(f"file differs: {path}")
            if (known_runtime_failure or expected.get(path) != actual_files.get(path)) and (
                original.get(path) != actual_files.get(path)
                and (path not in known_actual or known_actual[path] != actual_files.get(path))
            ):
                unexpected_edits = True
                mismatches.append(f"unexpected edit: {path}")
        known_gap = spec["expectation"] == "todo"
        status = ("TODO" if known_gap else "FAIL") if mismatches else ("XPASS" if known_gap else "PASS")
        if unexpected_edits or invariant_mismatches:
            status = "FAIL"
        return {
            "id": spec["id"],
            "category": spec["category"],
            "status": status,
            "todo": spec.get("todo", ""),
            "mismatches": mismatches,
            "observed": observed,
            "exit_code": process.returncode,
            "requests": server.requests,
            "description": spec["description"],
            "audit": spec.get("audit", []),
            "source": spec.get("source", []),
        }


def main():
    directory = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", type=Path, default=directory / "cases")
    parser.add_argument("--binary", type=Path, default=directory.parent.parent / "target/debug/fluxrepo-update")
    parser.add_argument("--case", action="append", default=[], help="run a named example (repeatable)")
    parser.add_argument("--json", action="store_true", help="emit the complete structured report")
    parser.add_argument("--strict", action="store_true", help="also exit nonzero for known TODOs")
    args = parser.parse_args()
    cases = sorted(path.parent for path in args.cases.glob("*/case.json"))
    if args.case:
        missing = set(args.case) - {path.name for path in cases}
        if missing:
            parser.error(f"unknown cases: {', '.join(sorted(missing))}")
        cases = [path for path in cases if path.name in args.case]
    if not cases:
        parser.error("no examples found")
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"CLI binary missing: {binary}; run cargo build --locked first")
    results = [run_case(path, binary) for path in cases]
    summary = {status: sum(result["status"] == status for result in results) for status in ("PASS", "TODO", "XPASS", "FAIL", "ERROR")}
    report = {"schema_version": 1, "summary": summary, "results": results}
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        for result in results:
            print(f"{result['status']:5} {result['id']} — {result.get('description', '')}")
            if result.get("todo"):
                print(f"      {result['todo']}")
            for problem in result.get("mismatches", []):
                print(f"      {problem[:300]}")
            if result.get("error"):
                print(f"      {result['error']}")
        print("\n" + ", ".join(f"{count} {status}" for status, count in summary.items()))
    return int(bool(summary["FAIL"] or summary["ERROR"] or (args.strict and summary["TODO"])))


if __name__ == "__main__":
    raise SystemExit(main())
