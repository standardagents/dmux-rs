//! Replay a transcript recorded from a real tmux 3.7b control-mode session
//! (see rust/crates/dmux-cc/tests/fixtures/) through the sans-io parser and
//! assert the structural invariants the client layer depends on.

use dmux_cc::{CcEvent, Parser};

#[test]
fn tmux_37b_attach_transcript() {
    let bytes = include_bytes!("fixtures/tmux37b-attach-session.txt");
    let mut parser = Parser::new();
    let mut events = Vec::new();
    // Feed in awkward chunk sizes to exercise the carry buffer.
    for chunk in bytes.chunks(7) {
        parser.feed(chunk, &mut events);
    }

    // 1. The stream starts with the hello block: %begin immediately followed by %end.
    assert!(matches!(events[0], CcEvent::ReplyBegin { .. }), "first event: {:?}", events[0]);
    assert!(matches!(events[1], CcEvent::ReplyEnd { ok: true, .. }), "second event: {:?}", events[1]);

    // 2. Reply blocks are balanced and FIFO: every %begin has a matching %end/%error.
    let begins = events.iter().filter(|e| matches!(e, CcEvent::ReplyBegin { .. })).count();
    let ends = events.iter().filter(|e| matches!(e, CcEvent::ReplyEnd { .. })).count();
    assert_eq!(begins, ends);
    assert_eq!(begins, 6, "attach hello + 5 commands");

    // 3. The bogus command produced an %error block whose payload is a ReplyLine.
    let error_pos = events
        .iter()
        .position(|e| matches!(e, CcEvent::ReplyEnd { ok: false, .. }))
        .expect("one %error block");
    assert!(
        matches!(&events[error_pos - 1], CcEvent::ReplyLine(l) if l.starts_with(b"parse error")),
        "error payload precedes %error: {:?}",
        events[error_pos - 1]
    );

    // 4. send-keys "hi\n" round-tripped as octal-escaped %output for pane %0.
    let outputs: Vec<&CcEvent> = events.iter().filter(|e| matches!(e, CcEvent::Output { .. })).collect();
    assert!(!outputs.is_empty());
    let all_output: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            CcEvent::Output { data, .. } => Some(data.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        all_output.windows(4).any(|w| w == b"hi\r\n"),
        "expected unescaped echo 'hi\\r\\n' in {:?}",
        String::from_utf8_lossy(&all_output)
    );

    // 5. Layout-change carries visible layout and flags on 3.7.
    assert!(events.iter().any(|e| matches!(
        e,
        CcEvent::LayoutChange { visible_layout: Some(_), raw_flags: Some(_), .. }
    )));

    // 6. The stream ends with %exit and nothing is misparsed as Unknown.
    assert!(matches!(events.last(), Some(CcEvent::Exit(_))));
    let unknown: Vec<&CcEvent> = events.iter().filter(|e| matches!(e, CcEvent::Unknown(_))).collect();
    assert!(unknown.is_empty(), "unexpected Unknown events: {unknown:?}");
}
