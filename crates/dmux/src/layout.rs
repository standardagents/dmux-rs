//! Compositor grid math: a fixed-width sidebar plus a grid of pane rects in
//! the content area. Ports the comfort band from the TS `LayoutCalculator`
//! (min 60 / max 100 cols, min 15 rows) without any tmux layout strings —
//! rects are ours alone.

use dmux_compositor::Rect;

pub const SIDEBAR_WIDTH: u16 = 40;
/// One column of chrome between sidebar and content.
pub const GUTTER: u16 = 1;
/// Rows of chrome above each pane (title bar).
pub const TITLE_ROWS: u16 = 1;

pub const DEFAULT_MIN_WIDTH: u16 = 60;
pub const DEFAULT_MAX_WIDTH: u16 = 100;
const MIN_COMFORTABLE_HEIGHT: u16 = 15;

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub sidebar: Rect,
    /// Content rect per visible pane, in input order. The rect covers the
    /// pane body only; the title bar sits in the `TITLE_ROWS` above it.
    pub panes: Vec<Rect>,
}

/// Compute the layout for `n` visible panes on a `cols`×`rows` host, with a
/// user-tunable comfort band (settings `minPaneWidth`/`maxPaneWidth`).
pub fn compute_with_band(cols: u16, rows: u16, n: usize, min_w: u16, max_w: u16) -> Layout {
    let sidebar = Rect::new(0, 0, SIDEBAR_WIDTH.min(cols), rows);
    let content_x = sidebar.w + GUTTER;
    let content_w = cols.saturating_sub(content_x);
    let mut layout = Layout {
        sidebar,
        panes: Vec::new(),
    };
    if n == 0 || content_w < 20 || rows < TITLE_ROWS + 3 {
        return layout;
    }

    let n16 = n as u16;
    // Choose a column count whose pane width lands closest to the comfort
    // band, weighting height comfort as a tiebreaker.
    let mut best = (1u16, i32::MIN);
    for grid_cols in 1..=n16 {
        let grid_rows = n16.div_ceil(grid_cols);
        let pane_w = content_w / grid_cols;
        let pane_h = rows / grid_rows;
        if pane_w < 20 || pane_h < TITLE_ROWS + 3 {
            continue;
        }
        let mut score: i32 = 0;
        score -= match pane_w {
            w if w < min_w => (min_w - w) as i32 * 3,
            w if w > max_w => (w - max_w) as i32,
            _ => 0,
        };
        if pane_h < MIN_COMFORTABLE_HEIGHT + TITLE_ROWS {
            score -= (MIN_COMFORTABLE_HEIGHT + TITLE_ROWS - pane_h) as i32 * 2;
        }
        // Prefer fuller last rows (fewer dangling panes).
        let used_last_row = n16 - (grid_rows - 1) * grid_cols;
        score -= (grid_cols - used_last_row) as i32;
        if score > best.1 {
            best = (grid_cols, score);
        }
    }
    let grid_cols = best.0;
    let grid_rows = n16.div_ceil(grid_cols);

    // Cap column width at the configured max. The grid anchors to the left
    // edge (beside the sidebar) and flows rightward as panes are added.
    let natural_w = content_w / grid_cols;
    let cell_w = natural_w.min(max_w + GUTTER);
    // Division remainder when the width cap is NOT in play (#43): the last
    // grid column absorbs it, exactly like the last row absorbs the height
    // remainder — otherwise up to grid_cols-1 unpainted canvas columns
    // strip along the terminal's right edge. A capped grid deliberately
    // leaves canvas instead.
    let w_remainder = if cell_w == natural_w {
        content_w - grid_cols * cell_w
    } else {
        0
    };
    let x0 = content_x;
    let pane_h = rows / grid_rows;
    for i in 0..n16 {
        let gc = i % grid_cols;
        let gr = i / grid_cols;
        let x = x0 + gc * cell_w;
        let y = gr * pane_h;
        // Last row absorbs the vertical remainder.
        let h = if gr == grid_rows - 1 {
            rows - gr * pane_h
        } else {
            pane_h
        };
        let w_extra = if gc == grid_cols - 1 { w_remainder } else { 0 };
        // Reserve the title bar; body starts below it. One column of spacing
        // between horizontally adjacent panes.
        let body = Rect::new(
            x,
            y + TITLE_ROWS,
            cell_w.saturating_sub(GUTTER) + w_extra,
            h.saturating_sub(TITLE_ROWS),
        );
        layout.panes.push(body);
    }
    layout
}

/// Default-band layout (tests and callers without settings).
#[cfg_attr(not(test), allow(dead_code))]
pub fn compute(cols: u16, rows: u16, n: usize) -> Layout {
    compute_with_band(cols, rows, n, DEFAULT_MIN_WIDTH, DEFAULT_MAX_WIDTH)
}

#[cfg(test)]
mod tests {
    #[test]
    fn last_column_absorbs_width_remainder() {
        // #43: content 159 wide, 2 columns → 79+79 left a one-column strip
        // at the terminal's right edge. The last column absorbs it: each
        // pane's border column (right()) tiles the space and the final
        // border lands on the terminal's last column.
        let l = super::compute(200, 50, 4);
        assert_eq!(l.panes.len(), 4);
        let rightmost = l.panes.iter().map(|p| p.right()).max().unwrap();
        assert_eq!(rightmost, 199, "border column reaches the final column");
        let bottom = l.panes.iter().map(|p| p.bottom()).max().unwrap();
        assert_eq!(bottom, 50, "last row reaches the final row");
        // Capped grids still leave deliberate canvas: enormous width, one
        // pane — the comfort cap wins over edge-filling.
        let capped = super::compute(400, 50, 1);
        assert!(capped.panes[0].right() < 399);
    }

    use super::*;

    #[test]
    fn empty_layout() {
        let l = compute(200, 60, 0);
        assert!(l.panes.is_empty());
        assert_eq!(l.sidebar.w, 40);
    }

    #[test]
    fn seven_panes_fill_area() {
        let l = compute(250, 70, 7);
        assert_eq!(l.panes.len(), 7);
        for r in &l.panes {
            assert!(r.x >= 41, "pane rect must clear sidebar: {r:?}");
            assert!(!r.is_empty());
            assert!(r.right() <= 250 && r.bottom() <= 70);
        }
    }

    #[test]
    fn single_pane_capped_and_left_anchored() {
        let l = compute(300, 50, 1);
        assert_eq!(l.panes.len(), 1);
        let r = l.panes[0];
        // Width capped at the comfort max, not stretched to 300.
        assert_eq!(r.w, DEFAULT_MAX_WIDTH);
        // Anchored beside the sidebar, flowing rightward.
        assert_eq!(r.x, SIDEBAR_WIDTH + GUTTER);
        assert_eq!(r.y, TITLE_ROWS);
    }

    #[test]
    fn wide_band_setting_respected() {
        let l = compute_with_band(400, 50, 1, 60, 200);
        assert_eq!(l.panes[0].w, 200);
    }

    #[test]
    fn tiny_host_yields_no_panes() {
        let l = compute(45, 5, 3);
        assert!(l.panes.is_empty());
    }
}
