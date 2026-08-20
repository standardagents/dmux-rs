//! Progress surface for a dmux-rs prototype build. Cargo output is captured by
//! the build worker and reduced to one live status line so it cannot paint over
//! the renderer's terminal.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dmux_compositor::{AttrFlags, CellBuffer, Rect};
use dmux_host::KeyEvent;
use dmux_ui::{centered, draw_hint_bar, draw_panel, spinner_frame, ClickMap, PanelStyle};

use super::{vkeys, ClickTarget, View, ViewCtx, ViewResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Building,
    Ready,
    Failed,
}

struct State {
    worktree: String,
    path: String,
    detail: String,
    failure: String,
    phase: Phase,
    started: Instant,
}

#[derive(Clone)]
pub struct PrototypeBuildHandle(Arc<Mutex<State>>);

impl PrototypeBuildHandle {
    pub fn detail(&self, detail: String) {
        if let Ok(mut state) = self.0.lock() {
            state.detail = detail;
        }
    }

    pub fn ready(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.phase = Phase::Ready;
            state.detail = "Build complete".into();
        }
    }

    pub fn failed(&self, failure: String) {
        if let Ok(mut state) = self.0.lock() {
            state.phase = Phase::Failed;
            state.failure = failure;
        }
    }
}

pub struct PrototypeBuildView {
    state: Arc<Mutex<State>>,
}

impl PrototypeBuildView {
    pub fn new(worktree: &Path) -> (Self, PrototypeBuildHandle) {
        let state = Arc::new(Mutex::new(State {
            worktree: worktree
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "dmux-rs worktree".into()),
            path: worktree.to_string_lossy().into_owned(),
            detail: "Preparing shared dependency cache".into(),
            failure: String::new(),
            phase: Phase::Building,
            started: Instant::now(),
        }));
        (
            Self {
                state: Arc::clone(&state),
            },
            PrototypeBuildHandle(state),
        )
    }
}

impl View for PrototypeBuildView {
    fn render(
        &mut self,
        buf: &mut CellBuffer,
        area: Rect,
        ctx: &ViewCtx<'_>,
        _clicks: &mut ClickMap<ClickTarget>,
    ) -> Option<(u16, u16)> {
        let Ok(state) = self.state.lock() else {
            return None;
        };
        let rect = centered(area, area.w.min(68), area.h.min(11));
        let inner = draw_panel(
            buf,
            rect,
            "Building dmux-rs prototype",
            ctx.theme,
            PanelStyle::Modal,
        );
        let bg = ctx.theme.bg_panel;
        buf.draw_text(
            inner.x + 1,
            inner.y,
            &state.worktree,
            ctx.theme.text,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        buf.draw_text(
            inner.x + 1,
            inner.y + 1,
            &state.path,
            ctx.theme.text_faint,
            bg,
            AttrFlags::empty(),
            inner,
        );
        let (glyph, label, color) = match state.phase {
            Phase::Building => (
                spinner_frame(ctx.anim),
                "Compiling release binary",
                ctx.theme.accent,
            ),
            Phase::Ready => (spinner_frame(ctx.anim), "Loading dmux-rs", ctx.theme.ok),
            Phase::Failed => ('✗', "Build failed", ctx.theme.danger),
        };
        let status_y = inner.y + 3;
        buf.draw_text(
            inner.x + 1,
            status_y,
            &glyph.to_string(),
            color,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        buf.draw_text(
            inner.x + 3,
            status_y,
            label,
            color,
            bg,
            AttrFlags::BOLD,
            inner,
        );
        let detail = if state.phase == Phase::Failed {
            &state.failure
        } else {
            &state.detail
        };
        buf.draw_text(
            inner.x + 3,
            status_y + 1,
            detail,
            ctx.theme.text_dim,
            bg,
            AttrFlags::empty(),
            inner,
        );
        buf.draw_text(
            inner.x + 1,
            inner.bottom().saturating_sub(3),
            "Shared dependency cache",
            ctx.theme.text_faint,
            bg,
            AttrFlags::empty(),
            inner,
        );
        let elapsed = format!("Elapsed: {}s", state.started.elapsed().as_secs());
        buf.draw_text(
            inner.x + 1,
            inner.bottom().saturating_sub(2),
            &elapsed,
            ctx.theme.text_faint,
            bg,
            AttrFlags::empty(),
            inner,
        );
        let footer = Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.w, 1);
        let escape_action = if state.phase == Phase::Failed {
            "close"
        } else {
            "hide"
        };
        draw_hint_bar(buf, footer, &[("esc", escape_action)], ctx.theme);
        None
    }

    fn on_key(&mut self, key: &KeyEvent) -> ViewResult {
        if vkeys::is_esc(key) {
            ViewResult::Close
        } else {
            ViewResult::Stay
        }
    }

    fn animating(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.phase != Phase::Failed)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmux_host::{KeyCode, Modifiers};

    #[test]
    fn build_handle_tracks_ready_and_failure_states() {
        let (view, handle) = PrototypeBuildView::new(Path::new("/tmp/dmux-worktree"));
        handle.detail("Compiling dmux v0.1.0".into());
        handle.ready();
        assert_eq!(view.state.lock().unwrap().phase, Phase::Ready);
        handle.failed("compiler error".into());
        let state = view.state.lock().unwrap();
        assert_eq!(state.phase, Phase::Failed);
        assert_eq!(state.failure, "compiler error");
    }

    #[test]
    fn progress_surface_contains_cargo_and_failure_states() {
        let (mut view, handle) = PrototypeBuildView::new(Path::new("/projects/dmux-candidate"));
        handle.detail("Compiling dmux v0.1.0".into());
        let mut buf = CellBuffer::new(100, 20);
        let theme = dmux_ui::Theme::default();
        let ctx = ViewCtx {
            theme: &theme,
            anim: 2,
            hovered: None,
            sidebar_right: 0,
            anchor: dmux_ui::Anchor::SidebarTop,
        };
        let area = buf.area();
        view.render(&mut buf, area, &ctx, &mut ClickMap::new());
        let mut frame = String::new();
        for y in 0..area.h {
            for x in 0..area.w {
                frame.push(buf.get(x, y).ch);
            }
        }
        assert!(frame.contains("Building dmux-rs prototype"));
        assert!(frame.contains("Compiling dmux v0.1.0"));
        assert!(frame.contains("Shared dependency cache"));

        handle.failed("compiler error".into());
        let key = KeyEvent {
            key: KeyCode::Escape,
            modifiers: Modifiers::NONE,
        };
        assert!(matches!(view.on_key(&key), ViewResult::Close));
    }
}
