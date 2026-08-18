use dmux_compositor::Color;

/// One visual system for every dmux surface. Accent hues come from the dmux
/// theme registry (violet default); everything else is a neutral ramp that
/// works on dark terminals.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub accent: Color,
    pub accent_soft: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub bg: Color,
    pub bg_raised: Color,
    pub bg_selected: Color,
    pub border: Color,
    pub danger: Color,
    pub ok: Color,
    pub warn: Color,
}

impl Theme {
    pub fn with_accent(accent: Color, accent_soft: Color) -> Self {
        Self { accent, accent_soft, ..Self::default() }
    }

    /// Accent palette by dmux theme name (mirrors `src/theme/colors.ts`
    /// accents; full palette port is a later phase).
    pub fn named(name: &str) -> Self {
        let (accent, soft) = match name {
            "violet" => (Color::Indexed(135), Color::Indexed(97)),
            "cyan" => (Color::Indexed(51), Color::Indexed(30)),
            "green" => (Color::Indexed(114), Color::Indexed(29)),
            "amber" => (Color::Indexed(214), Color::Indexed(130)),
            "rose" => (Color::Indexed(211), Color::Indexed(132)),
            "blue" => (Color::Indexed(75), Color::Indexed(25)),
            "slate" => (Color::Indexed(146), Color::Indexed(60)),
            "ember" => (Color::Indexed(203), Color::Indexed(95)),
            _ => (Color::Indexed(135), Color::Indexed(97)),
        };
        Self::with_accent(accent, soft)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            accent: Color::Indexed(135),
            accent_soft: Color::Indexed(97),
            text: Color::Indexed(253),
            text_dim: Color::Indexed(246),
            text_faint: Color::Indexed(240),
            bg: Color::Indexed(233),
            bg_raised: Color::Indexed(235),
            bg_selected: Color::Indexed(237),
            border: Color::Indexed(240),
            danger: Color::Indexed(203),
            ok: Color::Indexed(114),
            warn: Color::Indexed(214),
        }
    }
}
