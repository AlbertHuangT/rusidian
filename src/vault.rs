use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
};

pub struct Vault {
    pub root: PathBuf,
    /// Markdown notes, sorted by path.
    pub files: Vec<PathBuf>,
    /// Every visible file (notes and attachments) by file name, for Obsidian's shortest links.
    by_name: HashMap<String, Vec<PathBuf>>,
    pub is_obsidian: bool,
    pub settings: ObsidianSettings,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ObsidianSettings {
    pub strict_line_breaks: bool,
    pub new_link_format: Option<String>,
    pub use_markdown_links: Option<bool>,
    pub attachment_folder_path: Option<String>,
    pub always_update_links: Option<bool>,
    pub user_ignore_filters: Vec<String>,
}

impl Vault {
    pub fn open(root: &Path) -> io::Result<Self> {
        // Absolute paths keep note and tree comparisons independent of how the vault was named.
        let root = &root.canonicalize()?;
        let config = root.join(".obsidian/app.json");
        let settings: ObsidianSettings = fs::read_to_string(&config)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default();
        let ignored = settings
            .user_ignore_filters
            .iter()
            .filter(|filter| !is_regex_filter(filter))
            .map(|filter| filter.trim_matches('/').to_owned())
            .filter(|filter| !filter.is_empty())
            .collect::<Vec<_>>();
        let mut all = Vec::new();
        visit(root, root, &ignored, &mut all)?;
        all.sort();
        let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in &all {
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                by_name
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(path.clone());
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            files: all.into_iter().filter(|path| is_markdown(path)).collect(),
            by_name,
            // Obsidian creates app.json only after a setting changes; the folder is the marker.
            is_obsidian: root.join(".obsidian").is_dir(),
            settings,
        })
    }

    pub fn resolve_note(&self, target: &str) -> Option<PathBuf> {
        let target = target.trim_end_matches(".md");
        let mut matches = self.files.iter().filter(|file| {
            let relative = file.strip_prefix(&self.root).unwrap_or(file);
            let without_extension = relative.with_extension("");
            without_extension == Path::new(target)
                || (!target.contains('/')
                    && file.file_stem().and_then(|stem| stem.to_str()) == Some(target))
        });
        let first = matches.next()?.clone();
        matches.next().is_none().then_some(first)
    }

    /// A uniquely named file anywhere in the vault, as Obsidian's shortest link format expects.
    fn find_by_name(&self, name: &str) -> Option<PathBuf> {
        match self.by_name.get(&name.to_lowercase())?.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        }
    }
}

/// One visible row of the vault's file tree.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeRow {
    Folder {
        path: PathBuf,
        name: String,
        depth: usize,
        expanded: bool,
    },
    Note {
        path: PathBuf,
        name: String,
        depth: usize,
    },
}

#[derive(Default)]
struct TreeFolder {
    /// Keyed by lowercase name so folders sort case-insensitively, like Obsidian.
    folders: BTreeMap<String, (String, TreeFolder)>,
    notes: BTreeMap<String, (String, PathBuf)>,
}

impl Vault {
    /// Folders first, then notes, each sorted by name; children of collapsed folders are hidden.
    pub fn tree_rows(&self, expanded: &HashSet<PathBuf>) -> Vec<TreeRow> {
        let mut tree = TreeFolder::default();
        for file in &self.files {
            let Ok(relative) = file.strip_prefix(&self.root) else {
                continue;
            };
            let mut folder = &mut tree;
            let components = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let Some((name, parents)) = components.split_last() else {
                continue;
            };
            for parent in parents {
                folder = &mut folder
                    .folders
                    .entry(parent.to_lowercase())
                    .or_insert_with(|| (parent.clone(), TreeFolder::default()))
                    .1;
            }
            let stem = Path::new(name)
                .file_stem()
                .map_or_else(|| name.clone(), |stem| stem.to_string_lossy().into_owned());
            folder
                .notes
                .insert(name.to_lowercase(), (stem, file.clone()));
        }
        let mut rows = Vec::new();
        flatten(&tree, &self.root, 0, expanded, &mut rows);
        rows
    }
}

