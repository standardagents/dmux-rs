# dmux-rs roadmap — gaps vs the plan and vs TS dmux

Status legend: ✅ done · 🔨 this iteration · ⏳ later phase · ✂ intentionally dropped

## From the approved plan (Phase 0 exits / Phase 1+)

| Item | Status |
|---|---|
| Control-mode client, per-pane VT, compositor, host backend | ✅ |
| Session attach/adopt/create, owner vs observe modes | ✅ |
| Flood throttling + pause/reseed flow control | ✅ |
| Perf HUD + metrics | ✅ |
| Native overlay UI framework (component library, not ad-hoc) | ✅ |
| Global key scheme that doesn't step on pane apps (leader + config) | ✅ |
| Kitty keyboard protocol on the host (Super-key globals when able) | ✅ |
| Settings UI (declarative registry port) + settings persistence (3 scopes) | ✅ |
| New-pane flow: worktree + agent launch via control mode | ✅ (native bootstrap loader: dmux runs worktree add + `worktree_created` hook itself, pane body shows a step-checklist card with live output + progress bar, agent launches when setup finishes) |
| Multi-allocation agent selector (N panes per agent, one prompt) | ✅ (new, beyond TS) |
| Pane actions: rename / hide-show / close + confirm dialogs | ✅ |
| Title-bar click affordances (buttons; not possible under tmux) | ✅ (new) |
| Animated working indicators (spinners; replaces 90ms tmux title rewrites) | ✅ |
| Config write-back of pane records (TS-compatible, unknown fields preserved) | ✅ |
| Status heuristics port (`paneAttentionHeuristics` on live grids) | ✅ (dmux-status crate + LLM escalation ✅) |
| LLM status escalation (`dmux-infer`), attention service, macOS helper client | ✅ (dmux-infer: openai-compatible/responses/anthropic + failover + PaneAnalyzer stage-1, settings/credentials compatible; native notifications via helper ✅; ChatGPT + Cohere protocols ✅) |
| Merge flows / PR creation / AI merge / conflict pane | ✅ (core merge flow: dirty-check → commit → merge → cleanup, conflict-abort; PR ✅, conflict pane ✅ (agent-assisted); AI auto-merge ✅) |
| Resume/reopen branches, agent crash restore (`paneAgentTracking` port) | ✅ (agent resume + exact-session tracking: process-tree walk, lsof/proc fd inspection, agentSessionId capture, --resume <id>) |
| Selection + OSC 52 copy (dmux-side selection, tmux buffer mirror) | ✅ (drag select + copy, Shift override, app-mouse drag forwarding; word-select ✅ / search ✅ (^b /)) |
| Kitty keyboard passthrough INTO panes (pane-requested flags) | ✅ (disambiguate CSI-u when the pane pushed flags) |
| Images (kitty graphics translation) | ⏳ |
| break-pane migration for legacy multi-pane windows | ✅ (owner mode, idempotent) |
| Multi-project sidebar, themes (8), i18n (en/ja) | multi-project grouping ✅ (headers, per-project pane records, project-scoped creation); themes accent ✅; i18n ✅ (en/ja catalog + locale switching; full string coverage growing) |
| Distribution (npm platform packages), auto-update | npm wrapper + platform packages + release workflow ✅ (publish gated on tags/NPM_TOKEN); auto-update ✅ (npm registry check + status notice) |

## TS-source features not yet in Rust (from the popup/action/settings inventory)

