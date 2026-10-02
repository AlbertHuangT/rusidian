use crate::markdown::{Block, BlockKind, MarkdownDocument};
use crate::math::{FONT_SIZE, Formula};
use crate::nvim::{Client as NvimClient, CursorShape, Event as NvimEvent, Grid as NvimGrid};
use crate::settings::ReadingKey;
use crate::theme::{Appearance, Theme};
use crate::update;
use crate::vault::{TreeRow, Vault};
use cargo_packager_updater::Update;
use gpui::{
    AnyElement, AnyWindowHandle, App, Bounds, ClipboardItem, Context, ElementInputHandler,
    EntityInputHandler, FocusHandle, FontStyle, FontWeight, HighlightStyle, Image, ImageFormat,
    KeyBinding, KeyDownEvent, Keystroke, Menu, MenuItem, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, Size, StrikethroughStyle, StyledText, TextLayout,
    UTF16Selection, UnderlineStyle, UniformListScrollHandle, WeakEntity, Window, WindowBounds,
    WindowOptions, actions, canvas, div, img, point, prelude::*, px, rgb, rgba, size, uniform_list,
};
use gpui_platform::application;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

const WINDOW_WIDTH: f32 = 960.0;
const WINDOW_HEIGHT: f32 = 640.0;
const SOURCE_FONT_SIZE: f32 = 14.0;
const SOURCE_LINE_HEIGHT: f32 = 20.0;
const GRID_PADDING: f32 = 12.0;
/// IME composition in the source view, drawn over Neovim's own colors.
const IME_MARKED_BACKGROUND: u32 = 0x7a3b20;
/// TikZ diagrams sit on a white card in every theme.
const WHITE: u32 = 0xffffff;
/// Starts the warning about Neovim's `<Esc>` mapping, so changing the reading key can clear it.
const ESCAPE_MAPPED_PREFIX: &str = "Neovim Normal 模式把 Esc 映射为 ";

actions!(
    rusidian,
    [
        OpenFile,
        OpenFolder,
        OpenSettings,
        Quit,
        NewWindow,
        CloseWindow,
        CloseTab,
        NextTab,
        PreviousTab,
        ToggleSidebar,
        QuickSwitcher,
        SearchVault,
        EnterSourceNormal,
        CopySelection,
        RenameNote
    ]
);

/// App shortcuts. macOS uses Command everywhere; on Linux Control belongs to Neovim in source
/// view, so the shortcuts only apply while reading.
fn key_bindings() -> Vec<KeyBinding> {
    let (modifier, context) = if cfg!(target_os = "macos") {
        ("cmd", None)
    } else {
        ("ctrl", Some("Reading"))
    };
    vec![
        KeyBinding::new(&format!("{modifier}-o"), OpenFile, context),
        KeyBinding::new(&format!("{modifier}-shift-o"), OpenFolder, context),
        KeyBinding::new(&format!("{modifier}-,"), OpenSettings, context),
        KeyBinding::new(&format!("{modifier}-q"), Quit, context),
        KeyBinding::new(&format!("{modifier}-n"), NewWindow, context),
        KeyBinding::new(&format!("{modifier}-w"), CloseTab, context),
        KeyBinding::new(&format!("{modifier}-shift-w"), CloseWindow, context),
        KeyBinding::new("ctrl-tab", NextTab, context),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, context),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-}"
            } else {
                "ctrl-pagedown"
            },
            NextTab,
            context,
        ),
        KeyBinding::new(
            if cfg!(target_os = "macos") {
                "cmd-{"
            } else {
                "ctrl-pageup"
            },
            PreviousTab,
            context,
        ),
        KeyBinding::new(&format!("{modifier}-\\"), ToggleSidebar, context),
        KeyBinding::new(&format!("{modifier}-p"), QuickSwitcher, context),
        KeyBinding::new(&format!("{modifier}-shift-f"), SearchVault, context),
        KeyBinding::new("enter", EnterSourceNormal, Some("Reading")),
        // The source view leaves copying to Neovim.
        KeyBinding::new(&format!("{modifier}-c"), CopySelection, Some("Reading")),
        KeyBinding::new("f2", RenameNote, Some("Reading")),
    ]
}

/// Paths from `file://` URLs the system asks the app to open (Finder, “Open With”).
fn paths_from_urls(urls: Vec<String>) -> Vec<PathBuf> {
    urls.into_iter()
        .filter_map(|url| {
            cargo_packager_updater::url::Url::parse(&url)
                .ok()?
                .to_file_path()
                .ok()
        })
        .collect()
}

pub fn run(initial_path: Option<PathBuf>) {
    let (opened_paths, opened) = async_channel::unbounded::<PathBuf>();
    let app = application();
    app.on_open_urls(move |urls| {
        for path in paths_from_urls(urls) {
            let _ = opened_paths.try_send(path);
        }
    });
    app.run(move |cx: &mut App| {
        crate::fonts::init(cx);
        cx.bind_keys(key_bindings());
        cx.set_menus([
            Menu::new("Rusidian").items([
                MenuItem::action("设置…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("退出 Rusidian", Quit),
            ]),
            Menu::new("文件").items([
                MenuItem::action("新建窗口", NewWindow),
                MenuItem::action("快速打开笔记…", QuickSwitcher),
                MenuItem::action("在笔记中搜索…", SearchVault),
                MenuItem::action("打开文件…", OpenFile),
                MenuItem::action("打开文件夹…", OpenFolder),
                MenuItem::action("重命名笔记…", RenameNote),
                MenuItem::separator(),
                MenuItem::action("关闭标签", CloseTab),
                MenuItem::action("关闭窗口", CloseWindow),
            ]),
            Menu::new("编辑").items([MenuItem::action("复制", CopySelection)]),
            Menu::new("显示").items([
                MenuItem::action("显示/隐藏文件列表", ToggleSidebar),
                MenuItem::separator(),
                MenuItem::action("下一个标签", NextTab),
                MenuItem::action("上一个标签", PreviousTab),
            ]),
        ]);
        cx.on_action(|_: &NewWindow, cx| {
            open_window(None, false, cx);
        });
        // Quitting asks every window's Neovim in turn; one refusal keeps the rest open.
        cx.on_action(|_: &Quit, cx| close_next_window(Some(PendingClose::Quit), cx));

        if open_window(initial_path, true, cx).is_none() {
            eprintln!("failed to open Rusidian window");
            cx.quit();
            return;
        }
        cx.spawn(async move |cx| {
            while let Ok(path) = opened.recv().await {
                cx.update(|cx| open_in_window(path, cx));
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Open a window showing `path`, or the welcome screen. Each window runs its own Neovim.
fn open_window(path: Option<PathBuf>, first: bool, cx: &mut App) -> Option<AnyWindowHandle> {
    // Cascade new windows so they do not hide the ones already open.
    let shift = px(28.0 * cx.windows().len() as f32);
    let mut bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    bounds.origin = point(bounds.origin.x + shift, bounds.origin.y + shift);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        |window, cx| {
            let handle = window.window_handle();
            let app = cx.new(|cx| {
                let mut app = RusidianApp::open_with_vault(path.as_deref());
                if let Some(path) = &path {
                    app.remember_recent(path);
                }
                app.window = Some(handle);
                app.focus_handle = Some(cx.focus_handle());
                app.compile_visuals(cx);
                app.start_nvim(cx);
                if first && app.auto_update && update::is_packaged_app() {
                    app.check_for_updates(true, cx);
                }
                app
            });
            window
                .observe_window_appearance(|window, _| window.refresh())
                .detach();
            app.update(cx, |_, cx| {
                cx.observe_window_activation(window, |this, window, cx| {
                    if window.is_window_active() {
                        // Settings may have changed in another window, and notes elsewhere.
                        this.load_settings();
                        this.refresh_vault(cx);
                        // Pick up edits made outside Rusidian (sync tools, git).
                        if let Some(nvim) = &this.nvim {
                            nvim.check_time();
                        }
                        this.notice_missing_file(cx);
                        cx.notify();
                    }
                })
                .detach();
            });
            let focus = app.read(cx).focus_handle.clone();
            if let Some(focus) = focus {
                window.focus(&focus, cx);
            }
            let close_app = app.downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                close_app
                    .update(cx, |app, cx| {
                        app.request_close(PendingClose::CloseWindow, cx)
                    })
                    .ok();
                false
            });
            app
        },
    );
    handle.ok().map(Into::into)
}

/// The window an opened file goes to: the active one, else any, else a new one.
fn open_in_window(path: PathBuf, cx: &mut App) {
    let target = cx
        .active_window()
        .and_then(|window| window.downcast::<RusidianApp>())
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<RusidianApp>())
        });
    let Some(target) = target else {
        open_window(Some(path), false, cx);
        return;
    };
    target
        .update(cx, |app, window, cx| {
            window.activate_window();
            let vault_root = app
                .vault
                .as_ref()
                .and_then(|vault| path.starts_with(&vault.root).then(|| vault.root.clone()));
            app.request_close(
                PendingClose::Open {
                    path,
                    vault_root,
                    fragment: None,
                },
                cx,
            );
        })
        .ok();
}

/// Continue closing windows after one closed: `then` (quit or restart) goes on to the next
/// window; with none left the app quits, or restarts.
fn close_next_window(then: Option<PendingClose>, cx: &mut App) {
    let next = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<RusidianApp>());
    match (next, then) {
        (Some(next), Some(action)) => {
            next.update(cx, |app, _, cx| app.request_close(action, cx))
                .ok();
        }
        (Some(_), None) => {}
        (None, Some(PendingClose::Restart)) => cx.restart(),
        (None, _) => cx.quit(),
    }
}

/// Bring every window's copy of the settings up to date after one window changed them.
fn sync_settings(cx: &mut App) {
    for window in cx.windows() {
        if let Some(window) = window.downcast::<RusidianApp>() {
            window
                .update(cx, |app, _, cx| {
                    app.load_settings();
                    cx.notify();
                })
                .ok();
        }
    }
}

struct Document {
    file: PathBuf,
    name: SharedString,
    path: SharedString,
    lines: Vec<String>,
    /// Only Markdown files have a reading view; other text files stay in Neovim.
    is_markdown: bool,
    markdown: MarkdownDocument,
    /// The file was on disk when last seen; a new note is not until it is saved.
    on_disk: bool,
    /// Some callout can fold; most notes have none, which spares line motions a scan.
    foldable: bool,
}

impl Document {
    fn parse(&mut self, strict_line_breaks: bool) {
        self.markdown = if self.is_markdown {
            crate::markdown::parse_with_options(&self.lines.join("\n"), strict_line_breaks)
        } else {
            MarkdownDocument { blocks: Vec::new() }
        };
        self.foldable = self
            .markdown
            .blocks
            .iter()
            .any(|block| block.callout_fold.is_some());
    }
}

/// The reading view's children: each drawn block alone, each run of other blocks together.
/// Returns the block ranges, each block's child and each block's top within its child.
fn group_blocks(
    drawn: &[bool],
    heights: &[Pixels],
    gaps: &[Pixels],
) -> (Vec<std::ops::Range<usize>>, Vec<usize>, Vec<Pixels>) {
    let mut children: Vec<std::ops::Range<usize>> = Vec::new();
    let mut child_of = Vec::with_capacity(drawn.len());
    let mut offsets = Vec::with_capacity(drawn.len());
    let mut run_height = px(0.0);
    for (index, &draw) in drawn.iter().enumerate() {
        match children.last_mut() {
            Some(run) if !draw && index > 0 && !drawn[index - 1] => {
                run.end = index + 1;
                offsets.push(run_height);
            }
            _ => {
                children.push(index..index + 1);
                offsets.push(px(0.0));
                run_height = px(0.0);
            }
        }
        run_height += heights[index] + gaps[index];
        child_of.push(children.len() - 1);
    }
    (children, child_of, offsets)
}

/// The reading view's padding above the first block.
const READING_PADDING: f32 = 32.0;
/// The reading column's widest text.
const READING_WIDTH: f32 = 820.0;

/// A block's height before it is first laid out, from its text: close enough to place what is
/// far from the view and to size the scroll bar until it comes near and is measured.
fn estimate_height(block: &Block, width: Pixels) -> Pixels {
    let (font, line, extra) = match &block.kind {
        BlockKind::Heading(level) => {
            let font = match level {
                1 => 32.0,
                2 => 27.0,
                3 => 23.0,
                _ => 19.0,
            };
            (font, font * 1.25, 0.0)
        }
        BlockKind::Code(_) | BlockKind::Html => {
            // Code keeps its lines (it scrolls sideways), in a padded card.
            return px(block.text.lines().count().max(1) as f32 * 22.0 + 56.0);
        }
        BlockKind::Math => return px(64.0),
        BlockKind::Image(_) => return px(240.0),
        BlockKind::Rule => return px(17.0),
        BlockKind::Table { .. } => (16.0, 24.0, 12.0),
        _ => (16.0, 24.0, 0.0),
    };
    let indent = 24.0 * (block.list_depth + block.quote_depth) as f32;
    // In half-em columns: about one for Latin letters, two for CJK.
    let columns = ((f32::from(width) - indent).max(120.0) / (font * 0.5)) as usize;
    let lines: usize = block
        .text
        .split('\n')
        .map(|line| {
            let width: usize = line
                .chars()
                .map(|character| if character.is_ascii() { 1 } else { 2 })
                .sum();
            width.div_ceil(columns).max(1)
        })
        .sum();
    let title = if block.callout_title.is_some() {
        34.0
    } else {
        0.0
    };
    px(lines as f32 * line + extra + title)
}

/// Heights of reading-view blocks. Runs of blocks far from the view are drawn as one stretch
/// of empty space, which keeps long notes responsive: drawing every block on each key press
/// took tens of milliseconds in a 3000-line note, and laying out a 3 MB note at once took
/// half a minute. Blocks are measured once they come near the view, estimated until then.
#[derive(Default)]
struct BlockHeights {
    /// The reading view's width when measured; another width wraps text differently.
    width: Pixels,
    /// Each block's height, without the space below it: as measured, or estimated.
    heights: Vec<Pixels>,
    /// Whether each height was measured since the block or the width last changed.
    measured: Vec<bool>,
    /// Each block's content hash, to keep the heights of blocks an edit left alone.
    keys: Vec<u64>,
    /// The note's text changed: match blocks to the earlier ones by `keys`.
    remap: bool,
    /// Something changed since the last frame was laid out, so it measured stale sizes.
    stale: bool,
    /// Space below each block, outside it.
    gaps: Vec<Pixels>,
    /// Blocks drawn in full in the last frame, whose heights can be measured.
    drawn: Vec<bool>,
    /// The reading view's children in the last frame: each a drawn block, or the run of blocks
    /// one stretch of empty space stands for.
    children: Vec<std::ops::Range<usize>>,
    /// Each block's index in `children`.
    child_of: Vec<usize>,
    /// Each block's top within its child: zero for drawn blocks.
    offsets: Vec<Pixels>,
}

/// Identifies a block's content across edits, for `BlockHeights::keys`.
fn block_key(block: &Block) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(&block.kind).hash(&mut hasher);
    block.text.hash(&mut hasher);
    (block.list_depth, block.quote_depth).hash(&mut hasher);
    hasher.finish()
}

struct RusidianApp {
    document: Option<Document>,
    vault: Option<Vault>,
    error: Option<SharedString>,
    /// Rendered TikZ keyed by block source, so unchanged diagrams survive edits elsewhere.
    tikz: HashMap<String, TikzState>,
    /// TikZ blocks of the last parse while editing, to tell new fences from edits.
    tikz_lines: Vec<TikzBlock>,
    math: HashMap<(String, bool), MathState>,
    view: View,
    nvim: Option<NvimClient>,
    pending_close: Option<PendingClose>,
    grid: NvimGrid,
    nvim_error: Option<SharedString>,
    nvim_warning: Option<SharedString>,
    /// Recommend im-select.nvim for switching input sources between Insert and Normal.
    ime_hint: bool,
    /// Grid size last requested from Neovim; shared with the layout pass that measures it.
    nvim_size: Rc<Cell<(i64, i64)>>,
    cell_size: Size<Pixels>,
    /// Top-left of the grid area from the last layout, for mapping mouse positions to cells.
    grid_origin: Rc<Cell<Point<Pixels>>>,
    mouse_button: Option<&'static str>,
    /// Scroll distance not yet sent to Neovim as a wheel step.
    scroll_remainder: Pixels,
    reading_cursor: ReadingCursor,
    /// The reading cursor as last placed from Neovim's cursor; unchanged means Enter keeps
    /// Neovim's exact position instead of moving it to the mapped character.
    synced_cursor: Option<ReadingCursor>,
    reading_column: Option<usize>,
    /// The horizontal position gj/gk keep while moving between screen lines.
    reading_desired_x: Option<Pixels>,
    /// Text layouts from the last rendered reading view, for gj/gk, paging and reveal.
    fragment_layouts: RefCell<Vec<FragmentLayout>>,
    /// Whether `fragment_layouts` were laid out and prepainted; GPUI panics on unlaid layouts.
    layouts_ready: Rc<Cell<bool>>,
    block_heights: RefCell<BlockHeights>,
    /// Heights of blocks laid out out of view last frame, as (block, height).
    block_measures: Rc<RefCell<Vec<(usize, Pixels)>>>,
    /// The cursor was revealed using estimated heights: reveal it again once drawn, at most
    /// this many more times.
    reveal_again: Cell<u8>,
    reading_pending_g: bool,
    /// `z` was typed; `za` toggles the callout at the cursor.
    reading_pending_z: bool,
    /// Foldable callouts (by quote id) toggled from how they start.
    callout_toggles: HashSet<usize>,
    /// A callout just folded with `za` and the cursor left on its title, which stays folded
    /// until the cursor moves. Anywhere else, the cursor opens a folded callout it is in.
    callout_cursor_parked: Option<(usize, ReadingCursor)>,
    reading_count: Option<usize>,
    reading_find: Option<FindPending>,
    reading_selection: Option<ReadingSelection>,
    mouse_press: Option<MousePress>,
    reading_search: Option<SearchPrompt>,
    last_search: Option<SearchPrompt>,
    reading_scroll: ScrollHandle,
    focus_handle: Option<FocusHandle>,
    marked_text: String,
    marked_selection: std::ops::Range<usize>,
    settings_open: bool,
    switcher: Option<Switcher>,
    switcher_scroll: ScrollHandle,
    /// The window showing this app, to close it once Neovim agrees.
    window: Option<AnyWindowHandle>,
    appearance: Appearance,
    reading_key: ReadingKey,
    /// Neovim's Normal-mode `<Esc>` mapping, which the reading key may shadow.
    escape_mapping: Option<String>,
    recent: Vec<PathBuf>,
    sidebar_visible: bool,
    expanded_folders: HashSet<PathBuf>,
    /// The tree's rows for the current vault and expanded folders, built once until either
    /// changes: large vaults made rebuilding it every frame slow.
    tree_rows: RefCell<Option<Rc<Vec<TreeRow>>>>,
    tree_scroll: UniformListScrollHandle,
    /// The note whose folders were last expanded in the tree.
    revealed_in_tree: Option<PathBuf>,
    /// Resolved at the start of every render from `appearance` and the window's appearance.
    theme: Theme,
    /// A transient message for the status bar, such as a failed link or a completed copy.
    notice: Option<Notice>,
    notice_generation: u64,
    applied_title: Option<String>,
    /// Neovim's buffer has changes that are not written to disk.
    modified: bool,
    /// A heading or block to reveal once the note being opened in Neovim arrives.
    pending_fragment: Option<String>,
    /// Put the reading cursor at the start of the note once its lines arrive from Neovim.
    place_initial_cursor: bool,
    /// The note changed in the source view and is parsed once typing pauses: parsing a long
    /// note on every key would slow typing down.
    parse_pending: bool,
    /// Counts edits waiting for that pause.
    parse_generation: u64,
    /// Notes open in Neovim (its listed buffers), shown as tabs, with unsaved flags.
    open_buffers: Vec<(PathBuf, bool)>,
    /// Remote images the user asked to load, by URL.
    remote_images: HashMap<String, RemoteImage>,
    /// Notes shown with `![[note]]`, read from disk by path.
    embeds: HashMap<PathBuf, EmbeddedNote>,
    /// Link destinations in the note that name no existing file, shown dimmed like Obsidian.
    unresolved_links: HashSet<String>,
    /// Lines of other notes linking to the note shown, for that note's path; `None` while they
    /// are being found.
    backlinks: Option<(PathBuf, Option<Vec<TextHit>>)>,
    /// Vaults whose remote images load automatically.
    remote_image_vaults: Vec<PathBuf>,
    auto_update: bool,
    update_status: UpdateStatus,
    available_update: Option<Update>,
}

struct Notice {
    text: SharedString,
    error: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Reading,
    Source,
}

enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(SharedString),
    Installing(SharedString),
    Installed(SharedString),
    Failed(SharedString),
}

enum PendingClose {
    Open {
        path: PathBuf,
        vault_root: Option<PathBuf>,
        /// A heading or `^block` to reveal after opening.
        fragment: Option<String>,
    },
    /// Close this window; the app quits when it was the last one.
    CloseWindow,
    /// Close every window, then quit.
    Quit,
    /// Close every window, then restart (after installing an update).
    Restart,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct ReadingCursor {
    block: usize,
    offset: usize,
}

#[derive(Clone, Copy)]
struct ReadingSelection {
    anchor: ReadingCursor,
    linewise: bool,
}

/// The left button went down on the reading view: a click, or the start of a drag that
/// selects text.
#[derive(Clone, Copy)]
struct MousePress {
    at: ReadingCursor,
    /// Pressed on the text itself, where a link or tag is followed once released in place.
    on_text: bool,
    dragged: bool,
}

#[derive(Clone, Copy)]
struct FindPending {
    forward: bool,
    till: bool,
}

#[derive(Clone)]
struct SearchPrompt {
    query: String,
    forward: bool,
}

/// The quick switcher: find a note by name, or by text in it, and open it.
#[derive(Default)]
struct Switcher {
    query: String,
    selected: usize,
    /// Searching the notes' text instead of their names.
    text: bool,
    /// Lines containing the query, once the background search for it finished.
    hits: Vec<TextHit>,
    /// For the query `#`: every tag and how many notes use it.
    tags: Vec<(String, usize)>,
    /// Renaming the note shown: the query is its new name.
    rename: bool,
    /// Counts searches, so a slower earlier one cannot replace newer results.
    generation: u64,
}

/// A line of a note containing the searched text.
#[derive(Clone)]
struct TextHit {
    path: PathBuf,
    name: String,
    folder: String,
    /// Zero-based source line.
    line: usize,
    text: String,
}

/// Opening a note at a line goes through the fragment used for headings and blocks; NUL never
/// appears in fragments written in Markdown.
const LINE_FRAGMENT: &str = "\u{0}line ";

fn line_fragment(line: usize) -> String {
    format!("{LINE_FRAGMENT}{line}")
}

fn line_from_fragment(fragment: &str) -> Option<usize> {
    fragment.strip_prefix(LINE_FRAGMENT)?.parse().ok()
}

/// Most text hits gathered per search.
const SEARCH_LIMIT: usize = 200;

/// Lines of `files` containing `query`, ignoring case, in file order; at most `limit`.
fn search_notes(files: &[PathBuf], root: Option<&Path>, query: &str, limit: usize) -> Vec<TextHit> {
    let needle = query.trim().to_lowercase();
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    for path in files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let relative = root
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path);
        // A `#tag` query also finds the tag in the front matter's tags, like Obsidian.
        let tag = needle
            .strip_prefix('#')
            .filter(|tag| !tag.is_empty() && !tag.contains(char::is_whitespace));
        let mut front_matter = false;
        let mut in_tags = false;
        for (line, content) in text.lines().enumerate() {
            let tagged = if line == 0 && content.trim() == "---" {
                front_matter = true;
                false
            } else if front_matter && content.trim() == "---" {
                front_matter = false;
                false
            } else {
                front_matter
                    && tag.is_some_and(|tag| front_matter_has_tag(content, &mut in_tags, tag))
            };
            if !tagged && !content.to_lowercase().contains(&needle) {
                continue;
            }
            hits.push(TextHit {
                path: path.clone(),
                name: relative
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                folder: relative
                    .parent()
                    .map(|folder| folder.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                line,
                text: hit_excerpt(content, &needle),
            });
            if hits.len() >= limit {
                return hits;
            }
        }
    }
    hits
}

/// The text search's query that lists every tag instead of searching.
const ALL_TAGS: &str = "#";

