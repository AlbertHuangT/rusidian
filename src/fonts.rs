use gpui::{App, SharedString};
use std::sync::OnceLock;

/// Monospace families in preference order; the Neovim grid needs a real fixed-width font.
const MONO_FAMILIES: &[&str] = if cfg!(target_os = "macos") {
    &["Menlo", "SF Mono", "Monaco", "Courier New"]
} else {
    &[
        "DejaVu Sans Mono",
        "Noto Sans Mono",
        "Liberation Mono",
        "Ubuntu Mono",
        "FreeMono",
    ]
};

/// Reading-view families. GPUI maps `.SystemUIFont` to IBM Plex Sans on Linux; when that is not
/// installed its fallback ignores weight and style, which would drop bold and italic text.
/// Families that usually ship italic faces come first; GPUI does not synthesize them.
const UI_FAMILIES: &[&str] = if cfg!(target_os = "macos") {
    &[".SystemUIFont"]
} else {
    &[
        "IBM Plex Sans",
        "Inter",
        "Noto Sans",
        "Ubuntu",
        "Liberation Sans",
        "DejaVu Sans",
        "FreeSans",
    ]
};

static MONO: OnceLock<SharedString> = OnceLock::new();
static UI: OnceLock<SharedString> = OnceLock::new();

/// Choose installed families once, before the first window renders.
pub fn init(cx: &App) {
    // `all_font_names` also lists GPUI's fallback stack, installed or not. Resolve each candidate
    // instead: a missing family resolves to a fallback font with a different family name.
    let text_system = cx.text_system();
    let installed = |family: &str| {
        family.starts_with('.')
            || text_system
                .get_font_for_id(text_system.resolve_font(&gpui::font(family.to_owned())))
                .is_some_and(|font| font.family.as_ref() == family)
    };
    let pick = |candidates: &[&'static str], default: &'static str| -> SharedString {
        candidates
            .iter()
            .find(|family| installed(family))
            .copied()
            .unwrap_or(default)
            .into()
    };
    MONO.get_or_init(|| pick(MONO_FAMILIES, MONO_FAMILIES[0]));
    UI.get_or_init(|| pick(UI_FAMILIES, ".SystemUIFont"));
}

pub fn mono() -> SharedString {
    MONO.get()
        .cloned()
        .unwrap_or_else(|| MONO_FAMILIES[0].into())
}

pub fn ui() -> SharedString {
    UI.get().cloned().unwrap_or_else(|| ".SystemUIFont".into())
}
