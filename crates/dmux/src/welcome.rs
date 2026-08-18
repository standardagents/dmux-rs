//! The welcome screen: shown in the content area whenever no panes are
//! visible. Not decorative like the TS welcome pane — a launcher: a card grid
//! of the things you'd want to do next, all clickable and keyboard-navigable.

use dmux_compositor::{AttrFlags, Cell, CellBuffer, Color, Rect};
use dmux_ui::{ClickMap, Theme};

use crate::views::{AppCmd, ClickTarget};

pub struct WelcomeCard {
    pub icon: &'static str,
    pub title: String,
    pub subtitle: String,
    pub cmd: AppCmd,
}

/// Block wordmark, drawn with a vertical accent gradient.
const WORDMARK: &[&str] = &[
    "     █                                ",
    "  ▄▄▄█  ▄▄▄▄▄▄▄  ▄    ▄  ▄▄  ▄▄",
    " █   █  █  █  █  █    █   ▀▄▄▀ ",
    " █   █  █  █  █  █    █   ▄▀▀▄ ",
    "  ▀▀▀▀  ▀  ▀  ▀   ▀▀▀▀▀  ▀▀  ▀▀",
];

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
    if content.w < 40 || content.h < 16 {
        return;
    }
    let gradient = [theme.accent, theme.accent, theme.accent_soft, theme.accent_soft, theme.text_faint];

    // Vertical layout: wordmark + tagline, card grid, agent strip, footer.
    let cards_rows = scene.cards.len().div_ceil(2) as u16 * 4;
    let total_h = WORDMARK.len() as u16 + 3 + cards_rows + 4;
    let top = content.y + (content.h.saturating_sub(total_h)) / 2;

    // Wordmark centered.
    let wm_w = WORDMARK.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
    let wm_x = content.x + (content.w.saturating_sub(wm_w)) / 2;
    for (i, line) in WORDMARK.iter().enumerate() {
        let color = gradient[i.min(gradient.len() - 1)];
        buf.draw_text(wm_x, top + i as u16, line, color, Color::Default, AttrFlags::BOLD, content);
    }
    let tagline = "The Agent Multiplexer";
    let tag_x = content.x + (content.w.saturating_sub(tagline.chars().count() as u16)) / 2;
    buf.draw_text(tag_x, top + WORDMARK.len() as u16 + 1, tagline, theme.text_dim, Color::Default, AttrFlags::ITALIC, content);

    // Card grid, two columns.
    let grid_top = top + WORDMARK.len() as u16 + 3;
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