/// Every tag in `files`, with how many notes use it, most used first: tags in the text (not
/// in code or comments) and in the front matter's `tags`. Tags differing only in case are one.
fn vault_tags(files: &[PathBuf]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    for path in files.iter().filter(|path| crate::vault::is_markdown(path)) {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut seen = HashSet::new();
        for block in crate::markdown::parse_with_options(&text, false).blocks {
            let tags: Vec<String> = if block.kind == BlockKind::Metadata {
                parse_properties(&block.text)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(Property::is_tags)
                    .flat_map(|property| property.values)
                    .map(|value| value.trim().trim_start_matches('#').to_owned())
                    .collect()
            } else {
                block
                    .spans
                    .iter()
                    .filter(|span| span.tag)
                    .map(|span| {
                        block.text[span.range.clone()]
                            .trim_start_matches('#')
                            .to_owned()
                    })
                    .collect()
            };
            for tag in tags.into_iter().filter(|tag| !tag.is_empty()) {
                let key = tag.to_lowercase();
                if seen.insert(key.clone()) {
                    counts.entry(key).or_insert((tag, 0)).1 += 1;
                }
            }
        }
    }
    let mut tags: Vec<(String, usize)> = counts.into_values().collect();
    tags.sort_by(|(a, first), (b, second)| {
        second
            .cmp(first)
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    tags
}

/// Lines of `files` (other notes of the vault at `root`) that link to `note`: wikilinks and
/// embeds by name or vault path, as Obsidian resolves them, and Markdown links by file path.
fn find_backlinks(files: &[PathBuf], root: &Path, note: &Path) -> Vec<TextHit> {
    let stem = note
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let relative = note
        .strip_prefix(root)
        .unwrap_or(note)
        .with_extension("")
        .to_string_lossy()
        .to_lowercase();
    let wiki_target = |target: &str| {
        let target = crate::vault::split_fragment(target).0.trim();
        let target = target.strip_suffix(".md").unwrap_or(target).to_lowercase();
        target == stem || target == relative || relative.ends_with(&format!("/{target}"))
    };
    let mut hits = Vec::new();
    for path in files.iter().filter(|path| *path != note) {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let folder = path.parent().unwrap_or(root);
        let links_here = |line: &str| {
            line.split("[[").skip(1).any(|rest| {
                rest.split_once("]]")
                    .is_some_and(|(inside, _)| wiki_target(inside.split('|').next().unwrap_or("")))
            }) || line.split("](").skip(1).any(|rest| {
                let destination = rest.split(')').next().unwrap_or("");
                let destination = crate::vault::percent_decode(
                    crate::vault::split_fragment(destination.trim()).0,
                );
                !destination.is_empty()
                    && !destination.contains("://")
                    && folder
                        .join(&destination)
                        .canonicalize()
                        .is_ok_and(|target| target == note)
            })
        };
        let relative_path = path.strip_prefix(root).unwrap_or(path);
        for (line, content) in text.lines().enumerate() {
            if links_here(content) {
                hits.push(TextHit {
                    path: path.clone(),
                    name: relative_path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    folder: relative_path
                        .parent()
                        .map(|folder| folder.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    line,
                    text: hit_excerpt(content, "[["),
                });
            }
        }
    }
    hits
}

/// Whether a front-matter line lists `tag` (lowercase, without `#`) or a tag nested in it,
/// in `tags: [a, b]`, `tags: a` or a `- a` item under `tags:`. `in_tags` tracks the latter.
fn front_matter_has_tag(line: &str, in_tags: &mut bool, tag: &str) -> bool {
    let trimmed = line.trim();
    let values: Vec<&str> = if let Some(item) = trimmed.strip_prefix("- ") {
        if !*in_tags {
            return false;
        }
        vec![item]
    } else if !line.starts_with(char::is_whitespace)
        && let Some((key, value)) = line.split_once(':')
    {
        *in_tags = matches!(key.trim().to_lowercase().as_str(), "tags" | "tag");
        if !*in_tags {
            return false;
        }
        let value = value.trim();
        value
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(value)
            .split(',')
            .collect()
    } else {
        return false;
    };
    values.iter().any(|value| {
        let value = value
            .trim()
            .trim_matches(|character| character == '"' || character == '\'')
            .trim_start_matches('#')
            .to_lowercase();
        value == tag || value.starts_with(&format!("{tag}/"))
    })
}

/// The part of a long line around the match of `needle` (lowercase).
fn hit_excerpt(line: &str, needle: &str) -> String {
    let line = line.trim();
    let characters: Vec<char> = line.chars().collect();
    if characters.len() <= 100 {
        return line.to_owned();
    }
    let lower: Vec<char> = line
        .chars()
        .map(|character| character.to_lowercase().next().unwrap_or(character))
        .collect();
    let wanted: Vec<char> = needle.chars().collect();
    let at = (0..lower.len())
        .find(|&index| lower[index..].starts_with(&wanted))
        .unwrap_or(0);
    let start = at.saturating_sub(30);
    let end = (start + 100).min(characters.len());
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        characters[start..end].iter().collect::<String>(),
        if end < characters.len() { "…" } else { "" }
    )
}

/// A note the quick switcher offers: its path, name and folder, and the alias that matched.
struct SwitcherItem {
    path: PathBuf,
    name: String,
    folder: String,
    alias: Option<String>,
    /// A note not written yet, named by the query: choosing it creates it.
    create: bool,
}

/// Most rows the quick switcher lists; it scrolls past the first dozen.
const SWITCHER_ROWS: usize = 50;

/// How well `query` matches `text` (a note's folder and name), ignoring case and spaces: its
/// characters must appear in order. Consecutive characters, word starts and matches in the
/// name (from character `name_start` on) score higher; `None` when it does not match.
fn fuzzy_score(query: &str, text: &str, name_start: usize) -> Option<i64> {
    let text: Vec<char> = text
        .chars()
        .map(|character| character.to_lowercase().next().unwrap_or(character))
        .collect();
    let mut score = 0;
    let mut from = 0;
    let mut previous: Option<usize> = None;
    for wanted in query
        .chars()
        .filter(|character| !character.is_whitespace())
        .map(|character| character.to_lowercase().next().unwrap_or(character))
    {
        let found = (from..text.len()).find(|&index| text[index] == wanted)?;
        score += 1;
        if previous.is_some_and(|previous| previous + 1 == found) {
            score += 5;
        }
        if found >= name_start {
            score += 2;
        }
        if found == 0 || matches!(text[found - 1], '/' | ' ' | '-' | '_' | '.') {
            score += 3;
        }
        previous = Some(found);
        from = found + 1;
    }
    Some(score * 100 - text.len() as i64)
}

#[derive(Clone, Copy)]
enum WordMotion {
    Next,
    Previous,
    End,
}

enum MathState {
    Loading,
    Ready(Arc<Formula>),
    Failed(SharedString),
}

enum TikzState {
    Loading,
    Ready(Arc<Image>),
    Failed(SharedString),
}

impl RusidianApp {
    fn empty() -> Self {
        let settings = crate::settings::load();
        Self {
            document: None,
            vault: None,
            error: None,
            tikz: HashMap::new(),
            tikz_lines: Vec::new(),
            math: HashMap::new(),
            view: View::Reading,
            nvim: None,
            pending_close: None,
            grid: NvimGrid::default(),
            nvim_error: None,
            nvim_warning: None,
            ime_hint: false,
            nvim_size: Rc::new(Cell::new((120, 40))),
            cell_size: size(px(SOURCE_FONT_SIZE * 0.6), px(SOURCE_LINE_HEIGHT)),
            grid_origin: Rc::new(Cell::new(point(px(0.0), px(0.0)))),
            mouse_button: None,
            scroll_remainder: px(0.0),
            reading_cursor: ReadingCursor::default(),
            synced_cursor: None,
            reading_column: None,
            reading_desired_x: None,
            fragment_layouts: RefCell::new(Vec::new()),
            layouts_ready: Rc::new(Cell::new(false)),
            block_heights: RefCell::new(BlockHeights::default()),
            block_measures: Rc::new(RefCell::new(Vec::new())),
            reveal_again: Cell::new(0),
            reading_pending_g: false,
            reading_pending_z: false,
            callout_toggles: HashSet::new(),
            callout_cursor_parked: None,
            reading_count: None,
            reading_find: None,
            reading_selection: None,
            mouse_press: None,
            reading_search: None,
            last_search: None,
            reading_scroll: ScrollHandle::new(),
            focus_handle: None,
            marked_text: String::new(),
            marked_selection: 0..0,
            settings_open: false,
            switcher: None,
            switcher_scroll: ScrollHandle::new(),
            window: None,
            appearance: settings.appearance,
            reading_key: settings.reading_key,
            escape_mapping: None,
            recent: settings.recent,
            sidebar_visible: true,
            expanded_folders: HashSet::new(),
            tree_rows: RefCell::new(None),
            tree_scroll: UniformListScrollHandle::new(),
            revealed_in_tree: None,
            theme: Theme::DARK,
            notice: None,
            notice_generation: 0,
            applied_title: None,
            modified: false,
            pending_fragment: None,
            place_initial_cursor: false,
            parse_pending: false,
            parse_generation: 0,
            open_buffers: Vec::new(),
            remote_images: HashMap::new(),
            embeds: HashMap::new(),
            unresolved_links: HashSet::new(),
            backlinks: None,
            remote_image_vaults: settings.remote_image_vaults,
            auto_update: settings.auto_update,
            update_status: UpdateStatus::Idle,
            available_update: None,
        }
    }

    fn open(path: Option<&Path>) -> Self {
        let mut app = Self::empty();
        let Some(path) = path else {
            return app;
        };

        if path.is_dir() {
            match Vault::open(path) {
                Ok(vault) => {
                    // Continue with the vault's most recently opened note, like Obsidian.
                    let first = app
                        .recent
                        .iter()
                        .find(|recent| vault.files.contains(recent))
                        .or_else(|| vault.files.first())
                        .cloned();
                    app = Self::open(first.as_deref());
                    app.attach_vault(vault);
                }
                Err(error) => {
                    app.error = Some(format!("无法打开文件夹 {}：{error}", path.display()).into());
                }
            }
            return app;
        }

        match read_text(path) {
            Ok(content) => {
                let path = &path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let mut document = Document {
                    file: path.to_path_buf(),
                    name: path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                    path: path.to_string_lossy().into_owned().into(),
                    lines: source_lines(&content),
                    is_markdown: crate::vault::is_markdown(path),
                    markdown: MarkdownDocument { blocks: Vec::new() },
                    on_disk: true,
                    foldable: false,
                };
                document.parse(false);
                if !document.is_markdown {
                    app.view = View::Source;
                }
                app.reading_cursor = initial_cursor(&document.markdown.blocks);
                app.document = Some(document);
            }
            Err(error) => {
                app.error = Some(format!("无法打开 {}：{error}", path.display()).into());
            }
        }
        app
    }

    fn attach_vault(&mut self, vault: Vault) {
        self.tree_rows.replace(None);
        if let Some(document) = &mut self.document {
            document.parse(vault.settings.strict_line_breaks);
        }
        self.vault = Some(vault);
    }

    fn pending_tikz(&mut self) -> Vec<String> {
        // Diagrams that failed only for want of network are tried again.
        self.tikz.retain(|_, state| {
            !matches!(state, TikzState::Failed(error) if crate::tikz::is_offline_failure(error))
        });
        let sources: HashSet<_> = self
            .shown_blocks()
            .filter(|block| is_tikz(block))
            .map(|block| block.text.clone())
            .collect();
        self.tikz.retain(|source, _| sources.contains(source));
        sources
            .into_iter()
            .filter(|source| {
                if self.tikz.contains_key(source) {
                    return false;
                }
                self.tikz.insert(source.clone(), TikzState::Loading);
                true
            })
            .collect()
    }

    fn compile_tikz(&mut self, cx: &mut Context<Self>) {
        let sources = self.pending_tikz();
        self.compile_tikz_sources(sources, cx);
    }

    /// While editing: compile diagrams whose fence was just closed, and edited diagrams once
    /// the cursor has left them (PRODUCT.md's TikZ timing).
    fn compile_finished_tikz(&mut self, cx: &mut Context<Self>) {
        let Some(document) = &self.document else {
            return;
        };
        // Runs on every cursor move while editing; most notes have no diagrams.
        if !document.markdown.blocks.iter().any(is_tikz) {
            self.tikz_lines.clear();
            return;
        }
        let source = document.lines.join("\n");
        let current = tikz_blocks(&document.markdown.blocks, &source);
        let ready = tikz_ready_to_compile(
            &self.tikz_lines,
            &current,
            self.grid.buffer_cursor_line,
            |source| self.tikz.contains_key(source),
        );
        self.tikz_lines = current;
        for source in &ready {
            self.tikz.insert(source.clone(), TikzState::Loading);
        }
        if !ready.is_empty() {
            self.invalidate_block_heights();
        }
        self.compile_tikz_sources(ready, cx);
    }

    fn compile_tikz_sources(&mut self, sources: Vec<String>, cx: &mut Context<Self>) {
        if sources.is_empty() {
            return;
        }
        let preamble: Arc<str> =
            crate::tikz::preamble(self.vault.as_ref().map(|vault| vault.root.as_path())).into();
        for source in sources {
            let executor = cx.background_executor().clone();
            let preamble = preamble.clone();
            cx.spawn(async move |this, cx| {
                let input = source.clone();
                let result = executor
                    .spawn(async move { crate::tikz::compile(&input, &preamble) })
                    .await;
                this.update(cx, |this, cx| {
                    if !this.tikz.contains_key(&source) {
                        return;
                    }
                    let state = match result {
                        Ok(bytes) => {
                            TikzState::Ready(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
                        }
                        Err(error) => TikzState::Failed(error.into()),
                    };
                    this.tikz.insert(source, state);
                    this.invalidate_block_heights();
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn pending_math(&mut self) -> Vec<(String, bool)> {
        let keys: HashSet<_> = self
            .shown_blocks()
            .flat_map(|block| {
                (block.kind == BlockKind::Math)
                    .then(|| (block.text.clone(), true))
                    .into_iter()
                    .chain(
                        block
                            .maths
                            .iter()
                            .map(|formula| (formula.source.clone(), false)),
                    )
            })
            .collect();
        // Only the current document owns layouts. Unchanged/duplicate formulas share one result.
        self.math.retain(|key, _| keys.contains(key));
        keys.into_iter()
            .filter(|key| {
                if self.math.contains_key(key) {
                    return false;
                }
                self.math.insert(key.clone(), MathState::Loading);
                true
            })
            .collect()
    }

    fn compile_math(&mut self, cx: &mut Context<Self>) {
        for key in self.pending_math() {
            let executor = cx.background_executor().clone();
            cx.spawn(async move |this, cx| {
                let input = key.clone();
                let result = executor
                    .spawn(async move { Formula::parse(&input.0, input.1) })
                    .await;
                this.update(cx, |this, cx| {
                    if !this.math.contains_key(&key) {
                        return;
                    }
                    this.invalidate_block_heights();
                    this.math.insert(
                        key,
                        match result {
                            Ok(formula) => MathState::Ready(Arc::new(formula)),
                            Err(error) => MathState::Failed(error.into()),
                        },
                    );
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn compile_visuals(&mut self, cx: &mut Context<Self>) {
        // First: embedded notes' diagrams and formulas compile with the note's own.
        self.load_embeds();
        self.find_unresolved_links();
        self.load_backlinks(false, cx);
        self.compile_tikz(cx);
        self.compile_math(cx);
        self.load_allowed_remote_images(cx);
    }

    /// Read the notes this note embeds, again whenever they changed on disk.
    fn load_embeds(&mut self) {
        let Some(document) = &self.document else {
            self.embeds.clear();
            return;
        };
        let links = Links {
            note: &document.file,
            vault: self.vault.as_ref(),
        };
        let wanted: HashSet<PathBuf> = document
            .markdown
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Image(source) => links.embedded_note(source),
                _ => None,
            })
            .collect();
        self.embeds.retain(|path, _| wanted.contains(path));
        let strict_line_breaks = self
            .vault
            .as_ref()
            .is_some_and(|vault| vault.settings.strict_line_breaks);
        for path in wanted {
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok();
            if modified.is_some()
                && self
                    .embeds
                    .get(&path)
                    .is_some_and(|embed| embed.modified == modified)
            {
                continue;
            }
            let markdown = match std::fs::metadata(&path) {
                Ok(metadata) if metadata.len() > EMBED_SIZE_LIMIT => {
                    Err(format!("{} 太大，无法嵌入", display_name(&path)))
                }
                _ => std::fs::read_to_string(&path)
                    .map(|text| crate::markdown::parse_with_options(&text, strict_line_breaks))
                    .map_err(|error| format!("无法读取 {}：{error}", display_name(&path))),
            };
            self.embeds
                .insert(path, EmbeddedNote { modified, markdown });
            self.invalidate_block_heights();
        }
    }

    /// Find the vault's notes linking to the note shown, in the background: once per note, or
    /// again when `refresh`, keeping the earlier list until the new one is ready.
    fn load_backlinks(&mut self, refresh: bool, cx: &mut Context<Self>) {
        let (Some(vault), Some(document)) = (&self.vault, &self.document) else {
            self.backlinks = None;
            return;
        };
        let note = document
            .file
            .canonicalize()
            .unwrap_or_else(|_| document.file.clone());
        let known = self
            .backlinks
            .as_ref()
            .is_some_and(|(path, _)| *path == note);
        if !document.is_markdown || (known && !refresh) {
            return;
        }
        if !known {
            self.backlinks = Some((note.clone(), None));
        }
        let files = vault.files.clone();
        let root = vault.root.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let target = note.clone();
            let hits = executor
                .spawn(async move { find_backlinks(&files, &root, &target) })
                .await;
            this.update(cx, |this, cx| {
                if let Some((path, found)) = &mut this.backlinks
                    && *path == note
                {
                    *found = Some(hits);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn find_unresolved_links(&mut self) {
        self.unresolved_links.clear();
        let Some(document) = &self.document else {
            return;
        };
        for link in document
            .markdown
            .blocks
            .iter()
            .flat_map(|block| &block.links)
        {
            let (target, _) = crate::vault::split_fragment(&link.destination);
            if !target.is_empty()
                && !is_external_link(&link.destination)
                && !self.unresolved_links.contains(&link.destination)
                && crate::vault::resolve_target(&document.file, self.vault.as_ref(), target)
                    .is_none()
            {
                self.unresolved_links.insert(link.destination.clone());
            }
        }
    }

    /// Blocks of the note and of the notes it embeds, whose diagrams and formulas are shown.
    fn shown_blocks(&self) -> impl Iterator<Item = &Block> {
        self.document
            .iter()
            .flat_map(|document| document.markdown.blocks.iter())
            .chain(
                self.embeds
                    .values()
                    .filter_map(|embed| embed.markdown.as_ref().ok())
                    .flat_map(|markdown| markdown.blocks.iter()),
            )
    }

    /// Remote image URLs in the current note.
    fn remote_sources(&self) -> Vec<String> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        document
            .markdown
            .blocks
            .iter()
            .flat_map(|block| {
                let own = match &block.kind {
                    BlockKind::Image(source) => Some(source.clone()),
                    _ => None,
                };
                own.into_iter()
                    .chain(block.images.iter().map(|image| image.source.clone()))
            })
            .filter(|source| crate::remote::is_remote(source))
            .collect()
    }

    /// Load the note's remote images when its vault allows them automatically.
    fn load_allowed_remote_images(&mut self, cx: &mut Context<Self>) {
        let allowed = self
            .vault
            .as_ref()
            .is_some_and(|vault| self.remote_image_vaults.contains(&vault.root));
        if allowed {
            for url in self.remote_sources() {
                self.load_remote_image(url, cx);
            }
        }
    }

    fn load_remote_image(&mut self, url: String, cx: &mut Context<Self>) {
        if matches!(
            self.remote_images.get(&url),
            Some(RemoteImage::Loading | RemoteImage::Ready(_))
        ) {
            return;
        }
        self.remote_images.insert(url.clone(), RemoteImage::Loading);
        self.invalidate_block_heights();
        cx.notify();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let request = url.clone();
            let result = executor
                .spawn(async move { crate::remote::fetch_image(&request) })
                .await;
            this.update(cx, |this, cx| {
                let state = match result {
                    Ok((format, bytes)) => {
                        RemoteImage::Ready(Arc::new(Image::from_bytes(format, bytes)))
                    }
                    Err(error) => RemoteImage::Failed(error.into()),
                };
                this.remote_images.insert(url, state);
                this.invalidate_block_heights();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Remember that this vault's remote images may load, then load the current note's.
    fn allow_vault_remote_images(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.vault.as_ref().map(|vault| vault.root.clone()) else {
            return;
        };
        match crate::settings::update(|settings| {
            if !settings.remote_image_vaults.contains(&root) {
                settings.remote_image_vaults.push(root.clone());
            }
        }) {
            Ok(settings) => {
                self.remote_image_vaults = settings.remote_image_vaults;
                self.load_allowed_remote_images(cx);
            }
            Err(error) => self.show_notice(error, true, cx),
        }
    }

    fn show_notice(&mut self, text: impl Into<SharedString>, error: bool, cx: &mut Context<Self>) {
        self.notice = Some(Notice {
            text: text.into(),
            error,
        });
        self.notice_generation += 1;
        let generation = self.notice_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(6))
                .await;
            this.update(cx, |this, cx| {
                if this.notice_generation == generation && this.notice.take().is_some() {
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = true;
        self.switcher = None;
        cx.notify();
    }

    fn open_switcher(&mut self, _: &QuickSwitcher, _: &mut Window, cx: &mut Context<Self>) {
        self.show_switcher(false, cx);
    }

    fn open_text_search(&mut self, _: &SearchVault, _: &mut Window, cx: &mut Context<Self>) {
        self.show_switcher(true, cx);
    }

    fn show_switcher(&mut self, text: bool, cx: &mut Context<Self>) {
        self.settings_open = false;
        self.marked_text.clear();
        self.switcher = Some(Switcher {
            text,
            ..Switcher::default()
        });
        cx.notify();
    }

    /// Ask for a new name for the note shown, starting from its current one.
    fn show_rename(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self
            .document
            .as_ref()
            .filter(|document| document.is_markdown)
        else {
            return;
        };
        let name = document
            .file
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.settings_open = false;
        self.marked_text.clear();
        self.switcher = Some(Switcher {
            query: name,
            rename: true,
            ..Switcher::default()
        });
        cx.notify();
    }

    /// Rename the note shown to `name` beside it, and update the links to it in the vault's
    /// notes, as Obsidian does. Notes with unsaved changes here are left alone and named.
    fn rename_note(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        let Some(document) = &self.document else {
            return;
        };
        let old = document.file.clone();
        if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            self.show_notice(
                "笔记名称不能为空，不能以 . 开头，也不能包含 / 或 \\",
                true,
                cx,
            );
            return;
        }
        if !document.on_disk {
            self.show_notice("这篇笔记还没有保存：先用 :w 保存，再重命名", true, cx);
            return;
        }
        if self.modified {
            self.show_notice("这篇笔记有未保存的修改：先用 :w 保存，再重命名", true, cx);
            return;
        }
        let Some(nvim) = &self.nvim else {
            self.show_notice("Neovim 未运行，无法重命名", true, cx);
            return;
        };
        let new = old.with_file_name(format!("{name}.md"));
        if new == old {
            return;
        }
        // Only a change of case may land on the same file (on a case-insensitive disk).
        let same = new.exists() && same_file(&new, &old);
        if new.exists() && !same {
            self.show_notice(format!("已有同名笔记：{name}.md"), true, cx);
            return;
        }
        // Links are found before the note moves, while they still resolve to it.
        let stem = old
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let files = match &self.vault {
            Some(vault) => vault
                .files
                .iter()
                .filter(|path| crate::vault::is_markdown(path))
                .cloned()
                .collect(),
            None => vec![old.clone()],
        };
        let mut rewrites = Vec::new();
        let mut skipped = Vec::new();
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !text.to_lowercase().contains(&stem) {
                continue;
            }
            let (text, links) =
                crate::vault::retarget_links(&text, &path, self.vault.as_ref(), &old, name);
            if links == 0 {
                continue;
            }
            if self
                .open_buffers
                .iter()
                .any(|(open, modified)| *modified && same_file(open, &path))
            {
                skipped.push(display_name(&path));
                continue;
            }
            rewrites.push((path, text, links));
        }
        if let Err(error) = std::fs::rename(&old, &new) {
            self.show_notice(format!("无法重命名：{error}"), true, cx);
            return;
        }
        // Moved on purpose: no warning that it went missing before Neovim follows.
        if let Some(document) = &mut self.document {
            document.on_disk = false;
        }
        let (mut notes, mut links) = (0, 0);
        for (path, text, count) in rewrites {
            // The note's own links to itself moved with it.
            let path = if path == old { new.clone() } else { path };
            match write_replacing(&path, &text) {
                Ok(()) => {
                    notes += 1;
                    links += count;
                }
                Err(_) => skipped.push(display_name(&path)),
            }
        }
        // Neovim follows: the renamed file in place of the old buffer, other notes reloaded.
        nvim.edit(new.clone());
        // Where only the case changed, Neovim sees one buffer for both names: keep it.
        if !same {
            nvim.close_buffer(old);
        }
        nvim.check_time();
        self.switcher = None;
        self.refresh_vault(cx);
        let mut message = format!("已重命名为 {name}");
        if links > 0 {
            message.push_str(&format!("，更新了 {notes} 篇笔记中的 {links} 个链接"));
        }
        if !skipped.is_empty() {
            message.push_str(&format!(
                "；{} 有未保存的修改或无法写入，其中的链接未更新",
                skipped.join("、")
            ));
        }
        self.show_notice(message, !skipped.is_empty(), cx);
    }

    /// The notes the switcher searches: the vault's, or recent files without a vault.
    fn switcher_files(&self) -> Vec<PathBuf> {
        match &self.vault {
            Some(vault) => vault.files.clone(),
            None => self
                .recent
                .iter()
                .filter(|path| path.is_file() && crate::vault::is_markdown(path))
                .cloned()
                .collect(),
        }
    }

    /// Rows the switcher lists now.
    fn switcher_rows(&self) -> usize {
        match &self.switcher {
            Some(switcher) if switcher.rename => 0,
            Some(switcher) if switcher.text && switcher.query.trim() == ALL_TAGS => {
                switcher.tags.len()
            }
            Some(switcher) if switcher.text => switcher.hits.len(),
            Some(_) => self.switcher_items().len(),
            None => 0,
        }
        .min(SWITCHER_ROWS)
    }

    /// Search the notes' text for the query in the background, shortly after typing pauses.
    fn search_text(&mut self, cx: &mut Context<Self>) {
        let Some(switcher) = self.switcher.as_mut().filter(|switcher| switcher.text) else {
            return;
        };
        switcher.generation += 1;
        switcher.selected = 0;
        let generation = switcher.generation;
        let query = switcher.query.clone();
        if query.trim().is_empty() {
            switcher.hits.clear();
            return;
        }
        let files = self.switcher_files();
        let root = self.vault.as_ref().map(|vault| vault.root.clone());
        let executor = cx.background_executor().clone();
        if query.trim() == ALL_TAGS {
            cx.spawn(async move |this, cx| {
                let tags = executor.spawn(async move { vault_tags(&files) }).await;
                this.update(cx, |this, cx| {
                    if let Some(switcher) = &mut this.switcher
                        && switcher.generation == generation
                    {
                        switcher.tags = tags;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
            return;
        }
        cx.spawn(async move |this, cx| {
            executor.timer(std::time::Duration::from_millis(120)).await;
            let current = |this: &RusidianApp| {
                this.switcher
                    .as_ref()
                    .is_some_and(|switcher| switcher.generation == generation)
            };
            if !this.read_with(cx, |this, _| current(this)).unwrap_or(false) {
                return;
            }
            let hits = executor
                .spawn(async move { search_notes(&files, root.as_deref(), &query, SEARCH_LIMIT) })
                .await;
            this.update(cx, |this, cx| {
                if current(this)
                    && let Some(switcher) = &mut this.switcher
                {
                    switcher.hits = hits;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Notes matching the switcher's query, best first: the vault's notes, or recent files
    /// without a vault. With no query, recent notes come first.
    fn switcher_items(&self) -> Vec<SwitcherItem> {
        let query = self
            .switcher
            .as_ref()
            .map(|switcher| switcher.query.as_str())
            .unwrap_or_default();
        let root = self.vault.as_ref().map(|vault| vault.root.as_path());
        let candidates: Vec<&PathBuf> = match &self.vault {
            Some(vault) => vault.files.iter().collect(),
            None => self.recent.iter().filter(|path| path.is_file()).collect(),
        };
        let item = |path: &PathBuf| {
            let relative = root
                .and_then(|root| path.strip_prefix(root).ok())
                .unwrap_or(path);
            // Like Obsidian, only notes drop their extension.
            let name = if crate::vault::is_markdown(path) {
                relative.file_stem()
            } else {
                relative.file_name()
            };
            SwitcherItem {
                path: path.clone(),
                name: name.unwrap_or_default().to_string_lossy().into_owned(),
                folder: relative
                    .parent()
                    .map(|folder| folder.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                alias: None,
                create: false,
            }
        };
        if query.trim().is_empty() {
            let recent = self.recent.iter().filter(|path| candidates.contains(path));
            let rest = candidates
                .iter()
                .copied()
                .filter(|path| !self.recent.contains(path));
            return recent.chain(rest).map(item).collect();
        }
        let mut scored: Vec<(i64, SwitcherItem)> = candidates
            .into_iter()
            .map(item)
            .filter_map(|item| {
                // Inside a vault the folder helps tell notes apart; recent files outside one
                // have absolute folders, which would match nearly anything.
                let text = if item.folder.is_empty() || root.is_none() {
                    item.name.clone()
                } else {
                    format!("{}/{}", item.folder, item.name)
                };
                let name_start = text.chars().count() - item.name.chars().count();
                // A note also matches by its front-matter aliases, whichever scores best.
                let mut best = fuzzy_score(query, &text, name_start).map(|score| (score, None));
                let aliases = self
                    .vault
                    .as_ref()
                    .and_then(|vault| vault.aliases.get(&item.path));
                for alias in aliases.into_iter().flatten() {
                    if let Some(score) = fuzzy_score(query, alias, 0)
                        && best.as_ref().is_none_or(|(best, _)| score > *best)
                    {
                        best = Some((score, Some(alias.clone())));
                    }
                }
                best.map(|(score, alias)| (score, SwitcherItem { alias, ..item }))
            })
            .collect();
        scored.sort_by(|(a, first), (b, second)| {
            b.cmp(a)
                .then_with(|| first.name.len().cmp(&second.name.len()))
                .then_with(|| first.path.cmp(&second.path))
        });
        let mut items: Vec<SwitcherItem> = scored.into_iter().map(|(_, item)| item).collect();
        if let Some(new) = self.switcher_new_note(query) {
            // After the matches, but still among the rows listed.
            items.insert(items.len().min(SWITCHER_ROWS - 1), new);
        }
        items
    }

    /// Like Obsidian's switcher, offer to create the note typed when no note has that name.
    fn switcher_new_note(&self, query: &str) -> Option<SwitcherItem> {
        let typed = query.trim().trim_end_matches(".md").trim_end_matches('/');
        let (folder, name) = typed.rsplit_once('/').unwrap_or(("", typed));
        // Note names compare without case, as in Obsidian's links.
        let root = self.vault.as_ref().map(|vault| vault.root.as_path());
        let same =
            |a: &std::ffi::OsStr, b: &str| a.to_string_lossy().to_lowercase() == b.to_lowercase();
        let mut files = match &self.vault {
            Some(vault) => vault.files.iter(),
            None => self.recent.iter(),
        };
        if files.any(|path| {
            let relative = root
                .and_then(|root| path.strip_prefix(root).ok())
                .unwrap_or(path);
            crate::vault::is_markdown(path)
                && relative.file_stem().is_some_and(|stem| same(stem, name))
                && (folder.is_empty()
                    || relative
                        .parent()
                        .is_some_and(|parent| same(parent.as_os_str(), folder)))
        }) {
            return None;
        }
        // Where Obsidian puts new notes; beside the open note without a vault.
        let note = match (&self.document, &self.vault) {
            (Some(document), _) => document.file.clone(),
            (None, Some(vault)) => vault.root.join("_"),
            (None, None) => return None,
        };
        let path = crate::vault::new_note_path(&note, self.vault.as_ref(), typed)
            .filter(|path| path.parent().is_some_and(Path::is_dir) && !path.exists())?;
        let shown = self
            .vault
            .as_ref()
            .and_then(|vault| path.parent()?.strip_prefix(&vault.root).ok())
            .map(|folder| folder.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                path.parent()
                    .map(|folder| folder.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        Some(SwitcherItem {
            name: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            folder: shown,
            path,
            alias: None,
            create: true,
        })
    }

    fn open_switcher_selection(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if let Some(switcher) = self.switcher.as_ref().filter(|switcher| switcher.rename) {
            let name = switcher.query.clone();
            self.rename_note(&name, cx);
            cx.notify();
            return;
        }
        let selected = index
            .or_else(|| self.switcher.as_ref().map(|switcher| switcher.selected))
            .unwrap_or(0);
        // A tag from the list of all tags: search for it.
        if let Some(switcher) = &mut self.switcher
            && switcher.text
            && switcher.query.trim() == ALL_TAGS
        {
            if let Some((tag, _)) = switcher.tags.get(selected) {
                switcher.query = format!("#{tag}");
                self.search_text(cx);
                cx.notify();
            }
            return;
        }
        let target = match &self.switcher {
            Some(switcher) if switcher.text => switcher
                .hits
                .get(selected)
                .map(|hit| (hit.path.clone(), Some(line_fragment(hit.line)), false)),
            _ => self
                .switcher_items()
                .get(selected)
                .map(|item| (item.path.clone(), None, item.create)),
        };
        let Some((path, fragment, create)) = target else {
            return;
        };
        self.switcher = None;
        self.marked_text.clear();
        let name = display_name(&path);
        self.request_close(
            PendingClose::Open {
                vault_root: self
                    .vault
                    .as_ref()
                    .filter(|vault| path.starts_with(&vault.root))
                    .map(|vault| vault.root.clone()),
                path,
                fragment,
            },
            cx,
        );
        if create {
            // Neovim opens a new buffer; the file exists once saved, as with links.
            self.show_notice(format!("新笔记 {name}：用 :w 保存后创建"), false, cx);
        }
        cx.notify();
    }

    fn check_for_updates(&mut self, install_automatically: bool, cx: &mut Context<Self>) {
        if matches!(
            self.update_status,
            UpdateStatus::Checking | UpdateStatus::Installing(_)
        ) {
            return;
        }
        self.update_status = UpdateStatus::Checking;
        self.available_update = None;
        cx.notify();

        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = executor.spawn(async { crate::update::check() }).await;
            this.update(cx, |this, cx| match result {
                Ok(Some(update)) => {
                    let version: SharedString = update.version.clone().into();
                    this.available_update = Some(update);
                    this.update_status = UpdateStatus::Available(version);
                    if install_automatically {
                        this.install_available_update(cx);
                    } else {
                        cx.notify();
                    }
                }
                Ok(None) => {
                    this.update_status = UpdateStatus::UpToDate;
                    cx.notify();
                }
                Err(error) => {
                    this.update_status = UpdateStatus::Failed(error.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn install_available_update(&mut self, cx: &mut Context<Self>) {
        let Some(update) = self.available_update.take() else {
            return;
        };
        let version: SharedString = update.version.clone().into();
        self.update_status = UpdateStatus::Installing(version.clone());
        cx.notify();

        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = executor
                .spawn(async move { crate::update::install(update) })
                .await;
            this.update(cx, |this, cx| {
                this.update_status = match result {
                    Ok(()) => UpdateStatus::Installed(version),
                    Err(error) => UpdateStatus::Failed(error.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Add an opened file or folder to the welcome screen's recent list.
    fn remember_recent(&mut self, path: &Path) {
        if self.document.is_none() && self.vault.is_none() {
            return;
        }
        // A new note that was never saved does not exist yet; it is not worth recalling.
        let Ok(path) = path.canonicalize() else {
            return;
        };
        if let Ok(settings) = crate::settings::update(|settings| settings.remember(path)) {
            self.recent = settings.recent;
        }
    }

    /// Open the global (or a vault's) TeX preamble in Neovim, creating it from a template.
    fn edit_preamble(&mut self, vault: Option<PathBuf>, cx: &mut Context<Self>) {
        let Some(path) = crate::tikz::preamble_path(vault.as_deref()) else {
            self.show_notice("无法确定系统设置目录", true, cx);
            return;
        };
        if let Err(error) = crate::tikz::ensure_preamble(&path) {
            self.show_notice(error, true, cx);
            return;
        }
        self.settings_open = false;
        self.request_close(
            PendingClose::Open {
                path,
                vault_root: self.vault.as_ref().map(|vault| vault.root.clone()),
                fragment: None,
            },
            cx,
        );
        cx.notify();
    }

    fn set_reading_key(&mut self, key: ReadingKey, cx: &mut Context<Self>) {
        self.reading_key = key;
        if let Err(error) = crate::settings::update(|settings| settings.reading_key = key) {
            self.show_notice(error, true, cx);
        }
        cx.defer(sync_settings);
        if key != ReadingKey::Escape
            && self
                .nvim_warning
                .as_ref()
                .is_some_and(|warning| warning.starts_with(ESCAPE_MAPPED_PREFIX))
        {
            self.nvim_warning = None;
        }
        cx.notify();
    }

    /// Point out a Neovim `<Esc>` mapping that the reading key hides, and how to free it.
    fn warn_about_escape_mapping(&mut self) {
        if self.reading_key != ReadingKey::Escape {
            return;
        }
        if let Some(mapping) = &self.escape_mapping {
            let settings = if cfg!(target_os = "macos") {
                "⌘,"
            } else {
                "Ctrl+,"
            };
            self.nvim_warning = Some(
                format!(
                    "{ESCAPE_MAPPED_PREFIX}{mapping}，Rusidian 用 Esc 返回阅读视图时它不会生效。可在设置（{settings}）中改用 {} 返回阅读，把 Esc 留给 Neovim。",
                    ReadingKey::SecondaryEnter.label()
                )
                .into(),
            );
        }
    }

    fn set_appearance(&mut self, appearance: Appearance, cx: &mut Context<Self>) {
        self.appearance = appearance;
        if let Err(error) = crate::settings::update(|settings| settings.appearance = appearance) {
            self.show_notice(error, true, cx);
        }
        cx.defer(sync_settings);
        cx.notify();
    }

    fn toggle_auto_update(&mut self, cx: &mut Context<Self>) {
        let enabled = !self.auto_update;
        match update::set_auto_update(enabled) {
            Ok(()) => {
                self.auto_update = enabled;
                cx.notify();
            }
            Err(error) => {
                self.update_status = UpdateStatus::Failed(error.into());
                cx.notify();
            }
        }
    }

    fn start_nvim(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.as_ref().map(|document| document.file.clone()) else {
            return;
        };
        // Relative :edit paths resolve from the vault root, or the note's folder without one.
        let directory = self
            .vault
            .as_ref()
            .map(|vault| vault.root.clone())
            .or_else(|| path.canonicalize().ok()?.parent().map(Path::to_path_buf));
        let client = NvimClient::start(path, directory, false, self.nvim_size.get());
        let events = client.events.clone();
        self.nvim = Some(client);

        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let result = this.update(cx, |this, cx| {
                    if !this
                        .nvim
                        .as_ref()
                        .is_some_and(|client| client.events.same_channel(&events))
                    {
                        return false;
                    }
                    match event {
                        NvimEvent::Redraw(events) => {
                            let line = this.grid.buffer_cursor_line;
                            if this.grid.apply_redraw(&events) {
                                cx.notify();
                            }
                            if this.view == View::Source && this.grid.buffer_cursor_line != line {
                                this.compile_finished_tikz(cx);
                            }
                        }
                        NvimEvent::BufferLines {
                            first,
                            last,
                            lines,
                            more,
                        } => {
                            if this.update_buffer(first, last, lines, more) && !more {
                                if std::mem::take(&mut this.place_initial_cursor)
                                    && let Some(document) = &this.document
                                {
                                    this.reading_cursor = initial_cursor(&document.markdown.blocks);
                                }
                                if let Some(fragment) = this.pending_fragment.take()
                                    && !this.reveal_fragment(&fragment)
                                {
                                    this.show_notice(
                                        format!("找不到标题或块：{fragment}"),
                                        true,
                                        cx,
                                    );
                                }
                                if this.view == View::Reading {
                                    this.compile_visuals(cx);
                                } else if this.parse_pending {
                                    this.parse_when_idle(cx);
                                } else {
                                    this.compile_finished_tikz(cx);
                                }
                                cx.notify();
                            }
                        }
                        NvimEvent::Error(error) => {
                            this.nvim_error = Some(error.into());
                            cx.notify();
                        }
                        NvimEvent::ImeHint => {
                            this.ime_hint = !crate::settings::load().hide_ime_hint;
                            cx.notify();
                        }
                        NvimEvent::EscapeMapped(mapping) => {
                            this.escape_mapping = Some(mapping);
                            this.warn_about_escape_mapping();
                            cx.notify();
                        }
                        NvimEvent::CloseRefused(warning) => {
                            this.pending_close = None;
                            this.nvim_warning = Some(warning.into());
                            this.view = View::Source;
                            // Quitting may have started from another window; show why it stopped.
                            if let Some(handle) = this.window {
                                cx.defer(move |cx| {
                                    handle
                                        .update(cx, |_, window, _| window.activate_window())
                                        .ok();
                                });
                            }
                            cx.notify();
                        }
                        NvimEvent::Cursor { line, column } => {
                            if this.view == View::Reading
                                && let Some(document) = &this.document
                                && let Some(cursor) = reading_position(
                                    &document.markdown.blocks,
                                    source_offset(&document.lines, line, column),
                                )
                            {
                                this.reading_cursor = cursor;
                                this.reading_column = None;
                                this.synced_cursor = Some(cursor);
                                this.reveal_reading_cursor();
                                cx.notify();
                            }
                        }
                        NvimEvent::BufferEntered(path) => {
                            this.follow_buffer(path, cx);
                        }
                        NvimEvent::Notice { text, error } => {
                            if error {
                                this.pending_fragment = None;
                            }
                            this.show_notice(text, error, cx);
                        }
                        NvimEvent::Buffers(buffers) => {
                            this.open_buffers = buffers;
                            cx.notify();
                        }
                        NvimEvent::Modified(modified) => {
                            if this.modified != modified {
                                this.modified = modified;
                                cx.notify();
                            }
                        }
                        NvimEvent::BufferWritten(path) if crate::tikz::is_preamble(&path) => {
                            // Every diagram depends on the preamble.
                            this.tikz.clear();
                            this.tikz_lines.clear();
                            this.show_notice("TeX 前导内容已保存，TikZ 将重新编译", false, cx);
                        }
                        NvimEvent::BufferWritten(path) => {
                            if let Some(document) = &mut this.document
                                && same_file(&document.file, &path)
                            {
                                document.on_disk = true;
                            }
                            if this.vault.as_ref().is_some_and(|vault| {
                                path.starts_with(&vault.root)
                                    && crate::vault::is_markdown(&path)
                                    && !vault.files.iter().any(|file| same_file(file, &path))
                            }) {
                                this.refresh_vault(cx);
                            }
                        }
                        NvimEvent::Exited => {
                            this.nvim = None;
                            this.modified = false;
                            this.open_buffers.clear();
                            if let Some(action) = this.pending_close.take() {
                                this.request_close(action, cx);
                            } else {
                                this.nvim_error =
                                    Some("Neovim 已退出；按 Enter 可重新打开源码视图".into());
                                if this.has_reading_view() {
                                    if this.parse_pending {
                                        this.parse_document();
                                    }
                                    this.view = View::Reading;
                                }
                                cx.notify();
                            }
                            return false;
                        }
                    }
                    true
                });
                if !matches!(result, Ok(true)) {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                if this
                    .nvim
                    .as_ref()
                    .is_some_and(|client| client.events.same_channel(&events))
                {
                    this.nvim = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn request_close(&mut self, action: PendingClose, cx: &mut Context<Self>) {
        // Notes open in the running Neovim, which keeps other (even unsaved) notes as hidden
        // buffers. Folders, or a Neovim that is not running, take the restart path below.
        if let PendingClose::Open { path, fragment, .. } = &action
            && !path.is_dir()
            && self.pending_close.is_none()
            && self.nvim_error.is_none()
            && let Some(nvim) = &self.nvim
        {
            if self
                .document
                .as_ref()
                .is_some_and(|document| same_file(&document.file, path))
            {
                if let Some(fragment) = fragment
                    && !self.reveal_fragment(fragment)
                {
                    self.show_notice(format!("找不到标题或块：{fragment}"), true, cx);
                }
                cx.notify();
                return;
            }
            nvim.edit(path.clone());
            self.pending_fragment = fragment.clone();
            let path = path.clone();
            self.remember_recent(&path);
            return;
        }
        if let Some(nvim) = &self.nvim {
            if self.pending_close.is_some() {
                return;
            }
            if nvim.close() {
                self.pending_close = Some(action);
                return;
            }
            self.nvim = None;
        }
        match action {
            PendingClose::Open {
                path,
                vault_root,
                fragment,
            } => {
                self.replace_document(&path, vault_root);
                self.remember_recent(&path);
                if let Some(fragment) = fragment
                    && !self.reveal_fragment(&fragment)
                {
                    self.show_notice(format!("找不到标题或块：{fragment}"), true, cx);
                }
                self.compile_visuals(cx);
                self.start_nvim(cx);
                cx.notify();
            }
            PendingClose::CloseWindow => self.close_window(None, cx),
            PendingClose::Quit => self.close_window(Some(PendingClose::Quit), cx),
            PendingClose::Restart => self.close_window(Some(PendingClose::Restart), cx),
        }
    }

    /// Remove this window now that its Neovim has exited, then carry `then` to the others.
    fn close_window(&mut self, then: Option<PendingClose>, cx: &mut Context<Self>) {
        let Some(handle) = self.window else {
            return;
        };
        // The window cannot remove itself while it is being updated.
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .ok();
            close_next_window(then, cx);
        });
    }

    /// The vault tree's visible rows, built when the vault or expanded folders changed.
    fn tree(&self) -> Rc<Vec<TreeRow>> {
        if let Some(rows) = self.tree_rows.borrow().as_ref() {
            return rows.clone();
        }
        let rows = Rc::new(
            self.vault
                .as_ref()
                .map(|vault| vault.tree_rows(&self.expanded_folders))
                .unwrap_or_default(),
        );
        self.tree_rows.replace(Some(rows.clone()));
        rows
    }

    /// Foldable callouts shown folded now: those starting folded (`[!tip]-`) unless toggled,
    /// and those starting open (`[!tip]+`) once toggled.
    fn folded_callouts(&self, blocks: &[Block]) -> HashSet<usize> {
        blocks
            .iter()
            .filter(|block| block.callout_title.is_some())
            .filter_map(|block| {
                let id = block.callout_quote?;
                let starts_folded = block.callout_fold?;
                (starts_folded != self.callout_toggles.contains(&id)).then_some(id)
            })
            .collect()
    }

    fn toggle_callout(&mut self, id: usize, cx: &mut Context<Self>) {
        if !self.callout_toggles.remove(&id) {
            self.callout_toggles.insert(id);
        }
        // Folding moves the cursor out of the hidden part, onto the title line.
        self.rest_on_fold();
        self.invalidate_block_heights();
        cx.notify();
    }

    /// Like on Vim's closed folds, a cursor inside a folded callout rests on its title rather
    /// than opening it.
    fn rest_on_fold(&mut self) {
        let Some(document) = self.document.as_ref().filter(|document| document.foldable) else {
            return;
        };
        let blocks = &document.markdown.blocks;
        if let Some(range) = fold_ranges(blocks, &self.folded_callouts(blocks))
            .into_iter()
            .find(|range| range.contains(&self.reading_cursor.block))
            && let Some(id) = blocks[range.start].callout_quote
        {
            self.reading_cursor = ReadingCursor {
                block: range.start,
                offset: 0,
            };
            self.callout_cursor_parked = Some((id, self.reading_cursor));
        }
    }

    /// Forget measured block heights after anything that can change a block's size: the
    /// note's text, or a formula, diagram, image or embed finishing loading.
    fn invalidate_block_heights(&self) {
        // The old heights stay as estimates, so what is shown keeps its place until measured.
        let mut cache = self.block_heights.borrow_mut();
        cache.measured.fill(false);
        cache.stale = true;
    }

    /// After the note's text changed: blocks may have come and gone.
    fn remap_block_heights(&self) {
        self.invalidate_block_heights();
        self.block_heights.borrow_mut().remap = true;
    }

    /// Where block `index` was laid out in the last frame, unscrolled like the scroll handle's
    /// child bounds. A block inside a stretch of empty space gets its share of that stretch.
    fn block_bounds(&self, index: usize) -> Option<Bounds<Pixels>> {
        let cache = self.block_heights.borrow();
        let child = *cache.child_of.get(index)?;
        let bounds = self.reading_scroll.bounds_for_item(child)?;
        if cache.drawn.get(index) == Some(&true) {
            return Some(bounds);
        }
        Some(Bounds::new(
            point(bounds.left(), bounds.top() + *cache.offsets.get(index)?),
            size(bounds.size.width, *cache.heights.get(index)?),
        ))
    }

    /// Which blocks to draw in full this frame: those within a screen and a half of the view,
    /// the cursor's neighborhood, and blocks with images (which size themselves once loaded).
    /// Blocks above the view that were not measured yet are laid out out of view first (the
    /// second list): drawn in place, their real height would move what is shown. Records how
    /// the rest will be grouped into stretches of space. `hidden` blocks are folded away.
    fn plan_blocks(
        &self,
        blocks: &[Block],
        gaps: Vec<Pixels>,
        hidden: &[bool],
    ) -> (Vec<bool>, Vec<usize>) {
        let view = self.reading_scroll.bounds();
        // Before the first layout, plan for the window's first size.
        let (width, height) = if view.size.height > px(0.0) {
            (view.size.width, view.size.height)
        } else {
            (px(WINDOW_WIDTH), px(WINDOW_HEIGHT))
        };
        let column = (width - px(2.0 * READING_PADDING)).min(px(READING_WIDTH));
        // Heights measured in the last frame: blocks drawn in place, then out of view.
        let positions: Vec<(usize, Pixels)> = {
            let cache = self.block_heights.borrow();
            (0..cache.drawn.len())
                .filter(|&index| cache.drawn[index])
                .filter_map(|index| Some((index, self.block_bounds(index)?.size.height)))
                .collect()
        };
        let measures = std::mem::take(&mut *self.block_measures.borrow_mut());
        let mut cache = self.block_heights.borrow_mut();
        let count = blocks.len();
        let mut fresh = !cache.stale && cache.width == width;
        if cache.heights.len() != count || cache.remap {
            // Blocks before and after an edit keep their heights; others are estimated.
            let keys: Vec<u64> = blocks.iter().map(block_key).collect();
            let old_count = cache.keys.len().min(cache.heights.len());
            let prefix = (0..old_count.min(count))
                .take_while(|&index| cache.keys[index] == keys[index])
                .count();
            let suffix = (0..(old_count - prefix).min(count - prefix))
                .take_while(|&back| cache.keys[old_count - 1 - back] == keys[count - 1 - back])
                .count();
            let heights = (0..count)
                .map(|index| {
                    if index < prefix {
                        cache.heights[index]
                    } else if index >= count - suffix {
                        cache.heights[index + old_count - count]
                    } else {
                        estimate_height(&blocks[index], column)
                    }
                })
                .collect();
            *cache = BlockHeights {
                heights,
                measured: vec![false; count],
                keys,
                ..BlockHeights::default()
            };
            fresh = false;
        } else if !fresh {
            cache.measured.fill(false);
        }
        cache.width = width;
        cache.stale = false;
        // Positions in the note are offsets from the view's top; the visible part starts at
        // minus the scroll offset.
        let mut scrolled = -self.reading_scroll.offset().y;
        if fresh {
            for (index, height) in positions {
                cache.heights[index] = height;
                cache.measured[index] = true;
            }
            // Blocks measured out of view were above it: keep what is shown in place.
            let mut tops = Vec::with_capacity(count);
            let mut top = px(READING_PADDING);
            for index in 0..count {
                tops.push(top);
                top += cache.heights[index] + cache.gaps.get(index).copied().unwrap_or_default();
            }
            let mut shift = px(0.0);
            for (index, height) in measures {
                if index >= count || cache.measured[index] {
                    continue;
                }
                if tops[index] + cache.heights[index] <= scrolled {
                    shift += height - cache.heights[index];
                }
                cache.heights[index] = height;
                cache.measured[index] = true;
            }
            if shift != px(0.0) {
                let offset = self.reading_scroll.offset();
                self.reading_scroll
                    .set_offset(point(offset.x, offset.y - shift));
                scrolled += shift;
            }
        }
        for (index, _) in hidden.iter().enumerate().filter(|(_, hidden)| **hidden) {
            cache.heights[index] = px(0.0);
            cache.measured[index] = true;
        }
        let margin = height * 1.5;
        let (near_top, near_bottom) = (scrolled - margin, scrolled + height + margin);
        let cursor = self.reading_cursor.block;
        let mut drawn = Vec::with_capacity(count);
        let mut measuring = Vec::new();
        let mut top = px(READING_PADDING);
        for (index, block) in blocks.iter().enumerate() {
            let bottom = top + cache.heights[index];
            let image = !block.images.is_empty() || matches!(block.kind, BlockKind::Image(_));
            let wanted =
                image || index.abs_diff(cursor) <= 2 || (bottom >= near_top && top <= near_bottom);
            if wanted && !image && !cache.measured[index] && bottom <= scrolled {
                measuring.push(index);
                drawn.push(false);
            } else {
                drawn.push(wanted);
            }
            top = bottom + gaps[index];
        }
        let (children, child_of, offsets) = group_blocks(&drawn, &cache.heights, &gaps);
        cache.children = children;
        cache.child_of = child_of;
        cache.offsets = offsets;
        cache.gaps = gaps;
        cache.drawn.clone_from(&drawn);
        (drawn, measuring)
    }

    /// Re-read settings another window may have changed.
    fn load_settings(&mut self) {
        let settings = crate::settings::load();
        self.appearance = settings.appearance;
        self.reading_key = settings.reading_key;
        self.recent = settings.recent;
        self.remote_image_vaults = settings.remote_image_vaults;
        self.auto_update = settings.auto_update;
        if self.reading_key != ReadingKey::Escape
            && self
                .nvim_warning
                .as_ref()
                .is_some_and(|warning| warning.starts_with(ESCAPE_MAPPED_PREFIX))
        {
            self.nvim_warning = None;
        }
    }

    /// Open a path; a note inside an Obsidian vault also loads that vault for links and embeds.
    fn open_with_vault(path: Option<&Path>) -> Self {
        let mut app = Self::open(path);
        if app.vault.is_none()
            && let Some(path) = path
            && let Some(root) = crate::vault::enclosing_vault_root(path)
            && let Ok(vault) = Vault::open(&root)
        {
            app.attach_vault(vault);
        }
        app
    }

    /// Load another document, keeping app-level state and the vault when it stays the same.
    fn replace_document(&mut self, path: &Path, vault_root: Option<PathBuf>) {
        let vault_root = vault_root.or_else(|| {
            (!path.is_dir())
                .then(|| crate::vault::enclosing_vault_root(path))
                .flatten()
        });
        let vault = self
            .vault
            .take()
            .filter(|vault| vault_root.as_ref() == Some(&vault.root));
        let mut next = Self::open(Some(path));
        if !path.is_dir()
            && let Some(vault) =
                vault.or_else(|| vault_root.and_then(|root| Vault::open(&root).ok()))
        {
            next.attach_vault(vault);
        }
        next.focus_handle = self.focus_handle.take();
        next.window = self.window;
        next.nvim_size = self.nvim_size.clone();
        next.cell_size = self.cell_size;
        next.grid_origin = self.grid_origin.clone();
        next.layouts_ready = self.layouts_ready.clone();
        next.layouts_ready.set(false);
        next.settings_open = self.settings_open;
        next.appearance = self.appearance;
        next.recent = std::mem::take(&mut self.recent);
        next.remote_images = std::mem::take(&mut self.remote_images);
        next.remote_image_vaults = std::mem::take(&mut self.remote_image_vaults);
        next.sidebar_visible = self.sidebar_visible;
        next.expanded_folders = std::mem::take(&mut self.expanded_folders);
        next.theme = self.theme;
        next.auto_update = self.auto_update;
        next.update_status = std::mem::replace(&mut self.update_status, UpdateStatus::Idle);
        next.available_update = self.available_update.take();
        *self = next;
    }

    /// A press on the reading view's text: move the cursor there. A link or tag under it is
    /// followed when the button comes up without a drag.
    fn click_reading(&mut self, block: usize, offset: usize, cx: &mut Context<Self>) {
        if !self.place_reading_cursor(block, offset) {
            return;
        }
        self.mouse_press = Some(MousePress {
            at: self.reading_cursor,
            on_text: true,
            dragged: false,
        });
        cx.notify();
    }

    /// A press beside the text (between blocks, in margins, on list markers): the cursor goes
    /// to the nearest position, without following a link there.
    fn click_nearest(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if let Some(cursor) = self.reading_position_at(position.x, position.y, true)
            && self.place_reading_cursor(cursor.block, cursor.offset)
        {
            self.mouse_press = Some(MousePress {
                at: self.reading_cursor,
                on_text: false,
                dragged: false,
            });
            cx.notify();
        }
    }

    /// Dragging with the button down selects from where it was pressed, like `v`.
    fn drag_reading(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.mouse_press = None;
            return;
        }
        let Some(press) = self.mouse_press else {
            return;
        };
        let Some(cursor) = self.reading_position_at(event.position.x, event.position.y, true)
        else {
            return;
        };
        if cursor == self.reading_cursor && (press.dragged || cursor == press.at) {
            return;
        }
        self.mouse_press = Some(MousePress {
            dragged: true,
            ..press
        });
        self.reading_selection = Some(ReadingSelection {
            anchor: press.at,
            linewise: false,
        });
        self.reading_cursor = cursor;
        self.reading_column = None;
        cx.notify();
    }

    /// The button came up: a click on a tag searches it, one on a link follows it.
    fn release_reading(&mut self, cx: &mut Context<Self>) {
        let Some(press) = self.mouse_press.take() else {
            return;
        };
        if press.dragged || !press.on_text || self.reading_cursor != press.at {
            return;
        }
        if let Some(tag) = self.current_tag() {
            self.search_tag(tag, cx);
            return;
        }
        if let Some(destination) = self.current_link() {
            if is_external_link(&destination) {
                cx.open_url(&destination);
            } else {
                self.open_internal_link(cx);
            }
        }
        cx.notify();
    }

    /// Copy the selection to the system clipboard: `y`, or the platform's copy shortcut.
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(selection) = self.reading_selection.take()
            && let Some(item) = self.selected_clipboard(selection)
        {
            let message = match item.text() {
                Some(text) => format!("已复制 {} 个字符", text.chars().count()),
                None => "已复制图片".to_owned(),
            };
            cx.write_to_clipboard(item);
            self.show_notice(message, false, cx);
        }
    }

    /// Put the reading cursor where the user clicked, ending a selection, search or count.
    fn place_reading_cursor(&mut self, block: usize, offset: usize) -> bool {
        if self.view != View::Reading || self.settings_open {
            return false;
        }
        let Some(length) = self
            .document
            .as_ref()
            .and_then(|document| document.markdown.blocks.get(block))
            .map(block_len)
            .filter(|length| *length > 0)
        else {
            return false;
        };
        self.reading_cursor = ReadingCursor {
            block,
            offset: offset.min(length - 1),
        };
        self.reading_column = None;
        self.reading_selection = None;
        self.reading_search = None;
        self.reading_pending_g = false;
        self.reading_count = None;
        self.reading_find = None;
        self.notice = None;
        true
    }

    fn current_tab(&self) -> Option<usize> {
        let document = self.document.as_ref()?;
        self.open_buffers
            .iter()
            .position(|(path, _)| same_file(path, &document.file))
    }

    fn cycle_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.open_buffers.len();
        let Some(current) = self.current_tab().filter(|_| count > 1) else {
            return;
        };
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        let path = self.open_buffers[next].0.clone();
        self.open_tab(path, cx);
    }

    fn open_tab(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.request_close(
            PendingClose::Open {
                vault_root: self
                    .vault
                    .as_ref()
                    .filter(|vault| path.starts_with(&vault.root))
                    .map(|vault| vault.root.clone()),
                path,
                fragment: None,
            },
            cx,
        );
    }

    /// Close the current note's tab, or the window when it is the last one.
    fn close_tab(&mut self, cx: &mut Context<Self>) {
        match (&self.nvim, self.document.as_ref()) {
            (Some(nvim), Some(document)) if self.open_buffers.len() > 1 => {
                nvim.close_buffer(document.file.clone());
            }
            _ => self.request_close(PendingClose::CloseWindow, cx),
        }
    }

    /// Show the buffer Neovim switched to. Its lines arrive next as a full buffer update.
    fn follow_buffer(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self
            .document
            .as_ref()
            .is_some_and(|document| same_file(&document.file, &path))
        {
            return;
        }
        let name: SharedString = if path.as_os_str().is_empty() {
            "[未命名]".into()
        } else {
            display_name(&path).into()
        };
        // A note in another Obsidian vault switches vaults; other files outside the current
        // vault (a scratch file, the TeX preamble) keep it.
        if !self
            .vault
            .as_ref()
            .is_some_and(|vault| path.starts_with(&vault.root))
            && let Some(vault) =
                crate::vault::enclosing_vault_root(&path).and_then(|root| Vault::open(&root).ok())
        {
            self.vault = Some(vault);
        }
        let is_markdown = crate::vault::is_markdown(&path);
        if !is_markdown {
            // Only Markdown has a reading view.
            self.view = View::Source;
        }
        self.document = Some(Document {
            name,
            path: path.to_string_lossy().into_owned().into(),
            is_markdown,
            on_disk: path.is_file(),
            foldable: false,
            file: path,
            lines: vec![String::new()],
            markdown: MarkdownDocument { blocks: Vec::new() },
        });
        self.error = None;
        self.reading_cursor = ReadingCursor::default();
        self.layouts_ready.set(false);
        // Another note's heights tell nothing about this one's.
        *self.block_heights.borrow_mut() = BlockHeights::default();
        self.block_measures.borrow_mut().clear();
        // Fold toggles number callouts within one note.
        self.callout_toggles.clear();
        self.callout_cursor_parked = None;
        self.place_initial_cursor = true;
        self.tikz_lines.clear();
        self.synced_cursor = None;
        self.reading_column = None;
        self.reading_selection = None;
        self.reading_search = None;
        self.reading_scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// Say once when the note shown was deleted or moved outside Rusidian: Neovim keeps its
    /// text, which closing the tab would lose.
    fn notice_missing_file(&mut self, cx: &mut Context<Self>) {
        let Some(document) = &mut self.document else {
            return;
        };
        if !document.on_disk || document.file.is_file() {
            return;
        }
        document.on_disk = false;
        let name = document.name.clone();
        self.show_notice(
            format!("{name} 已在磁盘上被删除或移动；内容仍在 Neovim 中，:w 可重新保存"),
            true,
            cx,
        );
    }

    /// Rescan the vault in the background: after a new note is saved from Neovim, or when the
    /// window comes back (notes may have been added, moved or deleted elsewhere).
    fn refresh_vault(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.vault.as_ref().map(|vault| vault.root.clone()) else {
            return;
        };
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let scanned = executor
                .spawn({
                    let root = root.clone();
                    async move { Vault::open(&root) }
                })
                .await;
            if let Ok(vault) = scanned {
                this.update(cx, |this, cx| {
                    if this
                        .vault
                        .as_ref()
                        .is_some_and(|current| current.root == root)
                    {
                        this.vault = Some(vault);
                        this.tree_rows.replace(None);
                        // Links and backlinks may now resolve differently.
                        this.find_unresolved_links();
                        this.load_backlinks(true, cx);
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Move the reading cursor to a heading (`#Heading`) or block reference (`#^id`).
    fn reveal_fragment(&mut self, fragment: &str) -> bool {
        let Some(document) = &self.document else {
            return false;
        };
        if let Some(line) = line_from_fragment(fragment) {
            let offset = source_offset(&document.lines, line, 0);
            let Some(cursor) = reading_position(&document.markdown.blocks, offset) else {
                return false;
            };
            self.reading_cursor = cursor;
            self.reading_column = None;
            self.reveal_reading_cursor();
            return true;
        }
        let target = if let Some(label) = crate::markdown::footnote_label(fragment) {
            document.markdown.blocks.iter().position(|block| {
                matches!(&block.kind, BlockKind::Footnote { label: found, .. } if found == label)
            })
        } else {
            fragment_block(&document.markdown.blocks, fragment)
        };
        let Some(block) = target else {
            return false;
        };
        self.reading_cursor = ReadingCursor { block, offset: 0 };
        self.reading_column = None;
        self.reveal_reading_cursor();
        true
    }

    fn choose_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_path(false, window, cx);
    }

    fn choose_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.choose_path(true, window, cx);
    }

    fn choose_path(&mut self, directory: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: false,
            prompt: Some(
                if directory {
                    "打开文件夹"
                } else {
                    "打开文件"
                }
                .into(),
            ),
        });

        cx.spawn_in(window, async move |this, cx| {
            let mut paths = match selected.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        let hint = if cfg!(target_os = "linux") {
                            "（Linux 需要 xdg-desktop-portal）；也可以在终端运行 rusidian <路径>"
                        } else {
                            ""
                        };
                        this.show_notice(
                            format!("无法打开系统文件选择器{hint}：{error}"),
                            true,
                            cx,
                        );
                    })
                    .ok();
                    return;
                }
                Ok(Ok(None)) | Err(_) => return,
            };
            let Some(path) = paths.pop() else {
                return;
            };

            this.update_in(cx, |this, _, cx| {
                this.request_close(
                    PendingClose::Open {
                        path,
                        vault_root: None,
                        fragment: None,
                    },
                    cx,
                );
            })
            .ok();
        })
        .detach();
    }

    fn update_buffer(
        &mut self,
        first: usize,
        last: Option<usize>,
        replacement: Vec<String>,
        more: bool,
    ) -> bool {
        let Some(document) = &mut self.document else {
            return false;
        };
        let end = last.unwrap_or(document.lines.len());
        if first > end || end > document.lines.len() {
            return false;
        }
        document.lines.splice(first..end, replacement);
        if !more {
            if self.view == View::Source
                && !self.place_initial_cursor
                && self.pending_fragment.is_none()
            {
                self.parse_pending = true;
            } else {
                self.parse_document();
            }
        }
        true
    }

    /// Parse the note's current lines for the reading view.
    fn parse_document(&mut self) {
        self.parse_pending = false;
        let strict_line_breaks = self
            .vault
            .as_ref()
            .is_some_and(|vault| vault.settings.strict_line_breaks);
        let Some(document) = &mut self.document else {
            return;
        };
        document.parse(strict_line_breaks);
        self.remap_block_heights();
        // Layouts from the last frame describe the old text until the next render.
        self.layouts_ready.set(false);
        self.clamp_reading_cursor();
    }

    /// Parse the edited note once typing pauses, then start diagrams that were finished.
    fn parse_when_idle(&mut self, cx: &mut Context<Self>) {
        self.parse_generation += 1;
        let generation = self.parse_generation;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            executor.timer(std::time::Duration::from_millis(200)).await;
            this.update(cx, |this, cx| {
                if this.parse_generation == generation && this.parse_pending {
                    this.parse_document();
                    this.compile_finished_tikz(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn clamp_reading_cursor(&mut self) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            self.reading_cursor = ReadingCursor::default();
            return;
        };
        if let Some(length) = blocks.get(self.reading_cursor.block).map(block_len)
            && length > 0
        {
            self.reading_cursor.offset = self.reading_cursor.offset.min(length - 1);
            return;
        }
        self.reading_cursor = blocks
            .iter()
            .enumerate()
            .find(|(_, block)| block_len(block) > 0)
            .map(|(block, _)| ReadingCursor { block, offset: 0 })
            .unwrap_or_default();
    }

    fn move_reading_cursor(&mut self, right: bool) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            return;
        };
        if let Some(cursor) = step_cursor(blocks, self.reading_cursor, right) {
            self.reading_cursor = cursor;
        }
    }

    fn move_reading_word(&mut self, motion: WordMotion) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            return;
        };
        let cursor = match motion {
            WordMotion::Next => next_word(blocks, self.reading_cursor),
            WordMotion::Previous => previous_word(blocks, self.reading_cursor),
            WordMotion::End => end_word(blocks, self.reading_cursor),
        };
        if let Some(cursor) = cursor {
            self.reading_cursor = cursor;
            self.reading_column = None;
        }
    }

    fn has_reading_view(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(|document| document.is_markdown)
    }

    /// `note.md — vault` inside a vault, `note.md — Rusidian` otherwise.
    fn window_title(&self) -> String {
        let Some(document) = &self.document else {
            return "Rusidian".into();
        };
        let context = self
            .vault
            .as_ref()
            .and_then(|vault| vault.root.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Rusidian".into());
        let marker = if self.modified { "● " } else { "" };
        format!("{marker}{} — {context}", document.name)
    }

    fn links(&self) -> Links<'_> {
        Links {
            note: self
                .document
                .as_ref()
                .map_or(Path::new(""), |document| document.file.as_path()),
            vault: self.vault.as_ref(),
        }
    }

    fn take_reading_count(&mut self) -> usize {
        self.reading_count.take().unwrap_or(1)
    }

    fn selected_clipboard(&self, selection: ReadingSelection) -> Option<ClipboardItem> {
        let document = self.document.as_ref()?;
        let blocks = &document.markdown.blocks;
        let bounds = selection_bounds(blocks, selection, self.reading_cursor)?;
        if bounds.0 == bounds.1 {
            let block = blocks.get(bounds.0.block)?;
            if let Some(image) = inline_image_at_offset(block, bounds.0.offset)
                && let Some(path) = self.links().image(&image.source)
                && let Some(format) = image_format(&path)
                && let Ok(bytes) = std::fs::read(path)
            {
                return Some(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
            }
            if let Some((_, formula)) = inline_math_at_offset(block, bounds.0.offset)
                && let Some(MathState::Ready(formula)) =
                    self.math.get(&(formula.source.clone(), false))
                && let Ok(bytes) = formula.clipboard_png()
            {
                return Some(ClipboardItem::new_image(&Image::from_bytes(
                    ImageFormat::Png,
                    bytes,
                )));
            }
            match &block.kind {
                BlockKind::Image(source) => {
                    if let Some(path) = self.links().image(source)
                        && let Some(format) = image_format(&path)
                        && let Ok(bytes) = std::fs::read(path)
                    {
                        return Some(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
                    }
                }
                BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz") => {
                    if let Some(TikzState::Ready(image)) = self.tikz.get(&block.text) {
                        return Some(ClipboardItem::new_image(image));
                    }
                }
                BlockKind::Math => {
                    if let Some(MathState::Ready(formula)) =
                        self.math.get(&(block.text.clone(), true))
                        && let Ok(bytes) = formula.clipboard_png()
                    {
                        return Some(ClipboardItem::new_image(&Image::from_bytes(
                            ImageFormat::Png,
                            bytes,
                        )));
                    }
                }
                _ => {}
            }
        }
        Some(ClipboardItem::new_string(selection_text(
            blocks,
            selection,
            self.reading_cursor,
        )))
    }

    fn execute_search(&mut self, prompt: &SearchPrompt, reverse: bool, cx: &mut Context<Self>) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            return;
        };
        let forward = prompt.forward != reverse;
        match search_cursor(blocks, self.reading_cursor, &prompt.query, forward) {
            Some((cursor, wrapped)) => {
                self.reading_cursor = cursor;
                self.reading_column = None;
                if wrapped {
                    self.show_notice(
                        if forward {
                            "已到文末，从开头继续搜索"
                        } else {
                            "已到开头，从文末继续搜索"
                        },
                        false,
                        cx,
                    );
                }
            }
            None => self.show_notice(format!("找不到：{}", prompt.query), true, cx),
        }
    }

    fn search_word(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            return;
        };
        let Some(query) = word_under_cursor(blocks, self.reading_cursor) else {
            return;
        };
        let prompt = SearchPrompt { query, forward };
        self.execute_search(&prompt, false, cx);
        self.last_search = Some(prompt);
    }

    /// The cursor's line on screen (top and bottom) as laid out in the last frame.
    fn cursor_screen_span(&self) -> Option<(Pixels, Pixels)> {
        let cursor = self.reading_cursor;
        let block = self.document.as_ref()?.markdown.blocks.get(cursor.block)?;
        if !is_object(block)
            && self.layouts_ready.get()
            && let Some(byte) = text_range(&block.text, cursor.offset).map(|range| range.start)
        {
            let layouts = self.fragment_layouts.borrow();
            if let Some(fragment) = layouts
                .iter()
                .find(|fragment| fragment.block == cursor.block && fragment.range.contains(&byte))
                && let Some(position) = fragment
                    .layout
                    .position_for_index(byte - fragment.range.start)
            {
                return Some((position.y, position.y + fragment.layout.line_height()));
            }
        }
        let bounds = self.block_bounds(cursor.block)?;
        let offset = self.reading_scroll.offset().y;
        Some((bounds.top() + offset, bounds.bottom() + offset))
    }

    /// Scroll just enough to show the cursor's line, also inside blocks taller than the view.
    fn reveal_reading_cursor(&self) {
        // Not drawn last frame, the cursor's place is estimated: once its block is drawn,
        // reveal it again.
        if self
            .block_heights
            .borrow()
            .drawn
            .get(self.reading_cursor.block)
            != Some(&true)
        {
            self.reveal_again.set(2);
        }
        self.scroll_to_cursor();
    }

    fn scroll_to_cursor(&self) {
        let view = self.reading_scroll.bounds();
        let Some((top, bottom)) = self
            .cursor_screen_span()
            .filter(|_| view.size.height > px(0.0))
        else {
            let block = self.reading_cursor.block;
            let child = self.block_heights.borrow().child_of.get(block).copied();
            self.reading_scroll.scroll_to_item(child.unwrap_or(block));
            return;
        };
        let margin = (view.size.height / 6.0).min(px(48.0));
        let offset = self.reading_scroll.offset();
        let mut y = offset.y;
        if top < view.top() + margin {
            y += view.top() + margin - top;
        } else if bottom > view.bottom() - margin {
            y -= bottom - (view.bottom() - margin);
        }
        let y = y.clamp(-self.reading_scroll.max_offset().y, px(0.0));
        if y != offset.y {
            self.reading_scroll.set_offset(point(offset.x, y));
        }
    }

    /// The reading position drawn at window position (x, y) in the last frame. Between blocks it
    /// is the nearest text line or object, preferring the direction of travel (`down`).
    fn reading_position_at(&self, x: Pixels, y: Pixels, down: bool) -> Option<ReadingCursor> {
        let blocks = &self.document.as_ref()?.markdown.blocks;
        let ready = self.layouts_ready.get();
        let layouts = self.fragment_layouts.borrow();
        let layouts = if ready { layouts.as_slice() } else { &[] };
        let horizontal = |bounds: Bounds<Pixels>| {
            if x < bounds.left() {
                bounds.left() - x
            } else if x > bounds.right() {
                x - bounds.right()
            } else {
                px(0.0)
            }
        };
        // Vertical distance to a span, with a small preference for the travel direction.
        let vertical = |top: Pixels, bottom: Pixels| {
            if y < top {
                (top - y) * if down { 1.0 } else { 1.5 }
            } else if y >= bottom {
                (y - bottom) * if down { 1.5 } else { 1.0 }
            } else {
                px(0.0)
            }
        };
        let scroll = self.reading_scroll.offset().y;
        let heights = self.block_heights.borrow();
        // Blocks drawn as empty space have no text layout; they count as one position.
        let placeholder = |index: usize| heights.drawn.get(index) == Some(&false);
        let objects = (0..blocks.len()).filter_map(|index| {
            let block = blocks.get(index)?;
            if !is_object(block) && !placeholder(index) {
                return None;
            }
            let bounds = self.block_bounds(index)?;
            Some((index, bounds.top() + scroll, bounds.bottom() + scroll))
        });
        enum Target<'a> {
            Text(&'a FragmentLayout),
            Object(usize),
        }
        let best = layouts
            .iter()
            .map(|fragment| {
                let bounds = fragment.layout.bounds();
                (
                    vertical(bounds.top(), bounds.bottom()),
                    horizontal(bounds),
                    Target::Text(fragment),
                )
            })
            .chain(objects.map(|(index, top, bottom)| {
                (vertical(top, bottom), px(0.0), Target::Object(index))
            }))
            .min_by(|a, b| {
                f32::from(a.0)
                    .total_cmp(&f32::from(b.0))
                    .then(f32::from(a.1).total_cmp(&f32::from(b.1)))
            });
        match best {
            Some((_, _, Target::Text(fragment))) => {
                let block = blocks.get(fragment.block)?;
                let bounds = fragment.layout.bounds();
                let probe = point(
                    x.clamp(bounds.left(), bounds.right()),
                    y.clamp(bounds.top(), bounds.bottom() - px(1.0)),
                );
                let index = fragment
                    .layout
                    .index_for_position(probe)
                    .unwrap_or_else(|nearest| nearest);
                let text = &block.text[fragment.range.clone()];
                let local = (0..=index.min(text.len()))
                    .rev()
                    .find(|byte| text.is_char_boundary(*byte))
                    .unwrap_or(0);
                let offset = visible_offset(&block.text, fragment.range.start + local)
                    .min(block_len(block).saturating_sub(1));
                Some(ReadingCursor {
                    block: fragment.block,
                    offset,
                })
            }
            Some((_, _, Target::Object(block))) => Some(ReadingCursor { block, offset: 0 }),
            None => None,
        }
    }

    /// Ctrl-d/u (half page) and Ctrl-f/b (full page): scroll, keeping the cursor at the same
    /// height on screen like Vim. At either end the cursor moves instead.
    fn scroll_reading(&mut self, down: bool, full_page: bool, count: usize) {
        let view = self.reading_scroll.bounds();
        if view.size.height <= px(0.0) {
            return;
        }
        let distance =
            view.size.height * if full_page { 1.0 } else { 0.5 } * count.clamp(1, 100) as f32;
        let offset = self.reading_scroll.offset();
        let maximum = self.reading_scroll.max_offset().y;
        let y = (offset.y + if down { -distance } else { distance }).clamp(-maximum, px(0.0));
        let delta = y - offset.y;
        let (anchor_x, anchor_y) = match self.cursor_screen_span() {
            Some((top, bottom)) => (
                self.reading_desired_x.unwrap_or(view.left()),
                ((top + bottom) / 2.0).clamp(view.top(), view.bottom() - px(1.0)),
            ),
            None => (view.left(), view.top()),
        };
        // Layouts are from before the scroll: the content that will sit at `anchor_y` is the
        // content currently at `anchor_y - delta`.
        let probe = if delta == px(0.0) {
            anchor_y + if down { distance } else { -distance }
        } else {
            anchor_y - delta
        };
        // Look up the new position before scrolling: text layouts and block positions must
        // both be those of the last frame, which the probe is measured in.
        let target = self.reading_position_at(anchor_x, probe, down);
        self.reading_scroll.set_offset(point(offset.x, y));
        if let Some(cursor) = target
            && cursor != self.reading_cursor
        {
            self.reading_cursor = cursor;
            self.rest_on_fold();
        } else if delta == px(0.0) {
            self.move_reading_document_edge(down);
        }
        self.reading_column = None;
    }

    fn current_link(&self) -> Option<String> {
        let block = self
            .document
            .as_ref()?
            .markdown
            .blocks
            .get(self.reading_cursor.block)?;
        let byte = text_range(&block.text, self.reading_cursor.offset)?.start;
        block
            .links
            .iter()
            .find(|link| link.range.contains(&byte))
            .map(|link| link.destination.clone())
    }

    /// The `#tag` under the reading cursor.
    fn current_tag(&self) -> Option<String> {
        let block = self
            .document
            .as_ref()?
            .markdown
            .blocks
            .get(self.reading_cursor.block)?;
        let byte = text_range(&block.text, self.reading_cursor.offset)?.start;
        block
            .spans
            .iter()
            .find(|span| span.tag && span.range.contains(&byte))
            .map(|span| block.text[span.range.clone()].to_owned())
    }

    /// Search all notes for a tag, as Obsidian's tag search does.
    fn search_tag(&mut self, tag: String, cx: &mut Context<Self>) {
        self.show_switcher(true, cx);
        if let Some(switcher) = &mut self.switcher {
            switcher.query = tag;
        }
        self.search_text(cx);
    }

    /// What `gf` and `gx` open: the link under the cursor, or the embedded note or image the
    /// cursor is on. Clicking only follows links, so clicking an image does not open it.
    fn cursor_target(&self) -> Option<String> {
        self.current_link().or_else(|| {
            let block = self
                .document
                .as_ref()?
                .markdown
                .blocks
                .get(self.reading_cursor.block)?;
            if let BlockKind::Image(source) = &block.kind {
                return Some(source.clone());
            }
            // An image or attachment inside a paragraph.
            let byte = text_range(&block.text, self.reading_cursor.offset)?.start;
            block
                .images
                .iter()
                .find(|image| image.range.contains(&byte))
                .map(|image| image.source.clone())
        })
    }

    fn open_internal_link(&mut self, cx: &mut Context<Self>) {
        if let Some(tag) = self.current_tag() {
            self.search_tag(tag, cx);
            return;
        }
        let Some(destination) = self.cursor_target() else {
            self.show_notice("光标处没有链接", false, cx);
            return;
        };
        if is_external_link(&destination) {
            self.show_notice("外部链接请用 gx 打开", false, cx);
            return;
        }
        let (target, fragment) = crate::vault::split_fragment(&destination);
        let Some(document) = &self.document else {
            return;
        };
        if target.is_empty() {
            if let Some(fragment) = fragment
                && !self.reveal_fragment(fragment)
            {
                self.show_notice(format!("找不到标题或块：{fragment}"), true, cx);
            }
            return;
        }
        let Some(path) = crate::vault::resolve_target(&document.file, self.vault.as_ref(), target)
        else {
            // Like Obsidian, a link to a missing note creates it: Neovim opens a new buffer,
            // and the file exists once saved.
            let create = crate::vault::new_note_path(&document.file, self.vault.as_ref(), target)
                .filter(|path| path.parent().is_some_and(Path::is_dir) && !path.exists());
            let Some(path) = create else {
                self.show_notice(format!("找不到或无法唯一确定链接：{target}"), true, cx);
                return;
            };
            let name = display_name(&path);
            self.request_close(
                PendingClose::Open {
                    vault_root: self.vault.as_ref().map(|vault| vault.root.clone()),
                    path,
                    fragment: None,
                },
                cx,
            );
            self.show_notice(format!("新笔记 {name}：用 :w 保存后创建"), false, cx);
            return;
        };
        if same_file(&path, &document.file) {
            if let Some(fragment) = fragment
                && !self.reveal_fragment(fragment)
            {
                self.show_notice(format!("找不到标题或块：{fragment}"), true, cx);
            }
            return;
        }
        if !crate::vault::is_markdown(&path) && !is_text_file(&path) {
            cx.open_with_system(&path);
            self.show_notice(
                format!("已用系统应用打开 {}", display_name(&path)),
                false,
                cx,
            );
            return;
        }
        self.request_close(
            PendingClose::Open {
                vault_root: self
                    .vault
                    .as_ref()
                    .filter(|vault| path.starts_with(&vault.root))
                    .map(|vault| vault.root.clone()),
                path,
                fragment: fragment.map(str::to_owned),
            },
            cx,
        );
    }

    /// Vim's `gx`: open the link under the cursor with the system handler.
    fn open_external_link(&mut self, cx: &mut Context<Self>) {
        let Some(destination) = self.cursor_target() else {
            self.show_notice("光标处没有链接", false, cx);
            return;
        };
        if is_external_link(&destination) {
            cx.open_url(&destination);
            return;
        }
        let (target, _) = crate::vault::split_fragment(&destination);
        match self.document.as_ref().and_then(|document| {
            crate::vault::resolve_target(&document.file, self.vault.as_ref(), target)
        }) {
            Some(path) => {
                cx.open_with_system(&path);
                self.show_notice(
                    format!("已用系统应用打开 {}", display_name(&path)),
                    false,
                    cx,
                );
            }
            None => self.show_notice(format!("找不到或无法唯一确定链接：{target}"), true, cx),
        }
    }

    fn move_reading_line(&mut self, down: bool) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            return;
        };
        let Some(current) = blocks.get(self.reading_cursor.block) else {
            return;
        };
        let (line, column) = cursor_line(current, self.reading_cursor.offset);
        let column = *self.reading_column.get_or_insert(column);
        // A folded callout the cursor rests on is a single line.
        let fold = self
            .document
            .as_ref()
            .is_some_and(|document| document.foldable)
            .then(|| {
                fold_ranges(blocks, &self.folded_callouts(blocks))
                    .into_iter()
                    .find(|range| range.contains(&self.reading_cursor.block))
            })
            .flatten();
        let lines = if fold.is_some() {
            0
        } else {
            block_line_count(current)
        };

        let target = if down {
            ((line + 1)..lines)
                .find_map(|line| cursor_for_line(current, self.reading_cursor.block, line, column))
                .or_else(|| {
                    blocks
                        .iter()
                        .enumerate()
                        .skip(fold.map_or(self.reading_cursor.block + 1, |range| range.end))
                        .find_map(|(block, value)| cursor_for_line(value, block, 0, column))
                })
        } else {
            (0..line.min(lines))
                .rev()
                .find_map(|line| cursor_for_line(current, self.reading_cursor.block, line, column))
                .or_else(|| {
                    blocks
                        .iter()
                        .enumerate()
                        .take(fold.map_or(self.reading_cursor.block, |range| range.start))
                        .rev()
                        .find_map(|(block, value)| {
                            (0..block_line_count(value))
                                .rev()
                                .find_map(|line| cursor_for_line(value, block, line, column))
                        })
                })
        };
        if let Some(target) = target {
            self.reading_cursor = target;
            self.rest_on_fold();
        }
    }

    /// gj/gk: move to the next or previous screen line of the current block, keeping the
    /// horizontal position. Returns false when the block has no such line.
    fn move_reading_screen_line(&mut self, down: bool) -> bool {
        let Some(block) = self
            .document
            .as_ref()
            .and_then(|document| document.markdown.blocks.get(self.reading_cursor.block))
        else {
            return false;
        };
        if is_object(block) {
            return false;
        }
        let Some(byte) =
            text_range(&block.text, self.reading_cursor.offset).map(|range| range.start)
        else {
            return false;
        };
        if !self.layouts_ready.get() {
            return false;
        }
        let layouts = self.fragment_layouts.borrow();
        let fragments = layouts
            .iter()
            .filter(|fragment| fragment.block == self.reading_cursor.block)
            .collect::<Vec<_>>();
        let Some(current) = fragments
            .iter()
            .find(|fragment| fragment.range.contains(&byte))
        else {
            return false;
        };
        let Some(position) = current
            .layout
            .position_for_index(byte - current.range.start)
        else {
            return false;
        };
        let line_height = current.layout.line_height();
        let x = *self.reading_desired_x.get_or_insert(position.x);
        // Each candidate is (fragment, the y to probe, the top of that screen line).
        let mut candidates = Vec::new();
        for fragment in &fragments {
            let bounds = fragment.layout.bounds();
            let height = fragment.layout.line_height();
            if std::ptr::eq(*fragment, *current) {
                let next = if down {
                    position.y + line_height
                } else {
                    position.y - line_height
                };
                if next >= bounds.top() && next + height <= bounds.bottom() + px(0.5) {
                    candidates.push((*fragment, next + height / 2.0, next));
                }
            } else if down && bounds.top() >= position.y + line_height - px(1.0) {
                candidates.push((*fragment, bounds.top() + height / 2.0, bounds.top()));
            } else if !down && bounds.bottom() <= position.y + px(1.0) {
                let top = bounds.bottom() - height;
                candidates.push((*fragment, top + height / 2.0, top));
            }
        }
        let Some(row) = candidates
            .iter()
            .map(|(_, _, top)| *top)
            .reduce(|a, b| if down { a.min(b) } else { a.max(b) })
        else {
            return false;
        };
        let distance = |bounds: Bounds<Pixels>| {
            if x < bounds.left() {
                bounds.left() - x
            } else if x > bounds.right() {
                x - bounds.right()
            } else {
                px(0.0)
            }
        };
        let Some((fragment, probe_y, _)) = candidates
            .iter()
            .filter(|(_, _, top)| (*top - row).abs() < px(2.0))
            .min_by(|a, b| {
                f32::from(distance(a.0.layout.bounds()))
                    .total_cmp(&f32::from(distance(b.0.layout.bounds())))
            })
        else {
            return false;
        };
        let bounds = fragment.layout.bounds();
        let probe = point(x.clamp(bounds.left(), bounds.right()), *probe_y);
        let index = fragment
            .layout
            .index_for_position(probe)
            .unwrap_or_else(|nearest| nearest);
        let text = &block.text[fragment.range.clone()];
        let mut local = (0..=index.min(text.len()))
            .rev()
            .find(|byte| text.is_char_boundary(*byte))
            .unwrap_or(0);
        // Past the last character of a line: stay on the line's last character.
        if local > 0
            && (local == text.len() || text[local..].starts_with('\n'))
            && let Some(at) = fragment.layout.position_for_index(local)
            && at.x <= probe.x
        {
            local = text[..local]
                .char_indices()
                .next_back()
                .map_or(0, |(index, _)| index);
        }
        let length = block_len(block);
        let offset = visible_offset(&block.text, fragment.range.start + local).min(length - 1);
        drop(layouts);
        self.reading_cursor.offset = offset;
        self.reading_column = None;
        true
    }

    fn move_reading_line_edge(&mut self, end: bool) {
        let Some(block) = self
            .document
            .as_ref()
            .and_then(|document| document.markdown.blocks.get(self.reading_cursor.block))
        else {
            return;
        };
        let (line, _) = cursor_line(block, self.reading_cursor.offset);
        if let Some(cursor) = cursor_for_line(
            block,
            self.reading_cursor.block,
            line,
            if end { usize::MAX } else { 0 },
        ) {
            self.reading_cursor = cursor;
            self.reading_column = None;
        }
    }

    fn move_reading_first_nonblank(&mut self) {
        let Some(block) = self
            .document
            .as_ref()
            .and_then(|document| document.markdown.blocks.get(self.reading_cursor.block))
        else {
            return;
        };
        let (line, _) = cursor_line(block, self.reading_cursor.offset);
        let column = if is_object(block) {
            0
        } else {
            block
                .text
                .split('\n')
                .nth(line)
                .and_then(|text| {
                    text.chars()
                        .position(|character| !character.is_whitespace())
                })
                .unwrap_or(0)
        };
        if let Some(cursor) = cursor_for_line(block, self.reading_cursor.block, line, column) {
            self.reading_cursor = cursor;
            self.reading_column = None;
        }
    }

    fn move_reading_document_edge(&mut self, end: bool) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            return;
        };
        let target = if end {
            blocks
                .iter()
                .enumerate()
                .rfind(|(_, block)| block_len(block) > 0)
                .map(|(block, value)| ReadingCursor {
                    block,
                    offset: block_len(value) - 1,
                })
        } else {
            blocks
                .iter()
                .enumerate()
                .find(|(_, block)| block_len(block) > 0)
                .map(|(block, _)| ReadingCursor { block, offset: 0 })
        };
        if let Some(target) = target {
            self.reading_cursor = target;
            self.reading_column = None;
            self.rest_on_fold();
        }
    }

    fn enter_source_normal(
        &mut self,
        _: &EnterSourceNormal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.document.is_none() || self.settings_open {
            return;
        }
        if self.nvim.is_none() {
            self.nvim_error = None;
            self.grid = NvimGrid::default();
            self.start_nvim(cx);
        }
        self.view = View::Source;
        if let Some(focus) = &self.focus_handle {
            window.focus(focus, cx);
        }
        if let Some(nvim) = &self.nvim {
            nvim.input("<Esc>");
            if self.synced_cursor != Some(self.reading_cursor)
                && let Some(document) = &self.document
                && let Some((line, column)) = source_position(document, self.reading_cursor)
            {
                nvim.set_cursor(line, column);
            }
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // Linux backends commit a propagated key's character through the input handler, so
        // every key handled here must stop; only real text input is allowed to continue.
        cx.stop_propagation();
        if self.settings_open {
            if event.keystroke.key == "escape" {
                self.settings_open = false;
                cx.notify();
            }
            return;
        }
        if self.switcher.is_some() {
            let control = event.keystroke.modifiers.control;
            let last = self.switcher_rows().saturating_sub(1);
            let Some(switcher) = &mut self.switcher else {
                return;
            };
            let up = matches!(event.keystroke.key.as_str(), "up")
                || (control && matches!(event.keystroke.key.as_str(), "p" | "k"));
            let down = matches!(event.keystroke.key.as_str(), "down")
                || (control && matches!(event.keystroke.key.as_str(), "n" | "j"));
            match event.keystroke.key.as_str() {
                "escape" => {
                    self.switcher = None;
                    self.marked_text.clear();
                }
                "enter" => self.open_switcher_selection(None, cx),
                _ if up || down => {
                    switcher.selected = if up {
                        switcher.selected.saturating_sub(1)
                    } else {
                        (switcher.selected + 1).min(last)
                    };
                    self.switcher_scroll.scroll_to_item(switcher.selected);
                }
                "backspace" => {
                    switcher.query.pop();
                    switcher.selected = 0;
                    self.search_text(cx);
                }
                _ if event.keystroke.key_char.is_some()
                    && !control
                    && !event.keystroke.modifiers.platform =>
                {
                    // Typed text, IME composition included, arrives through the input handler.
                    cx.propagate();
                    return;
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        if self.view == View::Reading {
            if self.notice.take().is_some() {
                cx.notify();
            }
            let key = event.keystroke.key_char.as_deref();
            if !(self.reading_pending_g && matches!(key, Some("j" | "k")))
                && !key.is_some_and(|key| key.parse::<usize>().is_ok())
                && key != Some("g")
            {
                self.reading_desired_x = None;
            }
            if event.keystroke.key == "escape" {
                self.reading_find = None;
                self.reading_pending_g = false;
                self.reading_count = None;
                self.marked_text.clear();
                if self.reading_selection.take().is_some() || self.reading_search.take().is_some() {
                    cx.notify();
                }
                return;
            }
            if self.reading_search.is_some() {
                match event.keystroke.key.as_str() {
                    "enter" => {
                        if let Some(prompt) = self.reading_search.take()
                            && !prompt.query.is_empty()
                        {
                            self.execute_search(&prompt, false, cx);
                            self.last_search = Some(prompt);
                        }
                        self.marked_text.clear();
                        self.reveal_reading_cursor();
                        cx.notify();
                    }
                    "backspace" => {
                        if let Some(search) = &mut self.reading_search {
                            search.query.pop();
                        }
                        cx.notify();
                    }
                    _ => cx.propagate(),
                }
                return;
            }
            if event.keystroke.modifiers.control {
                let (down, full_page) = match event.keystroke.key.as_str() {
                    "d" => (true, false),
                    "u" => (false, false),
                    "f" => (true, true),
                    "b" => (false, true),
                    _ => return,
                };
                let count = self.take_reading_count();
                self.scroll_reading(down, full_page, count);
                self.reading_pending_g = false;
                cx.notify();
                return;
            }
            if let Some(find) = self.reading_find.take() {
                if let Some(target) = key.and_then(single_character) {
                    let count = self.take_reading_count();
                    if let Some(blocks) = self
                        .document
                        .as_ref()
                        .map(|document| &document.markdown.blocks)
                        && let Some(cursor) =
                            find_character(blocks, self.reading_cursor, target, find, count)
                    {
                        self.reading_cursor = cursor;
                        self.reading_column = None;
                        self.reveal_reading_cursor();
                        cx.notify();
                    }
                }
                return;
            }
            if key == Some("v") || key == Some("V") {
                let linewise = key == Some("V");
                self.reading_selection = match self.reading_selection {
                    Some(selection) if selection.linewise == linewise => None,
                    _ => Some(ReadingSelection {
                        anchor: self.reading_cursor,
                        linewise,
                    }),
                };
                self.reading_count = None;
                cx.notify();
                return;
            }
            if key == Some("y") {
                self.copy_selection(cx);
                self.reading_count = None;
                return;
            }
            if key == Some("/") || key == Some("?") {
                self.reading_search = Some(SearchPrompt {
                    query: String::new(),
                    forward: key == Some("/"),
                });
                self.reading_count = None;
                cx.notify();
                return;
            }
            if key == Some("n") || key == Some("N") {
                if let Some(search) = self.last_search.clone() {
                    self.execute_search(&search, key == Some("N"), cx);
                    self.reveal_reading_cursor();
                    cx.notify();
                } else {
                    self.show_notice("还没有搜索过；用 / 或 ? 开始搜索", false, cx);
                }
                return;
            }
            if key == Some("*") || key == Some("#") {
                self.search_word(key == Some("*"), cx);
                self.reveal_reading_cursor();
                cx.notify();
                return;
            }
            if let Some(digit) = key.and_then(|key| key.parse::<usize>().ok())
                && (digit != 0 || self.reading_count.is_some())
            {
                self.reading_count = Some(append_count(self.reading_count.unwrap_or(0), digit));
                return;
            }
            if self.reading_pending_g {
                self.reading_pending_g = false;
                match key {
                    Some("g") => {
                        self.move_reading_document_edge(false);
                        let count = self.take_reading_count();
                        for _ in 1..count {
                            self.move_reading_line(true);
                        }
                        self.reveal_reading_cursor();
                        cx.notify();
                    }
                    Some("j") | Some("k") => {
                        let down = key == Some("j");
                        for _ in 0..self.take_reading_count() {
                            if !self.move_reading_screen_line(down) {
                                self.move_reading_line(down);
                            }
                        }
                        self.reveal_reading_cursor();
                        cx.notify();
                        return;
                    }
                    Some("f") => {
                        self.open_internal_link(cx);
                        cx.notify();
                    }
                    Some("x") => {
                        self.open_external_link(cx);
                        cx.notify();
                    }
                    _ => self.reading_count = None,
                }
                return;
            }
            if std::mem::take(&mut self.reading_pending_z) {
                if key == Some("a")
                    && let Some(id) = self.document.as_ref().and_then(|document| {
                        let block = document.markdown.blocks.get(self.reading_cursor.block)?;
                        let id = block.callout_quote?;
                        // Only callouts written as foldable fold.
                        document
                            .markdown
                            .blocks
                            .iter()
                            .any(|block| {
                                block.callout_quote == Some(id) && block.callout_fold.is_some()
                            })
                            .then_some(id)
                    })
                {
                    self.toggle_callout(id, cx);
                    self.reveal_reading_cursor();
                }
                self.reading_count = None;
                return;
            }
            if key == Some("z") {
                self.reading_pending_z = true;
                return;
            }
            if key == Some("g") {
                self.reading_pending_g = true;
                return;
            }
            if let Some(find) = match key {
                Some("f") => Some(FindPending {
                    forward: true,
                    till: false,
                }),
                Some("F") => Some(FindPending {
                    forward: false,
                    till: false,
                }),
                Some("t") => Some(FindPending {
                    forward: true,
                    till: true,
                }),
                Some("T") => Some(FindPending {
                    forward: false,
                    till: true,
                }),
                _ => None,
            } {
                self.reading_find = Some(find);
                self.reading_pending_g = false;
                return;
            }
            self.reading_pending_g = false;
            let had_count = self.reading_count.is_some();
            let count = self.take_reading_count();
            match key {
                Some("h") => {
                    self.reading_column = None;
                    for _ in 0..count {
                        self.move_reading_cursor(false);
                    }
                }
                Some("l") => {
                    self.reading_column = None;
                    for _ in 0..count {
                        self.move_reading_cursor(true);
                    }
                }
                Some("j") => {
                    for _ in 0..count {
                        self.move_reading_line(true);
                    }
                }
                Some("k") => {
                    for _ in 0..count {
                        self.move_reading_line(false);
                    }
                }
                Some("0") => self.move_reading_line_edge(false),
                Some("^") => self.move_reading_first_nonblank(),
                Some("$") => {
                    for _ in 1..count {
                        self.move_reading_line(true);
                    }
                    self.move_reading_line_edge(true);
                }
                Some("G") if had_count => {
                    self.move_reading_document_edge(false);
                    for _ in 1..count {
                        self.move_reading_line(true);
                    }
                }
                Some("G") => self.move_reading_document_edge(true),
                Some("w") => {
                    for _ in 0..count {
                        self.move_reading_word(WordMotion::Next);
                    }
                }
                Some("b") => {
                    for _ in 0..count {
                        self.move_reading_word(WordMotion::Previous);
                    }
                }
                Some("e") => {
                    for _ in 0..count {
                        self.move_reading_word(WordMotion::End);
                    }
                }
                _ => return,
            }
            self.reveal_reading_cursor();
            cx.notify();
            return;
        }
        let unusable = self.nvim_error.is_some() || self.nvim.is_none();
        // Without a working Neovim, Esc also leaves: nothing else can receive it.
        let leaves_source = (self.reading_key.matches(&event.keystroke)
            && (self.grid.is_normal() || unusable))
            || (unusable && event.keystroke.key == "escape");
        if self.nvim.is_none() && !leaves_source {
            // Neovim exited or failed to start; Enter starts it again.
            if event.keystroke.key == "enter" && self.document.is_some() {
                self.nvim_error = None;
                self.grid = NvimGrid::default();
                self.start_nvim(cx);
                cx.notify();
                return;
            }
        }
        if self.has_reading_view() && leaves_source {
            if self.parse_pending {
                self.parse_document();
            }
            self.view = View::Reading;
            if let Some(nvim) = &self.nvim {
                nvim.query_cursor();
            }
            self.compile_visuals(cx);
            cx.notify();
        } else if self.grid.accepts_text_input()
            && event.keystroke.key_char.is_some()
            && !event.keystroke.modifiers.control
            && (!event.keystroke.modifiers.alt || composes_with_option(&event.keystroke))
            && !event.keystroke.modifiers.platform
            && !matches!(
                event.keystroke.key.as_str(),
                "enter" | "escape" | "backspace" | "tab" | "delete"
            )
        {
            // Printable text is committed through EntityInputHandler so IME composition is not duplicated.
            cx.propagate();
        } else if let Some(nvim) = &self.nvim
            && let Some(keys) = nvim_key(&event.keystroke)
        {
            nvim.input(keys);
        }
    }

    fn render_source(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        if let Some(error) = &self.nvim_error {
            return div()
                .flex_1()
                .p_8()
                .text_color(rgb(theme.error_text))
                .when_some(self.focus_handle.clone(), |element, focus| {
                    element.track_focus(&focus)
                })
                .child(error.clone())
                .into_any_element();
        }

        let family = crate::fonts::mono();
        let font_size = px(SOURCE_FONT_SIZE);
        let line_height = px(SOURCE_LINE_HEIGHT);
        let font_id = window
            .text_system()
            .resolve_font(&gpui::font(family.clone()));
        let cell_width = window
            .text_system()
            .advance(font_id, font_size, 'm')
            .map(|advance| advance.width)
            .ok()
            .filter(|width| *width > px(0.0))
            .unwrap_or(px(SOURCE_FONT_SIZE * 0.6));
        self.cell_size = size(cell_width, line_height);

        let view = cx.entity();
        let focus = self.focus_handle.clone();
        let (foreground, background) = self.grid.colors();
        let padding = px(GRID_PADDING);
        let cell = |column: usize, row: usize| {
            (
                padding + cell_width * column as f32,
                padding + line_height * row as f32,
            )
        };

        let mut layers = Vec::new();
        for row in 0..self.grid.height() {
            for run in self.grid.row_runs(row) {
                let (left, top) = cell(run.column, row);
                let style = run.highlight;
                layers.push(
                    div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(cell_width * run.width as f32)
                        .h(line_height)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .when_some(style.background, |element, color| element.bg(rgb(color)))
                        .text_color(rgb(style.foreground))
                        .when(style.bold, |element| element.font_weight(FontWeight::BOLD))
                        .when(style.italic, |element| element.italic())
                        .when(style.underline, |element| {
                            element
                                .underline()
                                .text_decoration_color(rgb(style.special))
                        })
                        .when(style.strikethrough, |element| element.line_through())
                        .child(run.text)
                        .into_any_element(),
                );
            }
        }
        if let Some(cursor) = self.grid.visible_cursor() {
            let (left, top) = cell(cursor.column, cursor.row);
            let width = cell_width * cursor.width as f32;
            let element = if !self.marked_text.is_empty() {
                // IME composition is drawn at the cursor until the text is committed to Neovim.
                div()
                    .absolute()
                    .left(left)
                    .top(top)
                    .h(line_height)
                    .whitespace_nowrap()
                    .bg(rgb(IME_MARKED_BACKGROUND))
                    .text_color(rgb(WHITE))
                    .underline()
                    .child(self.marked_text.clone())
            } else {
                match cursor.shape {
                    CursorShape::Block => div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(width)
                        .h(line_height)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .bg(rgb(foreground))
                        .text_color(rgb(background))
                        .child(cursor.text),
                    CursorShape::Vertical(fraction) => div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w((cell_width * fraction).max(px(2.0)))
                        .h(line_height)
                        .bg(rgb(foreground)),
                    CursorShape::Horizontal(fraction) => {
                        let height = (line_height * fraction).max(px(2.0));
                        div()
                            .absolute()
                            .left(left)
                            .top(top + line_height - height)
                            .w(width)
                            .h(height)
                            .bg(rgb(foreground))
                    }
                }
            };
            layers.push(element.into_any_element());
        }

        let resizer = self.nvim.as_ref().map(NvimClient::resizer);
        let requested = self.nvim_size.clone();
        let origin = self.grid_origin.clone();
        let warning = self.nvim_warning.clone().map(|warning| {
            div()
                .flex_none()
                .flex()
                .items_start()
                .justify_between()
                .gap_3()
                .m_3()
                .mb_0()
                .p_3()
                .rounded_md()
                .bg(rgb(theme.warning_bg))
                .text_color(rgb(theme.warning_text))
                .text_sm()
                // Without min_w_0 the text claims its unwrapped width and runs off the window.
                .child(div().flex_1().min_w_0().child(warning))
                .child(
                    div()
                        .id("dismiss-nvim-warning")
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|element| element.bg(rgb(theme.warning_hover)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nvim_warning = None;
                            cx.notify();
                        }))
                        .child("×"),
                )
        });
        let ime_hint = self.ime_hint.then(|| {
            let action = |id: &'static str, label: &'static str, forever: bool| {
                div()
                    .id(id)
                    .flex_none()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|element| element.bg(rgb(theme.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.ime_hint = false;
                        if forever
                            && let Err(error) =
                                crate::settings::update(|settings| settings.hide_ime_hint = true)
                        {
                            this.show_notice(error, true, cx);
                        }
                        cx.notify();
                    }))
                    .child(label)
            };
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .m_3()
                .mb_0()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(theme.block))
                .text_color(rgb(theme.muted))
                .text_sm()
                .child(
                    div().flex_1().min_w_0().child(
                        "提示：可以用 im-select.nvim 在 Insert/Normal 间自动切换中英文输入源。",
                    ),
                )
                .child(action("hide-ime-hint", "不再提示", true))
                .child(action("close-ime-hint", "×", false))
        });

        div()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(rgb(background))
            .children(warning)
            .children(ime_hint)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .text_color(rgb(foreground))
                    .font_family(family)
                    .text_size(font_size)
                    .line_height(line_height)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.grid_mouse("left", "press", event.position, &event.modifiers, cx);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.grid_mouse("right", "press", event.position, &event.modifiers, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        if let Some(button) = this.mouse_button
                            && event.pressed_button.is_some()
                        {
                            this.grid_mouse(button, "drag", event.position, &event.modifiers, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseUpEvent, _, cx| {
                            this.grid_mouse(
                                "left",
                                "release",
                                event.position,
                                &event.modifiers,
                                cx,
                            );
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseUpEvent, _, cx| {
                            this.grid_mouse(
                                "right",
                                "release",
                                event.position,
                                &event.modifiers,
                                cx,
                            );
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                        this.grid_scroll(event, cx);
                    }))
                    .children(layers)
                    .when_some(focus, |element, focus| {
                        element.track_focus(&focus).child(
                            canvas(
                                move |bounds, _, _| {
                                    origin.set(bounds.origin);
                                    let columns = ((bounds.size.width - padding * 2.0) / cell_width)
                                        .floor()
                                        .max(20.0)
                                        as i64;
                                    let rows = ((bounds.size.height - padding * 2.0) / line_height)
                                        .floor()
                                        .max(5.0)
                                        as i64;
                                    if requested.get() != (columns, rows) {
                                        requested.set((columns, rows));
                                        if let Some(resizer) = &resizer {
                                            resizer.resize(columns, rows);
                                        }
                                    }
                                },
                                move |bounds, _, window, cx| {
                                    window.handle_input(
                                        &focus,
                                        ElementInputHandler::new(bounds, view),
                                        cx,
                                    );
                                },
                            )
                            .absolute()
                            .size_full(),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The note title, or a tab per note open in Neovim when there are several.
    fn render_tabs(&self, title: SharedString, cx: &mut Context<Self>) -> AnyElement {
        if self.open_buffers.len() < 2 {
            return div()
                .text_lg()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(title)
                .into_any_element();
        }
        let theme = self.theme;
        let current = self.current_tab();
        div()
            .id("tabs")
            .flex_1()
            .min_w_0()
            .mr_4()
            .flex()
            .gap_1()
            .overflow_x_scroll()
            .children(
                self.open_buffers
                    .iter()
                    .enumerate()
                    .map(|(index, (path, modified))| {
                        let selected = current == Some(index);
                        let name = path.file_stem().map_or_else(
                            || display_name(path),
                            |stem| stem.to_string_lossy().into_owned(),
                        );
                        let open = path.clone();
                        let close = path.clone();
                        div()
                            .id(("tab", index))
                            .flex_none()
                            .max_w(px(200.0))
                            .h(px(30.0))
                            .pl_3()
                            .pr_1()
                            .flex()
                            .items_center()
                            .gap_1()
                            .rounded_md()
                            .text_sm()
                            .cursor_pointer()
                            .when(selected, |element| {
                                element
                                    .bg(rgb(theme.accent_soft_bg))
                                    .text_color(rgb(theme.accent_soft_text))
                            })
                            .when(!selected, |element| {
                                element
                                    .text_color(rgb(theme.muted))
                                    .hover(|element| element.bg(rgb(theme.hover)))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_tab(open.clone(), cx);
                            }))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(name),
                            )
                            .child(
                                div()
                                    .id(("close-tab", index))
                                    .flex_none()
                                    .w(px(18.0))
                                    .text_center()
                                    .rounded_md()
                                    .hover(|element| element.bg(rgb(theme.hover)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        if let Some(nvim) = &this.nvim {
                                            nvim.close_buffer(close.clone());
                                        }
                                    }))
                                    .child(if *modified { "●" } else { "×" }),
                            )
                    }),
            )
            .into_any_element()
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let shortcut = |key: &str| {
            if cfg!(target_os = "macos") {
                format!("⌘{key}")
            } else {
                format!("Ctrl+{key}")
            }
        };
        let button = |id: &'static str, label: &'static str, hint: String, directory: bool| {
            div()
                .id(id)
                .w(px(220.0))
                .px_4()
                .py_2()
                .flex()
                .justify_between()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(theme.block))
                .hover(|element| element.bg(rgb(theme.hover)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose_path(directory, window, cx);
                }))
                .child(label)
                .child(div().text_color(rgb(theme.muted)).child(hint))
        };
        let recent = self
            .recent
            .iter()
            .filter(|path| path.exists())
            .take(8)
            .enumerate()
            .map(|(index, path)| {
                let target = path.clone();
                let folder = path.is_dir();
                div()
                    .id(("recent", index))
                    .w(px(440.0))
                    .px_3()
                    .py_1()
                    .flex()
                    .gap_3()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|element| element.bg(rgb(theme.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.request_close(
                            PendingClose::Open {
                                path: target.clone(),
                                vault_root: None,
                                fragment: None,
                            },
                            cx,
                        );
                    }))
                    .child(
                        div()
                            .flex_none()
                            .text_color(rgb(theme.accent))
                            .child(display_name(path) + if folder { "/" } else { "" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(rgb(theme.faint))
                            .child(
                                path.parent()
                                    .map(|parent| parent.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            ),
                    )
            })
            .collect::<Vec<_>>();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(div().text_2xl().child("本地 Markdown，真实 Neovim"))
            .when_some(self.error.clone(), |element, error| {
                element.child(
                    div()
                        .max_w(px(560.0))
                        .text_sm()
                        .text_color(rgb(theme.error_text))
                        .child(error),
                )
            })
            .child(
                div()
                    .mt_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .text_sm()
                    .child(button("open-file", "打开文件…", shortcut("O"), false))
                    .child(button(
                        "open-folder",
                        "打开文件夹…",
                        shortcut(if cfg!(target_os = "macos") {
                            "⇧O"
                        } else {
                            "Shift+O"
                        }),
                        true,
                    )),
            )
            .when(!recent.is_empty(), |element| {
                element
                    .child(
                        div()
                            .mt_6()
                            .w(px(440.0))
                            .px_3()
                            .text_xs()
                            .text_color(rgb(theme.muted))
                            .child("最近打开"),
                    )
                    .child(div().flex().flex_col().text_sm().children(recent))
            })
            .into_any_element()
    }

    fn render_status_bar(&self, search_prompt: Option<String>) -> AnyElement {
        let theme = self.theme;
        let mode: SharedString = match self.view {
            View::Reading => match self.reading_selection {
                Some(selection) if selection.linewise => "V-LINE".into(),
                Some(_) => "VISUAL".into(),
                None => "阅读".into(),
            },
            View::Source if self.nvim.is_none() || self.nvim_error.is_some() => "源码".into(),
            View::Source => source_mode_label(self.grid.mode()).into(),
        };
        let mut pending = String::new();
        if self.view == View::Reading {
            if let Some(count) = self.reading_count {
                pending.push_str(&count.to_string());
            }
            if self.reading_pending_g {
                pending.push('g');
            }
            if self.reading_pending_z {
                pending.push('z');
            }
            if let Some(find) = self.reading_find {
                pending.push(match (find.forward, find.till) {
                    (true, false) => 'f',
                    (false, false) => 'F',
                    (true, true) => 't',
                    (false, true) => 'T',
                });
            }
        }
        let unsaved = self.modified.then(|| {
            div()
                .flex_none()
                .text_color(rgb(theme.warning_text))
                .child("● 未保存")
        });
        let hint = match self.view {
            View::Reading if self.document.is_some() => "Enter 编辑",
            View::Source if self.nvim.is_none() && self.document.is_some() => "Enter 重新打开",
            View::Source if self.has_reading_view() && self.grid.is_normal() => {
                match self.reading_key {
                    ReadingKey::Escape => "Esc 返回阅读",
                    ReadingKey::SecondaryEnter if cfg!(target_os = "macos") => "⌘Enter 返回阅读",
                    ReadingKey::SecondaryEnter => "Ctrl+Enter 返回阅读",
                }
            }
            _ => "",
        };
        let message = if let Some(prompt) = search_prompt {
            div()
                .flex_1()
                .min_w_0()
                .font_family(crate::fonts::mono())
                .text_color(rgb(theme.text))
                .child(prompt)
        } else {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .when_some(self.notice.as_ref(), |element, notice| {
                    element
                        .text_color(rgb(if notice.error {
                            theme.error_text
                        } else {
                            theme.success_text
                        }))
                        .child(notice.text.clone())
                })
        };
        div()
            .flex_none()
            .h(px(28.0))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface))
            .text_xs()
            .text_color(rgb(theme.muted))
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(if self.view == View::Source {
                        theme.source_chip_bg
                    } else {
                        theme.accent_soft_bg
                    }))
                    .text_color(rgb(if self.view == View::Source {
                        theme.source_chip_text
                    } else {
                        theme.accent_soft_text
                    }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(mode),
            )
            .when(!pending.is_empty(), |element| {
                element.child(
                    div()
                        .flex_none()
                        .font_family(crate::fonts::mono())
                        .text_color(rgb(theme.text))
                        .child(pending),
                )
            })
            .child(message)
            .children(unsaved)
            .child(div().flex_none().child(hint))
            .into_any_element()
    }

    /// The grid cell under a window position, clamped to the grid.
    fn grid_cell(&self, position: Point<Pixels>) -> (usize, usize) {
        let local = position - self.grid_origin.get();
        let padding = px(GRID_PADDING);
        let column = ((local.x - padding) / self.cell_size.width)
            .floor()
            .max(0.0) as usize;
        let row = ((local.y - padding) / self.cell_size.height)
            .floor()
            .max(0.0) as usize;
        (
            row.min(self.grid.height().saturating_sub(1)),
            column.min(self.grid.width().saturating_sub(1)),
        )
    }

    fn grid_mouse(
        &mut self,
        button: &'static str,
        action: &'static str,
        position: Point<Pixels>,
        modifiers: &Modifiers,
        cx: &mut Context<Self>,
    ) {
        match action {
            "press" => self.mouse_button = Some(button),
            "release" => self.mouse_button = None,
            _ => {}
        }
        if let Some(nvim) = &self.nvim {
            nvim.mouse(
                button,
                action,
                mouse_modifiers(modifiers),
                self.grid_cell(position),
            );
        }
        cx.stop_propagation();
    }

    /// Send whole wheel steps; trackpads report small pixel deltas that accumulate.
    fn grid_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(nvim) = &self.nvim else {
            return;
        };
        // Neovim scrolls three lines per wheel step by default ('mousescroll').
        let step = self.cell_size.height * 3.0;
        self.scroll_remainder += event.delta.pixel_delta(self.cell_size.height).y;
        let cell = self.grid_cell(event.position);
        let modifier = mouse_modifiers(&event.modifiers);
        while self.scroll_remainder.abs() >= step {
            let up = self.scroll_remainder > px(0.0);
            nvim.mouse(
                "wheel",
                if up { "up" } else { "down" },
                modifier.clone(),
                cell,
            );
            self.scroll_remainder -= if up { step } else { -step };
        }
        cx.stop_propagation();
    }

    /// Notes linking to this one, under its last block.
    fn render_backlinks(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = self.theme;
        let hits = self.backlinks.as_ref()?.1.as_ref()?;
        let vault_root = self.vault.as_ref().map(|vault| vault.root.clone());
        // A hub note can have hundreds; every row is drawn on each frame.
        const SHOWN: usize = 100;
        let rows = hits.iter().take(SHOWN).enumerate().map(|(index, hit)| {
            let path = hit.path.clone();
            let line = hit.line;
            let vault_root = vault_root.clone();
            div()
                .id(("backlink", index))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|element| element.bg(rgb(theme.hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.request_close(
                        PendingClose::Open {
                            path: path.clone(),
                            vault_root: vault_root.clone(),
                            fragment: Some(line_fragment(line)),
                        },
                        cx,
                    );
                }))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap_3()
                        .child(div().text_color(rgb(theme.accent)).child(hit.name.clone()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(theme.faint))
                                .child(hit.folder.clone()),
                        ),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_sm()
                        .text_color(rgb(theme.muted))
                        .child(hit.text.clone()),
                )
        });
        let notes = hits
            .iter()
            .map(|hit| &hit.path)
            .collect::<HashSet<_>>()
            .len();
        Some(
            div()
                .mx_auto()
                .w_full()
                .max_w(px(820.0))
                .flex_none()
                .mt_8()
                .pt_4()
                .border_t_1()
                .border_color(rgb(theme.border))
                .flex()
                .flex_col()
                .gap_1()
                // Not text: the reading cursor stays where it is.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .mb_1()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.muted))
                        .child(if hits.is_empty() {
                            "反向链接：没有其他笔记链接到这里".to_owned()
                        } else {
                            format!("反向链接 · {notes} 个笔记")
                        }),
                )
                .children(rows)
                .when(hits.len() > SHOWN, |element| {
                    element.child(div().px_3().text_sm().text_color(rgb(theme.faint)).child(
                        format!(
                            "还有 {} 处，可用 {} 搜索",
                            hits.len() - SHOWN,
                            if cfg!(target_os = "macos") {
                                "⌘⇧F"
                            } else {
                                "Ctrl+Shift+F"
                            }
                        ),
                    ))
                })
                .into_any_element(),
        )
    }

    fn render_switcher(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let Some(switcher) = self.switcher.as_ref() else {
            return div().into_any_element();
        };
        let query = switcher.query.clone();
        let typed = format!("{query}{}", self.marked_text);
        let placeholder = match (switcher.text, self.vault.is_some()) {
            _ if switcher.rename => "新的笔记名称…",
            (true, true) => "搜索所有笔记的文字…",
            (true, false) => "搜索最近打开的笔记的文字…",
            (false, true) => "输入笔记名称…",
            (false, false) => "输入最近打开的文件名…",
        };
        let input = div()
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(rgb(theme.border))
            .when(typed.is_empty(), |element| {
                element.text_color(rgb(theme.faint)).child(placeholder)
            })
            .when(!typed.is_empty(), |element| element.child(typed))
            .child(div().ml_px().w(px(2.0)).h(px(18.0)).bg(rgb(theme.accent)));
        // Each row: a title (note name), its folder, and for text hits the matching line.
        let all_tags = switcher.text && query.trim() == ALL_TAGS;
        let entries: Vec<(String, String, Option<String>)> = if switcher.rename {
            Vec::new()
        } else if all_tags {
            switcher
                .tags
                .iter()
                .map(|(tag, notes)| (format!("#{tag}"), format!("{notes} 篇笔记"), None))
                .collect()
        } else if switcher.text {
            switcher
                .hits
                .iter()
                .map(|hit| (hit.name.clone(), hit.folder.clone(), Some(hit.text.clone())))
                .collect()
        } else {
            self.switcher_items()
                .into_iter()
                .map(|item| match item.alias {
                    _ if item.create => (
                        format!("＋ {}", item.name),
                        if item.folder.is_empty() {
                            "新建笔记".to_owned()
                        } else {
                            format!("新建笔记 · {}", item.folder)
                        },
                        None,
                    ),
                    // An alias match shows the alias, then the note it names.
                    Some(alias) => {
                        let note = if item.folder.is_empty() {
                            item.name
                        } else {
                            format!("{}/{}", item.folder, item.name)
                        };
                        (alias, format!("↪ {note}"), None)
                    }
                    None => (item.name, item.folder, None),
                })
                .collect()
        };
        let empty = entries.is_empty();
        let selected = switcher.selected.min(entries.len().saturating_sub(1));
        let rows = entries.into_iter().take(SWITCHER_ROWS).enumerate().map(
            |(index, (name, folder, line))| {
                let current = index == selected;
                div()
                    .id(("switcher-row", index))
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .cursor_pointer()
                    .when(current, |element| element.bg(rgb(theme.accent_soft_bg)))
                    .when(!current, |element| {
                        element.hover(|element| element.bg(rgb(theme.hover)))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_switcher_selection(Some(index), cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_3()
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(320.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_color(rgb(if current {
                                        theme.accent_soft_text
                                    } else {
                                        theme.text
                                    }))
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .text_sm()
                                    .text_color(rgb(theme.faint))
                                    .child(folder),
                            ),
                    )
                    .when_some(line, |element, line| {
                        element.child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_sm()
                                .text_color(rgb(theme.muted))
                                .child(line),
                        )
                    })
            },
        );
        let status = if switcher.rename {
            Some(if self.vault.is_some() {
                "重命名这篇笔记，并更新 vault 中链接到它的笔记"
            } else {
                "重命名这篇笔记"
            })
        } else if !empty {
            None
        } else if query.trim().is_empty() {
            (!switcher.text).then_some("没有可打开的笔记")
        } else if all_tags {
            Some("没有找到标签")
        } else if switcher.text {
            Some("没有找到这段文字")
        } else {
            Some("没有匹配的笔记")
        };
        div()
            .absolute()
            .size_full()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(72.0))
            .px_4()
            .bg(rgb(theme.overlay).opacity(theme.overlay_opacity * 0.5))
            .id("switcher-backdrop")
            .on_click(cx.listener(|this, _, _, cx| {
                this.switcher = None;
                this.marked_text.clear();
                cx.notify();
            }))
            .child(
                div()
                    .id("switcher")
                    .w(px(560.0))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(theme.card_border))
                    .bg(rgb(theme.panel))
                    // Clicks inside the panel do not close it.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(input)
                    .child(
                        div()
                            .id("switcher-rows")
                            .max_h(px(440.0))
                            .overflow_y_scroll()
                            .track_scroll(&self.switcher_scroll)
                            .children(rows),
                    )
                    .when_some(status, |element, status| {
                        element.child(
                            div()
                                .px_4()
                                .py_3()
                                .text_sm()
                                .text_color(rgb(theme.muted))
                                .child(status),
                        )
                    })
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(rgb(theme.border))
                            .text_xs()
                            .text_color(rgb(theme.faint))
                            .child(if switcher.rename {
                                "Enter 重命名 · Esc 取消"
                            } else if all_tags {
                                "↑↓ 选择 · Enter 搜索该标签 · Esc 关闭"
                            } else if switcher.text {
                                "↑↓ 选择 · Enter 打开到该行 · 只输入 # 列出所有标签 · Esc 关闭"
                            } else {
                                "↑↓ 选择 · Enter 打开 · Esc 关闭"
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let busy = matches!(
            self.update_status,
            UpdateStatus::Checking | UpdateStatus::Installing(_)
        );
        let status: SharedString = match &self.update_status {
            UpdateStatus::Idle => "尚未检查更新".into(),
            UpdateStatus::Checking => "正在检查更新…".into(),
            UpdateStatus::UpToDate => "已是最新版本".into(),
            UpdateStatus::Available(version) => format!("发现新版本 {version}").into(),
            UpdateStatus::Installing(version) => format!("正在下载并安装 {version}…").into(),
            UpdateStatus::Installed(version) => format!("{version} 已安装，重启后生效").into(),
            UpdateStatus::Failed(error) => error.clone(),
        };
        let packaged = update::is_packaged_app();
        let failed = matches!(self.update_status, UpdateStatus::Failed(_));
        let status_color = if failed {
            theme.error_text
        } else {
            theme.muted
        };
        let check_label = if busy {
            "处理中…"
        } else if failed {
            "重试"
        } else if matches!(self.update_status, UpdateStatus::UpToDate) {
            "重新检查"
        } else {
            "检查更新"
        };
        let check_button = (!matches!(
            self.update_status,
            UpdateStatus::Available(_) | UpdateStatus::Installed(_)
        ))
        .then(|| {
            div()
                .id("check-updates")
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(if busy {
                    theme.disabled_bg
                } else {
                    theme.button
                }))
                .text_color(rgb(if busy {
                    theme.disabled_text
                } else {
                    theme.button_text
                }))
                .font_weight(FontWeight::SEMIBOLD)
                .when(!busy, |element| {
                    element
                        .cursor_pointer()
                        .hover(|element| element.bg(rgb(theme.button_hover)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.check_for_updates(false, cx);
                        }))
                })
                .child(check_label)
        });
        let install_button = self.available_update.as_ref().map(|update| {
            let label: SharedString = if packaged {
                format!("安装 {}", update.version).into()
            } else {
                "请在 Rusidian.app 中安装".into()
            };
            div()
                .id("install-update")
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(if packaged {
                    theme.button
                } else {
                    theme.disabled_bg
                }))
                .text_color(rgb(if packaged {
                    theme.button_text
                } else {
                    theme.disabled_text
                }))
                .font_weight(FontWeight::SEMIBOLD)
                .when(packaged, |element| {
                    element
                        .cursor_pointer()
                        .hover(|element| element.bg(rgb(theme.button_hover)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.install_available_update(cx);
                        }))
                })
                .child(label)
        });

        let restart_button = matches!(self.update_status, UpdateStatus::Installed(_)).then(|| {
            div()
                .id("restart-after-update")
                .px_4()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(theme.button))
                .text_color(rgb(theme.button_text))
                .font_weight(FontWeight::SEMIBOLD)
                .hover(|element| element.bg(rgb(theme.button_hover)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.request_close(PendingClose::Restart, cx);
                }))
                .child("重启应用")
        });
        let appearance_picker = div()
            .flex()
            .p_1()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.card_border))
            .bg(rgb(theme.card))
            .children(
                [
                    (Appearance::System, "跟随系统"),
                    (Appearance::Light, "亮色"),
                    (Appearance::Dark, "暗色"),
                ]
                .into_iter()
                .map(|(appearance, label)| {
                    let selected = self.appearance == appearance;
                    div()
                        .id(label)
                        .flex_1()
                        .py_2()
                        .rounded_md()
                        .text_center()
                        .text_sm()
                        .cursor_pointer()
                        .when(selected, |element| {
                            element
                                .bg(rgb(theme.button))
                                .text_color(rgb(theme.button_text))
                                .font_weight(FontWeight::SEMIBOLD)
                        })
                        .when(!selected, |element| {
                            element.hover(|element| element.bg(rgb(theme.hover)))
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_appearance(appearance, cx);
                        }))
                        .child(label)
                }),
            );
        let reading_key_picker = div()
            .flex()
            .p_1()
            .gap_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.card_border))
            .bg(rgb(theme.card))
            .children(
                [ReadingKey::Escape, ReadingKey::SecondaryEnter]
                    .into_iter()
                    .map(|key| {
                        let selected = self.reading_key == key;
                        div()
                            .id(key.label())
                            .flex_1()
                            .py_2()
                            .rounded_md()
                            .text_center()
                            .text_sm()
                            .cursor_pointer()
                            .when(selected, |element| {
                                element
                                    .bg(rgb(theme.button))
                                    .text_color(rgb(theme.button_text))
                                    .font_weight(FontWeight::SEMIBOLD)
                            })
                            .when(!selected, |element| {
                                element.hover(|element| element.bg(rgb(theme.hover)))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_reading_key(key, cx);
                            }))
                            .child(key.label())
                    }),
            );
        let reading_key_note: SharedString = match &self.escape_mapping {
            Some(mapping) => format!(
                "在 Neovim Normal 模式按此键返回阅读视图。你的 Neovim 把 Esc 映射为 {mapping}；选择 {} 可让这个映射生效。",
                ReadingKey::SecondaryEnter.label()
            )
            .into(),
            None => "在 Neovim Normal 模式按此键返回阅读视图；其他按键都交给 Neovim。".into(),
        };
        let tex_button = |id: &'static str, label: &'static str, vault: Option<PathBuf>| {
            div()
                .id(id)
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(theme.card_border))
                .bg(rgb(theme.card))
                .cursor_pointer()
                .hover(|element| element.bg(rgb(theme.hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.edit_preamble(vault.clone(), cx);
                }))
                .child(label)
        };
        let tex_buttons = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .text_sm()
            .child(tex_button("edit-global-preamble", "编辑全局前导内容", None))
            .when_some(
                self.vault.as_ref().map(|vault| vault.root.clone()),
                |element, root| {
                    element.child(tex_button(
                        "edit-vault-preamble",
                        "编辑此 vault 的前导内容",
                        Some(root),
                    ))
                },
            )
            .child(
                div()
                    .w_full()
                    .text_color(rgb(theme.faint))
                    .child("放在 \\begin{document} 之前，保存在系统设置目录，不写入 vault。"),
            );
        let auto_toggle = div()
            .w(px(44.0))
            .h(px(24.0))
            .p_1()
            .rounded_md()
            .flex()
            .items_center()
            .when(self.auto_update, |element| element.justify_end())
            .bg(rgb(if self.auto_update {
                theme.button
            } else {
                theme.toggle_off
            }))
            .child(
                div()
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded_full()
                    .bg(rgb(theme.knob)),
            );

        let (command, shift, reading_only) = if cfg!(target_os = "macos") {
            ("⌘", "⇧", "")
        } else {
            ("Ctrl+", "Shift+", "（阅读视图中）")
        };
        let shortcuts = div()
            .flex()
            .flex_col()
            .gap_1()
            .text_sm()
            .children(
                [
                    (format!("{command}P"), "按名称快速打开笔记".to_owned()),
                    (
                        format!("{command}{shift}F"),
                        "在所有笔记中搜索文字（只输入 # 列出所有标签）".to_owned(),
                    ),
                    (
                        format!("{command}O / {command}{shift}O"),
                        "打开文件 / 文件夹".to_owned(),
                    ),
                    (format!("{command}N"), "新建窗口".to_owned()),
                    (
                        format!("{command}W / {command}{shift}W"),
                        "关闭标签 / 窗口".to_owned(),
                    ),
                    (format!("{command}\\"), "显示或隐藏文件列表".to_owned()),
                    ("F2".to_owned(), "重命名笔记并更新链接到它的笔记".to_owned()),
                    ("Enter".to_owned(), "编辑：进入 Neovim Normal".to_owned()),
                    (
                        self.reading_key.label().to_owned(),
                        "在 Normal 模式返回阅读视图".to_owned(),
                    ),
                    (
                        "j k · gj gk · w b e".to_owned(),
                        "按行、屏幕行、词移动".to_owned(),
                    ),
                    ("/ ? n N · * #".to_owned(), "查找".to_owned()),
                    (
                        format!("v V · y · {command}C"),
                        "选择并复制（也可用鼠标拖选）".to_owned(),
                    ),
                    ("za".to_owned(), "折叠 / 展开可折叠的 callout".to_owned()),
                    (
                        "gf · gx".to_owned(),
                        "打开链接、嵌入、标签 / 用系统应用打开".to_owned(),
                    ),
                ]
                .into_iter()
                .map(|(keys, action)| {
                    div()
                        .flex()
                        .gap_3()
                        .child(
                            div()
                                .w(px(170.0))
                                .flex_none()
                                .font_family(crate::fonts::mono())
                                .text_color(rgb(theme.muted))
                                .child(keys),
                        )
                        .child(div().flex_1().min_w_0().child(action))
                }),
            )
            .child(div().text_color(rgb(theme.faint)).child(format!(
                "{command} 组合键{reading_only}由 Rusidian 处理，其余按键交给 Neovim。"
            )));
        // Linux builds come from source; the updater only installs the signed macOS app.
        let updates = if cfg!(target_os = "macos") {
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(theme.card_border))
                        .bg(rgb(theme.card))
                        .child(
                            div()
                                .p_4()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child("当前版本"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(status_color))
                                                .child(status),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .children(check_button)
                                        .children(install_button)
                                        .children(restart_button),
                                ),
                        )
                        .child(div().h(px(1.0)).bg(rgb(theme.border)))
                        .child(
                            div()
                                .id("toggle-auto-update")
                                .p_4()
                                .flex()
                                .items_center()
                                .justify_between()
                                .cursor_pointer()
                                .hover(|element| element.bg(rgb(theme.hover)))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_auto_update(cx)))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child("自动更新"),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(theme.muted))
                                                .child("启动时检查，验证签名后自动安装"),
                                        ),
                                )
                                .child(auto_toggle),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(theme.faint))
                        .child("更新仅访问 GitHub Release；笔记内容不会离开本机。"),
                )
                .into_any_element()
        } else {
            div()
                .text_sm()
                .text_color(rgb(theme.muted))
                .child("Linux 版本从源码构建：拉取最新代码并重新构建即可更新。")
                .into_any_element()
        };

        div()
            .absolute()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(theme.overlay).opacity(theme.overlay_opacity))
            .p_4()
            .child(
                div()
                    .w(px(620.0))
                    // Short windows scroll the settings instead of cutting off the header.
                    .max_h_full()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(theme.card_border))
                    .bg(rgb(theme.panel))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex_none()
                            .h(px(76.0))
                            .px_6()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .w(px(10.0))
                                            .h(px(32.0))
                                            .rounded_md()
                                            .bg(rgb(theme.button)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(
                                                div()
                                                    .text_xl()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child("设置"),
                                            )
                                            .child(
                                                div().text_sm().text_color(rgb(theme.muted)).child(
                                                    format!(
                                                        "Rusidian {}",
                                                        env!("CARGO_PKG_VERSION")
                                                    ),
                                                ),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .id("close-settings")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_color(rgb(theme.muted))
                                    .hover(|element| element.bg(rgb(theme.hover)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = false;
                                        cx.notify();
                                    }))
                                    .child("完成  Esc"),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-body")
                            .min_h_0()
                            .overflow_y_scroll()
                            .border_t_1()
                            .border_color(rgb(theme.border))
                            .p_6()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.accent))
                                    .child("外观"),
                            )
                            .child(appearance_picker)
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.accent))
                                    .child("返回阅读视图"),
                            )
                            .child(reading_key_picker)
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(theme.faint))
                                    .child(reading_key_note),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.accent))
                                    .child("TikZ 前导内容"),
                            )
                            .child(tex_buttons)
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.accent))
                                    .child("软件更新"),
                            )
                            .child(updates)
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.accent))
                                    .child("快捷键"),
                            )
                            .child(shortcuts),
                    ),
            )
            .into_any_element()
    }
}

