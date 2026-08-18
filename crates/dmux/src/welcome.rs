//! The welcome screen: shown in the content area whenever no panes are
//! visible. Not decorative like the TS welcome pane — a launcher: a card grid
//! of the things you'd want to do next, all clickable and keyboard-navigable.

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::{ClickMap, Theme};

use crate::views::{AppCmd, ClickTarget};

/// Digital-rain background: sparse columns of falling glyphs, drawn beneath
/// the welcome content. Deliberately subtle — dim greys with an accent head,
/// low density, gentle speeds.
pub struct MatrixRain {
    drops: Vec<Drop>,
    cols: u16,
    rows: u16,
    rng: u32,
}

struct Drop {
    col: u16,
    /// Head row in 1/16ths (fixed point) so speeds below one row/tick work.
    head_fp: i32,
    speed_fp: i32,
    len: u16,
    seed: u32,
}

/// Half-width katakana + digits + sparse punctuation, all display width 1.
const RAIN_GLYPHS: &[char] = &[
    'ｱ', 'ｲ', 'ｳ', 'ｴ', 'ｵ', 'ｶ', 'ｷ', 'ｸ', 'ｹ', 'ｺ', 'ｻ', 'ｼ', 'ｽ', 'ｾ', 'ｿ', 'ﾀ', 'ﾁ', 'ﾂ',
    'ﾃ', 'ﾄ', 'ﾅ', 'ﾆ', 'ﾇ', 'ﾈ', 'ﾉ', 'ﾊ', 'ﾋ', 'ﾌ', 'ﾍ', 'ﾎ', 'ﾏ', 'ﾐ', 'ﾑ', 'ﾒ', 'ﾓ', 'ﾔ',
    '0', '1', '2', '3', '4', '5', '7', '8', '9', '+', '*', '=', '<', '>', ':', '·', '¦', 'ﾘ', 'ﾚ',
];

fn xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x.max(1);
    x
}

impl MatrixRain {
    pub fn new(cols: u16, rows: u16) -> Self {
        let mut rain = Self {
            drops: Vec::new(),
            cols: cols.max(1),
            rows: rows.max(1),
            rng: 0x9e37_79b9 ^ (cols as u32) << 8 ^ rows as u32,
        };
        // Density: roughly one drop per 5 columns keeps it airy.
        let count = (cols / 5).max(4) as usize;
        for _ in 0..count {
            let drop = rain.spawn(true);
            rain.drops.push(drop);
        }
        rain
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (self.cols, self.rows) != (cols.max(1), rows.max(1)) {
            *self = Self::new(cols, rows);
        }
    }

    fn spawn(&mut self, scatter: bool) -> Drop {
        let col = (xorshift(&mut self.rng) % self.cols as u32) as u16;
        // 0.25 .. 1.0 rows per tick, in 16ths.
        let speed_fp = 4 + (xorshift(&mut self.rng) % 13) as i32;
        let len = 4 + (xorshift(&mut self.rng) % 11) as u16;
        // Start above the top so drops enter gradually; on first fill,
        // scatter through the whole area so it doesn't start empty.
        let head_fp = if scatter {
            (xorshift(&mut self.rng) % (self.rows as u32 * 16)) as i32
        } else {
            -((xorshift(&mut self.rng) % (self.rows as u32 * 8)) as i32)
        };
        Drop { col, head_fp, speed_fp, len, seed: xorshift(&mut self.rng) }
    }

    pub fn step(&mut self) {
        let rows = self.rows;
        for i in 0..self.drops.len() {
            self.drops[i].head_fp += self.drops[i].speed_fp;
            let tail_row = self.drops[i].head_fp / 16 - self.drops[i].len as i32;
            if tail_row > rows as i32 {
                self.drops[i] = self.spawn(false);
            }
        }
    }

    /// Paint into `area`. Draw FIRST; content paints over it.
    pub fn draw(&self, buf: &mut CellBuffer, area: Rect, theme: &Theme, tick: u64) {
        for drop in &self.drops {
            let head_row = drop.head_fp / 16;
            for i in 0..drop.len {
                let row = head_row - i as i32;
                if row < 0 || row >= area.h as i32 {
                    continue;
                }
                let col = area.x + drop.col;
                if col >= area.right() {
                    continue;
                }
                // Glyph flickers occasionally, keyed by cell + slow tick.
                let mut h = drop
                    .seed
                    .wrapping_add(row as u32)
                    .wrapping_mul(0x8000_71fd)
                    .wrapping_add((tick as u32 / 6).wrapping_mul(if i == 0 { 3 } else { 1 }));
                let glyph = RAIN_GLYPHS[(xorshift(&mut h) % RAIN_GLYPHS.len() as u32) as usize];
                // Head glows accent; the trail fades through dim greys.
                let fg = if i == 0 {
                    theme.accent_soft
                } else if i <= 2 {
                    Color::Indexed(242)
                } else if i * 3 >= drop.len * 2 {
                    Color::Indexed(235)
                } else {
                    Color::Indexed(238)
                };
                buf.set(
                    col,
                    area.y + row as u16,
                    Cell { ch: glyph, fg, bg: Color::Default, ..Cell::default() },
                );
            }
        }
    }
}

