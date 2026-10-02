use gpui::WindowAppearance;
use serde::{Deserialize, Serialize};

/// The reading view's appearance preference. The source view keeps Neovim's colorscheme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Colors for Rusidian's own UI, as 0xRRGGBB values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub background: u32,
    pub text: u32,
    pub muted: u32,
    pub faint: u32,
    pub border: u32,
    pub border_strong: u32,
    /// Sidebar and status bar.
    pub surface: u32,
    /// Code blocks, formulas and placeholders.
    pub block: u32,
    pub inline_code: u32,
    /// Obsidian `==highlight==`.
    pub highlight: u32,
    pub table_header: u32,
    pub accent: u32,
    pub accent_soft_bg: u32,
    pub accent_soft_text: u32,
    pub source_chip_bg: u32,
    pub source_chip_text: u32,
    pub selection: u32,
    pub cursor_bg: u32,
    pub cursor_fg: u32,
    pub atom_active: u32,
    pub error_bg: u32,
    pub error_text: u32,
    pub warning_bg: u32,
    pub warning_text: u32,
    pub warning_hover: u32,
    pub success_text: u32,
    pub quote_border: u32,
    pub quote_text: u32,
    pub button: u32,
    pub button_hover: u32,
    pub button_text: u32,
    pub disabled_bg: u32,
    pub disabled_text: u32,
    pub overlay: u32,
    pub overlay_opacity: f32,
    pub panel: u32,
    pub card: u32,
    pub card_border: u32,
    pub hover: u32,
    pub toggle_off: u32,
    pub knob: u32,
}

impl Theme {
    pub const DARK: Self = Self {
        background: 0x111418,
        text: 0xe6e9ed,
        muted: 0x98a2ad,
        faint: 0x717983,
        border: 0x2a3038,
        border_strong: 0x3a424d,
        surface: 0x0c0f12,
        block: 0x1c2229,
        inline_code: 0x242a32,
        highlight: 0x5e4c16,
        table_header: 0x1a1f25,
        accent: 0xf28c45,
        accent_soft_bg: 0x452519,
        accent_soft_text: 0xffb07a,
        source_chip_bg: 0x2b3a4a,
        source_chip_text: 0x9ecbff,
        selection: 0x8a3b1c,
        cursor_bg: 0xe6e9ed,
        cursor_fg: 0x111418,
        atom_active: 0x3c2a22,
        error_bg: 0x3a1f24,
        error_text: 0xffa7b2,
        warning_bg: 0x4a3518,
        warning_text: 0xffd38a,
        warning_hover: 0x5c4420,
        success_text: 0xa4e4c7,
        quote_border: 0x56606d,
        quote_text: 0xb8c0cc,
        button: 0xf06a24,
        button_hover: 0xff7a32,
        button_text: 0x1a0d07,
        disabled_bg: 0x34383e,
        disabled_text: 0x8b929a,
        overlay: 0x080a0c,
        overlay_opacity: 0.94,
        panel: 0x15191e,
        card: 0x1b2026,
        card_border: 0x343a41,
        hover: 0x20262d,
        toggle_off: 0x3a4047,
        knob: 0xf4f1e8,
    };

    pub const LIGHT: Self = Self {
        background: 0xfbfaf7,
        text: 0x1f2328,
        muted: 0x5f6873,
        faint: 0x8a9199,
        border: 0xe3e0d8,
        border_strong: 0xcfccc4,
        surface: 0xf3f1ec,
        block: 0xf0eee8,
        inline_code: 0xe9e6df,
        highlight: 0xfbe68a,
        table_header: 0xf3f1ec,
        accent: 0xc4501a,
        accent_soft_bg: 0xfbe3d3,
        accent_soft_text: 0x9a3b10,
        source_chip_bg: 0xdce8f5,
        source_chip_text: 0x1e5a99,
        selection: 0xf5c9a8,
        cursor_bg: 0x1f2328,
        cursor_fg: 0xfbfaf7,
        atom_active: 0xf7dccb,
        error_bg: 0xfde8ea,
        error_text: 0xb42335,
        warning_bg: 0xfdf0d5,
        warning_text: 0x7a4b00,
        warning_hover: 0xf8e2b5,
        success_text: 0x1f7a4d,
        quote_border: 0xc5c9cf,
        quote_text: 0x4f5761,
        button: 0xf06a24,
        button_hover: 0xff7a32,
        button_text: 0x1a0d07,
        disabled_bg: 0xe4e2dd,
        disabled_text: 0x8a9199,
        overlay: 0x1f2328,
        overlay_opacity: 0.35,
        panel: 0xffffff,
        card: 0xf7f6f2,
        card_border: 0xe3e0d8,
        hover: 0xefede8,
        toggle_off: 0xcfccc4,
        knob: 0xffffff,
    };

    pub fn resolve(preference: Appearance, window: WindowAppearance) -> Self {
        let dark = match preference {
            Appearance::System => matches!(
                window,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            Appearance::Light => false,
            Appearance::Dark => true,
        };
        if dark { Self::DARK } else { Self::LIGHT }
    }
}

/// Accent color for an Obsidian or GitHub callout kind, readable on light and dark themes.
pub fn callout_color(kind: &str) -> u32 {
    match kind {
        "abstract" | "summary" | "tldr" | "tip" | "hint" | "important" => 0x1fa8a0,
        "success" | "check" | "done" => 0x3fa95b,
        "question" | "help" | "faq" => 0xd19a1e,
        "warning" | "caution" | "attention" => 0xe0782f,
        "failure" | "fail" | "missing" | "danger" | "error" | "bug" => 0xe0484a,
        "example" => 0x9b6cf0,
        "quote" | "cite" => 0x8b949e,
        _ => 0x3f88e0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_system_unless_forced() {
        assert_eq!(
            Theme::resolve(Appearance::System, WindowAppearance::VibrantDark),
            Theme::DARK
        );
        assert_eq!(
            Theme::resolve(Appearance::System, WindowAppearance::Light),
            Theme::LIGHT
        );
        assert_eq!(
            Theme::resolve(Appearance::Dark, WindowAppearance::Light),
            Theme::DARK
        );
        assert_eq!(
            Theme::resolve(Appearance::Light, WindowAppearance::Dark),
            Theme::LIGHT
        );
    }
}