impl EntityInputHandler for RusidianApp {
    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let length = self.marked_text.encode_utf16().count();
        adjusted.replace(0..length);
        Some(self.marked_text.clone())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.marked_selection.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        (!self.marked_text.is_empty()).then(|| 0..self.marked_text.encode_utf16().count())
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked_text.clear();
        self.marked_selection = 0..0;
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text.clear();
        self.marked_selection = 0..0;
        if let Some(switcher) = &mut self.switcher {
            switcher.query.push_str(text);
            switcher.selected = 0;
            self.search_text(cx);
            window.invalidate_character_coordinates();
            cx.notify();
            return;
        }
        match self.view {
            View::Reading if !self.settings_open => {
                if let Some(search) = &mut self.reading_search {
                    search.query.push_str(text);
                }
            }
            View::Source if !self.settings_open => {
                if let Some(nvim) = &self.nvim {
                    nvim.input_text(text);
                }
            }
            _ => {}
        }
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        new_text: &str,
        selected: Option<std::ops::Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text.clear();
        self.marked_text.push_str(new_text);
        let length = new_text.encode_utf16().count();
        self.marked_selection = selected.unwrap_or(length..length);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.switcher.is_some() {
            // The candidate window opens under the switcher's query.
            return Some(Bounds::new(
                point(
                    element_bounds.center().x - px(260.0),
                    element_bounds.top() + px(96.0),
                ),
                size(px(8.0), px(18.0)),
            ));
        }
        if self.view == View::Reading && self.reading_search.is_some() {
            return Some(Bounds::new(
                point(
                    element_bounds.left() + px(16.0),
                    element_bounds.bottom() - px(28.0),
                ),
                size(px(8.0), px(18.0)),
            ));
        }
        let (row, column) = self.grid.cursor;
        Some(Bounds::new(
            point(
                element_bounds.left() + px(GRID_PADDING) + self.cell_size.width * column as f32,
                element_bounds.top() + px(GRID_PADDING) + self.cell_size.height * row as f32,
            ),
            self.cell_size,
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.marked_text.encode_utf16().count())
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.switcher.is_some()
            || (self.view == View::Source && self.grid.accepts_text_input())
            || (self.view == View::Reading && self.reading_search.is_some())
    }
}

