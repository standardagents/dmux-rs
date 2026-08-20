//! Perf-HUD pointer behavior (#103): the title row drags the card (grab
//! offset preserved, position clamped so the overlay is always
//! recoverable), the ✕ dismisses it. Keyboard toggling is untouched.

use crate::input::MouseKind;
use crate::render;
use crate::views::ClickTarget;
use crate::App;

impl App {
    /// Handle a press on a HUD control. `HudClose` dismisses; `HudTitle`
    /// starts a drag anchored at the pointer's offset within the card.
    pub(crate) fn hud_press(&mut self, target: Option<ClickTarget>, col: u16, row: u16) -> bool {
        match target {
            Some(ClickTarget::HudClose) => {
                self.hud = false;
                self.force_full = true;
                self.dirty = true;
            }
            Some(ClickTarget::HudTitle) => {
                let rect = render::hud_layout(self.back.area(), &self.metrics, self.hud_pos);
                self.hud_drag = Some((col.saturating_sub(rect.x), row.saturating_sub(rect.y)));
            }
            _ => {}
        }
        true
    }

    /// While a HUD drag is active, follow the pointer (minus the grab
    /// offset, clamped on screen) and swallow the mouse until release.
    /// Returns None when no drag is active.
    pub(crate) fn hud_drag_motion(
        &mut self,
        kind: MouseKind,
        is_press: bool,
        col: u16,
        row: u16,
    ) -> Option<bool> {
        let (gx, gy) = self.hud_drag?;
        match kind {
            MouseKind::LeftHeld if !is_press => {
                let area = self.back.area();
                let rect = render::hud_layout(area, &self.metrics, self.hud_pos);
                self.hud_pos = Some(render::hud_clamp(
                    (col.saturating_sub(gx), row.saturating_sub(gy)),
                    (rect.w, rect.h),
                    area,
                ));
                self.dirty = true;
                Some(true)
            }
            MouseKind::Release => {
                self.hud_drag = None;
                Some(true)
            }
            _ => None,
        }
    }
}
