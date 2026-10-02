//! App settings, stored in the system config directory and never inside a vault.

use crate::theme::Appearance;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub auto_update: bool,
    pub appearance: Appearance,
    /// Recently opened files and folders, newest first.
    pub recent: Vec<PathBuf>,
    /// Vaults whose remote images load without asking each time.
    pub remote_image_vaults: Vec<PathBuf>,
    /// The user asked not to be reminded about im-select.nvim again.
    pub hide_ime_hint: bool,
    pub reading_key: ReadingKey,
}

/// The key that returns from the source view's Normal mode to the reading view. Users whose
/// Neovim maps `<Esc>` in Normal mode can move Rusidian off it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadingKey {
    #[default]
    Escape,
    /// ⌘Enter on macOS, Ctrl+Enter elsewhere.
    SecondaryEnter,
}

impl ReadingKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Escape => "Esc",
            Self::SecondaryEnter if cfg!(target_os = "macos") => "⌘Enter",
            Self::SecondaryEnter => "Ctrl+Enter",
        }
    }

    pub fn matches(self, keystroke: &gpui::Keystroke) -> bool {
        let modifiers = &keystroke.modifiers;
        match self {
            Self::Escape => keystroke.key == "escape",
            Self::SecondaryEnter => {
                keystroke.key == "enter"
                    && modifiers.secondary()
                    && !modifiers.shift
                    && !modifiers.alt
            }
        }
    }
}

const RECENT_LIMIT: usize = 10;

impl Settings {
    pub fn remember(&mut self, path: PathBuf) {
        self.recent.retain(|recent| *recent != path);
        self.recent.insert(0, path);
        self.recent.truncate(RECENT_LIMIT);
    }
}

pub fn load() -> Settings {
    settings_path()
        .ok()
        .map(|path| load_from(&path))
        .unwrap_or_default()
}

/// Read, change and atomically write the settings, keeping fields this change does not touch.
pub fn update(change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
    update_at(&settings_path()?, change)
}

fn load_from(path: &Path) -> Settings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn update_at(path: &Path, change: impl FnOnce(&mut Settings)) -> Result<Settings, String> {
    let mut settings = load_from(path);
    change(&mut settings);
    let parent = path.parent().ok_or("无法确定设置目录")?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建设置目录：{error}"))?;
    let temporary = path.with_extension("json.tmp");
    let bytes =
        serde_json::to_vec_pretty(&settings).map_err(|error| format!("无法编码设置：{error}"))?;
    fs::write(&temporary, bytes).map_err(|error| format!("无法保存设置：{error}"))?;
    fs::rename(&temporary, path).map_err(|error| format!("无法替换设置：{error}"))?;
    Ok(settings)
}

fn settings_path() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|directory| directory.join("rusidian/settings.json"))
        .ok_or_else(|| "无法确定系统设置目录".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_keep_other_settings_and_old_files_still_load() {
        let directory =
            std::env::temp_dir().join(format!("rusidian-settings-{}", std::process::id()));
        let path = directory.join("settings.json");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, r#"{"auto_update":true}"#).unwrap();
        assert_eq!(
            load_from(&path),
            Settings {
                auto_update: true,
                appearance: Appearance::System,
                recent: Vec::new(),
                remote_image_vaults: Vec::new(),
                hide_ime_hint: false,
                reading_key: ReadingKey::Escape,
            }
        );
        update_at(&path, |settings| settings.appearance = Appearance::Dark).unwrap();
        let settings = load_from(&path);
        assert!(settings.auto_update);
        assert_eq!(settings.appearance, Appearance::Dark);
        update_at(&path, |settings| {
            settings.reading_key = ReadingKey::SecondaryEnter;
        })
        .unwrap();
        assert!(
            String::from_utf8(fs::read(&path).unwrap())
                .unwrap()
                .contains(r#""reading_key": "secondary-enter""#)
        );
        assert_eq!(load_from(&path).reading_key, ReadingKey::SecondaryEnter);
        let mut remembered = Settings::default();
        for index in 0..12 {
            remembered.remember(PathBuf::from(format!("/notes/{index}.md")));
        }
        remembered.remember(PathBuf::from("/notes/5.md"));
        assert_eq!(remembered.recent.len(), RECENT_LIMIT);
        assert_eq!(remembered.recent[0], PathBuf::from("/notes/5.md"));
        assert_eq!(remembered.recent[1], PathBuf::from("/notes/11.md"));
        fs::write(&path, "not json").unwrap();
        assert_eq!(load_from(&path), Settings::default());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reading_keys_match_their_keystrokes() {
        let key = |source: &str| gpui::Keystroke::parse(source).unwrap();
        let secondary = if cfg!(target_os = "macos") {
            "cmd-enter"
        } else {
            "ctrl-enter"
        };
        assert!(ReadingKey::Escape.matches(&key("escape")));
        assert!(!ReadingKey::Escape.matches(&key("enter")));
        assert!(ReadingKey::SecondaryEnter.matches(&key(secondary)));
        assert!(!ReadingKey::SecondaryEnter.matches(&key("enter")));
        assert!(!ReadingKey::SecondaryEnter.matches(&key("escape")));
        assert!(!ReadingKey::SecondaryEnter.matches(&key(&format!("shift-{secondary}"))));
    }
}
