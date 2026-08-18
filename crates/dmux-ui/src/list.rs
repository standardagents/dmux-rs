/// Selection + scroll state for any vertical list. Views own one per list and
/// pair it with their item slices at draw time.
#[derive(Debug, Default, Clone)]
pub struct ListState {
    pub selected: usize,
    pub scroll: usize,
}

impl ListState {
    pub fn step(&mut self, delta: i32, len: usize) {
        if len == 0 {
            self.selected = 0;
            return;
        }
        let cur = self.selected as i32;
        self.selected = (cur + delta).rem_euclid(len as i32) as usize;
    }

    pub fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    /// Adjust scroll so the selection is visible within `visible_rows`.
    pub fn ensure_visible(&mut self, visible_rows: usize) {
        if visible_rows == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible_rows {
            self.scroll = self.selected + 1 - visible_rows;
        }
    }
}
