//! Locating external tools (Neovim, Tectonic, Poppler).
//!
//! Apps started from Finder or the Dock get a minimal `PATH` without Homebrew, MacPorts or
//! Nix folders, so `nvim` installed the usual way would not be found. Build a search path from
//! the inherited `PATH`, the login shell's `PATH` and well-known install folders.

use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{OnceLock, mpsc},
    time::Duration,
};

/// The `PATH` used to find tools and passed to Neovim so its plugins find theirs.
pub fn search_path() -> &'static OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let home = dirs::home_dir();
        let mut folders = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .unwrap_or_default();
        if launched_outside_terminal()
            && let Some(login) = login_shell_path()
        {
            folders.extend(std::env::split_paths(&login));
        }
        folders.extend(well_known_folders(home.as_deref()));
        join_unique(folders)
    })
}

/// The full path of `name` on the search path, or `name` itself so spawning reports it missing.
pub fn executable(name: &str) -> PathBuf {
    find_in(search_path(), name).unwrap_or_else(|| PathBuf::from(name))
}

fn find_in(path: &OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|folder| folder.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// A terminal sets TERM; Finder, the Dock and desktop launchers do not.
fn launched_outside_terminal() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("TERM").is_none()
}

/// `PATH` from the user's login shell, where Homebrew's shellenv usually lives.
fn login_shell_path() -> Option<OsString> {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let output = Command::new(shell)
            .args(["-l", "-c", "printf '%s' \"$PATH\""])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = sender.send(output);
    });
    // A slow or broken shell profile must not block tool lookup for long.
    let output = receiver.recv_timeout(Duration::from_secs(3)).ok()?.ok()?;
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (output.status.success() && !path.is_empty()).then(|| path.into())
}

fn well_known_folders(home: Option<&Path>) -> Vec<PathBuf> {
    let mut folders = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/local/bin"),
        PathBuf::from("/nix/var/nix/profiles/default/bin"),
        PathBuf::from("/run/current-system/sw/bin"),
        PathBuf::from("/snap/bin"),
    ];
    if let Some(home) = home {
        folders.extend(
            [".local/bin", "bin", ".nix-profile/bin", ".cargo/bin"]
                .iter()
                .map(|folder| home.join(folder)),
        );
    }
    folders
}

fn join_unique(folders: Vec<PathBuf>) -> OsString {
    let mut seen = Vec::new();
    for folder in folders {
        if !folder.as_os_str().is_empty() && !seen.contains(&folder) {
            seen.push(folder);
        }
    }
    std::env::join_paths(seen).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tools_in_well_known_folders_and_keeps_path_order() {
        let folder = std::env::temp_dir().join(format!("rusidian-paths-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let tool = folder.join("rusidian-test-tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        let path = join_unique(vec![
            PathBuf::from("/usr/bin"),
            folder.clone(),
            PathBuf::from("/usr/bin"),
        ]);
        assert_eq!(std::env::split_paths(&path).count(), 2);
        // Not executable yet.
        assert_eq!(find_in(&path, "rusidian-test-tool"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(find_in(&path, "rusidian-test-tool"), Some(tool));
        }
        assert!(
            well_known_folders(Some(Path::new("/home/me")))
                .contains(&PathBuf::from("/home/me/.local/bin"))
        );
        assert!(well_known_folders(None).contains(&PathBuf::from("/opt/homebrew/bin")));
        std::fs::remove_dir_all(folder).unwrap();
    }
}