fn flatten(
    folder: &TreeFolder,
    path: &Path,
    depth: usize,
    expanded: &HashSet<PathBuf>,
    rows: &mut Vec<TreeRow>,
) {
    for (name, child) in folder.folders.values() {
        let path = path.join(name);
        let open = expanded.contains(&path);
        rows.push(TreeRow::Folder {
            path: path.clone(),
            name: name.clone(),
            depth,
            expanded: open,
        });
        if open {
            flatten(child, &path, depth + 1, expanded, rows);
        }
    }
    for (name, path) in folder.notes.values() {
        rows.push(TreeRow::Note {
            path: path.clone(),
            name: name.clone(),
            depth,
        });
    }
}

/// The nearest enclosing Obsidian vault (a folder with `.obsidian/`) of a note.
pub fn enclosing_vault_root(note: &Path) -> Option<PathBuf> {
    let note = note.canonicalize().ok()?;
    note.ancestors()
        .skip(1)
        .find(|folder| folder.join(".obsidian").is_dir())
        .map(Path::to_path_buf)
}

/// Resolve a link or embed target written in `note` to a local file.
///
/// Tries the note's folder, then (inside a vault) the vault root, a unique file name and a
/// unique note name. Remote URLs and absolute paths are not resolved.
pub fn resolve_target(note: &Path, vault: Option<&Vault>, target: &str) -> Option<PathBuf> {
    let target = percent_decode(target);
    let target = target.trim();
    if target.is_empty()
        || target.contains("://")
        || target.starts_with("data:")
        || target.starts_with("mailto:")
        || Path::new(target).is_absolute()
    {
        return None;
    }
    let with_markdown = |path: PathBuf| {
        if path.is_file() {
            Some(path)
        } else if path.extension().is_none() {
            Some(path.with_extension("md")).filter(|path| path.is_file())
        } else {
            None
        }
    };
    let folder = note.parent().unwrap_or_else(|| Path::new("."));
    if let Some(path) = with_markdown(folder.join(target)) {
        return Some(path);
    }
    let vault = vault?;
    if let Some(path) = with_markdown(vault.root.join(target)) {
        return Some(path);
    }
    let name = Path::new(target).file_name()?.to_str()?;
    vault
        .find_by_name(name)
        .or_else(|| vault.find_by_name(&format!("{name}.md")))
        .or_else(|| vault.resolve_note(target))
}

/// Split `path#heading` (or `path#^block`) into its file and fragment parts.
pub fn split_fragment(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment).filter(|fragment| !fragment.is_empty())),
        None => (target, None),
    }
}

/// Decode `%XX` escapes as used in Markdown link destinations. Invalid escapes stay as written.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = text.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| text.to_owned())
}

pub fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

/// Obsidian writes regular-expression filters as `/pattern/`; only folder and file prefixes are
/// supported here.
fn is_regex_filter(filter: &str) -> bool {
    filter.len() > 1 && filter.starts_with('/') && filter.ends_with('/')
}

