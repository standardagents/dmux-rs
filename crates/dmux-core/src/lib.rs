//! Renderer-independent dmux domain model: config file compatibility with the
//! TypeScript implementation, the pane-title identity contract, and session
//! naming. Every struct preserves unknown fields so a Rust read→write cycle
//! never strips data the TS implementation needs.

mod config;
mod session;
mod title;

pub use config::{DmuxConfig, DmuxPane, PaneKind};
pub use session::session_name_for_root;
pub use title::{parse_pane_title, PaneTitle, PANE_TITLE_DELIMITER};
