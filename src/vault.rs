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
    /// Front-matter aliases of notes that have them, which the quick switcher also matches.
    pub aliases: HashMap<PathBuf, Vec<String>>,
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
    /// Where new notes go: `root` (the default), `current` (beside the open note) or `folder`.
    pub new_file_location: Option<String>,
    pub new_file_folder_path: Option<String>,
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
        let files: Vec<PathBuf> = all.into_iter().filter(|path| is_markdown(path)).collect();
        let aliases = files
            .iter()
            .filter_map(|path| {
                let aliases = read_aliases(path);
                (!aliases.is_empty()).then(|| (path.clone(), aliases))
            })
            .collect();
        Ok(Self {
            root: root.to_path_buf(),
            files,
            aliases,
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
    /// Keyed so folders and notes sort like Obsidian's file list.
    folders: BTreeMap<NaturalKey, (String, TreeFolder)>,
    notes: BTreeMap<NaturalKey, (String, PathBuf)>,
}

/// A name ordered like Obsidian's file list: ignoring case, with numbers by value, so
/// `Note 2` comes before `Note 10`.
#[derive(PartialEq, Eq)]
struct NaturalKey(String);

impl NaturalKey {
    fn new(name: &str) -> Self {
        Self(name.to_lowercase())
    }
}

impl Ord for NaturalKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        natural_cmp(&self.0, &other.0)
    }
}

impl PartialOrd for NaturalKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let digits = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut number = String::new();
                    while let Some(digit) = chars.next_if(char::is_ascii_digit) {
                        number.push(digit);
                    }
                    number
                };
                let (first, second) = (digits(&mut a), digits(&mut b));
                let (x, y) = (
                    first.trim_start_matches('0'),
                    second.trim_start_matches('0'),
                );
                // By value, then fewer leading zeros first.
                let order = x
                    .len()
                    .cmp(&y.len())
                    .then_with(|| x.cmp(y))
                    .then_with(|| first.len().cmp(&second.len()));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a.next();
                b.next();
            }
        }
    }
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
                    .entry(NaturalKey::new(parent))
                    .or_insert_with(|| (parent.clone(), TreeFolder::default()))
                    .1;
            }
            let stem = Path::new(name)
                .file_stem()
                .map_or_else(|| name.clone(), |stem| stem.to_string_lossy().into_owned());
            folder
                .notes
                .insert(NaturalKey::new(name), (stem, file.clone()));
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

/// Where following a link to a missing note creates it, as Obsidian would: a link with a folder
/// is relative to the vault root, a bare name goes to the vault's new-note location (beside the
/// open note without a vault). `None` for targets that are not note names.
pub fn new_note_path(note: &Path, vault: Option<&Vault>, target: &str) -> Option<PathBuf> {
    let target = percent_decode(target);
    let target = target.trim().trim_end_matches(".md");
    let path = Path::new(target);
    let plain_name = path
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)));
    // `image.png` names an attachment; `v1.2 release` is still a note.
    let attachment = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.len() <= 5 && extension.chars().all(|c| c.is_ascii_alphanumeric())
        });
    if target.is_empty() || !plain_name || attachment || target.contains("://") {
        return None;
    }
    let file = format!("{target}.md");
    let beside = || note.parent().unwrap_or_else(|| Path::new(".")).join(&file);
    let Some(vault) = vault else {
        return Some(beside());
    };
    if target.contains('/') {
        return Some(vault.root.join(&file));
    }
    Some(match vault.settings.new_file_location.as_deref() {
        Some("current") => beside(),
        Some("folder") => vault
            .root
            .join(vault.settings.new_file_folder_path.as_deref().unwrap_or(""))
            .join(&file),
        _ => vault.root.join(&file),
    })
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

/// Point the links in `text` (the note at `note`) that lead to the note at `old` to a note named
/// `new_name` in the same folder: wikilinks and embeds by name or path, and Markdown links by
/// relative path, keeping each link's folder, `.md` and fragment. Code is left alone. Returns
/// the new text and how many links changed.
pub fn retarget_links(
    text: &str,
    note: &Path,
    vault: Option<&Vault>,
    old: &Path,
    new_name: &str,
) -> (String, usize) {
    let old = old.canonicalize().unwrap_or_else(|_| old.to_path_buf());
    let points_to_old = |target: &str| {
        resolve_target(note, vault, target)
            .and_then(|path| path.canonicalize().ok())
            .is_some_and(|path| path == old)
    };
    let mut result = String::with_capacity(text.len());
    let mut count = 0;
    let mut fence: Option<&str> = None;
    for line in text.split_inclusive('\n') {
        let opening = line.trim_start();
        match fence {
            Some(marker) => {
                if opening.starts_with(marker) {
                    fence = None;
                }
                result.push_str(line);
            }
            None if opening.starts_with("```") || opening.starts_with("~~~") => {
                fence = Some(&opening[..3]);
                result.push_str(line);
            }
            None => {
                let (line, changed) = retarget_line(line, &points_to_old, new_name);
                result.push_str(&line);
                count += changed;
            }
        }
    }
    (result, count)
}

