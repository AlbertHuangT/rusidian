use cargo_packager_updater::{Config, Update, UpdaterBuilder, semver::Version, url::Url};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const ENDPOINTS: [&str; 2] = [
    "https://github.com/AlbertHuangT/rusidian/releases/latest/download/latest.json",
    "https://github.com/AlbertHuangT/rusidian/releases/download/nightly/latest.json",
];
const PUBLIC_KEY: &str = include_str!("../assets/update.pubkey");

pub fn set_auto_update(enabled: bool) -> Result<(), String> {
    crate::settings::update(|settings| settings.auto_update = enabled).map(|_| ())
}

pub fn is_packaged_app() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| app_bundle_for_executable(&path))
        .is_some()
}

pub fn check() -> Result<Option<Update>, String> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|error| format!("当前版本无效：{error}"))?;
    let endpoints = ENDPOINTS
        .iter()
        .map(|endpoint| Url::parse(endpoint).map_err(|error| format!("更新地址无效：{error}")))
        .collect::<Result<Vec<_>, _>>()?;
    let config = Config {
        endpoints,
        pubkey: PUBLIC_KEY.trim().to_owned(),
        ..Default::default()
    };
    UpdaterBuilder::new(current, config)
        .timeout(Duration::from_secs(20))
        .build()
        .and_then(|updater| updater.check())
        .map_err(|error| format!("检查更新失败：{error}"))
}

pub fn install(update: Update) -> Result<(), String> {
    if !is_packaged_app() {
        return Err("只有从 Rusidian.app 启动时才能自动安装更新".into());
    }
    update
        .download_and_install()
        .map_err(|error| format!("安装更新失败：{error}"))
}

fn app_bundle_for_executable(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle
            .extension()
            .is_some_and(|extension| extension == "app"))
    .then(|| bundle.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_app_bundle_executables_are_installable() {
        assert_eq!(
            app_bundle_for_executable(Path::new(
                "/Applications/Rusidian.app/Contents/MacOS/rusidian"
            )),
            Some(PathBuf::from("/Applications/Rusidian.app"))
        );
        assert_eq!(
            app_bundle_for_executable(Path::new("target/release/rusidian")),
            None
        );
    }
}
