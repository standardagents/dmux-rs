//! tmux control-mode (`tmux -C`) protocol: a sans-io parser producing typed
//! events, an octal unescaper, command quoting, and a tokio client adapter
//! that multiplexes command replies over the FIFO reply stream.

mod command;
mod event;
mod parse;
mod unescape;

#[cfg(feature = "tokio-adapter")]
mod client;

pub use command::command_is_line_safe;
pub use command::quote_arg;
pub use event::{CcEvent, PaneId, SessionId, WindowId};
pub use parse::Parser;
pub use unescape::unescape_output;

#[cfg(feature = "tokio-adapter")]
pub use client::{CcError, Client, Reply, ReplyRouter, Routed};
