#!/usr/bin/env python3
"""Move an issue's card on the configured GitHub Project board via the
@standardagents/issues GitHub App credentials (~/.standardagents/issues/).

Usage: board.py <issue-number> <status>

<status> is matched case-insensitively against the Status options
discovered from the configured Project — whatever vocabulary that Project
defines. The repository is derived from the checkout's origin remote; the
organization and Project come from the credentials file. The App token
carries organization-project write; this needs no extra gh scopes. Exits 0
with a note when credentials or permissions are missing so an unattended
loop never blocks on it.
"""
import base64, json, os, re, subprocess, sys, time, urllib.request


def repo_from_origin(url: str) -> str:
    """Repository name from a git remote URL (SSH or HTTPS), sans .git."""
    m = re.search(r"[:/]([^/:]+)/([^/]+?)(?:\.git)?/?$", url.strip())
    return m.group(2) if m else ""


def pick_option(options, want: str):
    """Case-insensitive match against the Project's discovered options."""
    return next((o for o in options if o["name"].lower() == want.lower()), None)


def unknown_status_message(want: str, options) -> str:
    names = ", ".join(o["name"] for o in options)
    return f"board.py: no '{want}' status option; this Project has: {names}; skipping"


def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def gh_api(url: str, token: str, payload=None, accept="application/vnd.github+json"):
    req = urllib.request.Request(url, method="POST" if payload is not None else "GET")
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Accept", accept)
    body = None
    if payload is not None:
        body = json.dumps(payload).encode()
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, body) as r:
        return json.load(r)


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: board.py <issue-number> <status>", file=sys.stderr)
        return 2
    issue_no, want_status = int(sys.argv[1]), sys.argv[2]

    creds_path = os.path.expanduser("~/.standardagents/issues/credentials.json")
    if not os.path.isfile(creds_path):
        print("board.py: no issue-CLI credentials; skipping", file=sys.stderr)
        return 0
    creds = json.load(open(creds_path))

    # App JWT (RS256 via openssl — no python-cryptography dependency).
    now = int(time.time())
    header = b64url(json.dumps({"alg": "RS256", "typ": "JWT"}).encode())
    claims = b64url(json.dumps({"iat": now - 60, "exp": now + 540, "iss": creds["app_id"]}).encode())
    signing_input = f"{header}.{claims}".encode()
    import tempfile

    with tempfile.NamedTemporaryFile("w", suffix=".pem", delete=False) as kf:
        kf.write(creds["private_key"])
        key_path = kf.name
    try:
        sig = subprocess.run(
            ["openssl", "dgst", "-sha256", "-sign", key_path],
            input=signing_input,
            capture_output=True,
            check=True,
        ).stdout
    finally:
        os.unlink(key_path)
    jwt = f"{header}.{claims}.{b64url(sig)}"

    tok = gh_api(
        f"https://api.github.com/app/installations/{creds['installation_id']}/access_tokens",
        jwt,
        payload={},
    )["token"]

    def gql(query: str, variables: dict):
        out = gh_api("https://api.github.com/graphql", tok, payload={"query": query, "variables": variables})
        if out.get("errors"):
            raise RuntimeError(json.dumps(out["errors"]))
        return out["data"]

    project_id = creds["project_id"]

    # Find the project item for this issue + the Status field options.
    origin = subprocess.run(
        ["git", "-C", os.path.dirname(os.path.abspath(__file__)), "remote", "get-url", "origin"],
        capture_output=True,
        text=True,
    ).stdout
    repo = repo_from_origin(origin)
    if not repo:
        print("board.py: no origin remote to derive the repository from; skipping", file=sys.stderr)
        return 0
    data = gql(
        """
        query($org:String!,$repo:String!,$num:Int!) {
          repository(owner:$org,name:$repo) {
            issue(number:$num) {
              projectItems(first:20) { nodes { id project { id } } }
            }
          }
        }""",
        {"org": creds["organization"], "repo": repo, "num": issue_no},
    )
    items = data["repository"]["issue"]["projectItems"]["nodes"]
    item = next((i for i in items if i["project"]["id"] == project_id), None)
    if item is None:
        print(f"board.py: issue #{issue_no} has no card on the org project; skipping", file=sys.stderr)
        return 0

    fields = gql(
        """
        query($proj:ID!) {
          node(id:$proj) { ... on ProjectV2 {
            field(name:"Status") { ... on ProjectV2SingleSelectField {
              id options { id name } } }
          } }
        }""",
        {"proj": project_id},
    )["node"]["field"]
    option = pick_option(fields["options"], want_status)
    if option is None:
        print(unknown_status_message(want_status, fields["options"]), file=sys.stderr)
        return 0

    gql(
        """
        mutation($proj:ID!,$item:ID!,$field:ID!,$opt:String!) {
          updateProjectV2ItemFieldValue(input:{
            projectId:$proj,itemId:$item,fieldId:$field,
            value:{singleSelectOptionId:$opt}}) { clientMutationId }
        }""",
        {"proj": project_id, "item": item["id"], "field": fields["id"], "opt": option["id"]},
    )
    print(f"board.py: issue #{issue_no} → {want_status}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:  # never block the loop on board plumbing
        print(f"board.py: {e}; skipping", file=sys.stderr)
        sys.exit(0)
