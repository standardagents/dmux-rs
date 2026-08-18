# dmux-rs — control-mode renderer prototype (Phase 0)

A native Rust prototype of dmux as a **tmux control-mode renderer**: it
attaches to the project's tmux session as a `tmux -C` client, runs a terminal
emulator per pane in-process, and composites the pane grid + sidebar itself,
writing damage-diffed, synchronized-output frames to the host terminal. tmux
stays the durable backbone (PTYs, processes, crash survival); dmux-rs owns
every pixel.

Design doc: the "dmux-rs: Native Rust dmux as a tmux Control-Mode Renderer"
plan (see the PR / plan file). This workspace is Phase 0 of that plan.

## Run

```bash
cd rust && cargo build --release
# From a NON-tmux terminal:
./target/release/dmux-rs            # attach or create this project's session
./target/release/dmux-rs --session dmux-myproj-abc12345 --hud
```

The session is a pure function of the project root — `.dmux/dmux.config.json`
found by walking up (its `projectRoot` wins, matching TS dmux), else the main
git worktree root, else the current directory. First run from a location
creates the session there (one shell pane, `terminal-1`); every later run
from the same location reattaches to it. Quitting the renderer (`^Q`) leaves
the session and its processes running.

- `^Q` quit · `^Y` perf HUD · `⌥←/→` cycle pane focus · `⌥1..9` focus pane N
- `⌥PgUp/PgDn` scrollback · mouse: click sidebar row to focus, wheel scrolls
- Everything else is forwarded to the focused pane (`send-keys -H`, verbatim
  bytes, app-cursor aware).

Two operating modes, decided at attach by the `@dmux_controller_pid` session
option:

- **Observe** — a live TypeScript dmux controller owns the session: dmux-rs
  renders and routes input but never touches window topology (and attaches
  with the `ignore-size` client flag so it can't resize anything).
- **Owner** — no controller: dmux-rs sizes each single-pane window to its
  compositor rect (`window-size manual` + `resize-window`), the plan's
  one-window-per-pane topology.

## Crates

| crate | what |
|---|---|
| `dmux-cc` | sans-io control-mode protocol parser (+octal unescaper, reply FIFO router), tokio client. Total stream order is preserved — replies resolve in the app loop, which is what makes pause→capture→reseed race-free. |
| `dmux-vt` | per-pane emulator (`alacritty_terminal` behind a `PaneTerm` trait): damage, scrollback, input-mode introspection, pty-response side effects (DA1/CPR/OSC color answers routed back into the pane). |
| `dmux-compositor` | headless cell model: `CellBuffer`, frame diff, stateful ANSI emitter (SGR runs, CUP, DECSET 2026 bracketing). |
| `dmux-host` | tty lifecycle (raw mode, alt screen, restore-on-drop), capability probe (DECRQM 2026), termwiz input parsing on a reader thread, SIGWINCH watcher. |
| `dmux-core` | TS-compatible domain model: `dmux.config.json` serde (unknown fields preserved), `__dmux__` pane-title identity, session naming (`dmux-<basename>-<md5[0:8]>`). |
| `dmux` | the binary: session adoption, layout math (60–100 col comfort band), sidebar/chrome/HUD renderer, input routing, flood throttling, event loop. |

## Behavior notes

- **Seeding**: pane grids seed from `capture-pane -epqJ` (tmux's server-side
  grid is authoritative); reseeds happen after `%pause`, size changes, and
  flood-throttle refreshes. Output arriving mid-reseed is buffered and applied
  after the seed — safe because reply completion and `%output` share one
  ordered stream.
- **Flood throttling**: a pane exceeding ~4 MB/s gets `refresh-client -A off`
  and refreshes by reseed every 500 ms (badge: `≫ fast output`) until it calms
  down. Keeps typing latency flat while `yes` runs; server-side `pause-after=1`
  remains armed as the backstop.
- **Perf** (measured, release build, 220×62 host, firehose active): frame
  p50 0.21 ms / p95 0.65 ms; idle = zero timers, zero polls, zero subprocesses.

## Tests

```bash
cargo test          # unit + a recorded tmux 3.7b control-mode transcript replay
```

E2E is exercised by running dmux-rs inside a scratch tmux pane attached to a
second scratch server (see the plan's verification section): rendering,
input round-trip, focus, vim/alt-screen fidelity, firehose+throttle,
convergence-vs-`capture-pane`, mouse, and resize have all been verified that
way against tmux 3.7b.

## Phase 0 gaps (by design)

Pane/worktree creation, merge/close, popups, status LLM escalation, OSC 52
forwarding, kitty keyboard passthrough, images, and the one-window-per-pane
break-pane migration for legacy multi-pane windows. See the plan's Phase 1+.