fn retarget_line(
    line: &str,
    points_to_old: &dyn Fn(&str) -> bool,
    new_name: &str,
) -> (String, usize) {
    // Byte ranges inside `code` spans, which are left alone.
    let ticks: Vec<usize> = line.match_indices('`').map(|(at, _)| at).collect();
    let in_code = |at: usize| {
        ticks
            .chunks(2)
            .any(|pair| pair.len() == 2 && pair[0] < at && at < pair[1])
    };
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    // [[target#fragment|alias]] and ![[...]]
    let mut from = 0;
    while let Some(open) = line[from..].find("[[") {
        let start = from + open + 2;
        let Some(close) = line[start..].find("]]") else {
            break;
        };
        let inside = &line[start..start + close];
        let target = &inside[..inside.find('|').unwrap_or(inside.len())];
        let name = &target[..target.find('#').unwrap_or(target.len())];
        let trimmed = name.trim();
        if !trimmed.is_empty() && !in_code(start) && points_to_old(trimmed) {
            let at = start + (name.len() - name.trim_start().len());
            edits.push((at..at + trimmed.len(), renamed(trimmed, new_name, false)));
        }
        from = start + close + 2;
    }
    // [text](path/to/note.md#fragment "title") and [text](<path with spaces.md>)
    let mut from = 0;
    while let Some(open) = line[from..].find("](") {
        let start = from + open + 2;
        let rest = &line[start..];
        let (offset, destination) = match rest.strip_prefix('<') {
            Some(inner) => (1, &inner[..inner.find('>').unwrap_or(0)]),
            None => (
                0,
                &rest[..rest
                    .find(|character: char| character == ')' || character.is_whitespace())
                    .unwrap_or(0)],
            ),
        };
        let path = &destination[..destination.find('#').unwrap_or(destination.len())];
        if !path.is_empty()
            && !path.contains("://")
            && !path.starts_with("mailto:")
            && !in_code(start)
            && points_to_old(path)
        {
            let at = start + offset;
            // Angle brackets allow spaces; elsewhere they are written %20.
            let encoded = offset == 0;
            edits.push((at..at + path.len(), renamed(path, new_name, encoded)));
        }
        from = start + destination.len().max(1);
    }
    let count = edits.len();
    let mut line = line.to_owned();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    for (range, replacement) in edits {
        line.replace_range(range, &replacement);
    }
    (line, count)
}

