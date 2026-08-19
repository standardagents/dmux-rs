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

This repo is designed to be worked by an agent in a loop. From a clone,
start Claude Code and use `/loop` with instructions along these lines:

> Work through open `render-incident` issues in standardagents/dmux-rs,
> oldest first, following the fixer-agent runbook in AGENTS.md. One issue
> per iteration. If the queue is empty, do nothing and wait.

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
- **Releases stay human.** The loop pushes to `main` and closes issues;
  a person runs `scripts/release.sh` after eyeballing the diffs. (Test-ring
  heads auto-update within ~1 minute of a release — don't hand that
  trigger to the loop.)
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
6. Release: `scripts/release.sh`, then close the issue referencing the
   commit and tag. Running heads pick the fix up automatically.

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
