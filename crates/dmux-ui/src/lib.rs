//! dmux's native UI component library: a small, consistent set of primitives
//! every overlay and chrome surface is built from — panels, lists, text
//! inputs, form controls, buttons, spinners — plus the click-region map that
//! makes any drawn thing a mouse target. Replaces the TS/Ink era's per-popup
//! reinvention with one visual system.

mod clickmap;
mod input;
mod list;
mod panel;
mod theme;
mod widgets;

pub use clickmap::ClickMap;
pub use input::{InputKey, TextInput};
pub use list::ListState;
pub use panel::{centered, draw_panel, draw_scrim, PanelStyle};
pub use theme::Theme;
pub use widgets::{
    draw_button, draw_checkbox, draw_counter, draw_hint_bar, draw_kv_row, draw_radio,
    draw_select_value, spinner_frame, ButtonStyle,
};