impl Render for RusidianApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.theme = Theme::resolve(self.appearance, window.appearance());
        let theme = self.theme;
        let title = self
            .document
            .as_ref()
            .map(|document| document.name.clone())
            .unwrap_or_else(|| "Rusidian".into());
        let window_title = self.window_title();
        if self.applied_title.as_deref() != Some(window_title.as_str()) {
            window.set_window_title(&window_title);
            self.applied_title = Some(window_title);
        }
        let reading_cursor = self.reading_cursor;
        let input_view = cx.entity();
        let reading_focus = self.focus_handle.clone();
        let search_prompt = self.reading_search.as_ref().map(|search| {
            format!(
                "{}{}{}",
                if search.forward { "/" } else { "?" },
                search.query,
                self.marked_text
            )
        });
        let settings = self.settings_open.then(|| self.render_settings(cx));
        let switcher = self.switcher.is_some().then(|| self.render_switcher(cx));

        let text_baseline = f32::from(
            cx.text_system().baseline_offset(
                cx.text_system()
                    .resolve_font(&gpui::font(crate::fonts::ui())),
                px(FONT_SIZE),
                px(24.0),
            ),
        );
        let view = cx.entity().downgrade();
        if self.view == View::Reading && self.parse_pending {
            self.parse_document();
        }
        if self.view == View::Reading && self.reveal_again.get() > 0 {
            // The last frame drew the cursor revealed by estimate: now place it exactly.
            self.reveal_again.set(self.reveal_again.get() - 1);
            self.scroll_to_cursor();
        }
        self.fragment_layouts.borrow_mut().clear();
        self.layouts_ready.set(false);
        // The cursor never hides: folded callouts it is in open, unless it rests on the title.
        if self.view == View::Reading
            && let Some(document) = &self.document
        {
            let blocks = &document.markdown.blocks;
            while let Some(range) = fold_ranges(blocks, &self.folded_callouts(blocks))
                .into_iter()
                .find(|range| range.contains(&self.reading_cursor.block))
                && let Some(id) = blocks[range.start].callout_quote
                && self.callout_cursor_parked != Some((id, self.reading_cursor))
            {
                if !self.callout_toggles.remove(&id) {
                    self.callout_toggles.insert(id);
                }
                self.callout_cursor_parked = None;
                self.invalidate_block_heights();
            }
        }
        let reading = if self.view == View::Source {
            div().into_any_element()
        } else if let Some(document) = &self.document {
            let context = RenderContext {
                theme: &theme,
                math: &self.math,
                text_baseline,
                links: Links {
                    note: &document.file,
                    vault: self.vault.as_ref(),
                },
                view: &view,
                layouts: &self.fragment_layouts,
                remote: &self.remote_images,
                remote_vault: self
                    .vault
                    .as_ref()
                    .map(|vault| vault.root.as_path())
                    .filter(|root| {
                        !self
                            .remote_image_vaults
                            .iter()
                            .any(|allowed| allowed == root)
                    }),
                tikz: &self.tikz,
                embeds: &self.embeds,
                unresolved: &self.unresolved_links,
                embedded: false,
                block_index: 0,
            };
            let selection = self.reading_selection.and_then(|selection| {
                selection_bounds(&document.markdown.blocks, selection, self.reading_cursor)
            });
            let blocks = &document.markdown.blocks;
            // A folded callout shows its title on its first block; the rest of it is hidden.
            // For each block: `Some(true)` for such a title, `Some(false)` when hidden.
            let mut fold = vec![None; blocks.len()];
            // The block shown after each folded callout.
            let mut after_fold = HashMap::new();
            for range in fold_ranges(blocks, &self.folded_callouts(blocks)) {
                fold[range.start] = Some(true);
                fold[range.start + 1..range.end].fill(Some(false));
                after_fold.insert(range.start, range.end);
            }
            // Space below each block: outside it, or inside a quote that continues.
            let spacing: Vec<(Pixels, usize)> = blocks
                .iter()
                .enumerate()
                .map(|(index, block)| match fold[index] {
                    Some(true) => {
                        // A folded callout ends at its title; quotes around it may go on.
                        let next = after_fold.get(&index).and_then(|&next| blocks.get(next));
                        (
                            block_gap(block, next),
                            shared_quote_depth(block, next)
                                .min(block.quote_depth.saturating_sub(1)),
                        )
                    }
                    Some(false) => (px(0.0), 0),
                    None => {
                        let next = blocks.get(index + 1);
                        (block_gap(block, next), shared_quote_depth(block, next))
                    }
                })
                .collect();
            let hidden: Vec<bool> = fold.iter().map(|fold| *fold == Some(false)).collect();
            let (drawn, measuring) = self.plan_blocks(
                blocks,
                spacing
                    .iter()
                    .map(|&(gap, shared)| if shared > 0 { px(0.0) } else { gap })
                    .collect(),
                &hidden,
            );
            if !measuring.is_empty() || self.reveal_again.get() > 0 {
                // Next frame places what was measured out of view, or reveals the cursor.
                window.request_animation_frame();
            }
            // A block as drawn in place, without the space below it.
            let block_element = |context: RenderContext, index: usize| {
                let block = &blocks[index];
                let (gap, shared) = spacing[index];
                let context = RenderContext {
                    block_index: index,
                    ..context
                };
                let wrapper = div()
                    .id(("block", index))
                    .mx_auto()
                    .w_full()
                    .max_w(px(READING_WIDTH));
                match fold[index] {
                    Some(true) => {
                        wrapper.when_some(block.callout_title.as_ref(), |wrapper, title| {
                            wrapper.child(render_folded_callout(
                                context,
                                block,
                                title,
                                reading_cursor.block == index,
                                (gap, shared),
                            ))
                        })
                    }
                    Some(false) => wrapper,
                    None => wrapper.flex().flex_col().child(render_block(
                        context,
                        block,
                        self.tikz.get(&block.text),
                        (reading_cursor.block == index).then_some(reading_cursor.offset),
                        selection.and_then(|bounds| {
                            selection_for_block(bounds, index, block_len(block))
                        }),
                        (if shared > 0 { gap } else { px(0.0) }, shared),
                    )),
                }
            };
            let plan = self.block_heights.borrow();
            let children = plan.children.iter().map(|run| {
                if !drawn[run.start] {
                    // One stretch of space for a run of blocks far from the view.
                    let height: Pixels = run
                        .clone()
                        .map(|index| plan.heights[index] + plan.gaps[index])
                        .sum();
                    // Without flex_none the column would shrink empty space to nothing.
                    return div().flex_none().h(height).into_any_element();
                }
                let (gap, shared) = spacing[run.start];
                block_element(context, run.start)
                    .when(shared == 0, |element| element.mb(gap))
                    .into_any_element()
            });
            // Laid out at the column's width but clipped away: only their heights are used.
            let scratch = RefCell::new(Vec::new());
            let measurer = (!measuring.is_empty()).then(|| {
                let context = RenderContext {
                    layouts: &scratch,
                    ..context
                };
                div()
                    .absolute()
                    .top_0()
                    .left(px(READING_PADDING))
                    .right(px(READING_PADDING))
                    .h_0()
                    .overflow_hidden()
                    .children(measuring.iter().map(|&index| {
                        let measures = self.block_measures.clone();
                        block_element(context, index).relative().flex_none().child(
                            canvas(
                                move |bounds, _, _| {
                                    measures.borrow_mut().push((index, bounds.size.height));
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .inset_0(),
                        )
                    }))
            });
            div()
                .flex_1()
                // Beside the sidebar, wide images or tables must not widen the column.
                .min_w_0()
                .flex()
                .flex_col()
                .id("document")
                .relative()
                .overflow_y_scroll()
                .p_8()
                .text_base()
                .track_scroll(&self.reading_scroll)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        this.click_nearest(event.position, cx);
                    }),
                )
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    this.drag_reading(event, cx);
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| this.release_reading(cx)),
                )
                .children(children)
                .child({
                    // Prepainted after every block: from here on this frame's text layouts can be
                    // queried. Right after the blocks so child indices match
                    // `BlockHeights::children`.
                    let ready = self.layouts_ready.clone();
                    canvas(move |_, _, _| ready.set(true), |_, _, _, _| {})
                        .absolute()
                        .size_0()
                })
                .children(measurer)
                .when(blocks.is_empty(), |element| {
                    element.child(
                        div()
                            .mx_auto()
                            .w_full()
                            .max_w(px(READING_WIDTH))
                            .text_color(rgb(theme.muted))
                            .child("空笔记：按 Enter 在 Neovim 中开始写作。"),
                    )
                })
                .children(self.render_backlinks(cx))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(self.render_welcome(cx))
                .into_any_element()
        };

        let body = if self.view == View::Source {
            self.render_source(window, cx)
        } else {
            reading
        };
        if let Some(document) = &self.document
            && self.revealed_in_tree.as_ref() != Some(&document.file)
        {
            // Open the folders containing a newly shown note; later collapses are kept.
            let file = document.file.clone();
            if let Some(vault) = &self.vault {
                self.expanded_folders.extend(
                    file.ancestors()
                        .skip(1)
                        .take_while(|folder| *folder != vault.root)
                        .map(Path::to_path_buf),
                );
                self.tree_rows.replace(None);
                if let Some(index) = self.tree().iter().position(
                    |row| matches!(row, TreeRow::Note { path, .. } if same_file(path, &file)),
                ) {
                    self.tree_scroll
                        .scroll_to_item(index, ScrollStrategy::Center);
                }
            }
            self.revealed_in_tree = Some(file);
        }
        let body = if let Some(vault) = self.vault.as_ref().filter(|_| self.sidebar_visible) {
            let current = self
                .document
                .as_ref()
                .map(|document| document.file.as_path());
            let root = vault.root.clone();
            let root_name = vault
                .root
                .file_name()
                .unwrap_or(vault.root.as_os_str())
                .to_string_lossy()
                .into_owned();
            let name: SharedString = format!(
                "{root_name}{}",
                if vault.is_obsidian {
                    " · Obsidian"
                } else {
                    ""
                }
            )
            .into();
            // Only the rows in view are built: a vault folder can hold thousands of notes.
            let rows = self.tree();
            let row_count = rows.len();
            let current = current.map(Path::to_path_buf);
            let view = cx.entity().downgrade();
            let tree = uniform_list("vault-files", row_count, move |range, _, _| {
                range
                    .map(|index| {
                        let row = rows[index].clone();
                        let (depth, label, selected) = match &row {
                            TreeRow::Folder {
                                name,
                                depth,
                                expanded,
                                ..
                            } => (
                                *depth,
                                format!("{} {name}", if *expanded { "▾" } else { "▸" }),
                                false,
                            ),
                            TreeRow::Note { path, name, depth } => {
                                (*depth, name.clone(), current.as_ref() == Some(path))
                            }
                        };
                        let is_folder = matches!(row, TreeRow::Folder { .. });
                        let vault_root = root.clone();
                        let view = view.clone();
                        div()
                            .id(("vault-row", index))
                            .pl(px(12.0 + depth as f32 * 14.0))
                            .pr_3()
                            .py_1()
                            .text_sm()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .cursor_pointer()
                            .when(is_folder, |element| element.text_color(rgb(theme.muted)))
                            .when(selected, |element| {
                                element
                                    .bg(rgb(theme.accent_soft_bg))
                                    .text_color(rgb(theme.accent_soft_text))
                            })
                            .when(!selected, |element| {
                                element.hover(|element| element.bg(rgb(theme.hover)))
                            })
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| match &row {
                                    TreeRow::Folder { path, .. } => {
                                        if !this.expanded_folders.remove(path) {
                                            this.expanded_folders.insert(path.clone());
                                        }
                                        this.tree_rows.replace(None);
                                        cx.notify();
                                    }
                                    TreeRow::Note { path, .. } => this.request_close(
                                        PendingClose::Open {
                                            path: path.clone(),
                                            vault_root: Some(vault_root.clone()),
                                            fragment: None,
                                        },
                                        cx,
                                    ),
                                })
                                .ok();
                            })
                            .child(label)
                    })
                    .collect::<Vec<_>>()
            })
            .flex_1()
            .pb_2()
            .track_scroll(&self.tree_scroll);
            div()
                .flex_1()
                .flex()
                .min_h_0()
                .child(
                    div()
                        .w(px(240.0))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .border_r_1()
                        .border_color(rgb(theme.border))
                        .bg(rgb(theme.surface))
                        .child(
                            div()
                                .h(px(40.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(tree),
                )
                .child(body)
                .into_any_element()
        } else {
            body
        };

        div()
            .key_context(if self.switcher.is_some() {
                // Keys belong to the switcher, not to Enter-to-edit or Neovim.
                "Switcher"
            } else if self.view == View::Source {
                "Source"
            } else if self.reading_search.is_some() {
                "ReadingSearch"
            } else {
                "Reading"
            })
            .on_action(cx.listener(Self::choose_file))
            .on_action(cx.listener(Self::choose_folder))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::open_switcher))
            .on_action(cx.listener(Self::open_text_search))
            .on_action(cx.listener(|this, _: &CloseTab, _, cx| this.close_tab(cx)))
            .on_action(cx.listener(|this, _: &CloseWindow, _, cx| {
                this.request_close(PendingClose::CloseWindow, cx);
            }))
            .on_action(cx.listener(|this, _: &NextTab, _, cx| this.cycle_tab(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, _, cx| this.cycle_tab(false, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_visible = !this.sidebar_visible;
                cx.notify();
            }))
            .on_action(cx.listener(Self::enter_source_normal))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &RenameNote, _, cx| this.show_rename(cx)))
            .on_key_down(cx.listener(Self::key_down))
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .font_family(crate::fonts::ui())
            .bg(rgb(theme.background))
            .text_color(rgb(theme.text))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(52.0))
                    .px_5()
                    .border_b_1()
                    .border_color(rgb(theme.border))
                    .child(self.render_tabs(title, cx))
                    .child(
                        div()
                            .max_w(px(620.0))
                            .text_ellipsis()
                            .text_sm()
                            .text_color(rgb(theme.muted))
                            .child(
                                self.document
                                    .as_ref()
                                    .map(|document| {
                                        // Inside a vault, the path within it says enough.
                                        self.vault
                                            .as_ref()
                                            .and_then(|vault| {
                                                document.file.strip_prefix(&vault.root).ok()
                                            })
                                            .map_or_else(
                                                || document.path.clone(),
                                                |relative| {
                                                    relative.to_string_lossy().into_owned().into()
                                                },
                                            )
                                    })
                                    .or_else(|| {
                                        let vault = self.vault.as_ref()?;
                                        Some(vault.root.to_string_lossy().into_owned().into())
                                    })
                                    .unwrap_or_default(),
                            ),
                    ),
            )
            .child(body)
            .child(self.render_status_bar(search_prompt))
            .when_some(
                (self.view == View::Reading)
                    .then_some(reading_focus)
                    .flatten(),
                |element, focus| {
                    element.track_focus(&focus).child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, cx| {
                                window.handle_input(
                                    &focus,
                                    ElementInputHandler::new(bounds, input_view),
                                    cx,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                },
            )
            .when_some(settings, |element, settings| element.child(settings))
            .when_some(switcher, |element, switcher| element.child(switcher))
    }
}