pub struct WelcomeCard {
    pub icon: &'static str,
    pub title: String,
    pub subtitle: String,
    pub cmd: AppCmd,
}

/// The dmux letterforms (same bitmap as the TS welcome pane / brand SVG).
/// Rendered double-height with horizontal scanline banding in brand orange —
/// the terminal translation of the slatted wordmark in dmux.svg.
const WORDMARK_BITMAP: &[&str] = &[
    "    ███                                     ",
    "    ███                                     ",
    "███████  █████████████   ███  ███  ███  ███",
    "██   ██  ███  ███  ███   ███  ███   ██████ ",
    "██   ██  ███  ███  ███   ███  ███    ████  ",
    "██   ██  ███  ███  ███   ███  ███   ██████ ",
    "███████  ███  ███  ███   ████████  ███  ███",
];

/// Brand orange gradient, light at the top → #ea6400 at the base.
const BRAND_RAMP: &[(u8, u8, u8)] = &[
    (255, 158, 80),
    (252, 143, 60),
    (248, 128, 40),
    (244, 114, 20),
    (240, 104, 8),
    (236, 100, 2),
    (234, 100, 0),
];

/// Draw the wordmark at (x, y). Each bitmap row becomes two screen rows; the
/// second row of upper pairs is a half-block, cutting horizontal slits that
/// tighten toward the baseline — the brand's scanline slats.
fn draw_wordmark(buf: &mut CellBuffer, x: u16, y: u16, clip: Rect, doubled: bool) {
    let subs: u16 = if doubled { 2 } else { 1 };
    for (bi, line) in WORDMARK_BITMAP.iter().enumerate() {
        let (r, g, b) = BRAND_RAMP[bi.min(BRAND_RAMP.len() - 1)];
        let color = Color::Rgb(r, g, b);
        for sub in 0..subs {
            let row = y + bi as u16 * subs + sub;
            // Slit rows: the second half of each pair in the upper 2/3 of the
            // mark renders as upper-half blocks, leaving a horizontal gap.
            let slit = doubled && sub == 1 && bi < WORDMARK_BITMAP.len() * 2 / 3;
            let mut cx = x;
            for ch in line.chars() {
                if ch != ' ' {
                    let glyph = if slit { '▀' } else { '█' };
                    buf.set(
                        cx,
                        row,
                        Cell { ch: glyph, fg: color, bg: Color::Default, ..Cell::default() },
                    );
                } else if clip.contains(cx, row) {
                    buf.set(cx, row, Cell::default());
                }
                cx += 1;
            }
        }
    }
}

pub fn build_cards(
    installed: &std::collections::HashSet<&'static str>,
    project_name: &str,
    worktrees: &[(String, String)],
) -> Vec<WelcomeCard> {
    let mut cards = vec![
        WelcomeCard {
            icon: "✦",
            title: "New agents".into(),
            subtitle: format!("run a prompt across {} installed agents", installed.len()),
            cmd: AppCmd::OpenNewAgent,
        },
        WelcomeCard {
            icon: "❯",
            title: "New terminal".into(),
            subtitle: format!("shell in {project_name}"),
            cmd: AppCmd::NewTerminal,
        },
    ];
    for (slug, path) in worktrees.iter().take(4) {
        cards.push(WelcomeCard {
            icon: "⎇",
            title: slug.clone(),
            subtitle: "reopen worktree in a terminal".into(),
            cmd: AppCmd::NewTerminalAt { path: path.clone(), name: slug.clone() },
        });
    }
    cards.push(WelcomeCard {
        icon: "⚙",
        title: "Settings".into(),
        subtitle: "agents · theme · layout · permissions".into(),
        cmd: AppCmd::OpenSettings,
    });
    cards.push(WelcomeCard {
        icon: "⌨",
        title: "Shortcuts".into(),
        subtitle: "leader key ^b · mouse everywhere".into(),
        cmd: AppCmd::OpenShortcuts,
    });
    cards
}

pub struct WelcomeScene<'a> {
    pub cards: &'a [WelcomeCard],
    pub selected: usize,
    pub session_name: &'a str,
    pub project_root: &'a str,
    pub installed: &'a std::collections::HashSet<&'static str>,
}

