use crate::event::{CcEvent, PaneId, SessionId, WindowId};
use crate::unescape::unescape_output;

/// Sans-io control-mode parser: feed raw bytes from the tmux stdout pipe, get
/// typed events. Owns a partial-line carry buffer and the in-reply state
/// needed to distinguish reply payload from notifications.
#[derive(Debug, Default)]
pub struct Parser {
    line: Vec<u8>,
    /// `Some(num)` while inside a `%begin`..`%end`/`%error` block.
    in_reply: Option<u64>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while inside a reply block (used by desync diagnostics).
    pub fn in_reply(&self) -> bool {
        self.in_reply.is_some()
    }

    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<CcEvent>) {
        for &b in bytes {
            if b == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(ev) = self.parse_line(&line) {
                    out.push(ev);
                }
            } else {
                self.line.push(b);
            }
        }
    }

    fn parse_line(&mut self, line: &[u8]) -> Option<CcEvent> {
        if let Some(num) = self.in_reply {
            // Inside a reply block every line is payload EXCEPT the terminator.
            // Payload lines may legitimately begin with '%' (e.g. capture-pane
            // of a shell prompt), so the terminator must be verified against
            // the block's command number before we accept it.
            if line.starts_with(b"%end ") || line.starts_with(b"%error ") {
                if let Some(ev) = parse_reply_end(line) {
                    if let CcEvent::ReplyEnd { num: n, .. } = ev {
                        if n == num {
                            self.in_reply = None;
                            return Some(ev);
                        }
                    }
                }
            }
            return Some(CcEvent::ReplyLine(line.to_vec()));
        }

        if !line.starts_with(b"%") {
            // Outside reply blocks tmux only emits notifications; anything else
            // is noise (or protocol drift) — surface it, don't drop it.
            if line.is_empty() {
                return None;
            }
            return Some(CcEvent::Unknown(String::from_utf8_lossy(line).into_owned()));
        }

        // %output / %extended-output payloads are raw pty bytes: tmux (3.4+)
        // passes valid UTF-8 through unescaped, and a pty read boundary can
        // split a multi-byte character across two %output lines. Such a line
        // is not valid UTF-8 on its own, so it must be parsed at the byte
        // level — a lossy decode would stamp U+FFFD into the pane stream.
        if let Some(rest) = line.strip_prefix(b"%output ") {
            let (pane_tok, data) = split_first_space(rest);
            let pane = parse_pane(std::str::from_utf8(pane_tok).ok()?)?;
            return Some(CcEvent::Output {
                pane,
                data: unescape_output(data),
            });
        }
        if let Some(rest) = line.strip_prefix(b"%extended-output ") {
            // %extended-output %<pane> <age> [flags...] : <data>
            let (pane_tok, tail) = split_first_space(rest);
            let pane = parse_pane(std::str::from_utf8(pane_tok).ok()?)?;
            let (meta, data) = match find_subslice(tail, b" : ") {
                Some(i) => (&tail[..i], &tail[i + 3..]),
                None => (tail.strip_suffix(b" :").unwrap_or(tail), &[][..]),
            };
            let age_ms = std::str::from_utf8(meta)
                .ok()
                .and_then(|m| m.split(' ').next())
                .and_then(|t| t.parse().ok())
                .unwrap_or(0);
            return Some(CcEvent::ExtendedOutput {
                pane,
                age_ms,
                data: unescape_output(data),
            });
        }

        let text = String::from_utf8_lossy(line);
        let mut parts = text.splitn(2, ' ');
        let verb = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");

        match verb {
            "%begin" => {
                let (time, num, flags) = parse_triple(rest)?;
                self.in_reply = Some(num);
                Some(CcEvent::ReplyBegin { time, num, flags })
            }
            // %end/%error outside a block is a hard protocol violation; surface
            // as Unknown so the client layer can treat it as desync.
            "%end" | "%error" => Some(CcEvent::Unknown(text.into_owned())),
            // Bare "%output"/"%extended-output" with no payload token: same
            // as before the byte-level fast path — malformed, dropped.
            "%output" | "%extended-output" => None,
            "%pause" => Some(CcEvent::Pause(parse_pane(rest.trim())?)),
            "%continue" => Some(CcEvent::Continue(parse_pane(rest.trim())?)),
            "%window-add" => Some(CcEvent::WindowAdd(parse_window(rest.trim())?)),
            "%window-close" => Some(CcEvent::WindowClose(parse_window(rest.trim())?)),
            "%unlinked-window-close" => {
                Some(CcEvent::UnlinkedWindowClose(parse_window(rest.trim())?))
            }
            "%window-renamed" => {
                let mut it = rest.splitn(2, ' ');
                let window = parse_window(it.next()?)?;
                Some(CcEvent::WindowRenamed {
                    window,
                    name: it.next().unwrap_or("").to_string(),
                })
            }
            "%unlinked-window-renamed" => {
                let mut it = rest.splitn(2, ' ');
                let window = parse_window(it.next()?)?;
                Some(CcEvent::UnlinkedWindowRenamed {
                    window,
                    name: it.next().unwrap_or("").to_string(),
                })
            }
            "%window-pane-changed" => {
                let mut it = rest.split(' ');
                let window = parse_window(it.next()?)?;
                let pane = parse_pane(it.next()?)?;
                Some(CcEvent::WindowPaneChanged { window, pane })
            }
            "%layout-change" => {
                // %layout-change @<win> <layout> [<visible-layout> [<flags>]]
                let mut it = rest.split(' ');
                let window = parse_window(it.next()?)?;
                let layout = it.next().unwrap_or("").to_string();
                let visible_layout = it.next().map(|s| s.to_string());
                let raw_flags = it.next().map(|s| s.to_string());
                Some(CcEvent::LayoutChange {
                    window,
                    layout,
                    visible_layout,
                    raw_flags,
                })
            }
            "%session-changed" => {
                let mut it = rest.splitn(2, ' ');
                let session = parse_session(it.next()?)?;
                Some(CcEvent::SessionChanged {
                    session,
                    name: it.next().unwrap_or("").to_string(),
                })
            }
            "%session-renamed" => Some(CcEvent::SessionRenamed {
                name: rest.to_string(),
            }),
            "%sessions-changed" => Some(CcEvent::SessionsChanged),
            "%session-window-changed" => {
                let mut it = rest.split(' ');
                let session = parse_session(it.next()?)?;
                let window = parse_window(it.next()?)?;
                Some(CcEvent::SessionWindowChanged { session, window })
            }
            "%client-session-changed" => {
                let mut it = rest.splitn(3, ' ');
                let client = it.next().unwrap_or("").to_string();
                let session = parse_session(it.next()?)?;
                Some(CcEvent::ClientSessionChanged {
                    client,
                    session,
                    name: it.next().unwrap_or("").to_string(),
                })
            }
            "%client-detached" => Some(CcEvent::ClientDetached {
                client: rest.trim().to_string(),
            }),
            "%pane-mode-changed" => Some(CcEvent::PaneModeChanged(parse_pane(rest.trim())?)),
            "%paste-buffer-changed" => Some(CcEvent::PasteBufferChanged {
                name: rest.to_string(),
            }),
            "%paste-buffer-deleted" => Some(CcEvent::PasteBufferDeleted {
                name: rest.to_string(),
            }),
            "%subscription-changed" => Some(CcEvent::SubscriptionChanged {
                raw: rest.to_string(),
            }),
            "%config-error" => Some(CcEvent::ConfigError(rest.to_string())),
            "%message" => Some(CcEvent::Message(rest.to_string())),
            "%exit" => {
                let reason = rest.trim();
                Some(CcEvent::Exit(if reason.is_empty() {
                    None
                } else {
                    Some(reason.to_string())
                }))
            }
            _ => Some(CcEvent::Unknown(text.into_owned())),
        }
    }
}