fn visit(
    root: &Path,
    directory: &Path,
    ignored: &[String],
    files: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        // Obsidian ignores hidden files and folders such as .obsidian, .git and .trash.
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if ignored.iter().any(|filter| {
            relative == *filter
                || relative
                    .strip_prefix(filter.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        }) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visit(root, &path, ignored, files)?;
        } else if file_type.is_file() {
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
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/B Note.md"), "# B").unwrap();
        fs::write(root.join("ignored.txt"), "text").unwrap();
        fs::write(hidden.join("state.md"), "state").unwrap();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git/HEAD.md"), "git").unwrap();
        fs::create_dir_all(root.join("Templates")).unwrap();
        fs::write(root.join("Templates/Daily.md"), "template").unwrap();
        fs::create_dir_all(root.join("assets")).unwrap();
        fs::write(root.join("assets/图 1.png"), "png").unwrap();
        fs::write(
            hidden.join("app.json"),
            r#"{"strictLineBreaks":true,"alwaysUpdateLinks":true,"userIgnoreFilters":["Templates/","/regex/"]}"#,
        )
        .unwrap();
        // The vault stores canonical paths; macOS temp folders sit behind a symlink.
        let root = root.canonicalize().unwrap();

        let vault = Vault::open(&root).unwrap();
        assert_eq!(
            vault.files,
            [root.join("a.md"), root.join("nested/B Note.md")]
        );
        assert!(vault.is_obsidian);
        assert!(vault.settings.strict_line_breaks);
        assert_eq!(vault.settings.always_update_links, Some(true));
        assert_eq!(
            vault.resolve_note("B Note"),
            Some(root.join("nested/B Note.md"))
        );
        assert_eq!(
            vault.resolve_note("nested/B Note"),
            Some(root.join("nested/B Note.md"))
        );

        let note = root.join("nested/B Note.md");
        assert_eq!(
            resolve_target(&note, Some(&vault), "图 1.png"),
            Some(root.join("assets/图 1.png"))
        );
        assert_eq!(
            resolve_target(&note, Some(&vault), "assets/%E5%9B%BE%201.png"),
            Some(root.join("assets/图 1.png"))
        );
        assert_eq!(
            resolve_target(&root.join("a.md"), Some(&vault), "B Note"),
            Some(root.join("nested/B Note.md"))
        );
        assert_eq!(
            resolve_target(&note, None, "../a"),
            Some(root.join("nested/../a.md"))
        );
        assert_eq!(
            resolve_target(&note, Some(&vault), "https://x.y/a.png"),
            None
        );
        assert_eq!(resolve_target(&note, None, "图 1.png"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_the_enclosing_obsidian_vault() {
        let root = std::env::temp_dir().join(format!("rusidian-enclosing-{}", std::process::id()));
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::create_dir_all(root.join("deep/er")).unwrap();
        fs::write(root.join("deep/er/note.md"), "x").unwrap();
        assert_eq!(
            enclosing_vault_root(&root.join("deep/er/note.md")),
            Some(root.canonicalize().unwrap())
        );
        assert_eq!(enclosing_vault_root(Path::new("examples/tikz.md")), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn builds_a_folder_first_collapsible_tree() {
        let root = PathBuf::from("/vault");
        let vault = Vault {
            files: [
                "zeta.md",
                "Alpha.md",
                "notes/b.md",
                "notes/deep/c.md",
                "Archive/old.md",
            ]
            .iter()
            .map(|file| root.join(file))
            .collect(),
            root: root.clone(),
            by_name: HashMap::new(),
            is_obsidian: false,
            settings: ObsidianSettings::default(),
        };
        let names = |rows: Vec<TreeRow>| {
            rows.into_iter()
                .map(|row| match row {
                    TreeRow::Folder { name, depth, .. } => format!("{depth}:{name}/"),
                    TreeRow::Note { name, depth, .. } => format!("{depth}:{name}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(vault.tree_rows(&HashSet::new())),
            ["0:Archive/", "0:notes/", "0:Alpha", "0:zeta"]
        );
        let expanded = HashSet::from([root.join("notes"), root.join("notes/deep")]);
        assert_eq!(
            names(vault.tree_rows(&expanded)),
            [
                "0:Archive/",
                "0:notes/",
                "1:deep/",
                "2:c",
                "1:b",
                "0:Alpha",
                "0:zeta"
            ]
        );
    }

    #[test]
    fn treats_any_obsidian_folder_as_a_vault() {
        let root = std::env::temp_dir().join(format!("rusidian-bare-vault-{}", std::process::id()));
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        assert!(Vault::open(&root).unwrap().is_obsidian);
        fs::remove_dir_all(root.join(".obsidian")).unwrap();
        assert!(!Vault::open(&root).unwrap().is_obsidian);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn decodes_link_destinations_and_fragments() {
        assert_eq!(percent_decode("My%20Note.md"), "My Note.md");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
        assert_eq!(split_fragment("Note#Heading"), ("Note", Some("Heading")));
        assert_eq!(split_fragment("#Heading"), ("", Some("Heading")));
        assert_eq!(split_fragment("Note#"), ("Note", None));
    }
}
