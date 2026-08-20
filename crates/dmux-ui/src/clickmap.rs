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
        Self {
            regions: Vec::new(),
        }
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

    /// Most recently drawn region registered for an exact target. This lets
    /// overlays resolve their source geometry from the current frame after a
    /// resize or sidebar reflow.
    pub fn rect_for(&self, target: &T) -> Option<Rect>
    where
        T: PartialEq,
    {
        self.regions
            .iter()
            .rev()
            .find(|(_, candidate)| candidate == target)
            .map(|(rect, _)| *rect)
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
        assert_eq!(m.rect_for(&"top"), Some(Rect::new(2, 2, 4, 4)));
    }
}
