use crate::{CellBuffer, Emitter};

#[derive(Debug, Default, Clone, Copy)]
pub struct FrameStats {
    pub rows_scanned: u32,
    pub rows_changed: u32,
    pub cells_emitted: u32,
    pub bytes_emitted: u32,
}

/// Diff `back` (the freshly composed frame) against `front` (what the terminal
/// currently shows) and emit the minimal update into `emitter`. On return,
/// `front` equals `back` for every dirty row and `back`'s dirty flags are
/// cleared. `force_full` repaints every row regardless of flags (resize,
/// reattach, or emitter-state invalidation).
pub fn diff_frame(
    front: &mut CellBuffer,
    back: &mut CellBuffer,
    emitter: &mut Emitter,
    force_full: bool,
) -> FrameStats {
    debug_assert_eq!(front.cols(), back.cols());
    debug_assert_eq!(front.rows(), back.rows());

    let mut stats = FrameStats::default();
    let before = emitter.len();
    let cols = back.cols();

    for row in 0..back.rows() {
        if !force_full && !back.row_dirty(row) {
            continue;
        }
        stats.rows_scanned += 1;

        let mut col = 0u16;
        let mut row_changed = false;
        while col < cols {
            let new = back.get(col, row);
            if new.wide_spacer() {
                // Covered by the wide cell to the left; comparison happens there.
                col += 1;
                continue;
            }
            let old = front.get(col, row);
            // A wide char must be re-emitted if either of its two columns changed.
            let changed = if force_full {
                true
            } else if new != old {
                true
            } else if new.display_width() == 2 && col + 1 < cols {
                back.get(col + 1, row) != front.get(col + 1, row)
            } else {
                false
            };

            if changed {
                emitter.move_to(col, row);
                emitter.put_cell(new);
                stats.cells_emitted += 1;
                row_changed = true;
            }
            col += new.display_width().max(1);
        }
        if row_changed {
            stats.rows_changed += 1;
        }

        // Sync front to back for this row.
        for c in 0..cols {
            let cell = back.get(c, row).clone();
            front.set(c, row, cell);
        }
    }

    back.clear_dirty();
    front.clear_dirty();
    stats.bytes_emitted = (emitter.len() - before) as u32;
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttrFlags, Cell, Color, Rect};

    fn buffers(cols: u16, rows: u16) -> (CellBuffer, CellBuffer) {
        (CellBuffer::new(cols, rows), CellBuffer::new(cols, rows))
    }

    #[test]
    fn no_change_emits_nothing() {
        let (mut front, mut back) = buffers(10, 2);
        back.clear_dirty();
        let mut em = Emitter::new();
        let stats = diff_frame(&mut front, &mut back, &mut em, false);
        assert_eq!(stats.cells_emitted, 0);
        assert!(em.is_empty());
    }

    #[test]
    fn single_cell_change_emits_one_cell() {
        let (mut front, mut back) = buffers(10, 2);
        // Settle both buffers to a known-equal state first.
        let mut em = Emitter::new();
        diff_frame(&mut front, &mut back, &mut em, true);
        em.take();

        back.draw_text(3, 1, "x", Color::Default, Color::Default, AttrFlags::empty(), back.area());
        let stats = diff_frame(&mut front, &mut back, &mut em, false);
        assert_eq!(stats.cells_emitted, 1);
        let out = String::from_utf8(em.take()).unwrap();
        assert!(out.contains("\x1b[2;4H"), "expected CUP to row 2 col 4, got {out:?}");
        assert!(out.ends_with('x'));
    }

    #[test]
    fn wide_char_reemitted_when_either_column_changes() {
        let (mut front, mut back) = buffers(10, 1);
        let mut em = Emitter::new();
        back.draw_text(0, 0, "漢", Color::Default, Color::Default, AttrFlags::empty(), back.area());
        diff_frame(&mut front, &mut back, &mut em, false);
        em.take();

        // Overwrite only the spacer column with a narrow char.
        back.draw_text(1, 0, "a", Color::Default, Color::Default, AttrFlags::empty(), back.area());
        // Column 0 must also be repainted (the wide char no longer fits).
        back.draw_text(0, 0, " ", Color::Default, Color::Default, AttrFlags::empty(), back.area());
        let stats = diff_frame(&mut front, &mut back, &mut em, false);
        assert!(stats.cells_emitted >= 2);
        let out = String::from_utf8(em.take()).unwrap();
        assert!(out.contains('a'));
    }

    #[test]
    fn non_ascii_cell_forces_explicit_cup_for_next_cell() {
        // A host may render ⏺/emoji/box-drawing wider or narrower than our
        // width table says; the emitter must never rely on implicit advance
        // after such a glyph. The cell following one must carry its own CUP.
        let (mut front, mut back) = buffers(20, 1);
        let mut em = Emitter::new();
        diff_frame(&mut front, &mut back, &mut em, true);
        em.take();

        back.draw_text(0, 0, "⏺ hi", Color::Default, Color::Default, AttrFlags::empty(), back.area());
        diff_frame(&mut front, &mut back, &mut em, false);
        let out = String::from_utf8(em.take()).unwrap();
        let bullet = out.find('⏺').expect("bullet emitted");
        let after = &out[bullet + '⏺'.len_utf8()..];
        // The unchanged space at col 2 is skipped; what matters is that the
        // next write does NOT rely on implicit advance past the glyph.
        assert!(
            after.starts_with("\x1b[1;3H"),
            "expected explicit CUP after the ambiguous-width glyph, got {out:?}"
        );
        // Plain ASCII runs still coalesce without per-cell CUPs.
        assert!(
            after.ends_with("hi"),
            "ascii continuation should be written contiguously, got {after:?}"
        );
    }

    #[test]
    fn front_converges_to_back() {
        let (mut front, mut back) = buffers(20, 3);
        let mut em = Emitter::new();
        back.draw_text(0, 0, "hello", Color::Indexed(2), Color::Default, AttrFlags::BOLD, back.area());
        back.fill(
            Rect::new(0, 2, 20, 1),
            &Cell { bg: Color::Rgb(10, 20, 30), ..Cell::default() },
        );
        diff_frame(&mut front, &mut back, &mut em, false);
        for row in 0..3 {
            for col in 0..20 {
                assert_eq!(front.get(col, row), back.get(col, row));
            }
        }
        // Second diff with no edits: nothing to do.
        let stats = diff_frame(&mut front, &mut back, &mut em, false);
        assert_eq!(stats.rows_scanned, 0);
    }
}