fn split_first_space(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&b| b == b' ') {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &[][..]),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_reply_end(line: &[u8]) -> Option<CcEvent> {
    let text = std::str::from_utf8(line).ok()?;
    let (verb, rest) = text.split_once(' ')?;
    let ok = match verb {
        "%end" => true,
        "%error" => false,
        _ => return None,
    };
    let (time, num, flags) = parse_triple(rest)?;
    Some(CcEvent::ReplyEnd {
        time,
        num,
        flags,
        ok,
    })
}

fn parse_triple(rest: &str) -> Option<(u64, u64, u64)> {
    let mut it = rest.split(' ');
    let time = it.next()?.parse().ok()?;
    let num = it.next()?.parse().ok()?;
    let flags = it.next().and_then(|f| f.parse().ok()).unwrap_or(0);
    Some((time, num, flags))
}

fn parse_pane(tok: &str) -> Option<PaneId> {
    tok.strip_prefix('%')?.parse().ok().map(PaneId)
}

fn parse_window(tok: &str) -> Option<WindowId> {
    tok.strip_prefix('@')?.parse().ok().map(WindowId)
}

fn parse_session(tok: &str) -> Option<SessionId> {
    tok.strip_prefix('$')?.parse().ok().map(SessionId)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(input: &[u8]) -> Vec<CcEvent> {
        let mut p = Parser::new();
        let mut out = Vec::new();
        p.feed(input, &mut out);
        out
    }

    #[test]
    fn hello_block_and_notification() {
        let events = feed_all(
            b"%begin 1618000000 0 0\r\n%end 1618000000 0 0\r\n%session-changed $3 dmux-proj\r\n",
        );
        assert_eq!(
            events,
            vec![
                CcEvent::ReplyBegin {
                    time: 1618000000,
                    num: 0,
                    flags: 0
                },
                CcEvent::ReplyEnd {
                    time: 1618000000,
                    num: 0,
                    flags: 0,
                    ok: true
                },
                CcEvent::SessionChanged {
                    session: SessionId(3),
                    name: "dmux-proj".into()
                },
            ]
        );
    }

    #[test]
    fn output_is_unescaped() {
        let events = feed_all(b"%output %7 hi\\033[1m there\\015\\012\n");
        assert_eq!(
            events,
            vec![CcEvent::Output {
                pane: PaneId(7),
                data: b"hi\x1b[1m there\r\n".to_vec()
            }]
        );
    }

    #[test]
    fn output_split_utf8_stays_raw() {
        // tmux 3.4+ passes valid UTF-8 through %output unescaped, and a pty
        // read boundary can split a multi-byte char across two lines (here
        // '✻' = e2 9c bb). The partial bytes must reach the emulator
        // untouched — a lossy decode manufactures U+FFFD cells the real
        // grid never had (issue #1).
        let events = feed_all(b"%output %9 x\xe2\x9c\n%output %9 \xbby\n");
        assert_eq!(
            events,
            vec![
                CcEvent::Output {
                    pane: PaneId(9),
                    data: b"x\xe2\x9c".to_vec()
                },
                CcEvent::Output {
                    pane: PaneId(9),
                    data: b"\xbby".to_vec()
                },
            ]
        );
    }

    #[test]
    fn extended_output_split_utf8_stays_raw() {
        let events = feed_all(b"%extended-output %5 250 : a\xe2\x9c\n");
        assert_eq!(
            events,
            vec![CcEvent::ExtendedOutput {
                pane: PaneId(5),
                age_ms: 250,
                data: b"a\xe2\x9c".to_vec()
            }]
        );
    }

    #[test]
    fn reply_payload_lines_are_raw_even_with_percent() {
        let input = b"%begin 100 5 1\n%output %1 fake\nreal line\n%end 99 4 0\n%end 100 5 1\n%output %2 x\n";
        let events = feed_all(input);
        assert_eq!(
            events,
            vec![
                CcEvent::ReplyBegin {
                    time: 100,
                    num: 5,
                    flags: 1
                },
                CcEvent::ReplyLine(b"%output %1 fake".to_vec()),
                CcEvent::ReplyLine(b"real line".to_vec()),
                // %end with wrong num is payload, not a terminator
                CcEvent::ReplyLine(b"%end 99 4 0".to_vec()),
                CcEvent::ReplyEnd {
                    time: 100,
                    num: 5,
                    flags: 1,
                    ok: true
                },
                CcEvent::Output {
                    pane: PaneId(2),
                    data: b"x".to_vec()
                },
            ]
        );
    }

    #[test]
    fn error_reply_terminates_block() {
        let events = feed_all(b"%begin 100 9 1\nbad command\n%error 100 9 1\n");
        assert_eq!(
            events[2],
            CcEvent::ReplyEnd {
                time: 100,
                num: 9,
                flags: 1,
                ok: false
            }
        );
    }

    #[test]
    fn split_across_feeds() {
        let mut p = Parser::new();
        let mut out = Vec::new();
        p.feed(b"%output %3 ab", &mut out);
        assert!(out.is_empty());
        p.feed(b"c\\04", &mut out);
        assert!(out.is_empty());
        p.feed(b"1\n", &mut out);
        assert_eq!(
            out,
            vec![CcEvent::Output {
                pane: PaneId(3),
                data: b"abc!".to_vec()
            }]
        );
    }

    #[test]
    fn pause_continue_and_extended_output() {
        let events =
            feed_all(b"%pause %5\n%extended-output %5 250 : data\\040here\n%continue %5\n");
        assert_eq!(
            events,
            vec![
                CcEvent::Pause(PaneId(5)),
                CcEvent::ExtendedOutput {
                    pane: PaneId(5),
                    age_ms: 250,
                    data: b"data here".to_vec()
                },
                CcEvent::Continue(PaneId(5)),
            ]
        );
    }

    #[test]
    fn window_lifecycle() {
        let events =
            feed_all(b"%window-add @12\n%window-renamed @12 my window name\n%window-close @12\n");
        assert_eq!(
            events,
            vec![
                CcEvent::WindowAdd(WindowId(12)),
                CcEvent::WindowRenamed {
                    window: WindowId(12),
                    name: "my window name".into()
                },
                CcEvent::WindowClose(WindowId(12)),
            ]
        );
    }

    #[test]
    fn layout_change_variants() {
        let events = feed_all(b"%layout-change @1 b25d,80x24,0,0,1\n%layout-change @2 abcd,10x5,0,0,2 efgh,10x5,0,0,2 *\n");
        assert_eq!(
            events[0],
            CcEvent::LayoutChange {
                window: WindowId(1),
                layout: "b25d,80x24,0,0,1".into(),
                visible_layout: None,
                raw_flags: None
            }
        );
        assert_eq!(
            events[1],
            CcEvent::LayoutChange {
                window: WindowId(2),
                layout: "abcd,10x5,0,0,2".into(),
                visible_layout: Some("efgh,10x5,0,0,2".into()),
                raw_flags: Some("*".into())
            }
        );
    }

    #[test]
    fn unknown_notification_is_surfaced() {
        let events = feed_all(b"%future-thing 1 2 3\n");
        assert_eq!(events, vec![CcEvent::Unknown("%future-thing 1 2 3".into())]);
    }

    #[test]
    fn exit_with_and_without_reason() {
        assert_eq!(feed_all(b"%exit\n"), vec![CcEvent::Exit(None)]);
        assert_eq!(
            feed_all(b"%exit detached\n"),
            vec![CcEvent::Exit(Some("detached".into()))]
        );
    }
}