/// Translate a GPUI keystroke into Neovim key notation.
fn nvim_key(key: &Keystroke) -> Option<String> {
    let modifiers = &key.modifiers;
    let named = match key.key.as_str() {
        "enter" => Some("CR".to_owned()),
        "escape" => Some("Esc".to_owned()),
        "backspace" => Some("BS".to_owned()),
        "delete" => Some("Del".to_owned()),
        "insert" => Some("Insert".to_owned()),
        "tab" => Some("Tab".to_owned()),
        "space" => Some("Space".to_owned()),
        "left" => Some("Left".to_owned()),
        "right" => Some("Right".to_owned()),
        "up" => Some("Up".to_owned()),
        "down" => Some("Down".to_owned()),
        "pageup" => Some("PageUp".to_owned()),
        "pagedown" => Some("PageDown".to_owned()),
        "home" => Some("Home".to_owned()),
        "end" => Some("End".to_owned()),
        function
            if function.len() > 1
                && function.starts_with('f')
                && function[1..].parse::<u8>().is_ok() =>
        {
            Some(function.to_uppercase())
        }
        _ => None,
    };
    if named.is_none()
        && let Some(text) = key.key_char.as_deref().filter(|text| !text.is_empty())
        && !modifiers.control
        && !modifiers.platform
        && (!modifiers.alt || composes_with_option(key))
    {
        // Shift (and macOS Option) are already applied to the typed character.
        return Some(text.replace('<', "<lt>"));
    }
    let name = match named.clone() {
        Some(name) => name,
        None => match key.key.as_str() {
            "<" => "lt".to_owned(),
            "\\" => "Bslash".to_owned(),
            "|" => "Bar".to_owned(),
            key if key.chars().count() == 1 => key.to_owned(),
            _ => return None,
        },
    };
    let mut prefix = String::new();
    if modifiers.control {
        prefix.push_str("C-");
    }
    if modifiers.alt {
        prefix.push_str("M-");
    }
    if modifiers.shift {
        prefix.push_str("S-");
    }
    if modifiers.platform {
        prefix.push_str("D-");
    }
    if prefix.is_empty() && named.is_none() {
        return Some(key.key.replace('<', "<lt>"));
    }
    Some(format!("<{prefix}{name}>"))
}

