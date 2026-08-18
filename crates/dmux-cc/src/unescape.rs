/// Unescape the data portion of `%output` / `%extended-output` lines.
///
/// tmux octal-escapes control bytes, backslash, and (version-dependent)
/// invalid-UTF-8 / high-bit bytes as `\ooo` (exactly three octal digits).
/// The payload must be treated as raw bytes until unescaped — escaped UTF-8
/// continuation bytes are common. A backslash not followed by three octal
/// digits is passed through literally (defensive: tmux should never emit one).
pub fn unescape_output(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let b = data[i];
        if b == b'\\' && i + 3 < data.len() {
            let (d1, d2, d3) = (data[i + 1], data[i + 2], data[i + 3]);
            if d1.is_ascii_digit() && d1 < b'8' && d2.is_ascii_digit() && d2 < b'8' && d3.is_ascii_digit() && d3 < b'8'
            {
                let val = ((d1 - b'0') as u16) * 64 + ((d2 - b'0') as u16) * 8 + (d3 - b'0') as u16;
                out.push(val as u8);
                i += 4;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_passthrough() {
        assert_eq!(unescape_output(b"hello world"), b"hello world");
    }

    #[test]
    fn octal_control_bytes() {
        assert_eq!(unescape_output(b"a\\033[1mb"), b"a\x1b[1mb");
        assert_eq!(unescape_output(b"\\015\\012"), b"\r\n");
    }

    #[test]
    fn escaped_backslash() {
        assert_eq!(unescape_output(b"c:\\134temp"), b"c:\\temp");
    }

    #[test]
    fn escaped_utf8_bytes_reassemble() {
        // "é" = 0xC3 0xA9, escaped as \303\251
        assert_eq!(unescape_output(b"\\303\\251"), "é".as_bytes());
    }

    #[test]
    fn lone_backslash_passthrough() {
        assert_eq!(unescape_output(b"a\\"), b"a\\");
        assert_eq!(unescape_output(b"a\\9x"), b"a\\9x");
    }

    #[test]
    fn max_octal_value() {
        assert_eq!(unescape_output(b"\\377"), &[0xffu8][..]);
    }
}