/// A link target to the old note with its name replaced, keeping its folder and `.md`.
fn renamed(target: &str, new_name: &str, encoded: bool) -> String {
    let (folder, file) = match target.rfind('/') {
        Some(slash) => target.split_at(slash + 1),
        None => ("", target),
    };
    let suffix = if file.to_lowercase().ends_with(".md") {
        &file[file.len() - 3..]
    } else {
        ""
    };
    let name = if encoded {
        new_name.replace('%', "%25").replace(' ', "%20")
    } else {
        new_name.to_owned()
    };
    format!("{folder}{name}{suffix}")
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

/// The `aliases` (or `alias`) listed in a note's front matter, read from its first 4 KiB.
fn read_aliases(path: &Path) -> Vec<String> {
    use std::io::Read;
    let mut head = Vec::new();
    if fs::File::open(path)
        .and_then(|file| file.take(4096).read_to_end(&mut head))
        .is_err()
    {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Vec::new();
    }
    let clean = |value: &str| {
        value
            .trim()
            .trim_matches(|character| character == '"' || character == '\'')
            .to_owned()
    };
    let mut aliases = Vec::new();
    let mut in_aliases = false;
    for line in lines {
        if line.trim_end() == "---" {
            break;
        }
        if let Some(item) = line.trim_start().strip_prefix("- ") {
            if in_aliases {
                aliases.push(clean(item));
            }
        } else if !line.starts_with(char::is_whitespace)
            && let Some((key, value)) = line.split_once(':')
        {
            in_aliases = matches!(key.trim(), "aliases" | "alias");
            let value = value.trim();
            if in_aliases && !value.is_empty() {
                let list = value
                    .strip_prefix('[')
                    .and_then(|value| value.strip_suffix(']'))
                    .unwrap_or(value);
                aliases.extend(list.split(',').map(clean));
            }
        }
    }
    aliases.retain(|alias| !alias.is_empty());
    aliases
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
    fn retargets_links_to_a_renamed_note() {
        let root = std::env::temp_dir().join(format!("rusidian-rename-{}", std::process::id()));
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("Old.md"), "# Old").unwrap();
        fs::write(root.join("Older.md"), "# Older").unwrap();
        fs::write(root.join("folder/other.md"), "").unwrap();
        let root = root.canonicalize().unwrap();
        let vault = Vault::open(&root).unwrap();
        let old = root.join("Old.md");
        let retarget = |note: &str, text: &str| {
            retarget_links(text, &root.join(note), Some(&vault), &old, "New name")
        };
        let text = "See [[Old]], [[old|alias]], [[Old#Heading]] and ![[Old.md]].\n\
                    Not [[Older]], `[[Old]]` or [web](https://x.org/Old.md).\n\
                    ```\n[[Old]]\n```\n\
                    [md](Old.md#part) and [angle](<Old.md>).\n";
        let (changed, count) = retarget("folder/../Older.md", text);
        assert_eq!(count, 6);
        assert_eq!(
            changed,
            "See [[New name]], [[New name|alias]], [[New name#Heading]] and ![[New name.md]].\n\
             Not [[Older]], `[[Old]]` or [web](https://x.org/Old.md).\n\
             ```\n[[Old]]\n```\n\
             [md](New%20name.md#part) and [angle](<New name.md>).\n"
        );
        // Relative Markdown links from another folder keep their path.
        let (changed, count) = retarget("folder/other.md", "[up](../Old.md) [[Old]]");
        assert_eq!(
            (changed.as_str(), count),
            ("[up](../New%20name.md) [[New name]]", 2)
        );
        assert_eq!(retarget("Older.md", "no links").1, 0);
        fs::remove_dir_all(root).unwrap();
    }

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
            aliases: HashMap::new(),
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
    fn places_new_notes_like_obsidian() {
        let root = std::env::temp_dir().join(format!("rusidian-new-note-{}", std::process::id()));
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        let note = root.join("sub/current.md");
        fs::write(&note, "x").unwrap();
        let mut vault = Vault::open(&root).unwrap();
        let root = vault.root.clone();
        let note = root.join("sub/current.md");
        assert_eq!(
            new_note_path(&note, Some(&vault), "New idea"),
            Some(root.join("New idea.md"))
        );
        assert_eq!(
            new_note_path(&note, Some(&vault), "Projects/plan.md"),
            Some(root.join("Projects/plan.md"))
        );
        vault.settings.new_file_location = Some("current".into());
        assert_eq!(
            new_note_path(&note, Some(&vault), "Idea"),
            Some(root.join("sub/Idea.md"))
        );
        vault.settings.new_file_location = Some("folder".into());
        vault.settings.new_file_folder_path = Some("Inbox".into());
        assert_eq!(
            new_note_path(&note, Some(&vault), "Idea"),
            Some(root.join("Inbox/Idea.md"))
        );
        assert_eq!(
            new_note_path(&note, None, "Idea"),
            Some(root.join("sub/Idea.md"))
        );
        assert_eq!(new_note_path(&note, Some(&vault), "image.png"), None);
        assert_eq!(
            new_note_path(&note, Some(&vault), "v1.2 release"),
            Some(root.join("Inbox/v1.2 release.md"))
        );
        assert_eq!(new_note_path(&note, Some(&vault), "../outside"), None);
        assert_eq!(new_note_path(&note, Some(&vault), "https://x.org"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sorts_names_naturally() {
        let mut names = [
            "note-25",
            "Note-3",
            "note-250",
            "note-2499",
            "Alpha",
            "note-03",
            "beta",
        ];
        names.sort_by(|a, b| NaturalKey::new(a).cmp(&NaturalKey::new(b)));
        assert_eq!(
            names,
            [
                "Alpha",
                "beta",
                "Note-3",
                "note-03",
                "note-25",
                "note-250",
                "note-2499"
            ]
        );
    }

    #[test]
    fn reads_front_matter_aliases() {
        let folder = std::env::temp_dir().join(format!("rusidian-aliases-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let note = |name: &str, text: &str| {
            let path = folder.join(name);
            fs::write(&path, text).unwrap();
            read_aliases(&path)
        };
        assert_eq!(
            note(
                "a.md",
                "---\naliases: [First, 'Second']\ntags: [x]\n---\nbody\n"
            ),
            ["First", "Second"]
        );
        assert_eq!(
            note(
                "b.md",
                "---\ntitle: t\naliases:\n  - One\n  - \"Two\"\ntags:\n  - no\n---\n"
            ),
            ["One", "Two"]
        );
        assert_eq!(note("c.md", "---\nalias: Solo\n---\n"), ["Solo"]);
        assert!(note("d.md", "# No front matter\naliases: [x]\n").is_empty());
        fs::remove_dir_all(folder).unwrap();
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
