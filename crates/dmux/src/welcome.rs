//! The welcome screen: shown in the content area whenever no panes are
//! visible. Not decorative like the TS welcome pane — a launcher: a card grid
//! of the things you'd want to do next, all clickable and keyboard-navigable.

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::{ClickMap, Theme};

use crate::views::{AppCmd, ClickTarget};
use dmux_core::i18n::t;

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
        // 0.3 .. 1.4 rows per tick, in 16ths — at ~30fps that's a lively
        // 9–40 rows/second spread.
        let speed_fp = 5 + (xorshift(&mut self.rng) % 18) as i32;
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

/// The dmux letterforms — verbatim from the TS welcome pane art
/// (`src/utils/asciiArt.ts`), which is the terminal rendition of the brand
/// mark. Scaled 2×2 when the host is large enough: chunky pixels ARE the
/// brand; no cleverness.
const WORDMARK_BITMAP: &[&str] = &[
    "     ███                                     ",
    "     ███                                     ",
    " ███████  █████████████   ███  ███  ███  ███ ",
    "███  ███  ███  ███  ████  ███  ███  ███  ███ ",
    "███  ███  ███  ███  ████  ███  ███    █████  ",
    "███  ███  ███  ███  ████  ███  ███  ███  ███ ",
    "████████  ███  ███  ████  ████████  ███  ███ ",
];

/// Gradient ramp derived from the active theme's accent: lightened toward
/// white at the cap, the pure accent at the baseline.
fn theme_ramp(base: (u8, u8, u8), row: usize, rows: usize) -> Color {
    let t = if rows <= 1 { 0.0 } else { row as f32 / (rows - 1) as f32 };
    // Lighten factor: 0.55 at the top row fading to 0 at the baseline.
    let f = 0.55 * (1.0 - t);
    let mix = |c: u8| -> u8 { (c as f32 + (255.0 - c as f32) * f).round().min(255.0) as u8 };
    Color::Rgb(mix(base.0), mix(base.1), mix(base.2))
}

pub(crate) fn wordmark_size(doubled: bool) -> (u16, u16) {
    let w = WORDMARK_BITMAP.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let h = WORDMARK_BITMAP.len() as u16;
    if doubled {
        (w * 2, h * 2)
    } else {
        (w, h)
    }
}

/// Draw the wordmark at (x, y), 2×2 pixel-scaled when `doubled`, colored by
/// a gradient of the active theme's accent per letterform row.
fn draw_wordmark(buf: &mut CellBuffer, x: u16, y: u16, clip: Rect, doubled: bool, theme: &Theme) {
    let scale: u16 = if doubled { 2 } else { 1 };
    let rows = WORDMARK_BITMAP.len();
    for (bi, line) in WORDMARK_BITMAP.iter().enumerate() {
        let color = theme_ramp(theme.accent_rgb, bi, rows);
        for sub in 0..scale {
            let row = y + bi as u16 * scale + sub;
            let mut cx = x;
            for ch in line.chars() {
                for _ in 0..scale {
                    if ch != ' ' {
                        buf.set(
                            cx,
                            row,
                            Cell { ch: '█', fg: color, bg: Color::Default, ..Cell::default() },
                        );
                    } else if clip.contains(cx, row) {
                        buf.set(cx, row, Cell::default());
                    }
                    cx += 1;
                }
            }
        }
    }
}

/// A reopenable worktree: slug, path, and the agent that lived there (drives
/// resume-vs-terminal card behavior).
pub struct WorktreeCard {
    pub slug: String,
    pub path: String,
    pub agent: Option<String>,
}

