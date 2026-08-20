# dmux-rs

Native Rust tmux control-mode renderer for dmux — **first-party test ring**.
You run it; it improves itself. See `AGENTS.md` for how the loop works and
`ROADMAP.md` for architecture.

## Install (one line)

```bash
gh api repos/standardagents/dmux-rs/contents/scripts/install.sh -H "Accept: application/vnd.github.raw" | bash
```

Needs the [GitHub CLI](https://cli.github.com) logged in (`gh auth login`)
with access to this repo, and tmux ≥ 3.3. Prefer clicking? Grab the binary
from the [latest release](https://github.com/standardagents/dmux-rs/releases/latest)
(`dmux-rs-macos-aarch64`), `chmod +x`, put it on your PATH.

## Use

```bash
cd your-project && dmux-rs
```

That's it. From then on:

- **It stays fresh by itself**: every running head polls releases **every
  minute** and hot-swaps in place — your tmux sessions, agents, and layout
  survive the swap (the sidebar shows the current build).
- **Using it makes it better**: the shadow verifier compares every settled
  pane against tmux's grid; a divergence auto-files an issue here with the
  exact bytes to reproduce it (🐛 chip in the sidebar, one issue per pane
  until the next build reloads you). The fix ships; your head updates;
  the bug is gone.

## Knobs (all optional)

| env | effect |
|---|---|
| `DMUX_NO_UPDATE=1` | don't self-update |
| `DMUX_UPDATE_INTERVAL_SECS` | poll cadence (default 60) |
| `DMUX_NO_REPORT=1` | don't auto-file issues |
| `DMUX_VERIFY=0` | disable the shadow verifier entirely |

## Testing

`scripts/validate.sh` is the complete pre-push gate (quality checks plus
the rendering-fidelity harness); `scripts/check.sh` is the fast inner
gate for mid-task iteration.

For focused unit tests, note the naming split: the Cargo **package** is
`dmux`, but it has no library target — all unit tests live in the
**binary target** `dmux-rs`. So `cargo test -p dmux --lib <filter>`
fails with `no library targets found`; use the binary target instead:

```bash
cargo test -p dmux --bin dmux-rs <filter>   # e.g. … session::tests
```

Other crates in the workspace (`dmux-vt`, `dmux-cc`, `dmux-compositor`,
…) are ordinary libraries where `cargo test -p <crate> <filter>` works
as usual.

## Sidebar preview

Contributors can render representative sidebar states through the production
renderer without connecting to a tmux server:

```bash
scripts/sidebar-preview.sh
```

The preview covers the standard 40-column sidebar, compact and tiny widths,
hover and leader states, diagnostics, and a long status message. ANSI colors
are included by default. Use
`scripts/sidebar-preview.sh --plain` for stable text output in a diff.
