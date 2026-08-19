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
