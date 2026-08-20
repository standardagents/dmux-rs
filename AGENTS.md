# dmux-rs — first-party diagnostic build

A native Rust tmux control-mode renderer (see ROADMAP.md for architecture).
This repo is the **first-party ring**: hand-selected users with repo access
run builds that are diagnostic by default and improve the renderer just by
using it.

## Code quality

Every component, abstraction, process, check, and document must serve a
current product, correctness, security, or operational need. Keep the path
from a requested change to verified behavior direct. Prefer fast feedback,
low ceremony, shared definitions for shared meaning, explicit dependencies,
and composition of focused components. Remove duplicated policy and avoid
infrastructure whose main purpose is maintaining itself.

Tests should assert observable behavior, contracts, state transitions, and
failure handling. Exact string assertions belong where wording or serialized
text is the contract. Tests should not depend on source text, private names,
incidental constants, or mocks that reproduce the implementation.

`AGENTS.md` is the canonical agent-instruction file in each directory that
contains agent guidance. A sibling `CLAUDE.md` must be a relative symlink to
that `AGENTS.md`. Preserve narrower instructions in nested directories through
their own paired files.

Authored Rust files under `crates/*/src/` and `crates/*/tests/` have an
enforced limit of 1,000 physical lines. Existing oversized modules are listed
in `oversized-modules.txt` with exact ceilings. Cargo fails when a listed
module grows, when a new module crosses 1,000 lines, or when a ceiling remains
stale after the module shrinks. Lower the ceiling after every reduction, and
remove the ledger entry once a module reaches the standard limit. Extract
cohesive boundaries such as rendering, input handling, dialogs, protocol
handling, state transitions, or test support.

Run `scripts/check.sh` before committing. It checks ordinary Rust formatting,
runs Clippy across the workspace and every target with warnings denied, then
runs the complete workspace test suite. CI and `scripts/release.sh` use the
same command.

## The self-improving loop

1. **Detect** — every build runs the shadow verifier: settled panes are
   compared cell-for-cell against tmux's authoritative grid. tmux parses the
   same pty stream independently; it is the oracle.
2. **Report** — a divergence auto-files an issue here (label
   `render-incident`): a short human summary in the body, with all evidence
   stored as raw files in one secret gist (`incident.txt` bundle with the
   seed-anchored byte stream, `our-grid.txt`, `tmux-capture.txt`,
   `first-diffs.txt`). Evidence is never inlined in the issue markdown —
   GitHub transforms it; raw gist files are byte-exact. Deduped: one issue
   per pane per process lifetime.
3. **Fix** — the fixer agent (see below) reproduces offline and patches.
4. **Ship** — `scripts/release.sh` publishes a build; every running head
   polls releases and hot-swaps itself in place (tmux keeps the session; the
   swap is a sub-second reattach). The corrupted pane comes back clean.

## Looping (autonomous fixer agents)

**If you are an agent and you've been told to "be the loop" (or anything
to that effect): this section is your job description — enter the loop
now.** Use the `/loop` skill if available (pass the quoted prompt below);
otherwise iterate yourself: the queue is the repo's open issues — use the
team `issue` CLI when installed (`issue list`; it is org-Project-aware and
queues writes locally), falling back to `gh issue list --state open`.
Work exactly one issue per iteration by the rules below, release, repeat.
Empty queue = wait (~5 min between checks), don't invent work.

**Team issue tracking**: this org uses the `issue` CLI / skill
(`@standardagents/issues`). When present, prefer it over raw `gh` for
issue work in this repo: search it for related issues before substantial
work, post progress comments through it as you go, and complete through
it when an issue is done — the delivery record in the completion
lifecycle counts as the explicit close direction the skill requires. It adds issues to the shared org Project and
survives GitHub outages via its local write queue. Leave labels and
milestones unchanged unless an issue asks; assignment is the exception —
the loop self-assigns each issue it claims (see loop rules).