/// On macOS, Option types layout characters such as `@` or `{` rather than acting as Meta.
fn composes_with_option(key: &Keystroke) -> bool {
    cfg!(target_os = "macos") && key.key_char.as_deref().is_some_and(|text| text != key.key)
}

fn source_lines(source: &str) -> Vec<String> {
    let lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// Resolves image and link targets relative to the open note and its vault.
#[derive(Clone, Copy)]
struct Links<'a> {
    note: &'a Path,
    vault: Option<&'a Vault>,
}

impl Links<'_> {
    /// The note an `![[note]]` (or `![](note.md)`) embed shows; `![[#heading]]` is this note.
    fn embedded_note(&self, source: &str) -> Option<PathBuf> {
        if crate::remote::is_remote(source) {
            return None;
        }
        let (target, _) = crate::vault::split_fragment(source);
        if target.is_empty() {
            return Some(self.note.to_path_buf());
        }
        crate::vault::resolve_target(self.note, self.vault, target)
            .filter(|path| crate::vault::is_markdown(path))
    }

    /// The local file for an image source. Unresolved local sources keep a note-relative path so
    /// the reading view can report the missing file; remote and absolute sources return `None`.
    fn image(&self, source: &str) -> Option<PathBuf> {
        let decoded = crate::vault::percent_decode(source);
        if decoded.contains("://")
            || decoded.starts_with("data:")
            || Path::new(&decoded).is_absolute()
        {
            return None;
        }
        crate::vault::resolve_target(self.note, self.vault, source).or_else(|| {
            Some(
                self.note
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(decoded),
            )
        })
    }
}

fn image_format(path: &Path) -> Option<ImageFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        "svg" => Some(ImageFormat::Svg),
        "bmp" => Some(ImageFormat::Bmp),
        "tif" | "tiff" => Some(ImageFormat::Tiff),
        "ico" => Some(ImageFormat::Ico),
        "pbm" | "ppm" | "pgm" => Some(ImageFormat::Pnm),
        _ => None,
    }
}

/// Larger notes are not embedded; they would be parsed again on every change.
const EMBED_SIZE_LIMIT: u64 = 1024 * 1024;

/// A note shown inside another with `![[note]]`.
struct EmbeddedNote {
    /// Its modification time when read, to read it again after it changed.
    modified: Option<std::time::SystemTime>,
    markdown: Result<MarkdownDocument, String>,
}

enum RemoteImage {
    Loading,
    Ready(Arc<Image>),
    Failed(SharedString),
}

/// Obsidian's image size after the last `|` of the alt text: `![[pic.png|200]]` or
/// `![logo|200x100](pic.png)`. Width alone keeps the image's proportions.
fn image_size(alt: &str) -> Option<(f32, Option<f32>)> {
    let spec = alt.rsplit('|').next()?.trim();
    let number = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<f32>().ok())
            .flatten()
            .filter(|number| *number > 0.0)
    };
    match spec.split_once('x') {
        Some((width, height)) => Some((number(width)?, Some(number(height)?))),
        None => Some((number(spec)?, None)),
    }
}

/// An image at its natural size, or the size its alt text asks for, never wider than its column.
fn sized_image(image: gpui::Img, alt: &str) -> gpui::Img {
    image
        .max_w_full()
        .when_some(image_size(alt), |image, (width, height)| {
            image
                .w(px(width))
                .when_some(height, |image, height| image.h(px(height)))
        })
}

/// A remote image: shown once loaded, otherwise a placeholder that loads it only when asked.
fn remote_image(context: RenderContext, url: &str, alt: &str, inline: bool) -> AnyElement {
    let theme = context.theme;
    match context.remote.get(url) {
        Some(RemoteImage::Ready(image)) => sized_image(img(image.clone()), alt).into_any_element(),
        Some(RemoteImage::Loading) => div()
            .px_2()
            .rounded_md()
            .bg(rgb(theme.block))
            .text_color(rgb(theme.muted))
            .child("正在加载远程图片…")
            .into_any_element(),
        state => {
            let failure = match state {
                Some(RemoteImage::Failed(error)) => Some(error.clone()),
                _ => None,
            };
            let load = |label: &'static str, id: &'static str, allow_vault: bool| {
                let view = context.view.clone();
                let url = url.to_owned();
                div()
                    .id(SharedString::from(format!("{id}-{url}")))
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(rgb(theme.accent))
                    .hover(|element| element.bg(rgb(theme.hover)))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            if allow_vault {
                                this.allow_vault_remote_images(cx);
                            } else {
                                this.load_remote_image(url.clone(), cx);
                            }
                        })
                        .ok();
                    })
                    .child(label)
            };
            let label = if alt.is_empty() { url } else { alt };
            if inline {
                return div()
                    .flex()
                    .px_1()
                    .rounded_md()
                    .bg(rgb(theme.inline_code))
                    .child(format!("🌐 {label}"))
                    .child(load("加载", "load-remote-inline", false))
                    .into_any_element();
            }
            div()
                .p_4()
                .rounded_md()
                .bg(rgb(theme.block))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(format!("远程图片：{label}")),
                )
                .child(
                    div()
                        .text_sm()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(rgb(theme.faint))
                        .child(url.to_owned()),
                )
                .when_some(failure, |element, error| {
                    element.child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.error_text))
                            .child(error),
                    )
                })
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .text_sm()
                        .child(load("加载一次", "load-remote", false))
                        .when(context.remote_vault.is_some(), |element| {
                            element.child(load("总是加载此 vault 的远程图片", "allow-remote", true))
                        }),
                )
                .into_any_element()
        }
    }
}

/// Where a block's text fragment was laid out in the last frame.
struct FragmentLayout {
    block: usize,
    range: std::ops::Range<usize>,
    layout: TextLayout,
}

/// Inputs shared by every block in one render of the reading view.
#[derive(Clone, Copy)]
struct RenderContext<'a> {
    theme: &'a Theme,
    math: &'a HashMap<(String, bool), MathState>,
    text_baseline: f32,
    links: Links<'a>,
    /// Receives clicks that place the reading cursor.
    view: &'a WeakEntity<RusidianApp>,
    /// Collects every text fragment's layout for screen-line motions.
    layouts: &'a RefCell<Vec<FragmentLayout>>,
    remote: &'a HashMap<String, RemoteImage>,
    /// The vault that could allow remote images automatically, if not already allowed.
    remote_vault: Option<&'a Path>,
    tikz: &'a HashMap<String, TikzState>,
    embeds: &'a HashMap<PathBuf, EmbeddedNote>,
    unresolved: &'a HashSet<String>,
    /// Rendering inside an embed: further embeds show as links, not content.
    embedded: bool,
    /// The block being rendered.
    block_index: usize,
}

impl RenderContext<'_> {
    /// A mouse-down handler that puts the reading cursor at a visible-character offset.
    fn click_at(&self, offset: usize) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        let view = self.view.clone();
        let block = self.block_index;
        move |_, _, cx| {
            // Handled here, not by the view's nearest-position fallback; embeds' inert views
            // fail and leave the click to the embed itself.
            if view
                .update(cx, |this, cx| this.click_reading(block, offset, cx))
                .is_ok()
            {
                cx.stop_propagation();
            }
        }
    }
}

/// Space below a block. Blocks carry no outer margins of their own: GPUI lays divs out as CSS
/// blocks, and taffy's measure cache drops margins that collapse through a parent, so spacing
/// that relied on collapsing changed between layout passes (lost entirely next to the sidebar).
fn block_gap(block: &Block, next: Option<&Block>) -> Pixels {
    match block.kind {
        // Table rows are separate blocks; space the table as a whole.
        BlockKind::Table { .. }
            if matches!(
                next.map(|next| &next.kind),
                Some(BlockKind::Table { header: false })
            ) =>
        {
            px(0.0)
        }
        // Up to the end of a tight list (nested lists included), items sit close together.
        BlockKind::Paragraph if block.tight && next.is_some_and(|next| next.list == block.list) => {
            px(4.0)
        }
        BlockKind::Rule | BlockKind::DefinitionTitle => px(0.0),
        BlockKind::Footnote { .. } | BlockKind::Definition => px(12.0),
        _ => px(16.0),
    }
}

/// How many quote levels the next block shares with this one; above zero the two read as
/// one quote. Blocks in the same innermost quote share all levels, sibling quotes all but the
/// innermost, and otherwise the shallower one's levels.
fn shared_quote_depth(block: &Block, next: Option<&Block>) -> usize {
    let Some(next) =
        next.filter(|next| block.quote_root.is_some() && next.quote_root == block.quote_root)
    else {
        return 0;
    };
    if next.quote == block.quote {
        block.quote_depth
    } else if next.quote_depth == block.quote_depth {
        block.quote_depth.saturating_sub(1).max(1)
    } else {
        block.quote_depth.min(next.quote_depth)
    }
}

/// `inner_gap` is spacing kept inside the first `continued` levels of a quote (a callout's
/// background), which continue into the next block.
fn render_block(
    context: RenderContext,
    block: &Block,
    tikz: Option<&TikzState>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    (inner_gap, continued): (Pixels, usize),
) -> AnyElement {
    let theme = context.theme;
    let links = context.links;
    let object_cursor = (cursor.is_some() || selection.is_some()) && is_object(block);
    // Only text blocks lay the fragment out; objects must not register an unused layout.
    let text = || styled_fragment(context, block, 0..block.text.len(), cursor, selection);

    let content = match &block.kind {
        BlockKind::Heading(level) => div()
            .text_size(px(match level {
                1 => 32.0,
                2 => 27.0,
                3 => 23.0,
                _ => 19.0,
            }))
            .font_weight(FontWeight::SEMIBOLD)
            .child(text())
            .into_any_element(),
        BlockKind::Paragraph if !block.images.is_empty() => {
            render_inline_paragraph(context, block, cursor, selection)
        }
        BlockKind::Paragraph if !block.maths.is_empty() => {
            render_inline_paragraph(context, block, cursor, selection)
        }
        BlockKind::Paragraph => div().child(text()).into_any_element(),
        BlockKind::Image(source) if let Some(path) = links.embedded_note(source) => {
            render_embed(context, source, &path, object_cursor)
        }
        // PDFs, audio, video and other attachments: a card that gf opens with the system app.
        BlockKind::Image(source)
            if let Some(path) = links
                .image(source)
                .filter(|path| image_format(path).is_none()) =>
        {
            div()
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(if object_cursor {
                    theme.accent
                } else {
                    theme.border_strong
                }))
                .child(
                    div()
                        .text_color(rgb(theme.accent))
                        .child(format!("📎 {}", display_name(&path))),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(theme.faint))
                        .child(if path.exists() {
                            "gf 用系统应用打开"
                        } else {
                            "找不到这个附件"
                        }),
                )
                .into_any_element()
        }
        BlockKind::Image(source) if crate::remote::is_remote(source) => div()
            .when(object_cursor, |element| {
                element.border_2().border_color(rgb(theme.accent))
            })
            .child(remote_image(context, source, &block.text, false))
            .into_any_element(),
        BlockKind::Image(source) => {
            let source_label = source.clone();
            let alt = block.text.clone();
            let Some(path) = links.image(source) else {
                // Files outside the note's folder and vault need a permission Rusidian does not
                // grant yet; show the reference instead.
                return decorate_block(
                    context,
                    block,
                    div()
                        .p_4()
                        .rounded_md()
                        .when(object_cursor, |element| {
                            element.border_2().border_color(rgb(theme.accent))
                        })
                        .bg(rgb(theme.block))
                        .on_mouse_down(MouseButton::Left, context.click_at(0))
                        .child(format!("![{alt}]({source_label})"))
                        .into_any_element(),
                    inner_gap,
                    continued,
                );
            };
            div()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(theme.accent))
                })
                .child(sized_image(img(path), &alt).with_fallback({
                    let theme = *theme;
                    move || {
                        div()
                            .p_4()
                            .rounded_md()
                            .bg(rgb(theme.error_bg))
                            .text_color(rgb(theme.error_text))
                            .child(format!("无法加载图片：{source_label}"))
                            .into_any_element()
                    }
                }))
                .into_any_element()
        }
        BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz") => match tikz {
            Some(TikzState::Ready(image)) => {
                let width = crate::tikz::png_size(image.bytes())
                    .map(|(width, _)| px(width as f32 / crate::tikz::PREVIEW_SCALE));
                div()
                    .p_4()
                    .rounded_md()
                    .when(object_cursor, |element| {
                        element.border_2().border_color(rgb(theme.accent))
                    })
                    .bg(rgb(WHITE))
                    .child(
                        img(image.clone())
                            .max_w_full()
                            .when_some(width, |element, width| element.w(width)),
                    )
                    .into_any_element()
            }
            Some(TikzState::Failed(error)) => div()
                .p_4()
                .rounded_md()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(theme.accent))
                })
                .bg(rgb(theme.error_bg))
                .text_color(rgb(theme.error_text))
                .child(error.clone())
                .into_any_element(),
            _ => div()
                .p_4()
                .rounded_md()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(theme.accent))
                })
                .bg(rgb(theme.block))
                .text_color(rgb(theme.muted))
                .child("正在编译 TikZ…")
                .into_any_element(),
        },
        BlockKind::Code(language) => div()
            .p_4()
            .rounded_md()
            .bg(rgb(theme.block))
            .font_family(crate::fonts::mono())
            .when_some(language.as_ref(), |element, language| {
                element.child(
                    div()
                        .mb_2()
                        .text_sm()
                        .text_color(rgb(theme.muted))
                        .child(language.clone()),
                )
            })
            .child(text())
            .into_any_element(),
        // Front matter reads as a properties card; the cursor or a selection shows the raw YAML.
        BlockKind::Metadata
            if cursor.is_none()
                && selection.is_none()
                && let Some(properties) = parse_properties(&block.text) =>
        {
            div()
                .px_4()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(theme.border))
                .text_sm()
                .on_mouse_down(MouseButton::Left, context.click_at(0))
                .children(properties.into_iter().map(|property| {
                    let tags = property.is_tags();
                    let value = if property.list || tags {
                        // Lists as chips; tags search the vault for the tag.
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(property.values.into_iter().map(|value| {
                                let tag = format!("#{}", value.trim_start_matches('#'));
                                let view = context.view.clone();
                                div()
                                    .px_2()
                                    .rounded_md()
                                    .bg(rgb(if tags {
                                        theme.accent_soft_bg
                                    } else {
                                        theme.inline_code
                                    }))
                                    .when(tags, |element| {
                                        element
                                            .cursor_pointer()
                                            .text_color(rgb(theme.accent_soft_text))
                                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                                if view
                                                    .update(cx, |this, cx| {
                                                        this.search_tag(tag.clone(), cx);
                                                    })
                                                    .is_ok()
                                                {
                                                    cx.stop_propagation();
                                                }
                                            })
                                    })
                                    .child(if tags {
                                        format!("#{}", value.trim_start_matches('#'))
                                    } else {
                                        value
                                    })
                            }))
                            .into_any_element()
                    } else {
                        div().child(property.values.join(", ")).into_any_element()
                    };
                    div()
                        .py_1()
                        .flex()
                        .gap_3()
                        .child(
                            div()
                                .w(px(140.0))
                                .flex_none()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(rgb(theme.muted))
                                .child(property.key),
                        )
                        .child(div().flex_1().min_w_0().child(value))
                }))
                .into_any_element()
        }
        BlockKind::Html | BlockKind::Metadata => div()
            .p_4()
            .rounded_md()
            .bg(rgb(theme.block))
            .font_family(crate::fonts::mono())
            .child(text())
            .into_any_element(),
        BlockKind::Rule => div()
            .my_4()
            .h(px(1.0))
            .w_full()
            .bg(rgb(theme.border_strong))
            .into_any_element(),
        BlockKind::Table { header } => div()
            .flex()
            .w_full()
            .when(*header, |element| element.bg(rgb(theme.table_header)))
            .children(block.cells.iter().enumerate().map(|(index, range)| {
                // Rows are separate blocks; share edges so inner lines are not doubled.
                div()
                    .flex_1()
                    .min_w_0()
                    .p_2()
                    .border_r_1()
                    .border_b_1()
                    .when(index == 0, |element| element.border_l_1())
                    .when(*header, |element| element.border_t_1())
                    .border_color(rgb(theme.border_strong))
                    .when(*header, |element| element.font_weight(FontWeight::BOLD))
                    .when(
                        block.table_alignments.get(index)
                            == Some(&pulldown_cmark::Alignment::Center),
                        |element| element.text_center(),
                    )
                    .when(
                        block.table_alignments.get(index)
                            == Some(&pulldown_cmark::Alignment::Right),
                        |element| element.text_right(),
                    )
                    .child(styled_fragment(
                        context,
                        block,
                        range.clone(),
                        cursor,
                        selection,
                    ))
            }))
            .into_any_element(),
        BlockKind::Math => match context.math.get(&(block.text.clone(), true)) {
            Some(MathState::Ready(formula)) => div()
                .p_4()
                .rounded_md()
                .bg(rgb(theme.block))
                .id("display-math")
                .overflow_x_scroll()
                .child(formula.element(FONT_SIZE))
                .into_any_element(),
            Some(MathState::Failed(error)) => div()
                .p_4()
                .rounded_md()
                .bg(rgb(theme.error_bg))
                .text_color(rgb(theme.error_text))
                .child(format!("$${}$$：{}", block.text, error))
                .into_any_element(),
            _ => div()
                .p_4()
                .rounded_md()
                .bg(rgb(theme.block))
                .text_color(rgb(theme.muted))
                .child("正在排版公式…")
                .into_any_element(),
        },
        BlockKind::Footnote { number, .. } => div()
            .flex()
            .text_sm()
            .child(
                div()
                    .w(px(30.0))
                    .flex_none()
                    .text_color(rgb(theme.accent))
                    .children(number.map(|number| format!("{number}."))),
            )
            .child(div().flex_1().min_w_0().child(text()))
            .into_any_element(),
        BlockKind::DefinitionTitle => div()
            .mt_3()
            .font_weight(FontWeight::BOLD)
            .child(text())
            .into_any_element(),
        BlockKind::Definition => div().ml_6().child(text()).into_any_element(),
    };
    let content = if is_object(block) {
        div()
            .on_mouse_down(MouseButton::Left, context.click_at(0))
            .child(content)
            .into_any_element()
    } else {
        content
    };
    decorate_block(context, block, content, inner_gap, continued)
}

/// A folded callout: its title alone, which a click (or `za`) opens.
/// The blocks of each outermost folded callout, from its title block to its end. Everything
/// in a folded callout folds with it, callouts nested in it included.
fn fold_ranges(blocks: &[Block], folded: &HashSet<usize>) -> Vec<std::ops::Range<usize>> {
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for (start, block) in blocks.iter().enumerate() {
        if block.callout_title.is_none()
            || ranges.last().is_some_and(|range| range.contains(&start))
        {
            continue;
        }
        let Some(id) = block.callout_quote.filter(|id| folded.contains(id)) else {
            continue;
        };
        // Like the callout's own blocks in the parser: its quote's, and deeper ones.
        let depth = block.quote_depth;
        let length = blocks[start + 1..]
            .iter()
            .take_while(|next| next.quote == Some(id) || next.quote_depth > depth)
            .count();
        ranges.push(start..start + 1 + length);
    }
    ranges
}

fn render_folded_callout(
    context: RenderContext,
    block: &Block,
    title: &str,
    active: bool,
    spacing: (Pixels, usize),
) -> AnyElement {
    let color = crate::theme::callout_color(block.callout.as_deref().unwrap_or_default());
    let view = context.view.clone();
    let id = block.callout_quote;
    let folded = div()
        .px_3()
        .py_2()
        .border_l_2()
        .border_color(rgb(color))
        .bg(rgba((color << 8) | if active { 0x30 } else { 0x14 }))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(color))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            if let Some(id) = id
                && view
                    .update(cx, |this, cx| this.toggle_callout(id, cx))
                    .is_ok()
            {
                cx.stop_propagation();
            }
        })
        .child(format!("▸ {title}"))
        .into_any_element();
    // Inside the quotes and callouts around it, like an open callout.
    let (gap, shared) = spacing;
    decorate_levels(
        context,
        block,
        folded,
        (if shared > 0 { gap } else { px(0.0) }, shared),
        block.quote_depth.saturating_sub(1),
    )
}

/// A note, heading section or block shown with `![[note]]`, in a card titled with its name.
fn render_embed(context: RenderContext, source: &str, path: &Path, selected: bool) -> AnyElement {
    let theme = context.theme;
    let (_, fragment) = crate::vault::split_fragment(source);
    let title = match fragment {
        Some(fragment) => format!(
            "{} › {}",
            path.file_stem().unwrap_or_default().to_string_lossy(),
            crate::vault::percent_decode(fragment.trim_start_matches('^'))
        ),
        None => path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    };
    let message = |text: String| {
        div()
            .text_sm()
            .text_color(rgb(theme.muted))
            .child(text)
            .into_any_element()
    };
    let body = match context.embeds.get(path).map(|embed| &embed.markdown) {
        // One level only: an embed inside an embed (or a note embedding itself) is a link.
        _ if context.embedded => message("嵌入的笔记中的嵌入：按 gf 打开该笔记查看".into()),
        Some(Ok(markdown)) => match embedded_blocks(&markdown.blocks, fragment) {
            Some(blocks) if !blocks.is_empty() => render_embedded_blocks(context, blocks),
            Some(_) => message("（空笔记）".into()),
            None => message(format!("找不到标题或块：{}", fragment.unwrap_or_default())),
        },
        Some(Err(error)) => message(error.clone()),
        None => message("正在读取…".into()),
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px_4()
        .py_3()
        .rounded_md()
        .border_1()
        .border_color(rgb(if selected {
            theme.accent
        } else {
            theme.border_strong
        }))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.muted))
                .child(title),
        )
        .child(body)
        .into_any_element()
}

/// Embedded blocks, rendered like the note's own but without cursor or click positions.
fn render_embedded_blocks(context: RenderContext, blocks: &[Block]) -> AnyElement {
    let layouts = RefCell::new(Vec::new());
    let inert = WeakEntity::new_invalid();
    div()
        .flex()
        .flex_col()
        .children(blocks.iter().enumerate().map(|(index, block)| {
            let next = blocks.get(index + 1);
            let gap = if next.is_some() {
                block_gap(block, next)
            } else {
                px(0.0)
            };
            let shared = shared_quote_depth(block, next);
            let joined = shared > 0;
            let nested = RenderContext {
                layouts: &layouts,
                view: &inert,
                embedded: true,
                block_index: index,
                ..context
            };
            div()
                .flex()
                .flex_col()
                .when(!joined, |element| element.mb(gap))
                .child(render_block(
                    nested,
                    block,
                    context.tikz.get(&block.text),
                    None,
                    None,
                    (if joined { gap } else { px(0.0) }, shared),
                ))
        }))
        .into_any_element()
}

/// The blocks an embed shows: the note without its properties, a heading's section, or the
/// block marked `^id`. `None` when the fragment names nothing in the note.
fn embedded_blocks<'a>(blocks: &'a [Block], fragment: Option<&str>) -> Option<&'a [Block]> {
    let Some(fragment) = fragment else {
        let start = blocks
            .iter()
            .position(|block| block.kind != BlockKind::Metadata)
            .unwrap_or(blocks.len());
        return Some(&blocks[start..]);
    };
    let start = fragment_block(blocks, fragment)?;
    let BlockKind::Heading(level) = blocks[start].kind else {
        return Some(&blocks[start..=start]);
    };
    let end = blocks[start + 1..]
        .iter()
        .position(|block| matches!(block.kind, BlockKind::Heading(other) if other <= level))
        .map_or(blocks.len(), |offset| start + 1 + offset);
    Some(&blocks[start..end])
}

/// The block a link fragment points to: a heading, or the block marked `^id`.
fn fragment_block(blocks: &[Block], fragment: &str) -> Option<usize> {
    if let Some(id) = fragment.strip_prefix('^') {
        return blocks
            .iter()
            .position(|block| block.block_id.as_deref() == Some(id));
    }
    let wanted = heading_key(&crate::vault::percent_decode(fragment));
    blocks.iter().position(|block| {
        matches!(block.kind, BlockKind::Heading(_)) && heading_key(&block.text) == wanted
    })
}

fn decorate_block(
    context: RenderContext,
    block: &Block,
    content: AnyElement,
    inner_gap: Pixels,
    continued: usize,
) -> AnyElement {
    decorate_levels(
        context,
        block,
        content,
        (inner_gap, continued),
        block.quote_depth,
    )
}

/// `decorate_block` drawing only the outermost `levels` quote levels.
fn decorate_levels(
    context: RenderContext,
    block: &Block,
    content: AnyElement,
    (inner_gap, continued): (Pixels, usize),
    levels: usize,
) -> AnyElement {
    // A quote inside a list item sits in the item; a list inside a quote sits in the quote.
    if block.quote_in_list {
        let quoted = quote_levels(context, block, content, (inner_gap, continued), levels);
        list_item(block, quoted)
    } else {
        let item = list_item(block, content);
        quote_levels(context, block, item, (inner_gap, continued), levels)
    }
}

/// A list item's marker, or the indent of its later blocks, around `content`.
fn list_item(block: &Block, content: AnyElement) -> AnyElement {
    let marker: Option<SharedString> = if let Some(checked) = block.task {
        Some(if checked { "☑" } else { "☐" }.into())
    } else {
        block.list_marker.clone().map(Into::into)
    };
    if let Some(marker) = marker {
        div()
            .flex()
            .ml(px(block.list_depth as f32 * 24.0))
            .child(div().w(px(30.0)).flex_none().child(marker))
            .child(div().flex_1().min_w_0().child(content))
            .into_any_element()
    } else if block.list.is_some() {
        // A later paragraph, code block or quote of the item lines up with its text.
        div()
            .ml(px(block.list_depth as f32 * 24.0 + 30.0))
            .child(content)
            .into_any_element()
    } else {
        content
    }
}

/// The outermost `levels` quotes and callouts around a block, each in its own style.
fn quote_levels(
    context: RenderContext,
    block: &Block,
    content: AnyElement,
    (inner_gap, continued): (Pixels, usize),
    levels: usize,
) -> AnyElement {
    let theme = context.theme;
    // One box per quote level, innermost first: a callout's colored box or a plain quote's
    // border. The space below goes inside the deepest level that continues.
    let mut quoted = content;
    for level in (1..=levels.min(block.quote_depth)).rev() {
        let inside_gap = if level == continued {
            inner_gap
        } else {
            px(0.0)
        };
        quoted = match block.quote_callouts.get(level - 1).cloned().flatten() {
            Some(kind) => {
                let color = crate::theme::callout_color(&kind);
                // The title is on the callout's first block, which is directly in it.
                let title = block
                    .callout_title
                    .clone()
                    .filter(|_| level == block.quote_depth);
                div()
                    .flex()
                    .flex_col()
                    .pl_3()
                    .pr_3()
                    .pb(match level.cmp(&continued) {
                        // Where the callout ends, its own bottom padding.
                        std::cmp::Ordering::Greater => px(10.0),
                        std::cmp::Ordering::Equal => inside_gap.max(px(10.0)),
                        std::cmp::Ordering::Less => px(0.0),
                    })
                    .border_l_2()
                    .border_color(rgb(color))
                    .bg(rgba((color << 8) | 0x14))
                    .text_color(rgb(theme.text))
                    .when_some(title, |element, title| {
                        // A foldable callout's title folds it.
                        let foldable = block.callout_fold.is_some();
                        let view = context.view.clone();
                        let id = block.callout_quote;
                        element.pt_2().child(
                            div()
                                .mb_2()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(color))
                                .when(foldable, |element| {
                                    element.cursor_pointer().on_mouse_down(
                                        MouseButton::Left,
                                        move |_, _, cx| {
                                            if let Some(id) = id
                                                && view
                                                    .update(cx, |this, cx| {
                                                        this.toggle_callout(id, cx)
                                                    })
                                                    .is_ok()
                                            {
                                                cx.stop_propagation();
                                            }
                                        },
                                    )
                                })
                                .child(if foldable {
                                    format!("▾ {title}")
                                } else {
                                    title
                                }),
                        )
                    })
                    .child(quoted)
            }
            None => div()
                .flex()
                .flex_col()
                .pl_3()
                .border_l_2()
                .border_color(rgb(theme.quote_border))
                .text_color(rgb(theme.quote_text))
                .pb(inside_gap)
                .child(quoted),
        }
        .into_any_element();
    }
    quoted
}

