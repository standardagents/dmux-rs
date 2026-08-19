# dmux-rs — first-party diagnostic build

A native Rust tmux control-mode renderer (see ROADMAP.md for architecture).
This repo is the **first-party ring**: hand-selected users with repo access
run builds that are diagnostic by default and improve the renderer just by
using it.

## The self-improving loop

1. **Detect** — every build runs the shadow verifier: settled panes are
   compared cell-for-cell against tmux's authoritative grid. tmux parses the
   same pty stream independently; it is the oracle.
2. **Report** — a divergence auto-files an issue here (label
   `render-incident`) with the build, both grids, first diffs, and a secret
   gist holding the full incident bundle including the seed-anchored byte
   stream. Deduped: one issue per pane per process lifetime.
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
work, post progress comments through it as you go, and close through it
when an issue is done (the runbook's close step counts as the explicit
direction it requires). It adds issues to the shared org Project and
survives GitHub outages via its local write queue. Leave assignments,
labels, claims, and milestones unchanged unless an issue asks.

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
  `scripts/` and the `DMUX_*` env knobs are your tools); fix; add a
  regression test; close with the commit sha and a one-paragraph
  explanation the reporter can understand.
- **Feature/UX request** → implement if the scope is clear and consistent
  with ROADMAP.md; verify e2e, then close with the sha. If the scope is
  ambiguous or the change is architecturally load-bearing, comment your
  questions or proposed design on the issue, label it `needs-info`, and
  move on — never guess at big scope.
- Prefer human-filed issues when ages are close: a person is waiting.

Loop rules:

- **One issue per iteration.** Reproduce → fix → corpus-lock → validate →
  push to `main` → close the issue with the commit sha. Never batch
  half-finished fixes.
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
- Work on a branch or worktree per issue if you prefer, but `main` must
  always be releasable.

Ordinary feature work loops the same way minus the issue queue: pick from
ROADMAP.md, validate identically, and keep `main` releasable.

## Fixer-agent runbook (`render-incident` issues)

For each open issue with label `render-incident`:

1. Download the gist linked in the issue body → `incident.txt`.
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
| `DMUX_FAULT_DROP_BYTES=N` | inject a stream fault (self-test the loop) |
| `dmuxRsRepo` (settings) | override the reporting/update repo |