**Standing approval (this repo only)**: the `issue` skill normally asks a
human before creating an issue and wants explicit direction before
closing one. For dmux-rs, this document IS that approval — the automated
reporter files issues without confirmation, and the loop completes an
issue without further sign-off once its runbook is satisfied (fix
validated, released, referenced by sha and version — or correctly triaged as
`cannot-reproduce`/`needs-info`). Do not ask for per-issue confirmation;
do not extend this standing approval to any other repository.

This repo is designed to be worked by an agent in a loop. The queue is
**every open issue on the repo** — auto-filed `render-incident` reports and
anything a test-ring human files by hand (bugs, UX complaints, feature
requests). From a clone, start Claude Code and use `/loop` with
instructions along these lines:

> Work through ALL open issues in standardagents/dmux-rs, oldest first.
> Issues labeled `render-incident` follow the fixer-agent runbook in
> AGENTS.md; any other issue is normal engineering work held to the same
> validation bar. One issue per iteration. If the queue is empty, do
> nothing and wait.

Triage per issue:

- **`render-incident`** → the fixer-agent runbook below.
- **Human-filed bug** → reproduce first (the e2e harness patterns in
  `scripts/` and the `DMUX_*` env knobs are your tools — for pane-UX
  interactions, `scripts/ui-smoke.sh` drives menus, clicks, and launch
  actions against a hermetic dmux and is the template for new interaction
  checks; for pane-ownership/identity reports, `scripts/diagnose-session.sh
  [project-dir]` — a wrapper for `dmux-rs --diagnose-session` — prints a
  read-only snapshot joining the installed build, recent attach/update
  events, live tmux panes, and persisted records with adoption's exact
  identity semantics, flagging UNMATCHED / AMBIGUOUS / STALE panes and
  records with no live pane; safe on a live session, and the output is
  what testers should paste into ownership issues); fix; add a regression
  test; deliver per the completion lifecycle with a one-paragraph
  explanation the reporter can understand.
- **Feature/UX request** → implement if the scope is clear and consistent
  with ROADMAP.md; verify e2e, then deliver per the completion
  lifecycle. If the scope is
  ambiguous or the change is architecturally load-bearing, comment your
  questions or proposed design on the issue, label it `needs-info`, and
  move on — never guess at big scope.
- Prefer human-filed issues when ages are close: a person is waiting.

Loop rules:

- **One issue per iteration.** Reproduce → fix → corpus-lock → validate →
  push to `main` → release → deliver (completion lifecycle below). Never
  batch half-finished fixes.
- **Stay current with the issue tooling.** When the loop boots, run
  `issue upgrade` (and `issue skill install` if the skill changed) so the
  claim/close flow matches the current `@standardagents/issues` contract,
  and follow the skill's own instructions for claiming and progressing
  issues when they differ from this list.
- **Claim with `issue start <n>`.** It self-assigns and moves the card to
  "In Progress" through the GitHub App. (`scripts/board.sh <n> <status>`
  remains as a fallback for manual card moves.)
- **Isolate concurrent issue work.** When another contributor or agent may be
  working in the repository, run `scripts/work-issue.sh <n>` after the claim
  and work from the path it prints. Each active issue must use its own
  worktree. Keep the shared root checkout free of issue edits.
