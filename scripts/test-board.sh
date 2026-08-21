#!/bin/bash
# CLI contract tests for the board helper (#95). GitHub and git are replaced
# at their process boundaries so assertions cover requests and user-visible
# outcomes without depending on the helper's source layout.
set -euo pipefail
cd "$(dirname "$0")/.."

python3 -B - <<'PY'
import builtins, contextlib, io, json, os, runpy, subprocess, sys, urllib.request

credentials = {
    "app_id": "1", "installation_id": "2", "organization": "some-org",
    "project_id": "project-1", "private_key": "fixture-key",
}

class Response(io.BytesIO):
    def __enter__(self): return self
    def __exit__(self, *_): self.close()

def run_cli(status):
    requests = []
    responses = [
        {"token": "fixture-token"},
        {"data": {"repository": {"issue": {"projectItems": {"nodes": [
            {"id": "item-1", "project": {"id": "project-1"}}
        ]}}}}},
        {"data": {"node": {"field": {"id": "status-field", "options": [
            {"id": "1", "name": "Backlog"}, {"id": "2", "name": "Doing"},
            {"id": "3", "name": "Shipped"},
        ]}}}},
        {"data": {"updateProjectV2ItemFieldValue": {"clientMutationId": None}}},
    ]
    real_open, real_isfile = builtins.open, os.path.isfile
    real_run, real_urlopen, real_argv = subprocess.run, urllib.request.urlopen, sys.argv

    def fake_open(path, *args, **kwargs):
        if str(path).endswith("/.standardagents/issues/credentials.json"):
            return io.StringIO(json.dumps(credentials))
        return real_open(path, *args, **kwargs)

    def fake_run(command, *args, **kwargs):
        if command[:3] == ["openssl", "dgst", "-sha256"]:
            return subprocess.CompletedProcess(command, 0, stdout=b"signature", stderr=b"")
        if command[:2] == ["git", "-C"]:
            return subprocess.CompletedProcess(
                command, 0, stdout="git@github.com:some-org/some-repo.git\n", stderr=""
            )
        return real_run(command, *args, **kwargs)

    def fake_urlopen(request, body=None):
        requests.append((request.full_url, json.loads(body) if body else None))
        return Response(json.dumps(responses.pop(0)).encode())

    builtins.open = fake_open
    os.path.isfile = lambda path: (
        str(path).endswith("/.standardagents/issues/credentials.json") or real_isfile(path)
    )
    subprocess.run, urllib.request.urlopen = fake_run, fake_urlopen
    sys.argv = ["scripts/board.py", "42", status]
    stdout, stderr, code = io.StringIO(), io.StringIO(), None
    try:
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            try: runpy.run_path("scripts/board.py", run_name="__main__")
            except SystemExit as error: code = error.code
    finally:
        builtins.open, os.path.isfile = real_open, real_isfile
        subprocess.run, urllib.request.urlopen, sys.argv = real_run, real_urlopen, real_argv
    return code, stdout.getvalue(), stderr.getvalue(), requests

failures = 0
def check(label, condition):
    global failures
    print(("PASS " if condition else "FAIL ") + label)
    failures += 0 if condition else 1

code, output, errors, requests = run_cli("dOiNg")
graphql = [payload for url, payload in requests if url.endswith("/graphql")]
check("successful CLI exit", code == 0)
check("repository derived from origin", graphql[0]["variables"]["repo"] == "some-repo")
check("issue number sent", graphql[0]["variables"]["num"] == 42)
check("case-insensitive status mutation", graphql[-1]["variables"]["opt"] == "2")
check("success is reported", "issue #42" in output and "dOiNg" in output and not errors)

code, output, errors, requests = run_cli("Done")
graphql = [payload for url, payload in requests if url.endswith("/graphql")]
check("unknown status is a successful skip", code == 0 and not output)
check("unknown status reports available options", "Backlog, Doing, Shipped" in errors)
check("unknown status sends no mutation", len(graphql) == 2)

raise SystemExit(1 if failures else 0)
PY
echo "board tests: PASS"
