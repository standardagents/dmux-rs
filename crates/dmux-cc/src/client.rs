use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::event::CcEvent;
use crate::parse::Parser;

#[derive(Debug, thiserror::Error)]
pub enum CcError {
    #[error("tmux spawn failed: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("tmux command failed: {0}")]
    Command(String),
    #[error("control-mode connection closed")]
    Closed,
    #[error("control-mode protocol desync: {0}")]
    Desync(String),
    #[error("command contains an embedded line break")]
    UnsafeCommand,
}

/// A completed command reply: payload lines between `%begin` and `%end`/`%error`.
#[derive(Debug, Clone)]
pub struct Reply {
    pub lines: Vec<Vec<u8>>,
    pub ok: bool,
    pub rtt: std::time::Duration,
}

impl Reply {
    /// Decode octal-escaped reply payload lines in place. tmux 3.5a (and
    /// kin) octal-escape control bytes in command replies — `0x01` field
    /// separators arrive as the four bytes `\001`, and real backslashes as
    /// `\134`, so decoding is unambiguous THERE. Newer servers (3.7b) send
    /// raw bytes, where decoding would corrupt captured pane text — callers
    /// gate this on a per-server probe (#19).
    pub fn unescape_lines(&mut self) {
        for line in &mut self.lines {
            *line = crate::unescape_output(line);
        }
    }

    pub fn text_lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect()
    }
}

struct PendingSlot<T> {
    sent_at: Instant,
    tag: Option<T>,
}

/// What the consumer gets after routing one raw event through [`ReplyRouter`].
#[derive(Debug)]
pub enum Routed<T> {
    /// A notification (never a reply-bracketing event).
    Notification(CcEvent),
    /// A command's reply, carrying the tag it was sent with.
    Reply(T, Reply),
    /// Reply plumbing (begin/line, or a reply for an untagged command) —
    /// nothing for the consumer to do.
    Consumed,
    /// A reply arrived with no pending command: protocol desync. The only
    /// safe recovery is to tear down the connection and reattach.
    Desync,
}

/// tmux control-mode client. All events — notifications *and* reply
/// bracketing — flow through ONE channel in exact stream order; the consumer
/// drives a [`ReplyRouter`] in its own loop so command completions are
/// totally ordered against `%output`. That ordering is what makes
/// pause→capture→reseed free of duplicate-application races.
pub struct Client<T> {
    cmd_tx: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<VecDeque<PendingSlot<T>>>>,
}

pub type SpawnedClient<T> = (Client<T>, mpsc::Receiver<CcEvent>, ReplyRouter<T>, Child);

impl<T> Clone for Client<T> {
    fn clone(&self) -> Self {
        Self {
            cmd_tx: self.cmd_tx.clone(),
            pending: self.pending.clone(),
        }
    }
}

impl<T> Client<T> {
    /// Spawn `tmux <args...>` (must include `-C` and an attach/new-session).
    /// Returns the client, the ordered event stream, the router, and the
    /// child process handle.
    pub fn spawn(tmux_bin: &str, args: &[String]) -> Result<SpawnedClient<T>, CcError> {
        let mut child = Command::new(tmux_bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;

        let stdout = child.stdout.take().expect("piped stdout");
        let mut stdin = child.stdin.take().expect("piped stdin");

        // Bounded: if the consumer falls behind, the reader stops reading the
        // pipe, tmux blocks on write, and server-side `pause-after` flow
        // control engages. This chain IS the backpressure design.
        let (event_tx, event_rx) = mpsc::channel::<CcEvent>(1024);
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<String>();

        // The hello block (the attach command's own reply) arrives before any
        // command we send: pre-register an untagged slot for it.
        let pending: Arc<Mutex<VecDeque<PendingSlot<T>>>> =
            Arc::new(Mutex::new(VecDeque::from([PendingSlot {
                sent_at: Instant::now(),
                tag: None,
            }])));

        tokio::spawn(async move {
            let mut parser = Parser::new();
            let mut buf = vec![0u8; 64 * 1024];
            let mut events = Vec::with_capacity(64);
            let mut stdout = stdout;
            let mut total: u64 = 0;
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) => {
                        tracing::debug!(total, "cc reader: EOF");
                        break;
                    }
                    Err(err) => {
                        tracing::debug!(total, %err, "cc reader: error");
                        break;
                    }
                    Ok(n) => {
                        total += n as u64;
                        tracing::trace!(n, total, "cc reader: bytes");
                        parser.feed(&buf[..n], &mut events);
                        for ev in events.drain(..) {
                            if event_tx.send(ev).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            let _ = event_tx
                .send(CcEvent::Exit(Some("stream closed".into())))
                .await;
        });

        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                tracing::trace!(%cmd, "cc writer");
                if stdin.write_all(cmd.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
            tracing::debug!("cc writer: channel closed");
        });

        let router = ReplyRouter {
            pending: pending.clone(),
            current: None,
        };
        Ok((Client { cmd_tx, pending }, event_rx, router, child))
    }

    /// Send a command whose reply the consumer wants, identified by `tag`.
    /// The reply surfaces from `ReplyRouter::route` in stream order.
    pub fn send_tagged(&self, cmd: impl Into<String>, tag: T) -> Result<(), CcError> {
        self.send_inner(cmd.into(), Some(tag))
    }

    /// Send a command and discard its reply (still consumed from the FIFO).
    pub fn send(&self, cmd: impl Into<String>) -> Result<(), CcError> {
        self.send_inner(cmd.into(), None)
    }

    fn send_inner(&self, cmd: String, tag: Option<T>) -> Result<(), CcError> {
        // A command with an embedded line break would desync the whole
        // control stream (#18) — refuse it loudly instead of dying quietly.
        if !crate::command_is_line_safe(&cmd) {
            tracing::warn!(cmd = %cmd.escape_default(), "dropped control-mode command with embedded line break");
            return Err(CcError::UnsafeCommand);
        }
        // Hold the pending lock across the channel send so slot order always
        // matches write order even with concurrent senders.
        let mut pending = self.pending.lock().unwrap();
        pending.push_back(PendingSlot {
            sent_at: Instant::now(),
            tag,
        });
        self.cmd_tx.send(cmd).map_err(|_| {
            pending.pop_back();
            CcError::Closed
        })
    }
}

/// Folds reply-bracketing events into complete replies. Must be driven by the
/// same loop that consumes the event stream.
pub struct ReplyRouter<T> {
    pending: Arc<Mutex<VecDeque<PendingSlot<T>>>>,
    current: Option<Vec<Vec<u8>>>,
}

impl<T> ReplyRouter<T> {
    pub fn route(&mut self, ev: CcEvent) -> Routed<T> {
        match ev {
            CcEvent::ReplyBegin { .. } => {
                self.current = Some(Vec::new());
                Routed::Consumed
            }
            CcEvent::ReplyLine(line) => {
                if let Some(lines) = &mut self.current {
                    lines.push(line);
                }
                Routed::Consumed
            }
            CcEvent::ReplyEnd { ok, .. } => {
                let lines = self.current.take().unwrap_or_default();
                let slot = self.pending.lock().unwrap().pop_front();
                match slot {
                    Some(slot) => {
                        let reply = Reply {
                            lines,
                            ok,
                            rtt: slot.sent_at.elapsed(),
                        };
                        match slot.tag {
                            Some(tag) => Routed::Reply(tag, reply),
                            None => Routed::Consumed,
                        }
                    }
                    None => Routed::Desync,
                }
            }
            other => Routed::Notification(other),
        }
    }
}