/// Styled text for `range` of a block; clicking it places the reading cursor on the character.
fn styled_fragment(
    context: RenderContext,
    block: &Block,
    range: std::ops::Range<usize>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
) -> AnyElement {
    let highlights = fragment_highlights(
        context.theme,
        block,
        &range,
        cursor,
        selection,
        context.unresolved,
    );
    let fragment: SharedString = block.text[range.clone()].to_owned().into();
    let text = StyledText::new(fragment.clone()).with_highlights(highlights);
    let layout = text.layout().clone();
    context.layouts.borrow_mut().push(FragmentLayout {
        block: context.block_index,
        range: range.clone(),
        layout: layout.clone(),
    });
    let before = visible_offset(&block.text, range.start);
    let view = context.view.clone();
    let index = context.block_index;
    div()
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            let byte = layout
                .index_for_position(event.position)
                .unwrap_or_else(|nearest| nearest)
                .min(fragment.len());
            let mut byte = (0..=byte)
                .rev()
                .find(|byte| fragment.is_char_boundary(*byte))
                .unwrap_or(0);
            // The nearest boundary may follow the clicked character; select the one under
            // the pointer instead.
            if let Some(at) = layout.position_for_index(byte)
                && byte > 0
                && event.position.x < at.x
                && event.position.y >= at.y
            {
                byte = fragment[..byte]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(index, _)| index);
            }
            let offset = before + visible_offset(&fragment, byte);
            if view
                .update(cx, |this, cx| this.click_reading(index, offset, cx))
                .is_ok()
            {
                cx.stop_propagation();
            }
        })
        .child(text)
        .into_any_element()
}

/// Visible characters (newlines excluded) before byte `byte` of `text`.
fn visible_offset(text: &str, byte: usize) -> usize {
    text[..byte]
        .chars()
        .filter(|character| *character != '\n')
        .count()
}

/// Text styles of `range`; links in `unresolved` (missing notes) are dimmed.
fn fragment_highlights(
    theme: &Theme,
    block: &Block,
    range: &std::ops::Range<usize>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    unresolved: &HashSet<String>,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut highlights = block
        .spans
        .iter()
        .filter_map(|span| {
            clipped_range(&span.range, range).map(|span_range| {
                (
                    span_range,
                    HighlightStyle {
                        font_weight: span.bold.then_some(FontWeight::BOLD),
                        font_style: span.italic.then_some(FontStyle::Italic),
                        color: span.tag.then(|| rgb(theme.accent_soft_text).into()),
                        background_color: if span.code {
                            Some(rgb(theme.inline_code).into())
                        } else if span.highlight {
                            Some(rgb(theme.highlight).into())
                        } else if span.tag {
                            Some(rgb(theme.accent_soft_bg).into())
                        } else {
                            None
                        },
                        strikethrough: span.strike.then_some(StrikethroughStyle {
                            thickness: px(1.0),
                            color: None,
                        }),
                        ..Default::default()
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    highlights.extend(block.links.iter().filter_map(|link| {
        clipped_range(&link.range, range).map(|range| {
            let missing = unresolved.contains(&link.destination);
            (
                range,
                HighlightStyle {
                    color: Some(rgb(theme.accent).into()),
                    fade_out: missing.then_some(0.45),
                    underline: Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: None,
                        wavy: false,
                    }),
                    ..Default::default()
                },
            )
        })
    }));
    if let Some((start, end)) = selection
        && let (Some(start), Some(end)) =
            (text_range(&block.text, start), text_range(&block.text, end))
        && let Some(selection_range) = clipped_range(&(start.start..end.end), range)
    {
        highlights.push((
            selection_range,
            HighlightStyle {
                background_color: Some(rgb(theme.selection).into()),
                ..Default::default()
            },
        ));
    }
    if let Some(cursor_range) = cursor
        .and_then(|offset| text_range(&block.text, offset))
        .and_then(|cursor| clipped_range(&cursor, range))
    {
        highlights.push((
            cursor_range,
            HighlightStyle {
                color: Some(rgb(theme.cursor_fg).into()),
                background_color: Some(rgb(theme.cursor_bg).into()),
                ..Default::default()
            },
        ));
    }
    merge_highlights(highlights)
}

/// GPUI turns highlights into text runs in order, so ranges must be sorted and must not overlap.
/// Split overlapping ranges into segments; later highlights take precedence.
fn merge_highlights(
    highlights: Vec<(std::ops::Range<usize>, HighlightStyle)>,
) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
    let mut boundaries = highlights
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut merged: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    for pair in boundaries.windows(2) {
        let segment = pair[0]..pair[1];
        let style = highlights
            .iter()
            .filter(|(range, _)| range.start <= segment.start && segment.end <= range.end)
            .map(|(_, style)| *style)
            .reduce(HighlightStyle::highlight);
        let Some(style) = style else {
            continue;
        };
        match merged.last_mut() {
            Some((last, last_style)) if last.end == segment.start && *last_style == style => {
                last.end = segment.end;
            }
            _ => merged.push((segment, style)),
        }
    }
    merged
}

fn clipped_range(
    subject: &std::ops::Range<usize>,
    container: &std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    let start = subject.start.max(container.start);
    let end = subject.end.min(container.end);
    (start < end).then(|| start - container.start..end - container.start)
}

enum InlineAtom<'a> {
    Image(&'a crate::markdown::InlineImage),
    Math(&'a crate::markdown::InlineMath),
}

fn inline_extents(
    block: &Block,
    math: &HashMap<(String, bool), MathState>,
    baseline: f32,
) -> (f32, f32) {
    // ponytail: all wrapped rows share the tallest formula's extents; per-row metrics if this wastes space.
    let mut ascent = baseline;
    let mut descent = 24.0 - baseline;
    for formula in &block.maths {
        if let Some(MathState::Ready(formula)) = math.get(&(formula.source.clone(), false)) {
            ascent = ascent.max(formula.ascent(FONT_SIZE) + 1.0);
            descent = descent.max(formula.descent(FONT_SIZE) + 1.0);
        }
    }
    (ascent, descent)
}

/// Push wrappable text pieces of `range`; `(top, height)` aligns them on the shared baseline.
fn push_inline_text(
    context: RenderContext,
    children: &mut Vec<AnyElement>,
    block: &Block,
    range: std::ops::Range<usize>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    (top, height): (f32, f32),
) {
    let text = &block.text[range.clone()];
    let mut start = 0;
    for (end, opportunity) in unicode_linebreak::linebreaks(text) {
        let content_end = if text[..end].ends_with('\n') {
            end - 1
        } else {
            end
        };
        if start < content_end {
            children.push(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .pt(px(top))
                    .h(px(height))
                    .child(styled_fragment(
                        context,
                        block,
                        range.start + start..range.start + content_end,
                        cursor,
                        selection,
                    ))
                    .into_any_element(),
            );
        }
        if opportunity == unicode_linebreak::BreakOpportunity::Mandatory && content_end < end {
            children.push(div().w_full().h(px(0.0)).into_any_element());
        }
        start = end;
    }
}

fn render_inline_paragraph(
    context: RenderContext,
    block: &Block,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
) -> AnyElement {
    let RenderContext {
        theme,
        math,
        text_baseline,
        links,
        ..
    } = context;
    let (ascent, descent) = inline_extents(block, math, text_baseline);
    let row_height = ascent + descent;
    let mut children = Vec::new();
    let mut start = 0;
    let mut atoms = block
        .images
        .iter()
        .map(|image| (image.range.clone(), InlineAtom::Image(image)))
        .chain(
            block
                .maths
                .iter()
                .map(|math| (math.range.clone(), InlineAtom::Math(math))),
        )
        .collect::<Vec<_>>();
    atoms.sort_by_key(|(range, _)| range.start);

    for (range, atom) in atoms {
        if start < range.start {
            push_inline_text(
                context,
                &mut children,
                block,
                start..range.start,
                cursor,
                selection,
                (ascent - text_baseline, row_height),
            );
        }
        let offset = block.text[..range.start]
            .chars()
            .filter(|character| *character != '\n')
            .count();
        let active = cursor == Some(offset)
            || selection.is_some_and(|(from, to)| from <= offset && offset <= to);
        // Images keep their own size and sit on the baseline; everything else fills the row.
        let mut picture = false;
        let child = match atom {
            InlineAtom::Image(image) if crate::remote::is_remote(&image.source) => {
                picture = matches!(
                    context.remote.get(&image.source),
                    Some(RemoteImage::Ready(_))
                );
                // The load placeholder is text and shares the text's baseline.
                div()
                    .when(!picture, |element| element.pt(px(ascent - text_baseline)))
                    .child(remote_image(context, &image.source, &image.alt, true))
                    .into_any_element()
            }
            InlineAtom::Image(image)
                if let Some(path) = links
                    .image(&image.source)
                    .filter(|path| image_format(path).is_none()) =>
            {
                div()
                    .pt(px(ascent - text_baseline))
                    .child(
                        div()
                            .px_1()
                            .rounded_md()
                            .bg(rgb(theme.inline_code))
                            .text_color(rgb(theme.accent))
                            .child(format!("📎 {}", display_name(&path))),
                    )
                    .into_any_element()
            }
            InlineAtom::Image(image) => {
                let alt = image.alt.clone();
                let source = image.source.clone();
                if let Some(path) = links.image(&image.source) {
                    picture = true;
                    sized_image(img(path), &image.alt)
                        .with_fallback(move || div().child(alt.clone()).into_any_element())
                        .into_any_element()
                } else {
                    div()
                        .pt(px(ascent - text_baseline))
                        .child(
                            div()
                                .px_1()
                                .bg(rgb(theme.inline_code))
                                .child(format!("![{alt}]({source})")),
                        )
                        .into_any_element()
                }
            }
            InlineAtom::Math(formula) => match math.get(&(formula.source.clone(), false)) {
                Some(MathState::Ready(formula)) => div()
                    .pt(px(ascent - formula.ascent(FONT_SIZE) - 1.0))
                    .child(formula.element(FONT_SIZE))
                    .into_any_element(),
                Some(MathState::Failed(error)) => div()
                    .px_1()
                    .bg(rgb(theme.error_bg))
                    .text_color(rgb(theme.error_text))
                    .child(format!("${}$：{}", formula.source, error))
                    .into_any_element(),
                _ => div()
                    .px_1()
                    .bg(rgb(theme.inline_code))
                    .child(format!("${}$", formula.source))
                    .into_any_element(),
            },
        };
        children.push(
            div()
                .flex_none()
                .when(picture, |element| element.max_w_full().pb(px(descent)))
                .when(!picture, |element| element.h(px(row_height)))
                .on_mouse_down(MouseButton::Left, context.click_at(offset))
                .when(active, |element| element.bg(rgb(theme.atom_active)))
                .child(child)
                .into_any_element(),
        );
        start = range.end;
    }
    if start < block.text.len() {
        push_inline_text(
            context,
            &mut children,
            block,
            start..block.text.len(),
            cursor,
            selection,
            (ascent - text_baseline, row_height),
        );
    }
    // Rows align at the bottom, so a tall image raises its row and text stays on the baseline.
    div()
        .flex()
        .flex_wrap()
        .items_end()
        .line_height(px(24.0))
        .children(children)
        .into_any_element()
}

/// Where reading starts in a newly shown note: the first text after any front matter.
fn initial_cursor(blocks: &[Block]) -> ReadingCursor {
    let cursorable = |(_, block): &(usize, &Block)| block_len(block) > 0;
    blocks
        .iter()
        .enumerate()
        .filter(cursorable)
        .find(|(_, block)| block.kind != BlockKind::Metadata)
        .or_else(|| blocks.iter().enumerate().find(cursorable))
        .map(|(block, _)| ReadingCursor { block, offset: 0 })
        .unwrap_or_default()
}

/// Simple YAML front matter (`key: value`, flow lists and `- item` lists) as display rows.
/// Anything more complex returns `None` and is shown as source.
/// A front-matter property: its key and value, or values when it is a list.
#[derive(Debug, PartialEq)]
struct Property {
    key: String,
    values: Vec<String>,
    list: bool,
}

impl Property {
    /// Tags, shown as chips that search for the tag.
    fn is_tags(&self) -> bool {
        matches!(self.key.to_lowercase().as_str(), "tags" | "tag")
    }
}

fn parse_properties(yaml: &str) -> Option<Vec<Property>> {
    let clean = |value: &str| {
        let value = value.trim();
        value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })
            .unwrap_or(value)
            .to_owned()
    };
    let mut rows: Vec<Property> = Vec::new();
    for line in yaml.lines().filter(|line| !line.trim().is_empty()) {
        let trimmed = line.trim_start();
        if let Some(item) = trimmed
            .strip_prefix("- ")
            .or((trimmed == "-").then_some(""))
        {
            let row = rows.last_mut()?;
            row.values.push(clean(item));
            row.list = true;
        } else if line.starts_with(char::is_whitespace) || trimmed.starts_with('#') {
            return None;
        } else {
            let (key, value) = line.split_once(':')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            let value = value.trim();
            let inline = value
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'));
            let values = match inline {
                Some(list) => list
                    .split(',')
                    .map(clean)
                    .filter(|item| !item.is_empty())
                    .collect(),
                None if value.is_empty() => Vec::new(),
                None => vec![clean(value)],
            };
            rows.push(Property {
                key: clean(key),
                values,
                list: inline.is_some(),
            });
        }
    }
    (!rows.is_empty()).then_some(rows)
}

/// Zero-based line and byte column in the source for a reading-view position.
fn source_position(document: &Document, cursor: ReadingCursor) -> Option<(usize, usize)> {
    let block = document.markdown.blocks.get(cursor.block)?;
    let byte = if is_object(block) {
        0
    } else {
        text_range(&block.text, cursor.offset)?.start
    };
    let offset = block.source_offset(byte)?;
    let mut remaining = offset;
    for (index, line) in document.lines.iter().enumerate() {
        if remaining <= line.len() {
            return Some((index, remaining));
        }
        remaining -= line.len() + 1;
    }
    let last = document.lines.len().saturating_sub(1);
    Some((last, document.lines.get(last).map_or(0, String::len)))
}

/// Byte offset in `lines.join("\n")` for a zero-based line and byte column.
fn source_offset(lines: &[String], line: usize, column: usize) -> usize {
    let line = line.min(lines.len().saturating_sub(1));
    lines[..line]
        .iter()
        .map(|text| text.len() + 1)
        .sum::<usize>()
        + column.min(lines.get(line).map_or(0, String::len))
}

/// The reading position showing source byte `offset`, or the next visible character after it.
fn reading_position(blocks: &[Block], offset: usize) -> Option<ReadingCursor> {
    for (index, block) in blocks.iter().enumerate() {
        let length = block_len(block);
        if length == 0 {
            continue;
        }
        if let Some(byte) = block.text_offset(offset) {
            let offset = if is_object(block) {
                0
            } else {
                block.text[..byte]
                    .chars()
                    .filter(|character| *character != '\n')
                    .count()
                    .min(length - 1)
            };
            return Some(ReadingCursor {
                block: index,
                offset,
            });
        }
    }
    blocks
        .iter()
        .enumerate()
        .rfind(|(_, block)| block_len(block) > 0)
        .map(|(block, value)| ReadingCursor {
            block,
            offset: block_len(value) - 1,
        })
}

/// Modifier letters for `nvim_input_mouse`.
fn mouse_modifiers(modifiers: &Modifiers) -> String {
    let mut letters = String::new();
    if modifiers.control {
        letters.push('C');
    }
    if modifiers.shift {
        letters.push('S');
    }
    if modifiers.alt {
        letters.push('A');
    }
    if modifiers.platform {
        letters.push('D');
    }
    letters
}

fn is_external_link(destination: &str) -> bool {
    destination.contains("://") || destination.starts_with("mailto:")
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Write `text` to `path` through a temporary file beside it, so a failed write leaves the
/// note as it was.
fn write_replacing(path: &Path, text: &str) -> std::io::Result<()> {
    let temporary = path.with_file_name(format!(
        ".{}.rusidian-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Whether a linked file can be edited as text: valid UTF-8 without NUL bytes near the start.
/// A file's text for the first view, before Neovim loads it. Text in another encoding is
/// shown as best it can be; Neovim decodes it properly and its lines replace these. Files with
/// NUL bytes are not text.
fn read_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "文件不存在".to_owned(),
        std::io::ErrorKind::PermissionDenied => "没有读取权限".to_owned(),
        _ if path.is_dir() => "这是文件夹".to_owned(),
        _ => error.to_string(),
    })?;
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return Err("这不是文本文件".into());
    }
    Ok(String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned()))
}

fn is_text_file(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(8192).read_to_end(&mut head).is_err() || head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&head) {
        Ok(_) => true,
        // A multi-byte character may be cut at the 8 KiB boundary.
        Err(error) => error.error_len().is_none(),
    }
}

/// Normalize headings the way link fragments refer to them: case-insensitive, with spaces,
/// hyphens and punctuation ignored.
fn heading_key(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A short label for Neovim's current mode, as shown in the status bar.
fn source_mode_label(mode: &str) -> String {
    match mode {
        "" => "NORMAL".into(),
        "operator" => "NORMAL".into(),
        "visual_select" => "SELECT".into(),
        mode if mode.starts_with("cmdline") => "COMMAND".into(),
        mode if mode.ends_with("_hover") => "NORMAL".into(),
        mode => mode.split('_').next().unwrap_or(mode).to_uppercase(),
    }
}

fn is_object(block: &Block) -> bool {
    matches!(&block.kind, BlockKind::Image(_)) || block.kind == BlockKind::Math || is_tikz(block)
}

/// A TikZ block's source lines, text and whether its fence is closed.
#[derive(Clone, Debug, PartialEq)]
struct TikzBlock {
    lines: std::ops::Range<usize>,
    source: String,
    closed: bool,
}

fn tikz_blocks(blocks: &[Block], source: &str) -> Vec<TikzBlock> {
    blocks
        .iter()
        .filter(|block| is_tikz(block))
        .filter_map(|block| {
            Some(TikzBlock {
                lines: block.source_lines(source)?,
                source: block.text.clone(),
                closed: block.fence_closed,
            })
        })
        .collect()
}

/// Diagrams to compile now: a block whose fence was just closed compiles at once; an edited
/// block (closed before, same first line) waits until the cursor is outside it.
fn tikz_ready_to_compile(
    previous: &[TikzBlock],
    current: &[TikzBlock],
    cursor_line: Option<usize>,
    known: impl Fn(&str) -> bool,
) -> Vec<String> {
    current
        .iter()
        .filter(|block| block.closed && !known(&block.source))
        .filter(|block| {
            let was_closed = previous
                .iter()
                .any(|old| old.closed && old.lines.start == block.lines.start);
            !was_closed || cursor_line.is_none_or(|line| !block.lines.contains(&line))
        })
        .map(|block| block.source.clone())
        .collect()
}

fn is_tikz(block: &Block) -> bool {
    matches!(&block.kind, BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz"))
}

fn block_len(block: &Block) -> usize {
    if is_object(block) {
        1
    } else {
        block
            .text
            .chars()
            .filter(|character| *character != '\n')
            .count()
    }
}

fn text_range(text: &str, offset: usize) -> Option<std::ops::Range<usize>> {
    let start = text
        .char_indices()
        .filter(|(_, character)| *character != '\n')
        .nth(offset)?
        .0;
    let end = text[start..]
        .char_indices()
        .nth(1)
        .map_or(text.len(), |(next, _)| start + next);
    Some(start..end)
}

fn block_line_count(block: &Block) -> usize {
    if is_object(block) {
        1
    } else {
        block.text.split('\n').count()
    }
}

fn cursor_line(block: &Block, offset: usize) -> (usize, usize) {
    if is_object(block) {
        return (0, 0);
    }
    let mut seen = 0;
    let mut line = 0;
    let mut column = 0;
    for character in block.text.chars() {
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            if seen == offset {
                return (line, column);
            }
            seen += 1;
            column += 1;
        }
    }
    (line, column.saturating_sub(1))
}

fn cursor_for_line(
    block: &Block,
    block_index: usize,
    target_line: usize,
    column: usize,
) -> Option<ReadingCursor> {
    if is_object(block) {
        return (target_line == 0).then_some(ReadingCursor {
            block: block_index,
            offset: 0,
        });
    }
    let mut offset = 0;
    for (line, text) in block.text.split('\n').enumerate() {
        let length = text.chars().count();
        if line == target_line {
            // Lazily: an empty line has no last column.
            return (length > 0).then(|| ReadingCursor {
                block: block_index,
                offset: offset + column.min(length - 1),
            });
        }
        offset += length;
    }
    None
}

fn step_cursor(blocks: &[Block], cursor: ReadingCursor, right: bool) -> Option<ReadingCursor> {
    if right {
        let length = blocks.get(cursor.block).map(block_len).unwrap_or(0);
        if cursor.offset + 1 < length {
            return Some(ReadingCursor {
                offset: cursor.offset + 1,
                ..cursor
            });
        }
        blocks
            .iter()
            .enumerate()
            .skip(cursor.block + 1)
            .find(|(_, block)| block_len(block) > 0)
            .map(|(block, _)| ReadingCursor { block, offset: 0 })
    } else if cursor.offset > 0 {
        Some(ReadingCursor {
            offset: cursor.offset - 1,
            ..cursor
        })
    } else {
        blocks
            .iter()
            .enumerate()
            .take(cursor.block)
            .rfind(|(_, block)| block_len(block) > 0)
            .map(|(block, value)| ReadingCursor {
                block,
                offset: block_len(value) - 1,
            })
    }
}

fn selection_bounds(
    blocks: &[Block],
    selection: ReadingSelection,
    cursor: ReadingCursor,
) -> Option<(ReadingCursor, ReadingCursor)> {
    let (mut start, mut end) = if selection.anchor <= cursor {
        (selection.anchor, cursor)
    } else {
        (cursor, selection.anchor)
    };
    if selection.linewise {
        let start_block = blocks.get(start.block)?;
        let end_block = blocks.get(end.block)?;
        start = cursor_for_line(
            start_block,
            start.block,
            cursor_line(start_block, start.offset).0,
            0,
        )?;
        end = cursor_for_line(
            end_block,
            end.block,
            cursor_line(end_block, end.offset).0,
            usize::MAX,
        )?;
    }
    Some((start, end))
}

fn selection_for_block(
    (start, end): (ReadingCursor, ReadingCursor),
    block: usize,
    length: usize,
) -> Option<(usize, usize)> {
    if length == 0 || block < start.block || block > end.block {
        return None;
    }
    Some((
        if block == start.block {
            start.offset
        } else {
            0
        },
        if block == end.block {
            end.offset
        } else {
            length - 1
        },
    ))
}

fn selection_text(blocks: &[Block], selection: ReadingSelection, cursor: ReadingCursor) -> String {
    let Some((start, end)) = selection_bounds(blocks, selection, cursor) else {
        return String::new();
    };
    let mut output = String::new();
    let mut position = start;
    while let Some(block) = blocks.get(position.block) {
        match &block.kind {
            BlockKind::Image(_) => output.push_str(&block.text),
            BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz") => {
                output.push_str("[TikZ]")
            }
            BlockKind::Math => {
                output.push_str("$$");
                output.push_str(&block.text);
                output.push_str("$$");
            }
            _ => {
                if let Some(image) = inline_image_at_offset(block, position.offset) {
                    output.push_str(&image.alt);
                } else if let Some((_, math)) = inline_math_at_offset(block, position.offset) {
                    output.push('$');
                    output.push_str(&math.source);
                    output.push('$');
                } else if let Some(character) = cursor_character(blocks, position) {
                    output.push(character);
                }
            }
        }
        if position == end {
            break;
        }
        let Some(next) = step_cursor(blocks, position, true) else {
            break;
        };
        if !same_line(blocks, position, next) {
            output.push('\n');
        }
        position = next;
    }
    if selection.linewise {
        output.push('\n');
    }
    output
}

#[derive(Clone, Copy, PartialEq)]
enum WordClass {
    Space,
    Keyword,
    Punctuation,
}

fn cursor_character(blocks: &[Block], cursor: ReadingCursor) -> Option<char> {
    let block = blocks.get(cursor.block)?;
    if is_object(block) {
        Some('\u{fffc}')
    } else {
        block
            .text
            .chars()
            .filter(|character| *character != '\n')
            .nth(cursor.offset)
    }
}

fn inline_image_at_offset(block: &Block, offset: usize) -> Option<&crate::markdown::InlineImage> {
    block.images.iter().find(|image| {
        block.text[..image.range.start]
            .chars()
            .filter(|character| *character != '\n')
            .count()
            == offset
    })
}

fn inline_math_at_offset(
    block: &Block,
    offset: usize,
) -> Option<(usize, &crate::markdown::InlineMath)> {
    block.maths.iter().enumerate().find(|(_, math)| {
        block.text[..math.range.start]
            .chars()
            .filter(|character| *character != '\n')
            .count()
            == offset
    })
}

fn word_class(character: char) -> WordClass {
    if character.is_whitespace() {
        WordClass::Space
    } else if character.is_alphanumeric() || character == '_' {
        WordClass::Keyword
    } else {
        WordClass::Punctuation
    }
}

fn append_count(current: usize, digit: usize) -> usize {
    current.saturating_mul(10).saturating_add(digit).min(9999)
}

fn single_character(text: &str) -> Option<char> {
    let mut characters = text.chars();
    let character = characters.next()?;
    characters.next().is_none().then_some(character)
}

fn find_character(
    blocks: &[Block],
    start: ReadingCursor,
    target: char,
    find: FindPending,
    count: usize,
) -> Option<ReadingCursor> {
    let mut cursor = start;
    let mut matches = 0;
    while let Some(next) = step_cursor(blocks, cursor, find.forward) {
        if !same_line(blocks, start, next) {
            break;
        }
        cursor = next;
        if cursor_character(blocks, cursor) == Some(target) {
            matches += 1;
            if matches == count {
                return if find.till {
                    step_cursor(blocks, cursor, !find.forward)
                } else {
                    Some(cursor)
                };
            }
        }
    }
    None
}

/// The next match of `query` from `start`, wrapping around the document like Vim, and whether
/// the search wrapped. An all-lowercase query matches case-insensitively (smartcase).
fn search_cursor(
    blocks: &[Block],
    start: ReadingCursor,
    query: &str,
    forward: bool,
) -> Option<(ReadingCursor, bool)> {
    let ignore_case = !query.chars().any(char::is_uppercase);
    let query = query.chars().collect::<Vec<_>>();
    if query.is_empty() {
        return None;
    }
    let edge = if forward {
        blocks
            .iter()
            .enumerate()
            .find(|(_, block)| block_len(block) > 0)
            .map(|(block, _)| ReadingCursor { block, offset: 0 })?
    } else {
        blocks
            .iter()
            .enumerate()
            .rfind(|(_, block)| block_len(block) > 0)
            .map(|(block, value)| ReadingCursor {
                block,
                offset: block_len(value) - 1,
            })?
    };
    let mut candidate = step_cursor(blocks, start, forward);
    let mut wrapped = false;
    loop {
        let Some(cursor) = candidate else {
            if wrapped {
                return None;
            }
            candidate = Some(edge);
            wrapped = true;
            continue;
        };
        if wrapped && cursor == start {
            // The only match may be the one under the cursor.
            return matches_query(blocks, cursor, &query, ignore_case).then_some((cursor, true));
        }
        if matches_query(blocks, cursor, &query, ignore_case) {
            return Some((cursor, wrapped));
        }
        candidate = step_cursor(blocks, cursor, forward);
    }
}

fn matches_query(
    blocks: &[Block],
    start: ReadingCursor,
    query: &[char],
    ignore_case: bool,
) -> bool {
    let mut cursor = start;
    for (index, expected) in query.iter().enumerate() {
        let Some(actual) = cursor_character(blocks, cursor) else {
            return false;
        };
        let equal = if ignore_case {
            actual.to_lowercase().eq(expected.to_lowercase())
        } else {
            actual == *expected
        };
        if !equal {
            return false;
        }
        if index + 1 < query.len() {
            let Some(next) = step_cursor(blocks, cursor, true) else {
                return false;
            };
            if !same_line(blocks, cursor, next) {
                return false;
            }
            cursor = next;
        }
    }
    true
}

fn word_under_cursor(blocks: &[Block], cursor: ReadingCursor) -> Option<String> {
    if blocks.get(cursor.block).is_some_and(is_object) {
        return None;
    }
    let class = cursor_word_class(blocks, cursor)?;
    if class == WordClass::Space {
        return None;
    }
    let mut start = cursor;
    while let Some(previous) = step_cursor(blocks, start, false) {
        if !same_line(blocks, start, previous) || cursor_word_class(blocks, previous) != Some(class)
        {
            break;
        }
        start = previous;
    }
    let mut output = String::new();
    let mut position = start;
    loop {
        output.push(cursor_character(blocks, position)?);
        let Some(next) = step_cursor(blocks, position, true) else {
            break;
        };
        if !same_line(blocks, position, next) || cursor_word_class(blocks, next) != Some(class) {
            break;
        }
        position = next;
    }
    Some(output)
}

fn cursor_word_class(blocks: &[Block], cursor: ReadingCursor) -> Option<WordClass> {
    cursor_character(blocks, cursor).map(word_class)
}

fn same_line(blocks: &[Block], left: ReadingCursor, right: ReadingCursor) -> bool {
    left.block == right.block
        && blocks.get(left.block).is_some_and(|block| {
            cursor_line(block, left.offset).0 == cursor_line(block, right.offset).0
        })
}

fn next_word(blocks: &[Block], start: ReadingCursor) -> Option<ReadingCursor> {
    let class = cursor_word_class(blocks, start)?;
    let mut cursor = start;
    while let Some(next) = step_cursor(blocks, cursor, true) {
        cursor = next;
        if !same_line(blocks, start, cursor) || cursor_word_class(blocks, cursor) != Some(class) {
            break;
        }
    }
    while cursor_word_class(blocks, cursor) == Some(WordClass::Space) {
        let Some(next) = step_cursor(blocks, cursor, true) else {
            break;
        };
        cursor = next;
    }
    Some(cursor)
}

fn previous_word(blocks: &[Block], start: ReadingCursor) -> Option<ReadingCursor> {
    let mut cursor = step_cursor(blocks, start, false)?;
    while cursor_word_class(blocks, cursor) == Some(WordClass::Space) {
        cursor = step_cursor(blocks, cursor, false)?;
    }
    let class = cursor_word_class(blocks, cursor)?;
    while let Some(previous) = step_cursor(blocks, cursor, false) {
        if !same_line(blocks, cursor, previous)
            || cursor_word_class(blocks, previous) != Some(class)
        {
            break;
        }
        cursor = previous;
    }
    Some(cursor)
}

