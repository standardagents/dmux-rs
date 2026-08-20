//! Overlay stack transitions shared by every native view.

use crate::views::{ClickTarget, ViewResult};
use crate::App;

impl App {
    pub(super) fn apply_view_result(&mut self, result: ViewResult) -> bool {
        if !matches!(&result, ViewResult::Stay)
            && matches!(self.hovered, Some(ClickTarget::Overlay(_)))
        {
            self.hovered = None;
        }
        match result {
            ViewResult::Stay => true,
            ViewResult::Close => {
                self.views.pop();
                self.dirty = true;
                true
            }
            ViewResult::Push(view) => {
                self.views.push(view);
                self.dirty = true;
                true
            }
            ViewResult::Cmd(cmd) => self.execute_cmd(cmd),
            ViewResult::CloseAnd(cmd) => {
                self.views.pop();
                self.dirty = true;
                self.execute_cmd(cmd)
            }
            ViewResult::CloseTwoAnd(cmd) => {
                self.views.pop();
                self.views.pop();
                self.dirty = true;
                self.execute_cmd(cmd)
            }
        }
    }
}
