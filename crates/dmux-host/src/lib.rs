//! Host-terminal backend: raw mode + alternate screen lifecycle, capability
//! probing, a blocking stdin reader feeding termwiz's input parser, and the
//! frame writer. This is the only crate that touches the real tty.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

pub use termwiz::escape::csi::KittyKeyboardFlags;
use termwiz::input::InputParser;
pub use termwiz::input::{
    InputEvent, KeyCode, KeyCodeEncodeModes, KeyEvent, KeyboardEncoding, Modifiers, MouseButtons,
    MouseEvent,
};
use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("stdout is not a tty")]
    NotATty,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Capabilities detected at startup.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostCaps {
    /// DECSET 2026 synchronized output.
    pub synchronized_output: bool,
    /// Kitty keyboard protocol — enables collision-free Super/Cmd chords.
    pub kitty_keyboard: bool,
}

// ?7l disables autowrap while we own the screen: the compositor addresses
// every cell explicitly, and wrap-pending state after last-column writes
// would otherwise let a stray byte scroll the whole host screen.
const ENTER: &[u8] = b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[?1002h\x1b[?1006h\x1b[?2004h";
const LEAVE: &[u8] = b"\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?7h\x1b[?25h\x1b[?1049l\x1b[0m";

/// Owns the tty state. Restores the terminal on `Drop` (including panics that
/// unwind) and via the explicit `restore` on clean shutdown paths.
pub struct HostTerminal {
    caps: HostCaps,
    restored: bool,
}

impl HostTerminal {
    /// Enter raw mode + alternate screen and probe capabilities.
    pub fn setup() -> Result<Self, HostError> {
        if !is_tty(std::io::stdout().as_raw_fd()) {
            return Err(HostError::NotATty);
        }
        crossterm::terminal::enable_raw_mode()?;
        let mut out = std::io::stdout().lock();
        out.write_all(ENTER)?;
        out.flush()?;
        drop(out);
        let caps = probe_caps();
        if caps.kitty_keyboard {
            // Push minimal kitty flags (disambiguate) so modifier chords —
            // including Super where the terminal forwards it — arrive as
            // unambiguous CSI u sequences.
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(b"\x1b[>1u");
            let _ = out.flush();
        }
        Ok(Self {
            caps,
            restored: false,
        })
    }

    pub fn caps(&self) -> HostCaps {
        self.caps
    }

    /// Current (cols, rows) of the controlling terminal.
    pub fn size(&self) -> (u16, u16) {
        term_size()
    }

    /// Write one frame's bytes and flush.
    pub fn write_frame(&mut self, bytes: &[u8]) -> Result<(), HostError> {
        let mut out = std::io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()?;
        Ok(())
    }

    pub fn restore(&mut self) {
        if self.restored {
            return;
        }
        self.restored = true;
        let mut out = std::io::stdout().lock();
        if self.caps.kitty_keyboard {
            let _ = out.write_all(b"\x1b[<u");
        }
        let _ = out.write_all(LEAVE);
        let _ = out.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

impl Drop for HostTerminal {
    fn drop(&mut self) {
        self.restore();
    }
}

fn is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

pub fn term_size() -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = std::io::stdout().as_raw_fd();
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0
    {
        (ws.ws_col, ws.ws_row)
    } else {
        (80, 24)
    }
}

/// Probe DECSET 2026 support: send DECRQM 2026 followed by DA1; whichever
/// response mentions 2026 with value 1/2 means supported, and the DA1 reply
/// is the fence that bounds the wait. Runs synchronously in raw mode before
/// the async input pipeline starts, so it owns stdin briefly.
fn probe_caps() -> HostCaps {
    let mut caps = HostCaps::default();
    // DECRQM 2026, kitty keyboard query, then DA1 as the reply fence.
    let query = b"\x1b[?2026$p\x1b[?u\x1b[c";
    {
        let mut out = std::io::stdout().lock();
        if out.write_all(query).and_then(|_| out.flush()).is_err() {
            return caps;
        }
    }

    let mut stdin = std::io::stdin().lock();
    set_stdin_nonblocking(true);
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 256];
    while Instant::now() < deadline {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                if find_decrpm_2026(&acc) {
                    caps.synchronized_output = true;
                }
                if find_kitty_reply(&acc) {
                    caps.kitty_keyboard = true;
                }
                // DA1 response terminator: ESC [ ? ... c
                if acc.windows(2).any(|w| w == b"[?") && acc.last() == Some(&b'c') {
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    set_stdin_nonblocking(false);
    caps
}

/// DECRPM reply: ESC [ ? 2026 ; Ps $ y with Ps in {1,2} meaning supported.
fn find_decrpm_2026(acc: &[u8]) -> bool {
    let needle = b"[?2026;";
    acc.windows(needle.len())
        .enumerate()
        .any(|(i, w)| w == needle && matches!(acc.get(i + needle.len()), Some(b'1') | Some(b'2')))
}

/// Kitty keyboard query reply: `ESC [ ? <flags> u`.
fn find_kitty_reply(acc: &[u8]) -> bool {
    let mut i = 0;
    while let Some(pos) = acc[i..].windows(2).position(|w| w == b"[?") {
        let start = i + pos + 2;
        let digits: usize = acc[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits > 0 && acc.get(start + digits) == Some(&b'u') {
            return true;
        }
        i = start;
    }
    false
}

fn set_stdin_nonblocking(nonblocking: bool) {
    let fd = std::io::stdin().as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        let flags = if nonblocking {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        libc::fcntl(fd, libc::F_SETFL, flags);
    }
}

/// Kitty CSI-u encodes control keys as their codepoints; termwiz surfaces
/// those as Char('\x1b') etc. Normalize so consumers always see the named
/// KeyCode regardless of host protocol.
fn normalize_key(mut ev: InputEvent) -> InputEvent {
    if let InputEvent::Key(k) = &mut ev {
        k.key = match k.key {
            KeyCode::Char('\u{1b}') => KeyCode::Escape,
            KeyCode::Char('\r') | KeyCode::Char('\n') => KeyCode::Enter,
            KeyCode::Char('\t') => KeyCode::Tab,
            KeyCode::Char('\u{7f}') => KeyCode::Backspace,
            other => other,
        };
    }
    ev
}

/// Spawn the blocking stdin reader thread. Parsed events land on the returned
/// channel; the thread exits when stdin closes or the receiver is dropped.
pub fn spawn_input_reader() -> mpsc::Receiver<InputEvent> {
    let (tx, rx) = mpsc::channel::<InputEvent>(64);
    std::thread::Builder::new()
        .name("dmux-input".into())
        .spawn(move || {
            let mut parser = InputParser::new();
            let mut stdin = std::io::stdin().lock();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut closed = false;
                        parser.parse(
                            &buf[..n],
                            |event| {
                                if tx.blocking_send(normalize_key(event)).is_err() {
                                    closed = true;
                                }
                            },
                            n == buf.len(),
                        );
                        if closed {
                            break;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })
        .expect("spawn input thread");
    rx
}

/// Async stream of terminal resize signals (SIGWINCH), coalesced.
pub fn spawn_resize_watcher() -> mpsc::Receiver<(u16, u16)> {
    let (tx, rx) = mpsc::channel::<(u16, u16)>(4);
    tokio::spawn(async move {
        let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        else {
            return;
        };
        while sig.recv().await.is_some() {
            if tx.try_send(term_size()).is_err() && tx.is_closed() {
                break;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decrpm_detection() {
        assert!(find_decrpm_2026(b"\x1b[?2026;1$y\x1b[?1;2c"));
        assert!(find_decrpm_2026(b"\x1b[?2026;2$y"));
        assert!(!find_decrpm_2026(b"\x1b[?2026;0$y"));
        assert!(!find_decrpm_2026(b"\x1b[?1;2c"));
    }
}
