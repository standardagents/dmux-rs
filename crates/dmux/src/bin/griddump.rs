//! Fidelity-harness helper: parse captured ANSI text through the same VT
//! stack dmux-rs uses and dump the grid as one normalized cell per token —
//! `ch·fg·bg` — so two renders can be diffed cell-for-cell.
//!
//! Usage: griddump <cols> <rows> [x y w h]  (rect defaults to the full grid)
//! Bytes are read from stdin.

use std::io::Read;

fn color_code(c: dmux_compositor::Color) -> String {
    match c {
        dmux_compositor::Color::Default => "d".into(),
        dmux_compositor::Color::Indexed(i) => format!("i{i}"),
        dmux_compositor::Color::Rgb(r, g, b) => format!("r{r:02x}{g:02x}{b:02x}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cols: u16 = args.first().and_then(|v| v.parse().ok()).unwrap_or(80);
    let rows: u16 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(24);
    let rect = if args.len() >= 6 {
        dmux_compositor::Rect::new(
            args[2].parse().unwrap_or(0),
            args[3].parse().unwrap_or(0),
            args[4].parse().unwrap_or(cols),
            args[5].parse().unwrap_or(rows),
        )
    } else {
        dmux_compositor::Rect::new(0, 0, cols, rows)
    };

    let raw = std::env::args().any(|a| a == "raw");
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes).expect("read stdin");
    let feed = if raw {
        // Raw pty stream (verifier incident replay): feed byte-exact.
        bytes
    } else {
        // The trailing newline after the LAST row would scroll the grid by
        // one (same pitfall finish_reseed avoids with its final CRLF).
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        // Captured text separates rows with \n; the emulator needs CR too.
        let mut feed = Vec::with_capacity(bytes.len());
        for b in bytes {
            if b == b'\n' {
                feed.push(b'\r');
            }
            feed.push(b);
        }
        feed
    };

    let mut term = dmux_vt::PaneTerm::new(cols, rows, 200);
    term.advance(&feed);
    let mut buf = dmux_compositor::CellBuffer::new(cols, rows);
    term.render_into(&mut buf, dmux_compositor::Rect::new(0, 0, cols, rows));

    for row in rect.y..rect.bottom().min(rows) {
        let mut line = String::new();
        for col in rect.x..rect.right().min(cols) {
            let cell = buf.get(col, row);
            let ch = if cell.wide_spacer() { '_' } else { cell.ch };
            // Tab-separated: space CELLS would break a space-separated format.
            line.push_str(&format!("{}·{}·{}\t", ch, color_code(cell.fg), color_code(cell.bg)));
        }
        println!("{}", line.trim_end_matches('\t'));
    }
}