pub fn draw(
    buf: &mut CellBuffer,
    content: Rect,
    theme: &Theme,
    scene: &WelcomeScene<'_>,
    clicks: &mut ClickMap<ClickTarget>,
) {
    if content.w < 48 || content.h < 16 {
        return;
    }

    // Vertical layout: wordmark + tagline, card grid, agent strip, footer.
    let cards_rows = scene.cards.len().div_ceil(2) as u16 * 4;
    let wm_bitmap_rows = WORDMARK_BITMAP.len() as u16;
    // Double-height brand mark when there's room; single height on short hosts.
    let doubled = content.h >= wm_bitmap_rows * 2 + 3 + cards_rows + 6;
    let wm_rows = if doubled { wm_bitmap_rows * 2 } else { wm_bitmap_rows };
    let total_h = wm_rows + 3 + cards_rows + 4;
    let top = content.y + (content.h.saturating_sub(total_h)) / 2;

    // Wordmark centered, with a clearance zone so the rain never crowds the
    // wordmark or tagline.
    let wm_w = WORDMARK_BITMAP.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let wm_x = content.x + (content.w.saturating_sub(wm_w)) / 2;
    let clearance = Rect::new(
        wm_x.saturating_sub(4),
        top.saturating_sub(1),
        wm_w + 8,
        wm_rows + 4,
    )
    .intersect(&content);
    buf.fill(clearance, &Cell::default());
    draw_wordmark(buf, wm_x, top, content, doubled);

    let tagline = "The Agent Multiplexer";
    let tag_x = content.x + (content.w.saturating_sub(tagline.chars().count() as u16)) / 2;
    buf.draw_text(tag_x, top + wm_rows + 1, tagline, theme.text_dim, Color::Default, AttrFlags::ITALIC, content);

    // Card grid, two columns.
    let grid_top = top + wm_rows + 3;
    let grid_w = content.w.min(96).saturating_sub(4);
    let grid_x = content.x + (content.w.saturating_sub(grid_w)) / 2;
    let card_w = grid_w / 2 - 1;
    for (i, card) in scene.cards.iter().enumerate() {
        let col = (i % 2) as u16;
        let row = (i / 2) as u16;
        let rect = Rect::new(grid_x + col * (card_w + 2), grid_top + row * 4, card_w, 3);
        draw_card(buf, rect, theme, card, i == scene.selected, content);
        clicks.add(rect, ClickTarget::WelcomeCard(i));
    }

    // Agent availability strip.
    let strip_y = grid_top + cards_rows + 1;
    let mut strip = String::new();
    for def in crate::agents::AGENTS.iter().filter(|d| d.default_enabled || scene.installed.contains(d.id)) {
        strip.push_str(&format!(
            "{} {}   ",
            if scene.installed.contains(def.id) { "●" } else { "○" },
            def.short
        ));
    }
    let strip = strip.trim_end();
    let strip_x = content.x + (content.w.saturating_sub(strip.chars().count() as u16)) / 2;
    // Draw dots colored by availability.
    let mut x = strip_x;
    for def in crate::agents::AGENTS.iter().filter(|d| d.default_enabled || scene.installed.contains(d.id)) {
        let ok = scene.installed.contains(def.id);
        let dot_color = if ok { theme.ok } else { theme.text_faint };
        x = buf.draw_text(x, strip_y, if ok { "●" } else { "○" }, dot_color, Color::Default, AttrFlags::empty(), content);
        x = buf.draw_text(x, strip_y, &format!(" {}   ", def.short), if ok { theme.text_dim } else { theme.text_faint }, Color::Default, AttrFlags::empty(), content);
    }

    // Footer status line, codex-style.
    let footer_y = (strip_y + 2).min(content.bottom().saturating_sub(1));
    let left = format!("● session ready — {}", scene.session_name);
    let lx = content.x + 2;
    let mut fx = buf.draw_text(lx, footer_y, "●", theme.ok, Color::Default, AttrFlags::empty(), content);
    fx = buf.draw_text(fx, footer_y, &format!(" session ready — {}", scene.session_name), theme.text_faint, Color::Default, AttrFlags::empty(), content);
    let _ = fx;
    let right = scene.project_root;
    let rx = content
        .right()
        .saturating_sub(right.chars().count() as u16 + 2)
        .max(lx + left.chars().count() as u16 + 3);
    buf.draw_text(rx, footer_y, right, theme.text_faint, Color::Default, AttrFlags::empty(), content);
}

fn draw_card(
    buf: &mut CellBuffer,
    rect: Rect,
    theme: &Theme,
    card: &WelcomeCard,
    selected: bool,
    clip: Rect,
) {
    let rect = rect.intersect(&clip);
    if rect.is_empty() {
        return;
    }
    let bg = if selected { theme.bg_selected } else { theme.bg_raised };
    buf.fill(rect, &Cell { bg, ..Cell::default() });
    // Accent bar on the left edge, brighter when selected.
    let bar_color = if selected { theme.accent } else { theme.border };
    for dy in 0..rect.h {
        buf.set(rect.x, rect.y + dy, Cell { ch: '▎', fg: bar_color, bg, ..Cell::default() });
    }
    let icon_fg = if selected { theme.accent } else { theme.text_dim };
    buf.draw_text(rect.x + 2, rect.y, card.icon, icon_fg, bg, AttrFlags::BOLD, rect);
    let title_fg = if selected { theme.text } else { theme.text_dim };
    buf.draw_text(rect.x + 4, rect.y, &card.title, title_fg, bg, AttrFlags::BOLD, rect);
    if selected {
        let hint = "⏎";
        buf.draw_text(rect.right().saturating_sub(2), rect.y, hint, theme.accent, bg, AttrFlags::BOLD, rect);
    }
    buf.draw_text(rect.x + 4, rect.y + 1, &card.subtitle, theme.text_faint, bg, AttrFlags::empty(), rect);
}
