/// tmux pane id (`%N`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

/// tmux window id (`@N`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u32);

/// tmux session id (`$N`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u32);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl std::fmt::Display for WindowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "@{}", self.0)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "${}", self.0)
    }
}

/// One parsed line of the control-mode stream.
///
/// Reply bracketing (`%begin`/`%end`/`%error`) is surfaced as events so the
/// adapter layer can fold payload lines into pending command replies; payload
/// lines arrive as `ReplyLine` and are raw bytes (command output is not
/// octal-escaped, unlike `%output` data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CcEvent {
    ReplyBegin { time: u64, num: u64, flags: u64 },
    ReplyLine(Vec<u8>),
    ReplyEnd { time: u64, num: u64, flags: u64, ok: bool },

    /// Unescaped output bytes for a pane.
    Output { pane: PaneId, data: Vec<u8> },
    /// Output with an age (ms the data spent buffered server-side); emitted
    /// instead of `%output` once `pause-after` flow control is enabled.
    ExtendedOutput { pane: PaneId, age_ms: u64, data: Vec<u8> },
    Pause(PaneId),
    Continue(PaneId),

    WindowAdd(WindowId),
    WindowClose(WindowId),
    UnlinkedWindowClose(WindowId),
    WindowRenamed { window: WindowId, name: String },
    UnlinkedWindowRenamed { window: WindowId, name: String },
    WindowPaneChanged { window: WindowId, pane: PaneId },
    LayoutChange { window: WindowId, layout: String, visible_layout: Option<String>, raw_flags: Option<String> },

    SessionChanged { session: SessionId, name: String },
    SessionRenamed { name: String },
    SessionsChanged,
    SessionWindowChanged { session: SessionId, window: WindowId },
    ClientSessionChanged { client: String, session: SessionId, name: String },
    ClientDetached { client: String },

    PaneModeChanged(PaneId),
    PasteBufferChanged { name: String },
    PasteBufferDeleted { name: String },
    SubscriptionChanged { raw: String },
    ConfigError(String),
    Message(String),
    /// Server is closing the control connection. Reason is present on newer tmux.
    Exit(Option<String>),

    /// A `%`-prefixed line we don't recognize (forward compatibility): logged,
    /// never fatal.
    Unknown(String),
}
