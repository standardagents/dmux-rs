use dmux_compositor::Color;

/// One visual system for every dmux surface. Accent hues come from the dmux
/// theme registry (violet default); everything else is a neutral ramp that
/// works on dark terminals.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Color,
    pub accent_soft: Color,
    /// True-color accent for gradients (wordmark, art) — matches `accent`.
    pub accent_rgb: (u8, u8, u8),
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub bg: Color,
    pub bg_raised: Color,
    pub bg_selected: Color,
    /// Unused content-area background — a hair lighter than the terminal
    /// default so free space reads as canvas, not as part of a pane.
    pub canvas: Color,
    pub border: Color,
    pub danger: Color,
    pub ok: Color,
    pub warn: Color,
}

impl Theme {
    pub fn with_accent(accent: Color, accent_soft: Color, accent_rgb: (u8, u8, u8)) -> Self {
        Self { accent, accent_soft, accent_rgb, ..Self::default() }
    }

    /// Accent palette by dmux theme name (mirrors `src/theme/colors.ts`
    /// accents; full palette port is a later phase). The RGB triple is the
    /// truecolor equivalent of the indexed accent.
    pub fn named(name: &str) -> Self {
        let (accent, soft, rgb) = match name {
            "violet" => (Color::Indexed(135), Color::Indexed(97), (0xaf, 0x5f, 0xff)),
            "cyan" => (Color::Indexed(51), Color::Indexed(30), (0x00, 0xd7, 0xff)),
            "green" => (Color::Indexed(114), Color::Indexed(29), (0x87, 0xd7, 0x87)),
            "amber" => (Color::Indexed(214), Color::Indexed(130), (0xff, 0xaf, 0x00)),
            "rose" => (Color::Indexed(211), Color::Indexed(132), (0xff, 0x87, 0xaf)),
            "blue" => (Color::Indexed(75), Color::Indexed(25), (0x5f, 0xaf, 0xff)),
            "slate" => (Color::Indexed(146), Color::Indexed(60), (0xaf, 0xaf, 0xd7)),
            "ember" => (Color::Indexed(203), Color::Indexed(95), (0xff, 0x5f, 0x5f)),
            _ => (Color::Indexed(135), Color::Indexed(97), (0xaf, 0x5f, 0xff)),
        };
        Self::with_accent(accent, soft, rgb)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Indexed(135),
            accent_soft: Color::Indexed(97),
            accent_rgb: (0xaf, 0x5f, 0xff),
            text: Color::Indexed(253),
            text_dim: Color::Indexed(246),
            text_faint: Color::Indexed(240),
            bg: Color::Indexed(233),
            bg_raised: Color::Indexed(235),
            bg_selected: Color::Indexed(237),
            canvas: Color::Indexed(234),
            border: Color::Indexed(240),
            danger: Color::Indexed(203),
            ok: Color::Indexed(114),
            warn: Color::Indexed(214),
        }
    }
}

/// TS project color themes (`sidebarProjects[].colorTheme` values). Names,
/// default, and auto-assignment order are the TS `themePalette` contract —
/// both implementations must pick identical colors for the same config.
pub const PROJECT_THEME_NAMES: &[&str] =
    &["red", "blue", "yellow", "orange", "green", "purple", "cyan", "magenta"];
pub const DEFAULT_PROJECT_THEME: &str = "orange";

/// (accent, soft) for a project color theme; unknown names get the default.
/// Indexes follow the TS palette's activeBorder / artTail values.
pub fn project_theme(name: &str) -> (Color, Color) {
    match name {
        "red" => (Color::Indexed(203), Color::Indexed(124)),
        "blue" => (Color::Indexed(75), Color::Indexed(27)),
        "yellow" => (Color::Indexed(221), Color::Indexed(178)),
        "green" => (Color::Indexed(77), Color::Indexed(34)),
        "purple" => (Color::Indexed(141), Color::Indexed(93)),
        "cyan" => (Color::Indexed(80), Color::Indexed(31)),
        "magenta" => (Color::Indexed(206), Color::Indexed(127)),
        _ => (Color::Indexed(214), Color::Indexed(130)),
    }
}

/// Auto-assignment order: the default theme first, then the rest (TS
/// `AUTO_SIDEBAR_THEME_ORDER`).
pub fn project_theme_auto_order() -> impl Iterator<Item = &'static str> {
    std::iter::once(DEFAULT_PROJECT_THEME)
        .chain(PROJECT_THEME_NAMES.iter().copied().filter(|n| *n != DEFAULT_PROJECT_THEME))
}
