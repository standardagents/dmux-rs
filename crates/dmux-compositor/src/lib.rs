//! Headless cell-buffer compositor: cell model, damage tracking, frame diffing,
//! and minimal ANSI emission. No terminal I/O lives here — bytes in, bytes out —
//! so the whole pipeline is testable without a tty.

mod cell;
mod diff;
mod emit;
mod rect;

pub use cell::{AttrFlags, Cell, CellBuffer, Color};
pub use diff::{diff_frame, FrameStats};
pub use emit::Emitter;
pub use rect::Rect;
