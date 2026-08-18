#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> u16 {
        self.x.saturating_add(self.w)
    }

    pub fn bottom(&self) -> u16 {
        self.y.saturating_add(self.h)
    }

    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x && col < self.right() && row >= self.y && row < self.bottom()
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect {
            x,
            y,
            w: right.saturating_sub(x),
            h: bottom.saturating_sub(y),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}
