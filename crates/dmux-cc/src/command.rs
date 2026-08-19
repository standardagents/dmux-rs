/// Quote a single argument for a tmux command line.
///
/// tmux's command parser understands single quotes (no escapes inside except
/// none — a literal `'` cannot appear), double quotes (with backslash
/// escapes), and backslash escaping. Strategy: pass bare when safe, otherwise
/// single-quote, breaking out embedded single quotes as `'\''`.
/// Control-mode commands are one per line: any embedded newline (or CR)
/// splits the command and desyncs the reply stream — tmux sees a truncated
/// command plus a garbage line, and the client's reply bookkeeping never
/// recovers (#18: multi-line drag selections passed through set-buffer took
/// the whole session down). Callers must never write such a command.
pub fn command_is_line_safe(cmd: &str) -> bool {
    !cmd.contains('\n') && !cmd.contains('\r')
}

pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'%' | b'=' | b','
                )
        })
    {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_when_safe() {
        assert_eq!(quote_arg("%12"), "%12");
        assert_eq!(quote_arg("list-panes"), "list-panes");
        assert_eq!(quote_arg("-F"), "-F");
    }

    #[test]
    fn quoted_with_spaces() {
        assert_eq!(quote_arg("hello world"), "'hello world'");
    }

    #[test]
    fn embedded_single_quote() {
        assert_eq!(quote_arg("it's"), "'it'\\''s'");
    }

    #[test]
    fn format_strings() {
        assert_eq!(
            quote_arg("#{pane_id}|#{pane_title}"),
            "'#{pane_id}|#{pane_title}'"
        );
    }

    #[test]
    fn empty_arg() {
        assert_eq!(quote_arg(""), "''");
    }
}

#[cfg(test)]
mod line_safety_tests {
    use super::*;

    #[test]
    fn multiline_selection_is_not_line_safe() {
        // #18: the exact crash shape — drag selections span lines, and the
        // quoted command still contains the raw newline.
        let cmd = format!("set-buffer -b dmux {}", quote_arg("line one\nline two"));
        assert!(!command_is_line_safe(&cmd));
        assert!(!command_is_line_safe("a\rb"));
        assert!(command_is_line_safe(
            "set-buffer -b dmux 'plain text with spaces'"
        ));
    }
}
