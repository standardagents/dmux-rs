use dmux_compositor::Rect;

/// Frame-scoped registry of mouse targets. Rebuilt on every composed frame in
/// draw order; hit-testing walks in reverse so the topmost drawn region wins.
/// This replaces the TS era's hand-derived click math (`sidebarClickMap.ts`)
/// with "whatever you drew is clickable".
#[derive(Debug)]
pub struct ClickMap<T> {
    regions: Vec<(Rect, T)>,
}

impl<T> Default for ClickMap<T> {
    fn default() -> Self {
        Self { regions: Vec::new() }
    }
}

impl<T: Clone> ClickMap<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.regions.clear();
    }

    pub fn add(&mut self, rect: Rect, tag: T) {
        if !rect.is_empty() {
            self.regions.push((rect, tag));
        }
    }

    /// Topmost (most recently added) region containing the point.
    pub fn hit(&self, col: u16, row: u16) -> Option<&T> {
        self.regions
            .iter()
            .rev()
            .find(|(r, _)| r.contains(col, row))
            .map(|(_, t)| t)
    }

    pub fn len(&self) -> usize {
        self.regions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topmost_wins() {
        let mut m = ClickMap::new();
        m.add(Rect::new(0, 0, 10, 10), "bottom");
        m.add(Rect::new(2, 2, 4, 4), "top");
        assert_eq!(m.hit(3, 3), Some(&"top"));
        assert_eq!(m.hit(8, 8), Some(&"bottom"));
        assert_eq!(m.hit(50, 50), None);
    }
}
