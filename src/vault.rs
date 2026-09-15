use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub struct Vault {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

impl Vault {
    pub fn open(root: &Path) -> io::Result<Self> {
        let mut files = Vec::new();
        visit(root, &mut files)?;
        files.sort();
        Ok(Self {
            root: root.to_path_buf(),
            files,
        })
    }
}

fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if entry.file_name() != ".obsidian" {
                visit(&path, files)?;
            }
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("md")
                        || extension.eq_ignore_ascii_case("markdown")
                })
        {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_markdown_without_vault_state() {
        let root = std::env::temp_dir().join(format!("rusidian-vault-test-{}", std::process::id()));
        let hidden = root.join(".obsidian");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(root.join("a.md"), "# A").unwrap();
        fs::write(root.join("ignored.txt"), "text").unwrap();
        fs::write(hidden.join("state.md"), "state").unwrap();

        let vault = Vault::open(&root).unwrap();
        assert_eq!(vault.files, [root.join("a.md")]);
        fs::remove_dir_all(root).unwrap();
    }
}
