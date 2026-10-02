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
            }
        );
        update_at(&path, |settings| settings.appearance = Appearance::Dark).unwrap();
        let settings = load_from(&path);
        assert!(settings.auto_update);
        assert_eq!(settings.appearance, Appearance::Dark);
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
}