pub fn build_cards(
    installed: &std::collections::HashSet<&'static str>,
    project_name: &str,
    worktrees: &[WorktreeCard],
) -> Vec<WelcomeCard> {
    let mut cards = vec![
        WelcomeCard {
            icon: "✦",
            title: t("welcome.new_agents").into(),
            subtitle: format!("run a prompt across {} installed agents", installed.len()),
            cmd: AppCmd::OpenNewAgent,
        },
        WelcomeCard {
            icon: "❯",
            title: t("welcome.new_terminal").into(),
            subtitle: format!("shell in {project_name}"),
            cmd: AppCmd::NewTerminal,
        },
    ];
    for wt in worktrees.iter().take(4) {
        match &wt.agent {
            Some(agent) if crate::agents::agent(agent).is_some_and(|d| d.resume_template.is_some() && installed.contains(d.id)) => {
                let short = crate::agents::agent(agent).map(|d| d.short).unwrap_or("??");
                cards.push(WelcomeCard {
                    icon: "⟲",
                    title: wt.slug.clone(),
                    subtitle: format!("resume {short} session in this worktree"),
                    cmd: AppCmd::ResumeWorktree {
                        path: wt.path.clone(),
                        slug: wt.slug.clone(),
                        agent: agent.clone(),
                    },
                });
            }
            _ => cards.push(WelcomeCard {
                icon: "⎇",
                title: wt.slug.clone(),
                subtitle: "reopen worktree in a terminal".into(),
                cmd: AppCmd::NewTerminalAt { path: wt.path.clone(), name: wt.slug.clone() },
            }),
        }
    }
    cards.push(WelcomeCard {
        icon: "⚙",
        title: t("welcome.settings").into(),
        subtitle: "agents · theme · layout · permissions".into(),
        cmd: AppCmd::OpenSettings,
    });
    cards.push(WelcomeCard {
        icon: "⌨",
        title: t("welcome.shortcuts").into(),
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
    let cards_rows = scene.cards.len().div_ceil(2) as u16 * 5;
    let wm_bitmap_rows = WORDMARK_BITMAP.len() as u16;
    // 2×2 brand mark when there's room; single scale on short/narrow hosts.
    let (wm_w2, _) = wordmark_size(true);
    let doubled =
        content.h >= wm_bitmap_rows * 2 + 3 + cards_rows + 6 && content.w >= wm_w2 + 8;
    let wm_rows = if doubled { wm_bitmap_rows * 2 } else { wm_bitmap_rows };
    let total_h = wm_rows + 3 + cards_rows + 4;
    let top = content.y + (content.h.saturating_sub(total_h)) / 2;

    // Wordmark centered, with a clearance zone so the rain never crowds the
    // wordmark or tagline.
    let (wm_w, _) = wordmark_size(doubled);
    let wm_x = content.x + (content.w.saturating_sub(wm_w)) / 2;
    let clearance = Rect::new(
        wm_x.saturating_sub(4),
        top.saturating_sub(1),
        wm_w + 8,
        wm_rows + 4,
    )
    .intersect(&content);
    buf.fill(clearance, &Cell::default());
    draw_wordmark(buf, wm_x, top, content, doubled, theme);

    let tagline = t("welcome.tagline");
    let tag_x = content.x + (content.w.saturating_sub(tagline.chars().count() as u16)) / 2;
    buf.draw_text(tag_x, top + wm_rows + 1, tagline, theme.text_dim, Color::Default, AttrFlags::ITALIC, content);

    // Card grid, two columns of bordered cards.
    let grid_top = top + wm_rows + 3;
    let grid_w = content.w.min(100).saturating_sub(4);
    let grid_x = content.x + (content.w.saturating_sub(grid_w)) / 2;
    let card_w = grid_w / 2 - 1;
    for (i, card) in scene.cards.iter().enumerate() {
        let col = (i % 2) as u16;
        let row = (i / 2) as u16;
        let rect = Rect::new(grid_x + col * (card_w + 2), grid_top + row * 5, card_w, 4);
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

    // Footer status line, codex-style — anchored to the bottom row of the
    // content area so it reads as a persistent footer at any height (#3).
    let footer_y = content.bottom().saturating_sub(1);
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

/// A Codex-style card: rounded border, icon block, bold title, dim subtitle.
fn draw_card(
    buf: &mut CellBuffer,
    rect: Rect,
    theme: &Theme,
    card: &WelcomeCard,
    selected: bool,
    clip: Rect,
) {
    let rect = rect.intersect(&clip);
    if rect.w < 10 || rect.h < 4 {
        return;
    }
    let bg = if selected { theme.bg_selected } else { theme.bg_raised };
    let border = if selected { theme.accent } else { theme.border };
    buf.fill(rect, &Cell { bg, ..Cell::default() });

    let (x0, y0, x1, y1) = (rect.x, rect.y, rect.right() - 1, rect.bottom() - 1);
    let horiz = Cell { ch: '─', fg: border, bg, ..Cell::default() };
    for col in x0 + 1..x1 {
        buf.set(col, y0, horiz.clone());
        buf.set(col, y1, horiz.clone());
    }
    let vert = Cell { ch: '│', fg: border, bg, ..Cell::default() };
    for row in y0 + 1..y1 {
        buf.set(x0, row, vert.clone());
        buf.set(x1, row, vert.clone());
    }
    buf.set(x0, y0, Cell { ch: '╭', fg: border, bg, ..Cell::default() });
    buf.set(x1, y0, Cell { ch: '╮', fg: border, bg, ..Cell::default() });
    buf.set(x0, y1, Cell { ch: '╰', fg: border, bg, ..Cell::default() });
    buf.set(x1, y1, Cell { ch: '╯', fg: border, bg, ..Cell::default() });

    let icon_fg = if selected { theme.accent } else { theme.text_dim };
    buf.draw_text(rect.x + 2, rect.y + 1, card.icon, icon_fg, bg, AttrFlags::BOLD, rect);
    let title_fg = if selected { theme.text } else { theme.text_dim };
    buf.draw_text(rect.x + 5, rect.y + 1, &card.title, title_fg, bg, AttrFlags::BOLD, rect);
    if selected {
        buf.draw_text(rect.right().saturating_sub(3), rect.y + 1, "⏎", theme.accent, bg, AttrFlags::BOLD, rect);
    }
    let max_sub = rect.w.saturating_sub(7) as usize;
    let subtitle: String = if card.subtitle.chars().count() > max_sub {
        let mut s: String = card.subtitle.chars().take(max_sub.saturating_sub(1)).collect();
        s.push('…');
        s
    } else {
        card.subtitle.clone()
    };
    buf.draw_text(rect.x + 5, rect.y + 2, &subtitle, theme.text_faint, bg, AttrFlags::empty(), rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_anchors_to_bottom_row() {
        let theme = Theme::named("dmux");
        let installed = std::collections::HashSet::new();
        let cards = build_cards(&installed, "proj", &[]);
        let scene = WelcomeScene {
            cards: &cards,
            selected: 0,
            session_name: "sess",
            project_root: "/tmp/proj",
            installed: &installed,
        };
        let mut clicks = ClickMap::new();
        // Tall terminal: the footer must sit on the bottom content row, not
        // trail the centered card grid (#3).
        let (w, h) = (120u16, 60u16);
        let mut buf = CellBuffer::new(w, h);
        draw(&mut buf, Rect::new(0, 0, w, h), &theme, &scene, &mut clicks);
        let bottom: String = (0..w).map(|c| buf.get(c, h - 1).ch).collect();
        assert!(
            bottom.contains("session ready"),
            "footer must be on the bottom row, got: {bottom:?}"
        );
        let right_ok = bottom.contains("/tmp/proj");
        assert!(right_ok, "project path shares the bottom row: {bottom:?}");
    }
}