fn end_word(blocks: &[Block], start: ReadingCursor) -> Option<ReadingCursor> {
    let current_class = cursor_word_class(blocks, start)?;
    let mut cursor = start;
    if current_class != WordClass::Space
        && step_cursor(blocks, cursor, true).is_some_and(|next| {
            same_line(blocks, cursor, next)
                && cursor_word_class(blocks, next) == Some(current_class)
        })
    {
        while let Some(next) = step_cursor(blocks, cursor, true) {
            if !same_line(blocks, cursor, next)
                || cursor_word_class(blocks, next) != Some(current_class)
            {
                break;
            }
            cursor = next;
        }
        return Some(cursor);
    }

    cursor = step_cursor(blocks, cursor, true)?;
    while cursor_word_class(blocks, cursor) == Some(WordClass::Space) {
        cursor = step_cursor(blocks, cursor, true)?;
    }
    let class = cursor_word_class(blocks, cursor)?;
    while let Some(next) = step_cursor(blocks, cursor, true) {
        if !same_line(blocks, cursor, next) || cursor_word_class(blocks, next) != Some(class) {
            break;
        }
        cursor = next;
    }
    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_math_after_edits_and_aligns_to_the_text_baseline() {
        let mut app = RusidianApp::open(Some(Path::new("examples/math.md")));
        app.document.as_mut().unwrap().markdown = crate::markdown::parse("中文 $x_i$ 重复 $x_i$。");
        assert_eq!(app.pending_math(), vec![("x_i".into(), false)]);
        let formula = Arc::new(Formula::parse("x_i", false).unwrap());
        app.math
            .insert(("x_i".into(), false), MathState::Ready(formula.clone()));
        assert!(app.pending_math().is_empty());
        app.document.as_mut().unwrap().markdown = crate::markdown::parse("修改正文 $x_i$。");
        assert!(app.pending_math().is_empty());
        let MathState::Ready(cached) = app.math.get(&("x_i".into(), false)).unwrap() else {
            panic!()
        };
        assert!(Arc::ptr_eq(cached, &formula));
        let block = &app.document.as_ref().unwrap().markdown.blocks[0];
        let (ascent, descent) = inline_extents(block, &app.math, 18.0);
        assert!(ascent >= 18.0 && descent >= 6.0);
        let formula_top = ascent - formula.ascent(FONT_SIZE) - 1.0;
        assert!((formula_top + 1.0 + formula.ascent(FONT_SIZE) - ascent).abs() < 0.001);
        app.document.as_mut().unwrap().markdown = crate::markdown::parse("$$x_i$$");
        assert_eq!(app.pending_math(), vec![("x_i".into(), true)]);
        assert!(!app.math.contains_key(&("x_i".into(), false)));
    }

    #[test]
    fn keeps_rendered_tikz_until_its_source_changes() {
        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        let source = "\\begin{tikzpicture}\\end{tikzpicture}";
        app.document.as_mut().unwrap().markdown =
            crate::markdown::parse(&format!("a\n\n```tikz\n{source}\n```\n"));
        assert_eq!(app.pending_tikz(), vec![source.to_owned()]);
        app.tikz
            .insert(source.into(), TikzState::Failed("cached".into()));
        app.document.as_mut().unwrap().markdown =
            crate::markdown::parse(&format!("changed\n\n```tikz\n{source}\n```\n"));
        assert!(app.pending_tikz().is_empty());
        assert!(matches!(app.tikz.get(source), Some(TikzState::Failed(_))));
        app.document.as_mut().unwrap().markdown = crate::markdown::parse("no diagrams");
        assert!(app.pending_tikz().is_empty());
        assert!(app.tikz.is_empty());
    }

    #[test]
    fn opens_text_file_and_reports_missing_file() {
        let opened = RusidianApp::open(Some(Path::new("README.md")));
        assert!(!opened.document.unwrap().markdown.blocks.is_empty());

        let missing = RusidianApp::open(Some(Path::new("missing-rusidian-test-file.md")));
        assert!(missing.error.is_some());

        let vault = RusidianApp::open(Some(Path::new("examples")));
        assert!(
            vault
                .vault
                .as_ref()
                .is_some_and(|vault| vault.files.len() >= 3)
        );
        assert!(vault.document.is_some());
    }

    #[test]
    fn offers_to_create_the_note_typed_in_the_switcher() {
        let mut app = RusidianApp::open(Some(Path::new("examples")));
        let mut items = |query: &str| {
            app.switcher = Some(Switcher {
                query: query.into(),
                ..Switcher::default()
            });
            let items = app.switcher_items();
            (items.iter().position(|item| item.create), items)
        };
        assert_eq!(items("markdown").0, None);
        assert_eq!(items("MARKDOWN.md").0, None);
        assert_eq!(items("../escape").0, None);
        assert_eq!(items("").0, None);
        let (Some(new), items) = items("Fresh idea") else {
            panic!("no offer to create the note");
        };
        assert_eq!(new, items.len() - 1);
        assert_eq!(items[new].name, "Fresh idea");
        assert!(items[new].path.ends_with("examples/Fresh idea.md"));
    }

    #[test]
    fn reads_paths_from_system_open_urls() {
        assert_eq!(
            paths_from_urls(vec![
                "file:///Users/me/My%20Notes/%E4%B8%AD%E6%96%87.md".into(),
                "https://example.com/a.md".into(),
            ]),
            [PathBuf::from("/Users/me/My Notes/中文.md")]
        );
    }

    #[test]
    fn shares_quote_levels_with_the_next_block() {
        let blocks =
            crate::markdown::parse("> a\n>\n> b\n> > c\n>\n> > d\n>\n> e\n\n> f\n\ng\n").blocks;
        let texts: Vec<_> = blocks.iter().map(|block| block.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "c", "d", "e", "f", "g"]);
        let shared: Vec<_> = (0..blocks.len())
            .map(|index| shared_quote_depth(&blocks[index], blocks.get(index + 1)))
            .collect();
        // a-b one level; b-c the outer one; c-d sibling nested quotes share the outer one;
        // d-e the outer; e and f are separate quotes; f-g none.
        assert_eq!(shared, [1, 1, 1, 1, 0, 0, 0]);
    }

    #[test]
    fn folds_callouts_with_the_callouts_nested_in_them() {
        let blocks = crate::markdown::parse(
            "a\n\n> [!note]- Outer\n> b\n>\n> > [!tip]- Inner\n> > c\n>\n> d\n\n> [!faq]+ Next\n> e\n\nf\n",
        )
        .blocks;
        let texts: Vec<_> = blocks.iter().map(|block| block.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "c", "d", "e", "f"]);
        let id = |index: usize| blocks[index].callout_quote.unwrap();
        let folded = |ids: &[usize]| ids.iter().copied().collect::<HashSet<_>>();
        assert!(fold_ranges(&blocks, &folded(&[])).is_empty());
        // The outer callout hides the inner one, folded or not.
        assert_eq!(fold_ranges(&blocks, &folded(&[id(1)])), vec![1..4]);
        assert_eq!(fold_ranges(&blocks, &folded(&[id(1), id(2)])), vec![1..4]);
        assert_eq!(fold_ranges(&blocks, &folded(&[id(2), id(4)])), [2..3, 4..5]);
    }

    #[test]
    fn lists_the_vault_tags_by_use() {
        let root = std::env::temp_dir().join(format!("rusidian-tags-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let note = |name: &str, text: &str| {
            let path = root.join(name);
            std::fs::write(&path, text).unwrap();
            path
        };
        let files = [
            note(
                "a.md",
                "---\ntags: [Project, idea]\n---\nBody #project #rust/gpui\n",
            ),
            note(
                "b.md",
                "---\ntags:\n  - idea\n---\n`#code` %%#hidden%% #Rust/GPUI\n",
            ),
            note("c.md", "```\n#fenced\n```\nNo tags here, issue #12.\n"),
            note("d.txt", "#plain"),
        ];
        // A note counts once per tag, whatever the case; code, comments and other files do
        // not count.
        assert_eq!(
            vault_tags(&files),
            [
                ("idea".to_owned(), 2),
                ("rust/gpui".to_owned(), 2),
                ("Project".to_owned(), 1),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn finds_backlinks() {
        let root = std::env::temp_dir().join(format!("rusidian-backlinks-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let root = root.canonicalize().unwrap();
        let note = root.join("sub/Target.md");
        std::fs::write(&note, "# Target\n[[Target]] links itself\n").unwrap();
        std::fs::write(
            root.join("a.md"),
            "See [[target]].\nAlias [[sub/Target|here]] and ![[Target#Part]].\nNot [[Targets]].\n",
        )
        .unwrap();
        std::fs::write(
            root.join("b.md"),
            "Markdown [link](sub/Target.md) and [web](https://x.org)\n",
        )
        .unwrap();
        let files = [root.join("a.md"), root.join("b.md"), note.clone()];
        let found: Vec<_> = find_backlinks(&files, &root, &note)
            .into_iter()
            .map(|hit| (hit.name, hit.line))
            .collect();
        assert_eq!(
            found,
            [
                ("a".to_owned(), 0),
                ("a".to_owned(), 1),
                ("b".to_owned(), 0)
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn searches_note_text() {
        let root = std::env::temp_dir().join(format!("rusidian-search-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.md"), "# A\n\nRust and GPUI\nnothing\n").unwrap();
        std::fs::write(root.join("sub/b.md"), "gpui again\n").unwrap();
        let files = [root.join("a.md"), root.join("sub/b.md")];
        let hits = search_notes(&files, Some(&root), "GPUI", 10);
        let found: Vec<_> = hits
            .iter()
            .map(|hit| {
                (
                    hit.name.as_str(),
                    hit.folder.as_str(),
                    hit.line,
                    hit.text.as_str(),
                )
            })
            .collect();
        assert_eq!(
            found,
            [("a", "", 2, "Rust and GPUI"), ("b", "sub", 0, "gpui again")]
        );
        assert_eq!(search_notes(&files, Some(&root), "gpui", 1).len(), 1);
        assert!(search_notes(&files, Some(&root), "  ", 10).is_empty());
        std::fs::write(
            root.join("c.md"),
            "---\ntags: [Project/rusidian, other]\naliases:\n  - project\n---\ntext\n",
        )
        .unwrap();
        std::fs::write(root.join("d.md"), "---\ntags:\n  - project\n---\n").unwrap();
        let tagged = search_notes(
            &[root.join("c.md"), root.join("d.md")],
            Some(&root),
            "#project",
            10,
        );
        let found: Vec<_> = tagged
            .iter()
            .map(|hit| (hit.name.as_str(), hit.line))
            .collect();
        assert_eq!(found, [("c", 1), ("d", 2)]);
        std::fs::remove_dir_all(root).unwrap();

        let long = format!("{}needle{}", "a".repeat(200), "b".repeat(200));
        let excerpt = hit_excerpt(&long, "needle");
        assert!(excerpt.starts_with('…') && excerpt.ends_with('…'));
        assert!(excerpt.contains("needle"));
        assert_eq!(line_from_fragment(&line_fragment(86)), Some(86));
        assert_eq!(line_from_fragment("Heading"), None);
    }

    #[test]
    fn scores_fuzzy_note_matches() {
        let score = |query: &str, folder: &str, name: &str| {
            let text = format!("{folder}{name}");
            fuzzy_score(query, &text, folder.chars().count())
        };
        assert!(score("plan", "Projects/Rusidian/", "plan").is_some());
        assert!(score("xyz", "Projects/", "plan").is_none());
        // Order matters; case and spaces do not.
        assert!(score("nalp", "", "plan").is_none());
        assert!(score("P LAN", "", "plan").is_some());
        // Consecutive letters in the name beat scattered ones in folders.
        assert!(score("plan", "", "plan") > score("plan", "people/landing/", "notes"));
        assert!(score("日记", "Daily/", "2026 日记") > score("日记", "日/", "记录"));
    }

    /// Random notes and reading motions: the cursor stays on a block and nothing panics.
    #[test]
    #[ignore = "slow: random inputs"]
    fn moves_through_random_notes_without_panicking() {
        let pieces = [
            "%%",
            "[[",
            "]]",
            "![[",
            "|",
            "#",
            "^id",
            "> ",
            "> [!note]- ",
            "> [!tip]+ x",
            "- ",
            "- [ ] ",
            "- [/] ",
            "1. ",
            "$",
            "$$",
            "==",
            "**",
            "*",
            "_",
            "~~",
            "`",
            "```",
            "\n",
            "\n\n",
            "\t",
            "    ",
            "中文",
            "😀",
            "é",
            "a",
            "b c",
            "http://x.y/z",
            "#tag",
            "[^1]",
            "[^1]: ",
            "<span>",
            "---\n",
            "|a|b|\n|-|-|\n",
            "\\",
            "[x](y.md)",
            "![i](p.png)",
            "%",
            "]",
            "[",
            "\u{200b}",
            "^",
            "> > ",
            "* * *",
            "\n> ",
            "\n- ",
        ];
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut app = RusidianApp::open(Some(Path::new("examples/markdown.md")));
        for round in 0..10000 {
            let count = (next() % 60) as usize;
            let mut source = String::new();
            for _ in 0..count {
                source.push_str(pieces[(next() % pieces.len() as u64) as usize]);
            }
            {
                let document = app.document.as_mut().unwrap();
                document.lines = source.split('\n').map(str::to_owned).collect();
                document.parse(round % 2 == 0);
            }
            let blocks_len = app.document.as_ref().unwrap().markdown.blocks.len();
            app.reading_cursor = initial_cursor(&app.document.as_ref().unwrap().markdown.blocks);
            app.reading_selection = None;
            app.reading_column = None;
            app.callout_toggles.clear();
            app.callout_cursor_parked = None;
            for id in 0..4 {
                if next() % 2 == 0 {
                    app.callout_toggles.insert(id);
                }
            }
            for _ in 0..80 {
                match next() % 16 {
                    0 => app.move_reading_line(true),
                    1 => app.move_reading_line(false),
                    2 => app.move_reading_cursor(true),
                    3 => app.move_reading_cursor(false),
                    4 => app.move_reading_word(WordMotion::Next),
                    5 => app.move_reading_word(WordMotion::Previous),
                    6 => app.move_reading_word(WordMotion::End),
                    7 => app.move_reading_line_edge(true),
                    8 => app.move_reading_line_edge(false),
                    9 => app.move_reading_first_nonblank(),
                    10 => app.move_reading_document_edge(next() % 2 == 0),
                    11 => {
                        app.reading_selection = Some(ReadingSelection {
                            anchor: app.reading_cursor,
                            linewise: next() % 2 == 0,
                        })
                    }
                    12 => {
                        if let Some(selection) = app.reading_selection {
                            let _ = app.selected_clipboard(selection);
                        }
                    }
                    13 => {
                        let blocks = &app.document.as_ref().unwrap().markdown.blocks;
                        let target = ['a', 'b', '中', ' ', '|', 'é'][(next() % 6) as usize];
                        let find = FindPending {
                            forward: next() % 2 == 0,
                            till: next() % 2 == 0,
                        };
                        if let Some(cursor) =
                            find_character(blocks, app.reading_cursor, target, find, 2)
                        {
                            app.reading_cursor = cursor;
                        }
                        let _ = word_under_cursor(blocks, app.reading_cursor);
                    }
                    14 => app.rest_on_fold(),
                    _ => {
                        let document = app.document.as_ref().unwrap();
                        let line = (next() as usize) % (document.lines.len() + 1);
                        let column = (next() as usize) % 8;
                        let offset = source_offset(&document.lines, line, column);
                        if let Some(cursor) = reading_position(&document.markdown.blocks, offset) {
                            app.reading_cursor = cursor;
                        }
                    }
                }
                let blocks = &app.document.as_ref().unwrap().markdown.blocks;
                if blocks_len > 0 {
                    assert!(app.reading_cursor.block < blocks_len, "{source:?}");
                }
                if let Some(selection) = app.reading_selection {
                    let _ = selection_bounds(blocks, selection, app.reading_cursor);
                }
            }
        }
    }

    #[test]
    fn groups_blocks_far_from_the_view() {
        let drawn = [false, false, true, false, false, false, true];
        let heights = [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0].map(px);
        let gaps = [px(1.0); 7];
        let (children, child_of, offsets) = group_blocks(&drawn, &heights, &gaps);
        assert_eq!(children, [0..2, 2..3, 3..6, 6..7]);
        assert_eq!(child_of, [0, 0, 1, 2, 2, 2, 3]);
        assert_eq!(offsets, [0.0, 11.0, 0.0, 0.0, 41.0, 92.0, 0.0].map(px));
    }

    #[test]
    fn estimates_heights_from_text() {
        let blocks = crate::markdown::parse(&format!(
            "# Title\n\n{}\n\n{}\n\n```\na\nb\nc\n```\n",
            "word ".repeat(100),
            "中文".repeat(250)
        ))
        .blocks;
        let width = px(400.0);
        let [heading, latin, cjk, code] =
            [0, 1, 2, 3].map(|index| f32::from(estimate_height(&blocks[index], width)));
        assert!(heading > 24.0 && heading < 60.0);
        // 500 half-em columns at 50 a line: ten lines; CJK takes twice the room.
        assert_eq!(latin, 240.0);
        assert_eq!(cjk, 480.0);
        assert!(code > 3.0 * 20.0);
    }

    #[test]
    fn keeps_measured_heights_of_blocks_an_edit_left_alone() {
        let mut app = RusidianApp::open(Some(Path::new("examples/markdown.md")));
        let parse = |app: &mut RusidianApp, text: &str| {
            let document = app.document.as_mut().unwrap();
            document.markdown = crate::markdown::parse(text);
            document.markdown.blocks.len()
        };
        let plan = |app: &RusidianApp| {
            let blocks = &app.document.as_ref().unwrap().markdown.blocks;
            app.plan_blocks(
                blocks,
                vec![px(0.0); blocks.len()],
                &vec![false; blocks.len()],
            );
            app.block_heights.borrow().heights.clone()
        };
        let count = parse(&mut app, "a\n\nb\n\nc\n");
        app.reading_cursor = ReadingCursor::default();
        plan(&app);
        app.block_heights.borrow_mut().heights = vec![px(100.0), px(200.0), px(300.0)];
        assert_eq!(count, 3);
        // A block inserted in the middle is estimated; the others keep their heights.
        parse(&mut app, "a\n\nnew\n\nb\n\nc\n");
        app.remap_block_heights();
        let heights = plan(&app);
        assert_eq!(heights[0], px(100.0));
        assert_eq!(heights[1], px(24.0));
        assert_eq!(heights[2..], [px(200.0), px(300.0)]);
        // Heights stay as estimates when something else changed, until measured.
        app.invalidate_block_heights();
        assert_eq!(plan(&app)[3], px(300.0));
    }

    #[test]
    fn reads_obsidian_image_sizes() {
        assert_eq!(image_size("120"), Some((120.0, None)));
        assert_eq!(image_size("logo|200x100"), Some((200.0, Some(100.0))));
        assert_eq!(image_size(" 64 "), Some((64.0, None)));
        assert_eq!(image_size("photo"), None);
        assert_eq!(image_size("1920x1080 screenshot"), None);
        assert_eq!(image_size("0"), None);
        assert_eq!(image_size(""), None);
    }

    #[test]
    fn embeds_notes_sections_and_blocks() {
        let blocks = crate::markdown::parse(
            "---\ntags: [a]\n---\n# Top\n\nintro\n\n## Part\n\nin part\n\n### Deeper\n\nstill part\n\n## Next\n\nmarked ^id1\n",
        )
        .blocks;
        let texts = |blocks: &[Block]| {
            blocks
                .iter()
                .map(|block| block.text.clone())
                .collect::<Vec<_>>()
        };
        // The whole note leaves out its properties.
        assert_eq!(embedded_blocks(&blocks, None).unwrap()[0].text, "Top");
        assert_eq!(
            texts(embedded_blocks(&blocks, Some("Part")).unwrap()),
            ["Part", "in part", "Deeper", "still part"]
        );
        assert_eq!(
            texts(embedded_blocks(&blocks, Some("^id1")).unwrap()),
            ["marked"]
        );
        assert!(embedded_blocks(&blocks, Some("Missing")).is_none());

        let folder = std::env::temp_dir().join(format!("rusidian-embed-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let note = folder.join("index.md");
        std::fs::write(folder.join("target.md"), "# Target\n").unwrap();
        std::fs::write(folder.join("pic.png"), b"png").unwrap();
        let links = Links {
            note: &note,
            vault: None,
        };
        assert_eq!(
            links.embedded_note("target#Part"),
            Some(folder.join("target.md"))
        );
        assert_eq!(links.embedded_note("#Part"), Some(note.clone()));
        assert_eq!(links.embedded_note("pic.png"), None);
        assert_eq!(links.embedded_note("https://example.com/a.md"), None);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn reveals_heading_and_block_fragments() {
        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        app.document.as_mut().unwrap().markdown = crate::markdown::parse(
            "# Intro\n\ntext[^n]\n\n## Second Part\n\nquoted line ^abc123\n\n[^n]: Note.\n",
        );
        assert!(app.reveal_fragment("Second%20Part"));
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 2,
                offset: 0
            }
        );
        assert!(app.reveal_fragment("second-part"));
        assert!(app.reveal_fragment("^abc123"));
        assert_eq!(app.reading_cursor.block, 3);
        assert!(!app.reveal_fragment("Missing"));
        // Footnote references link to their definition.
        app.reading_cursor = ReadingCursor {
            block: 1,
            offset: 5,
        };
        assert_eq!(app.current_link().as_deref(), Some("#[^n]"));
        assert!(app.reveal_fragment("[^n]"));
        assert_eq!(app.reading_cursor.block, 4);
        assert!(!app.reveal_fragment("[^missing]"));
        assert_eq!(heading_key("Hello, World!"), heading_key("hello-world"));
        assert!(is_text_file(Path::new("README.md")));
        assert!(!is_text_file(Path::new("assets/app-icon.png")));
        assert!(is_external_link("https://example.com") && !is_external_link("note.md"));
    }

    #[test]
    fn opens_plain_text_files_in_source_view_only() {
        let text = RusidianApp::open(Some(Path::new("Cargo.toml")));
        assert!(text.view == View::Source && !text.has_reading_view());
        assert!(text.document.as_ref().unwrap().markdown.blocks.is_empty());
        let note = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        assert!(note.view == View::Reading && note.has_reading_view());
    }

    #[test]
    fn marks_unsaved_changes_in_the_title() {
        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        app.modified = true;
        assert_eq!(app.window_title(), "● tikz.md — Rusidian");
    }

    #[test]
    fn titles_the_window_with_note_and_vault() {
        assert_eq!(RusidianApp::open(None).window_title(), "Rusidian");
        assert_eq!(
            RusidianApp::open(Some(Path::new("examples/tikz.md"))).window_title(),
            "tikz.md — Rusidian"
        );
        let vault = RusidianApp::open(Some(Path::new("examples")));
        assert!(vault.window_title().ends_with(" — examples"));
    }

    #[test]
    fn maps_reading_positions_to_source_and_back() {
        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        let source = "# Title\n\nSome **bold** and `code`.\n\n$$x^2$$\n\n- item\n";
        let document = app.document.as_mut().unwrap();
        document.lines = source_lines(source);
        document.parse(false);
        let document = app.document.as_ref().unwrap();
        let blocks = &document.markdown.blocks;
        // "b" of "bold" is the sixth visible character of the paragraph.
        let bold = ReadingCursor {
            block: 1,
            offset: 5,
        };
        assert_eq!(source_position(document, bold), Some((2, 7)));
        assert_eq!(
            reading_position(blocks, source_offset(&document.lines, 2, 7)),
            Some(bold)
        );
        // Hidden markup maps forward to the next visible character.
        assert_eq!(
            reading_position(blocks, source_offset(&document.lines, 2, 5)),
            Some(bold)
        );
        assert_eq!(
            reading_position(blocks, source_offset(&document.lines, 0, 0)),
            Some(ReadingCursor::default())
        );
        let math = ReadingCursor {
            block: 2,
            offset: 0,
        };
        assert_eq!(
            reading_position(blocks, source_offset(&document.lines, 4, 3)),
            Some(math)
        );
        assert_eq!(source_position(document, math), Some((4, 2)));
        assert_eq!(
            reading_position(blocks, source_offset(&document.lines, 6, 4)),
            Some(ReadingCursor {
                block: 3,
                offset: 2
            })
        );
        assert_eq!(
            source_offset(&document.lines, 99, 99),
            source.trim_end().len()
        );
    }

    #[test]
    fn reads_simple_front_matter_as_properties() {
        assert_eq!(
            parse_properties(
                "title: \"Edge: cases\"\ntags: [a, 'b']\naliases:\n  - one\n  - two\nempty:"
            ),
            Some(vec![
                Property {
                    key: "title".into(),
                    values: vec!["Edge: cases".into()],
                    list: false,
                },
                Property {
                    key: "tags".into(),
                    values: vec!["a".into(), "b".into()],
                    list: true,
                },
                Property {
                    key: "aliases".into(),
                    values: vec!["one".into(), "two".into()],
                    list: true,
                },
                Property {
                    key: "empty".into(),
                    values: Vec::new(),
                    list: false,
                },
            ])
        );
        assert_eq!(parse_properties("nested:\n  key: value"), None);
        let note = crate::markdown::parse("---\ntitle: x\n---\n\n# Body\n");
        assert_eq!(
            initial_cursor(&note.blocks),
            ReadingCursor {
                block: 1,
                offset: 0
            }
        );
        let only = crate::markdown::parse("---\ntitle: x\n---\n");
        assert_eq!(initial_cursor(&only.blocks), ReadingCursor::default());
        assert_eq!(parse_properties("- orphan"), None);
        assert_eq!(parse_properties("just text"), None);
    }

    #[test]
    fn compiles_tikz_when_the_fence_closes_or_the_cursor_leaves() {
        let block = |start: usize, source: &str, closed: bool| TikzBlock {
            lines: start..start + 3,
            source: source.into(),
            closed,
        };
        let never_compiled = |_: &str| false;
        // An open fence never compiles; closing it compiles at once, even with the cursor inside.
        assert!(
            tikz_ready_to_compile(&[], &[block(2, "a", false)], Some(3), never_compiled).is_empty()
        );
        assert_eq!(
            tikz_ready_to_compile(
                &[block(2, "a", false)],
                &[block(2, "ab", true)],
                Some(3),
                never_compiled
            ),
            ["ab"]
        );
        // Editing a closed block waits for the cursor to leave it.
        let edited = [block(2, "abc", true)];
        assert!(
            tikz_ready_to_compile(&[block(2, "ab", true)], &edited, Some(3), never_compiled)
                .is_empty()
        );
        assert_eq!(
            tikz_ready_to_compile(&edited, &edited, Some(9), never_compiled),
            ["abc"]
        );
        // Diagrams already compiled or compiling are left alone.
        assert!(tikz_ready_to_compile(&edited, &edited, Some(9), |_| true).is_empty());
    }

    #[test]
    fn edits_invalidate_layouts_from_the_last_frame() {
        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        app.layouts_ready.set(true);
        assert!(app.update_buffer(0, None, vec!["# Changed".into()], false));
        assert!(!app.layouts_ready.get());
        assert!(!app.move_reading_screen_line(true));
        assert!(app.reading_position_at(px(0.0), px(0.0), true).is_none());
    }

    #[test]
    fn labels_neovim_modes() {
        assert_eq!(source_mode_label("normal"), "NORMAL");
        assert_eq!(source_mode_label("insert"), "INSERT");
        assert_eq!(source_mode_label("visual"), "VISUAL");
        assert_eq!(source_mode_label("cmdline_normal"), "COMMAND");
        assert_eq!(source_mode_label("operator"), "NORMAL");
        assert_eq!(source_mode_label("replace"), "REPLACE");
    }

    #[test]
    fn translates_gpui_keys_for_neovim() {
        let key = |source: &str| nvim_key(&Keystroke::parse(source).unwrap());
        assert_eq!(key("a").as_deref(), Some("a"));
        assert_eq!(key("ctrl-a").as_deref(), Some("<C-a>"));
        assert_eq!(key("left").as_deref(), Some("<Left>"));
        assert_eq!(key("f5").as_deref(), Some("<F5>"));
        assert_eq!(key("shift-tab").as_deref(), Some("<S-Tab>"));
        assert_eq!(key("ctrl-<").as_deref(), Some("<C-lt>"));
        let typed = |key: &str, character: &str, modifiers| {
            nvim_key(&Keystroke {
                modifiers,
                key: key.into(),
                key_char: Some(character.into()),
            })
        };
        // Shifted symbols must arrive as the typed character, not as <S-4>.
        assert_eq!(typed("4", "$", Modifiers::shift()).as_deref(), Some("$"));
        assert_eq!(typed("a", "A", Modifiers::shift()).as_deref(), Some("A"));
        assert_eq!(typed(",", "<", Modifiers::shift()).as_deref(), Some("<lt>"));
        assert_eq!(
            typed("enter", "\n", Modifiers::none()).as_deref(),
            Some("<CR>")
        );
    }

    #[test]
    fn updates_preview_from_neovim_lines() {
        let mut app = RusidianApp::open(Some(Path::new("README.md")));
        assert!(app.update_buffer(0, None, vec!["# 未保存标题".into()], false));
        assert_eq!(
            app.document.unwrap().markdown.blocks[0].kind,
            BlockKind::Heading(1)
        );
    }

    #[test]
    fn moves_reading_cursor_across_visible_content() {
        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        app.move_reading_cursor(false);
        assert_eq!(app.reading_cursor, ReadingCursor::default());
        for _ in 0..12 {
            app.move_reading_cursor(true);
        }
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 1,
                offset: 0
            }
        );
        app.move_reading_cursor(false);
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 0,
                offset: 11
            }
        );
        assert_eq!(text_range("中文", 1), Some(3..6));
    }

    #[test]
    fn moves_reading_cursor_by_physical_line() {
        let mut app = RusidianApp::open(Some(Path::new("README.md")));
        app.document.as_mut().unwrap().markdown = crate::markdown::parse("abc\ndef\n\nxy");
        app.reading_cursor = ReadingCursor {
            block: 0,
            offset: 2,
        };

        app.move_reading_line(true);
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 0,
                offset: 5
            }
        );
        app.move_reading_line(true);
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 1,
                offset: 1
            }
        );
        app.move_reading_line(false);
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 0,
                offset: 5
            }
        );
        app.move_reading_line_edge(false);
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 0,
                offset: 3
            }
        );
        app.move_reading_document_edge(true);
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 1,
                offset: 1
            }
        );
        assert_eq!(text_range("abc\ndef", 3), Some(4..5));

        app.document.as_mut().unwrap().markdown = crate::markdown::parse("```\n  abc\n```");
        app.reading_cursor = ReadingCursor {
            block: 0,
            offset: 4,
        };
        app.move_reading_first_nonblank();
        assert_eq!(
            app.reading_cursor,
            ReadingCursor {
                block: 0,
                offset: 2
            }
        );
    }

    #[test]
    fn moves_by_words_and_builds_counts() {
        let document = crate::markdown::parse("one two,中文\nnext");
        let blocks = &document.blocks;
        assert_eq!(
            next_word(blocks, ReadingCursor::default()),
            Some(ReadingCursor {
                block: 0,
                offset: 4
            })
        );
        assert_eq!(
            end_word(blocks, ReadingCursor::default()),
            Some(ReadingCursor {
                block: 0,
                offset: 2
            })
        );
        assert_eq!(
            previous_word(
                blocks,
                ReadingCursor {
                    block: 0,
                    offset: 10
                }
            ),
            Some(ReadingCursor {
                block: 0,
                offset: 8
            })
        );
        assert_eq!(append_count(12, 3), 123);
        assert_eq!(append_count(9999, 9), 9999);
    }

    #[test]
    fn finds_characters_with_vim_semantics() {
        let document = crate::markdown::parse("a.b.a");
        let blocks = &document.blocks;
        let forward = FindPending {
            forward: true,
            till: false,
        };
        assert_eq!(
            find_character(blocks, ReadingCursor::default(), '.', forward, 2),
            Some(ReadingCursor {
                block: 0,
                offset: 3
            })
        );
        assert_eq!(
            find_character(
                blocks,
                ReadingCursor::default(),
                '.',
                FindPending {
                    till: true,
                    ..forward
                },
                2,
            ),
            Some(ReadingCursor {
                block: 0,
                offset: 2
            })
        );
        assert_eq!(single_character("中"), Some('中'));
        assert_eq!(single_character("中文"), None);
    }

    #[test]
    fn selects_rendered_text() {
        let document = crate::markdown::parse("one two\nthree\n\nfour");
        let blocks = &document.blocks;
        assert_eq!(
            selection_text(
                blocks,
                ReadingSelection {
                    anchor: ReadingCursor {
                        block: 0,
                        offset: 1
                    },
                    linewise: false,
                },
                ReadingCursor {
                    block: 0,
                    offset: 5
                },
            ),
            "ne tw"
        );
        assert_eq!(
            selection_text(
                blocks,
                ReadingSelection {
                    anchor: ReadingCursor {
                        block: 0,
                        offset: 4
                    },
                    linewise: true,
                },
                ReadingCursor {
                    block: 0,
                    offset: 8
                },
            ),
            "one two\nthree\n"
        );

        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        app.reading_cursor = ReadingCursor {
            block: 2,
            offset: 0,
        };
        let image = app
            .selected_clipboard(ReadingSelection {
                anchor: app.reading_cursor,
                linewise: false,
            })
            .unwrap();
        assert!(image.text().is_none());
        let links = Links {
            note: Path::new("note.md"),
            vault: None,
        };
        assert!(links.image("https://example.com/a.png").is_none());
        assert_eq!(links.image("a%20b.png"), Some(PathBuf::from("a b.png")));

        app.document.as_mut().unwrap().markdown = crate::markdown::parse("a ![图](rusidian.svg) b");
        app.reading_cursor = ReadingCursor {
            block: 0,
            offset: 2,
        };
        let inline_block = &app.document.as_ref().unwrap().markdown.blocks[0];
        assert_eq!(inline_block.images.len(), 1);
        assert_eq!(
            inline_image_at_offset(inline_block, 2).unwrap().source,
            "rusidian.svg"
        );
        let inline = app
            .selected_clipboard(ReadingSelection {
                anchor: app.reading_cursor,
                linewise: false,
            })
            .unwrap();
        assert!(inline.text().is_none());
        assert_eq!(
            selection_text(
                &app.document.as_ref().unwrap().markdown.blocks,
                ReadingSelection {
                    anchor: ReadingCursor::default(),
                    linewise: false,
                },
                ReadingCursor {
                    block: 0,
                    offset: 4
                },
            ),
            "a 图 b"
        );
    }

    #[test]
    fn searches_rendered_text_with_wraparound() {
        let document = crate::markdown::parse("alpha beta\nalpha");
        let blocks = &document.blocks;
        assert_eq!(
            search_cursor(blocks, ReadingCursor::default(), "alpha", true),
            Some((
                ReadingCursor {
                    block: 0,
                    offset: 10
                },
                false
            ))
        );
        assert_eq!(
            search_cursor(
                blocks,
                ReadingCursor {
                    block: 0,
                    offset: 10
                },
                "alpha",
                true
            ),
            Some((ReadingCursor::default(), true))
        );
        assert!(search_cursor(blocks, ReadingCursor::default(), "betaalpha", true).is_none());
        // Smartcase: lowercase queries ignore case, mixed-case queries do not.
        let mixed = crate::markdown::parse("Rust and rust");
        assert_eq!(
            search_cursor(&mixed.blocks, ReadingCursor::default(), "rust", true),
            Some((
                ReadingCursor {
                    block: 0,
                    offset: 9
                },
                false
            ))
        );
        assert_eq!(
            search_cursor(
                &mixed.blocks,
                ReadingCursor {
                    block: 0,
                    offset: 9
                },
                "Rust",
                true
            ),
            Some((ReadingCursor::default(), true))
        );
        // A single match under the cursor is found again after wrapping.
        let single = crate::markdown::parse("only once");
        assert_eq!(
            search_cursor(&single.blocks, ReadingCursor::default(), "only", true),
            Some((ReadingCursor::default(), true))
        );
        assert_eq!(
            word_under_cursor(
                blocks,
                ReadingCursor {
                    block: 0,
                    offset: 7
                }
            )
            .as_deref(),
            Some("beta")
        );
        assert_eq!(clipped_range(&(0..2), &(4..6)), None);

        let mut app = RusidianApp::open(Some(Path::new("examples/markdown.md")));
        let link_block = &app.document.as_ref().unwrap().markdown.blocks[1];
        let byte = link_block.text.find("本地链接").unwrap();
        app.reading_cursor = ReadingCursor {
            block: 1,
            offset: link_block.text[..byte].chars().count(),
        };
        assert_eq!(app.current_link().as_deref(), Some("linked.md"));
    }

    #[test]
    fn cursor_and_selection_highlights_do_not_overlap_styled_spans() {
        let document = crate::markdown::parse("a **[bold](x.md)** c");
        let block = &document.blocks[0];
        assert_eq!(block.text, "a bold c");
        let highlights = fragment_highlights(
            &Theme::DARK,
            block,
            &(0..block.text.len()),
            Some(3),
            Some((2, 4)),
            &HashSet::new(),
        );
        for pair in highlights.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{highlights:?}");
        }
        let cursor = highlights
            .iter()
            .find(|(_, style)| style.color == Some(rgb(Theme::DARK.cursor_fg).into()))
            .unwrap();
        assert_eq!(cursor.0, 3..4);
        assert_eq!(cursor.1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(
            highlights
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>(),
            [2..3, 3..4, 4..5, 5..6]
        );
    }

    #[test]
    fn highlight_ranges_stay_on_utf8_boundaries() {
        let source = std::fs::read_to_string("examples/markdown.md").unwrap();
        let document = crate::markdown::parse(&source);
        for block in &document.blocks {
            let mut boundaries = block
                .text
                .char_indices()
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            boundaries.push(block.text.len());
            for pair in boundaries.windows(2) {
                let range = pair[0]..pair[1];
                let fragment = &block.text[range.clone()];
                for cursor in 0..block_len(block) {
                    for (highlight, _) in fragment_highlights(
                        &Theme::DARK,
                        block,
                        &range,
                        Some(cursor),
                        Some((0, cursor)),
                        &HashSet::new(),
                    ) {
                        assert!(fragment.is_char_boundary(highlight.start));
                        assert!(fragment.is_char_boundary(highlight.end));
                    }
                }
            }
        }
    }
}