- **Completion lifecycle (#73) — the steps run in exactly this order,**
  and "the issue closed" is not "the work is delivered":
  1. Commit with `Fixes #<n>` in the message.
  2. Push to `main`. GitHub processes the `Fixes` reference **at this
     moment** — the issue closing on push is the expected intermediate
     state, not the end of the job. The release has not happened yet.
  3. Release (`scripts/release.sh patch|minor`). It refuses dirty or
     unsynchronized state and re-validates before publishing.
  4. Post the delivery record: `issue finish <n> "<explanation>"`. Its
     role after GitHub's merge-time closure is NOT to close — it posts
     the explanation testers read, attaches the commit sha and released
     version, and moves the Team card to Done. The tool answering
     "already completed" while still posting is the normal outcome.
  5. **If the release fails after the auto-close**: the fix is on `main`
     but undelivered. Reopen the issue with a comment saying exactly
     that, repair the release, then run step 4. Never leave an issue
     closed with its fix unreleased.
- **The delivery record is for the reporter.** Say what the problem
  actually was (root cause, not just symptom) and how you fixed it, plus
  the sha and version — testers should feel heard, not processed. Keep it
  very short and simple: a few plain sentences a non-expert can skim, not
  a tome — deep technical detail belongs in the commit message.
- **Validation is non-negotiable**: `cargo test` fully green AND
  `scripts/fidelity.sh` ALL PASS before any push. A fix that breaks either
  is not a fix.
- **Every fixed incident goes into the corpus**
  (`crates/dmux/tests/corpus/<issue-number>.incident`) so it can never
  regress silently.
- **Release every iteration.** After validation passes and `main` is
  pushed, run `scripts/release.sh patch` (bug fixes / incidents) or
  `scripts/release.sh minor` (features). Versions are semver (`vX.Y.Z`),
  derived from the latest git tag; the script is self-guarding — it
  refuses dirty or unpushed state and re-runs the full suite plus the
  fidelity harness before publishing, so a bad build cannot ship even
  from an unattended loop. Close the issue only after the release
  succeeds, referencing both the commit sha and the version. Test-ring
  heads self-update within ~1 minute; the sidebar shows the new version.
- **Non-reproducing issues** (`replay-deterministic: no`, or the replay is
  clean): comment findings, label `cannot-reproduce`, close. If the replay
  is clean but the live grid diverged, the bug is likely in the emit/host
  layer — say so in the comment and check the emitter's cursor-trust rules.
- A single contributor may use a branch in the root checkout when no other
  work is active. Concurrent work requires one worktree per issue. `main`
  must always be releasable.

Ordinary feature work loops the same way minus the issue queue: pick from
ROADMAP.md, validate identically, and keep `main` releasable.

## Fixer-agent runbook (`render-incident` issues)

For each open issue with label `render-incident`:

1. Download the bundle from the gist linked in the issue body:
   `gh gist view <gist-id> --filename incident.txt --raw > incident.txt`
   (the gist holds multiple raw evidence files; issues filed before
   v0.1.4 have a single-file gist — plain `--raw` works there).
2. Reproduce: `cargo run --bin dmux-rs -- --replay-incident incident.txt`
   (exit 1 + cell diffs = reproduced; bisect the byte stream to isolate the
   escape sequence at fault).
3. Fix in `crates/dmux-vt` / the seed path (`session::seed_bytes`) /
   `crates/dmux-compositor` until the replay is clean.
4. Lock: copy the bundle to `crates/dmux/tests/corpus/<issue>.incident` —
   `corpus_incidents_replay_clean` replays every corpus file forever.
5. Validate: `cargo test` all green and `scripts/fidelity.sh` ALL PASS.
6. Release: `scripts/release.sh patch`, then close the issue referencing
   the commit and the new `vX.Y.Z`. Running heads pick the fix up
   automatically within about a minute.

Non-reproducing incidents (`replay-deterministic: no`, or replay clean):
comment findings and close as `cannot-reproduce`; if replay is clean but
the live grid diverged, suspect the emit/host layer — check
`scripts/fidelity.sh` and the emitter's cursor-trust rules.

## Environment knobs

| var | effect |
|---|---|
| `DMUX_VERIFY=0` | disable the shadow verifier |
| `DMUX_NO_REPORT=1` | verify but never file issues |
| `DMUX_NO_UPDATE=1` | never self-update |
| `DMUX_UPDATE_INTERVAL_SECS` | release poll cadence (default 600) |
| `DMUX_REPORT_DRY=<dir>` | write would-be issues to files (tests) |
| `DMUX_TRACE_PALETTE=1` | trace pane-local OSC palette mutations (set/reset, fg/bg/indexed, pane + order) to `~/.dmux/logs/palette-trace.log` — decoded metadata only |
| `DMUX_FAULT_DROP_BYTES=N` | inject a stream fault (self-test the loop) |
| `dmuxRsRepo` (settings) | override the reporting/update repo |