- Popups → native overlays: newPane ✅, settings ✅, kebab/pane menu ✅, confirm ✅, input ✅, shortcuts ✅, agentChoice (superseded by allocator ✅), logs ✅, merge (core) ✅, reopenWorktree ✅ (welcome resume cards), prReview ✂ (superseded: PR runs interactively via `gh pr create` in a pane), diffPeek ✅ (worktree diff pager incl. untracked), enabledAgents ✅, inferenceSetup ✅ (status view: targets + detected credentials; editing stays JSON), notificationSounds ✅ (checklist), hooks ✅ (inventory view; `worktree_created` + `pre_merge` execute), progress ✂ (superseded by toasts/badges — no blocking progress dialogs), projectSelect ✅ (add-project + sidebar grouping)
- Actions (18): view/focus ✅, close ✅, rename ✅, hide/show ✅, duplicate ✅ (same agent + prompt, fresh worktree), merge ✅, PR ✅, copyPath ✅, openInEditor ✅, toggleAutopilot ✅ (per-pane, TS `autopilot` config field, LLM option-dialog auto-accept), test/dev runners ✅ (`run_test`/`run_dev` hooks in a terminal pane via pane menu)
- Hooks: all eleven lifecycle hooks ✅ — `worktree_created` (in-pane bootstrap), `pre_merge` (veto on nonzero exit), `post_merge`, `before_pane_create`/`pane_created`, `before_pane_close`/`pane_closed`, `before_worktree_remove`/`worktree_removed` (detached, DMUX_* env), `run_test`/`run_dev` (menu runners)
- Notification sounds: catalog port (ids ↔ `.caf` resources) ✅, helper `soundName` payload ✅, rotation among enabled set ✅
- Terminal auto-naming ✅ (live from pane title reports — ESC k / OSC 2; LLM naming ✅ — settled output names terminals, LLM names beat shell titles, human renames beat both, 120s re-check cadence) · footer tips ✅ (rotating idle-footer tips, `showFooterTips`) · toasts ✅ (status line + linger) · file browser ✂ (native file picking rethought later) · web/remote ✂
- Remote-pane-action queue (`M-M` + SIGUSR2) ✂ — replaced by real global keys
- Welcome/spacer panes ✂ — native empty-state view instead

## Rendering-fidelity contract

**Shadow verifier** (`DMUX_VERIFY=1`): in any real session, settled panes are
periodically compared cell-for-cell against tmux's grid; a mismatch toasts
and writes `~/.dmux/incidents/render-*.txt` with both grids plus the raw
`%output` ring (base64 — replay with `base64 -d | griddump COLS ROWS raw`).
Every real-world artifact becomes a reproducible regression test. Run with
it on when chasing rendering bugs.


`scripts/fidelity.sh` proves dmux-rs paints panes **cell-for-cell identical
(chars + fg + bg) to tmux's grid**, through the full pipeline (control mode →
VT → compositor → host emission), on both the live-output path and the seed
path (restart), over a corpus of hostile content (BCE bands, page
backgrounds, scrolled bands, wide/ambiguous glyphs, emoji, styles, dynamic
palettes). Run it after any change to dmux-vt / dmux-compositor / the seed
path. Documented, deliberate divergences from tmux's own client rendering:

- **Scroll-compacted trailing backgrounds**: tmux drops trailing BCE
  backgrounds when scrolled lines are compacted (its own clients render them
  default); we keep them, like real terminals do.
- **OSC 10/11/4 pane palettes**: tmux themes only written cells; we theme
  erased cells too (real-terminal semantics — themed shells look right).
  The palette cannot be recovered across a dmux restart (tmux doesn't expose
  it); it returns with the app's next repaint.
- **tmux-level theming** (`window-style`, `pane-border-status`, status bar)
  is not reproduced — dmux owns pane chrome. `pane-border-status` is forced
  off on dmux windows (it steals a content row inside the window).

## Key-command policy (the "toes" problem)

Panes run vim/emacs/Claude Code/fzf — they own the keyboard. Rules:

1. **Leader prefix** `Ctrl+b` (tmux muscle memory; configurable later). `Ctrl+b Ctrl+b` sends a literal `Ctrl+b` to the pane. All dmux commands live behind the leader; a one-key overlay cheat-sheet shows on leader press.
2. **Super/Cmd combos** registered only when the host terminal speaks the kitty keyboard protocol (probed at startup) — those keys never reach pane apps anyway, so they're collision-free: instant `Super+1..9`, `Super+n/t/,/w/r/h`.
3. **Only two bare chords** outside the leader: `Ctrl+q` (quit, confirm) and `Ctrl+y` (HUD) — plus `Alt` navigation alternates, all forwarded to the pane instead whenever an overlay isn't open and the focused pane has actually bound them? No — we can't know; Alt combos are kept because terminal apps overwhelmingly ignore them, but every Alt binding also exists behind the leader so a collision always has an escape hatch.
4. Overlays swallow all input while open; `Esc` closes.
