use crate::markdown::{Block, BlockKind, MarkdownDocument};
use crate::math::{FONT_SIZE, Formula};
use crate::nvim::{Client as NvimClient, CursorShape, Event as NvimEvent, Grid as NvimGrid};
use crate::update;
use crate::vault::Vault;
use cargo_packager_updater::Update;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, FontStyle, FontWeight, HighlightStyle, Image, ImageFormat, KeyBinding,
    KeyDownEvent, Keystroke, Menu, MenuItem, PathPromptOptions, Pixels, Point, ScrollHandle,
    SharedString, Size, StrikethroughStyle, StyledText, UTF16Selection, UnderlineStyle, Window,
    WindowBounds, WindowOptions, actions, canvas, div, img, point, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use std::{
    cell::Cell,
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

actions!(
    rusidian,
    [
        OpenFile,
        OpenFolder,
        OpenSettings,
        Quit,
        CloseWindow,
        EnterSourceNormal
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
        KeyBinding::new(&format!("{modifier}-w"), CloseWindow, context),
        KeyBinding::new("enter", EnterSourceNormal, Some("Reading")),
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
                MenuItem::action("打开文件…", OpenFile),
                MenuItem::action("打开文件夹…", OpenFolder),
                MenuItem::separator(),
                MenuItem::action("关闭窗口", CloseWindow),
            ]),
        ]);

        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let app = cx.new(|cx| {
                    let mut app = RusidianApp::open_with_vault(initial_path.as_deref());
                    app.focus_handle = Some(cx.focus_handle());
                    app.compile_visuals(cx);
                    app.start_nvim(cx);
                    if app.auto_update && update::is_packaged_app() {
                        app.check_for_updates(true, cx);
                    }
                    app
                });
                let focus = app.read(cx).focus_handle.clone();
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
                let quit_app = app.downgrade();
                cx.on_action(move |_: &Quit, cx| {
                    quit_app
                        .update(cx, |app, cx| app.request_close(PendingClose::Quit, cx))
                        .ok();
                });
                // The app has a single window, so closing it quits after Neovim agrees.
                let close_window_app = app.downgrade();
                cx.on_action(move |_: &CloseWindow, cx| {
                    close_window_app
                        .update(cx, |app, cx| app.request_close(PendingClose::Quit, cx))
                        .ok();
                });
                let open_app = app.downgrade();
                cx.spawn(async move |cx| {
                    while let Ok(path) = opened.recv().await {
                        let opened = open_app.update(cx, |app, cx| {
                            let vault_root = app.vault.as_ref().and_then(|vault| {
                                path.starts_with(&vault.root).then(|| vault.root.clone())
                            });
                            app.request_close(
                                PendingClose::Open {
                                    path,
                                    vault_root,
                                    fragment: None,
                                },
                                cx,
                            );
                        });
                        if opened.is_err() {
                            break;
                        }
                    }
                })
                .detach();
                let close_app = app.downgrade();
                window.on_window_should_close(cx, move |_, cx| {
                    close_app
                        .update(cx, |app, cx| app.request_close(PendingClose::Quit, cx))
                        .ok();
                    false
                });
                app
            },
        )
        .expect("failed to open Rusidian window");

        cx.activate(true);
    });
}

struct Document {
    file: PathBuf,
    name: SharedString,
    path: SharedString,
    lines: Vec<String>,
    markdown: MarkdownDocument,
}

struct RusidianApp {
    document: Option<Document>,
    vault: Option<Vault>,
    error: Option<SharedString>,
    /// Rendered TikZ keyed by block source, so unchanged diagrams survive edits elsewhere.
    tikz: HashMap<String, TikzState>,
    math: HashMap<(String, bool), MathState>,
    view: View,
    nvim: Option<NvimClient>,
    pending_close: Option<PendingClose>,
    grid: NvimGrid,
    nvim_error: Option<SharedString>,
    nvim_warning: Option<SharedString>,
    /// Grid size last requested from Neovim; shared with the layout pass that measures it.
    nvim_size: Rc<Cell<(i64, i64)>>,
    cell_size: Size<Pixels>,
    reading_cursor: ReadingCursor,
    reading_column: Option<usize>,
    reading_pending_g: bool,
    reading_count: Option<usize>,
    reading_find: Option<FindPending>,
    reading_selection: Option<ReadingSelection>,
    reading_search: Option<SearchPrompt>,
    last_search: Option<SearchPrompt>,
    reading_scroll: ScrollHandle,
    focus_handle: Option<FocusHandle>,
    marked_text: String,
    marked_selection: std::ops::Range<usize>,
    settings_open: bool,
    /// A transient message for the status bar, such as a failed link or a completed copy.
    notice: Option<Notice>,
    notice_generation: u64,
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
    Quit,
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
        Self {
            document: None,
            vault: None,
            error: None,
            tikz: HashMap::new(),
            math: HashMap::new(),
            view: View::Reading,
            nvim: None,
            pending_close: None,
            grid: NvimGrid::default(),
            nvim_error: None,
            nvim_warning: None,
            nvim_size: Rc::new(Cell::new((120, 40))),
            cell_size: size(px(SOURCE_FONT_SIZE * 0.6), px(SOURCE_LINE_HEIGHT)),
            reading_cursor: ReadingCursor::default(),
            reading_column: None,
            reading_pending_g: false,
            reading_count: None,
            reading_find: None,
            reading_selection: None,
            reading_search: None,
            last_search: None,
            reading_scroll: ScrollHandle::new(),
            focus_handle: None,
            marked_text: String::new(),
            marked_selection: 0..0,
            settings_open: false,
            notice: None,
            notice_generation: 0,
            auto_update: update::auto_update_enabled(),
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
                    let first = vault.files.first().cloned();
                    app = Self::open(first.as_deref());
                    app.attach_vault(vault);
                }
                Err(error) => {
                    app.error = Some(format!("无法打开文件夹 {}：{error}", path.display()).into());
                }
            }
            return app;
        }

        match std::fs::read_to_string(path) {
            Ok(content) => {
                app.document = Some(Document {
                    file: path.to_path_buf(),
                    name: path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                    path: path.to_string_lossy().into_owned().into(),
                    lines: source_lines(&content),
                    markdown: crate::markdown::parse(&content),
                });
            }
            Err(error) => {
                app.error = Some(format!("无法打开 {}：{error}", path.display()).into());
            }
        }
        app
    }

    fn attach_vault(&mut self, vault: Vault) {
        if let Some(document) = &mut self.document {
            document.markdown = crate::markdown::parse_with_options(
                &document.lines.join("\n"),
                vault.settings.strict_line_breaks,
            );
        }
        self.vault = Some(vault);
    }

    fn pending_tikz(&mut self) -> Vec<String> {
        let Some(document) = &self.document else {
            self.tikz.clear();
            return Vec::new();
        };
        let sources: HashSet<_> = document
            .markdown
            .blocks
            .iter()
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
        for source in self.pending_tikz() {
            let executor = cx.background_executor().clone();
            cx.spawn(async move |this, cx| {
                let input = source.clone();
                let result = executor
                    .spawn(async move { crate::tikz::compile(&input) })
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
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn pending_math(&mut self) -> Vec<(String, bool)> {
        let Some(document) = &self.document else {
            self.math.clear();
            return Vec::new();
        };
        let keys: HashSet<_> = document
            .markdown
            .blocks
            .iter()
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
        self.compile_tikz(cx);
        self.compile_math(cx);
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
        let client = NvimClient::start(path, false, self.nvim_size.get());
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
                            if this.grid.apply_redraw(&events) {
                                cx.notify();
                            }
                        }
                        NvimEvent::BufferLines {
                            first,
                            last,
                            lines,
                            more,
                        } => {
                            if this.update_buffer(first, last, lines, more) && !more {
                                if this.view == View::Reading {
                                    this.compile_visuals(cx);
                                }
                                cx.notify();
                            }
                        }
                        NvimEvent::Error(error) => {
                            this.nvim_error = Some(error.into());
                            cx.notify();
                        }
                        NvimEvent::Warning(warning) => {
                            this.nvim_warning = Some(warning.into());
                            cx.notify();
                        }
                        NvimEvent::CloseRefused(warning) => {
                            this.pending_close = None;
                            this.nvim_warning = Some(warning.into());
                            this.view = View::Source;
                            cx.notify();
                        }
                        NvimEvent::Exited => {
                            this.nvim = None;
                            if let Some(action) = this.pending_close.take() {
                                this.request_close(action, cx);
                            } else {
                                this.nvim_error =
                                    Some("Neovim 已退出；按 Enter 可重新打开源码视图".into());
                                this.view = View::Reading;
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
                if let Some(fragment) = fragment
                    && !self.reveal_fragment(&fragment)
                {
                    self.show_notice(format!("找不到标题或块：{fragment}"), true, cx);
                }
                self.compile_visuals(cx);
                self.start_nvim(cx);
                cx.notify();
            }
            PendingClose::Quit => cx.quit(),
            PendingClose::Restart => cx.restart(),
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
        next.nvim_size = self.nvim_size.clone();
        next.cell_size = self.cell_size;
        next.settings_open = self.settings_open;
        next.auto_update = self.auto_update;
        next.update_status = std::mem::replace(&mut self.update_status, UpdateStatus::Idle);
        next.available_update = self.available_update.take();
        *self = next;
    }

    /// Move the reading cursor to a heading (`#Heading`) or block reference (`#^id`).
    fn reveal_fragment(&mut self, fragment: &str) -> bool {
        let Some(document) = &self.document else {
            return false;
        };
        let target = if let Some(id) = fragment.strip_prefix('^') {
            let marker = format!("^{id}");
            document.markdown.blocks.iter().position(|block| {
                block
                    .text
                    .split_whitespace()
                    .last()
                    .is_some_and(|word| word == marker)
            })
        } else {
            let wanted = heading_key(&crate::vault::percent_decode(fragment));
            document.markdown.blocks.iter().position(|block| {
                matches!(block.kind, BlockKind::Heading(_)) && heading_key(&block.text) == wanted
            })
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
            let Ok(Ok(Some(mut paths))) = selected.await else {
                return;
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
        let strict_line_breaks = self
            .vault
            .as_ref()
            .is_some_and(|vault| vault.settings.strict_line_breaks);
        let Some(document) = &mut self.document else {
            return false;
        };
        let end = last.unwrap_or(document.lines.len());
        if first > end || end > document.lines.len() {
            return false;
        }
        document.lines.splice(first..end, replacement);
        if !more {
            document.markdown =
                crate::markdown::parse_with_options(&document.lines.join("\n"), strict_line_breaks);
            self.clamp_reading_cursor();
        }
        true
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

    fn execute_search(&mut self, prompt: &SearchPrompt, reverse: bool) {
        let Some(blocks) = self
            .document
            .as_ref()
            .map(|document| &document.markdown.blocks)
        else {
            return;
        };
        if let Some(cursor) = search_cursor(
            blocks,
            self.reading_cursor,
            &prompt.query,
            prompt.forward != reverse,
        ) {
            self.reading_cursor = cursor;
            self.reading_column = None;
        }
    }

    fn search_word(&mut self, forward: bool) {
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
        self.execute_search(&prompt, false);
        self.last_search = Some(prompt);
    }

    fn reveal_reading_cursor(&self) {
        self.reading_scroll
            .scroll_to_item(self.reading_cursor.block);
    }

    fn scroll_reading(&mut self, down: bool, full_page: bool) {
        let height = self.reading_scroll.bounds().size.height;
        if height <= px(0.0) {
            return;
        }
        let distance = height * if full_page { 1.0 } else { 0.5 };
        let offset = self.reading_scroll.offset();
        let maximum = self.reading_scroll.max_offset().y;
        let y = (offset.y + if down { -distance } else { distance }).clamp(-maximum, px(0.0));
        self.reading_scroll.set_offset(point(offset.x, y));

        let visible = self
            .reading_scroll
            .bottom_item()
            .saturating_sub(self.reading_scroll.top_item())
            + 1;
        let lines = if full_page {
            visible
        } else {
            visible.div_ceil(2)
        };
        for _ in 0..lines {
            self.move_reading_line(down);
        }
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

    fn open_internal_link(&mut self, cx: &mut Context<Self>) {
        let Some(destination) = self.current_link() else {
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
            self.show_notice(format!("找不到或无法唯一确定链接：{target}"), true, cx);
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
        let Some(destination) = self.current_link() else {
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

        let target = if down {
            ((line + 1)..block_line_count(current))
                .find_map(|line| cursor_for_line(current, self.reading_cursor.block, line, column))
                .or_else(|| {
                    blocks
                        .iter()
                        .enumerate()
                        .skip(self.reading_cursor.block + 1)
                        .find_map(|(block, value)| cursor_for_line(value, block, 0, column))
                })
        } else {
            (0..line)
                .rev()
                .find_map(|line| cursor_for_line(current, self.reading_cursor.block, line, column))
                .or_else(|| {
                    blocks
                        .iter()
                        .enumerate()
                        .take(self.reading_cursor.block)
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
        }
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
        if self.view == View::Reading {
            if self.notice.take().is_some() {
                cx.notify();
            }
            let key = event.keystroke.key_char.as_deref();
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
                            self.execute_search(&prompt, false);
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
                for _ in 0..count {
                    self.scroll_reading(down, full_page);
                }
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
                    self.execute_search(&search, key == Some("N"));
                    self.reveal_reading_cursor();
                    cx.notify();
                }
                return;
            }
            if key == Some("*") || key == Some("#") {
                self.search_word(key == Some("*"));
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
        if event.keystroke.key == "escape"
            && (self.grid.is_normal() || self.nvim_error.is_some() || self.nvim.is_none())
        {
            self.view = View::Reading;
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
        if let Some(error) = &self.nvim_error {
            return div()
                .flex_1()
                .p_8()
                .text_color(rgb(0xffa7b2))
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
                    .bg(rgb(0x7a3b20))
                    .text_color(rgb(0xffffff))
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
                .bg(rgb(0x4a3518))
                .text_color(rgb(0xffd38a))
                .text_sm()
                .child(div().flex_1().child(warning))
                .child(
                    div()
                        .id("dismiss-nvim-warning")
                        .px_2()
                        .rounded_md()
                        .cursor_pointer()
                        .hover(|element| element.bg(rgb(0x5c4420)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nvim_warning = None;
                            cx.notify();
                        }))
                        .child("×"),
                )
        });

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(rgb(background))
            .children(warning)
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
                    .children(layers)
                    .when_some(focus, |element, focus| {
                        element.track_focus(&focus).child(
                            canvas(
                                move |bounds, _, _| {
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

    fn render_status_bar(&self, search_prompt: Option<String>) -> AnyElement {
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
            if let Some(find) = self.reading_find {
                pending.push(match (find.forward, find.till) {
                    (true, false) => 'f',
                    (false, false) => 'F',
                    (true, true) => 't',
                    (false, true) => 'T',
                });
            }
        }
        let hint = match self.view {
            View::Reading if self.document.is_some() => "Enter 编辑",
            View::Source if self.grid.is_normal() || self.nvim.is_none() => "Esc 返回阅读",
            _ => "",
        };
        let message = if let Some(prompt) = search_prompt {
            div()
                .flex_1()
                .min_w_0()
                .font_family(crate::fonts::mono())
                .text_color(rgb(0xe6e9ed))
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
                        .text_color(rgb(if notice.error { 0xffa7b2 } else { 0xa4e4c7 }))
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
            .border_color(rgb(0x2a3038))
            .bg(rgb(0x0c0f12))
            .text_xs()
            .text_color(rgb(0x98a2ad))
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .rounded_md()
                    .bg(rgb(if self.view == View::Source {
                        0x2b3a4a
                    } else {
                        0x3a2a20
                    }))
                    .text_color(rgb(if self.view == View::Source {
                        0x9ecbff
                    } else {
                        0xffb07a
                    }))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(mode),
            )
            .when(!pending.is_empty(), |element| {
                element.child(
                    div()
                        .flex_none()
                        .font_family(crate::fonts::mono())
                        .text_color(rgb(0xe6e9ed))
                        .child(pending),
                )
            })
            .child(message)
            .child(div().flex_none().child(hint))
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
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
        let status_color = if failed { 0xffa7a7 } else { 0xc7cbd1 };
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
                .bg(rgb(if busy { 0x34383e } else { 0xf06a24 }))
                .text_color(rgb(if busy { 0x8b929a } else { 0x1a0d07 }))
                .font_weight(FontWeight::SEMIBOLD)
                .when(!busy, |element| {
                    element
                        .cursor_pointer()
                        .hover(|element| element.bg(rgb(0xff7a32)))
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
                .bg(rgb(if packaged { 0xf06a24 } else { 0x34383e }))
                .text_color(rgb(if packaged { 0x1a0d07 } else { 0x8b929a }))
                .font_weight(FontWeight::SEMIBOLD)
                .when(packaged, |element| {
                    element
                        .cursor_pointer()
                        .hover(|element| element.bg(rgb(0xff7a32)))
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
                .bg(rgb(0xf06a24))
                .text_color(rgb(0x1a0d07))
                .font_weight(FontWeight::SEMIBOLD)
                .hover(|element| element.bg(rgb(0xff7a32)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.request_close(PendingClose::Restart, cx);
                }))
                .child("重启应用")
        });
        let auto_toggle = div()
            .w(px(44.0))
            .h(px(24.0))
            .p_1()
            .rounded_md()
            .flex()
            .items_center()
            .when(self.auto_update, |element| element.justify_end())
            .bg(rgb(if self.auto_update { 0xf06a24 } else { 0x3a4047 }))
            .child(
                div()
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded_full()
                    .bg(rgb(0xf4f1e8)),
            );

        div()
            .absolute()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x080a0c).opacity(0.94))
            .child(
                div()
                    .w(px(620.0))
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x343a41))
                    .bg(rgb(0x15191e))
                    .flex()
                    .flex_col()
                    .child(
                        div()
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
                                            .bg(rgb(0xf06a24)),
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
                                                div().text_sm().text_color(rgb(0x8f969f)).child(
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
                                    .text_color(rgb(0x98a2ad))
                                    .hover(|element| element.bg(rgb(0x252a30)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_open = false;
                                        cx.notify();
                                    }))
                                    .child("完成  Esc"),
                            ),
                    )
                    .child(
                        div()
                            .border_t_1()
                            .border_color(rgb(0x30363d))
                            .p_6()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xf28c45))
                                    .child("软件更新"),
                            )
                            .child(
                                div()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(rgb(0x343a41))
                                    .bg(rgb(0x1b2026))
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
                                    .child(div().h(px(1.0)).bg(rgb(0x30363d)))
                                    .child(
                                        div()
                                            .id("toggle-auto-update")
                                            .p_4()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .cursor_pointer()
                                            .hover(|element| element.bg(rgb(0x20262d)))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_auto_update(cx)
                                            }))
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
                                                            .text_color(rgb(0x8f969f))
                                                            .child(
                                                                "启动时检查，验证签名后自动安装",
                                                            ),
                                                    ),
                                            )
                                            .child(auto_toggle),
                                    ),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x717983))
                                    .child("更新仅访问 GitHub Release；笔记内容不会离开本机。"),
                            ),
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
        (self.view == View::Source && self.grid.accepts_text_input())
            || (self.view == View::Reading && self.reading_search.is_some())
    }
}

impl Render for RusidianApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .document
            .as_ref()
            .map(|document| document.name.clone())
            .unwrap_or_else(|| "Rusidian".into());
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

        let text_baseline = f32::from(
            cx.text_system().baseline_offset(
                cx.text_system()
                    .resolve_font(&gpui::font(crate::fonts::ui())),
                px(FONT_SIZE),
                px(24.0),
            ),
        );
        let reading = if let Some(document) = &self.document {
            let links = Links {
                note: &document.file,
                vault: self.vault.as_ref(),
            };
            let selection = self.reading_selection.and_then(|selection| {
                selection_bounds(&document.markdown.blocks, selection, self.reading_cursor)
            });
            div()
                .flex_1()
                .flex()
                .flex_col()
                .id("document")
                .relative()
                .overflow_y_scroll()
                .p_8()
                .text_base()
                .track_scroll(&self.reading_scroll)
                .children(
                    document
                        .markdown
                        .blocks
                        .iter()
                        .enumerate()
                        .map(|(index, block)| {
                            // Table rows are separate blocks; space the table as a whole.
                            let table_end = matches!(block.kind, BlockKind::Table { .. })
                                && !matches!(
                                    document
                                        .markdown
                                        .blocks
                                        .get(index + 1)
                                        .map(|next| &next.kind),
                                    Some(BlockKind::Table { header: false })
                                );
                            div()
                                .id(("block", index))
                                .mx_auto()
                                .w_full()
                                .max_w(px(820.0))
                                .when(table_end, |element| element.mb_4())
                                .child(render_block(
                                    block,
                                    self.tikz.get(&block.text),
                                    &self.math,
                                    text_baseline,
                                    links,
                                    (reading_cursor.block == index)
                                        .then_some(reading_cursor.offset),
                                    selection.and_then(|bounds| {
                                        selection_for_block(bounds, index, block_len(block))
                                    }),
                                ))
                        }),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(div().text_2xl().child("本地 Markdown，真实 Neovim"))
                .child(
                    div().text_sm().text_color(rgb(0x98a2ad)).child(
                        self.error
                            .clone()
                            .unwrap_or_else(|| "运行 rusidian path/to/note.md".into()),
                    ),
                )
                .into_any_element()
        };

        let body = if self.view == View::Source {
            self.render_source(window, cx)
        } else {
            reading
        };
        let body = if let Some(vault) = &self.vault {
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
                        .border_color(rgb(0x2a3038))
                        .bg(rgb(0x0c0f12))
                        .child(
                            div()
                                .h(px(40.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .id("vault-files")
                                .overflow_y_scroll()
                                .children(vault.files.iter().enumerate().map(|(index, path)| {
                                    let selected = current == Some(path.as_path());
                                    let file = path.clone();
                                    let vault_root = root.clone();
                                    let label: SharedString = path
                                        .strip_prefix(&root)
                                        .unwrap_or(path)
                                        .to_string_lossy()
                                        .into_owned()
                                        .into();
                                    div()
                                        .id(("vault-file", index))
                                        .px_3()
                                        .py_2()
                                        .text_sm()
                                        .cursor_pointer()
                                        .when(selected, |element| {
                                            element.bg(rgb(0x452519)).text_color(rgb(0xffb07a))
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.request_close(
                                                PendingClose::Open {
                                                    path: file.clone(),
                                                    vault_root: Some(vault_root.clone()),
                                                    fragment: None,
                                                },
                                                cx,
                                            );
                                        }))
                                        .child(label)
                                })),
                        ),
                )
                .child(body)
                .into_any_element()
        } else {
            body
        };

        div()
            .key_context(if self.view == View::Source {
                "Source"
            } else if self.reading_search.is_some() {
                "ReadingSearch"
            } else {
                "Reading"
            })
            .on_action(cx.listener(Self::choose_file))
            .on_action(cx.listener(Self::choose_folder))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::enter_source_normal))
            .on_key_down(cx.listener(Self::key_down))
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .font_family(crate::fonts::ui())
            .bg(rgb(0x111418))
            .text_color(rgb(0xe6e9ed))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(52.0))
                    .px_5()
                    .border_b_1()
                    .border_color(rgb(0x2a3038))
                    .child(div().text_lg().child(title))
                    .child(
                        div()
                            .max_w(px(620.0))
                            .text_ellipsis()
                            .text_sm()
                            .text_color(rgb(0x98a2ad))
                            .child(
                                self.document
                                    .as_ref()
                                    .map(|document| document.path.clone())
                                    .unwrap_or_else(|| "技术原型".into()),
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

fn render_block(
    block: &Block,
    tikz: Option<&TikzState>,
    math: &HashMap<(String, bool), MathState>,
    text_baseline: f32,
    links: Links,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
) -> AnyElement {
    let object_cursor = (cursor.is_some() || selection.is_some()) && is_object(block);
    let text = styled_fragment(block, 0..block.text.len(), cursor, selection);

    let content = match &block.kind {
        BlockKind::Heading(level) => div()
            .mb_4()
            .text_size(px(match level {
                1 => 32.0,
                2 => 27.0,
                3 => 23.0,
                _ => 19.0,
            }))
            .font_weight(FontWeight::SEMIBOLD)
            .child(text)
            .into_any_element(),
        BlockKind::Paragraph if !block.images.is_empty() => {
            render_inline_paragraph(block, math, text_baseline, links, cursor, selection)
        }
        BlockKind::Paragraph if !block.maths.is_empty() => {
            render_inline_paragraph(block, math, text_baseline, links, cursor, selection)
        }
        BlockKind::Paragraph => div().mb_4().child(text).into_any_element(),
        BlockKind::Image(source) => {
            let source_label = source.clone();
            let alt = block.text.clone();
            let Some(path) = links.image(source) else {
                return decorate_block(
                    block,
                    div()
                        .mb_4()
                        .p_4()
                        .rounded_md()
                        .when(object_cursor, |element| {
                            element.border_2().border_color(rgb(0xf28c45))
                        })
                        .bg(rgb(0x1c2229))
                        .child(format!("![{alt}]({source_label})"))
                        .into_any_element(),
                );
            };
            div()
                .mb_4()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(0xf28c45))
                })
                .child(img(path).max_w_full().with_fallback(move || {
                    div()
                        .p_4()
                        .rounded_md()
                        .bg(rgb(0x3a1f24))
                        .text_color(rgb(0xffa7b2))
                        .child(format!("无法加载图片：{source_label}"))
                        .into_any_element()
                }))
                .into_any_element()
        }
        BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz") => match tikz {
            Some(TikzState::Ready(image)) => {
                let width = crate::tikz::png_size(image.bytes())
                    .map(|(width, _)| px(width as f32 / crate::tikz::PREVIEW_SCALE));
                div()
                    .mb_4()
                    .p_4()
                    .rounded_md()
                    .when(object_cursor, |element| {
                        element.border_2().border_color(rgb(0xf28c45))
                    })
                    .bg(rgb(0xffffff))
                    .child(
                        img(image.clone())
                            .max_w_full()
                            .when_some(width, |element, width| element.w(width)),
                    )
                    .into_any_element()
            }
            Some(TikzState::Failed(error)) => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(0xf28c45))
                })
                .bg(rgb(0x3a1f24))
                .text_color(rgb(0xffa7b2))
                .child(error.clone())
                .into_any_element(),
            _ => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(0xf28c45))
                })
                .bg(rgb(0x1c2229))
                .text_color(rgb(0x98a2ad))
                .child("正在编译 TikZ…")
                .into_any_element(),
        },
        BlockKind::Code(language) => div()
            .mb_4()
            .p_4()
            .rounded_md()
            .bg(rgb(0x1c2229))
            .font_family(crate::fonts::mono())
            .when_some(language.as_ref(), |element, language| {
                element.child(
                    div()
                        .mb_2()
                        .text_sm()
                        .text_color(rgb(0x98a2ad))
                        .child(language.clone()),
                )
            })
            .child(text)
            .into_any_element(),
        BlockKind::Html | BlockKind::Metadata => div()
            .mb_4()
            .p_4()
            .rounded_md()
            .bg(rgb(0x1c2229))
            .font_family(crate::fonts::mono())
            .child(text)
            .into_any_element(),
        BlockKind::Rule => div()
            .my_4()
            .h(px(1.0))
            .w_full()
            .bg(rgb(0x3a424d))
            .into_any_element(),
        BlockKind::Table { header } => div()
            .flex()
            .w_full()
            .when(*header, |element| element.bg(rgb(0x1a1f25)))
            .children(block.cells.iter().enumerate().map(|(index, range)| {
                div()
                    .flex_1()
                    .min_w_0()
                    .p_2()
                    .border_1()
                    .border_color(rgb(0x3a424d))
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
                    .child(styled_fragment(block, range.clone(), cursor, selection))
            }))
            .into_any_element(),
        BlockKind::Math => match math.get(&(block.text.clone(), true)) {
            Some(MathState::Ready(formula)) => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .bg(rgb(0x1c2229))
                .id("display-math")
                .overflow_x_scroll()
                .child(formula.element(FONT_SIZE))
                .into_any_element(),
            Some(MathState::Failed(error)) => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .bg(rgb(0x3a1f24))
                .text_color(rgb(0xffa7b2))
                .child(format!("$${}$$：{}", block.text, error))
                .into_any_element(),
            _ => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .bg(rgb(0x1c2229))
                .text_color(rgb(0x98a2ad))
                .child("正在排版公式…")
                .into_any_element(),
        },
        BlockKind::Footnote(label) => div()
            .mb_3()
            .flex()
            .text_sm()
            .child(
                div()
                    .mr_2()
                    .text_color(rgb(0xf28c45))
                    .child(format!("[^{label}]")),
            )
            .child(text)
            .into_any_element(),
        BlockKind::DefinitionTitle => div()
            .mt_3()
            .font_weight(FontWeight::BOLD)
            .child(text)
            .into_any_element(),
        BlockKind::Definition => div().mb_3().ml_6().child(text).into_any_element(),
    };
    decorate_block(block, content)
}

fn decorate_block(block: &Block, content: AnyElement) -> AnyElement {
    let marker: Option<SharedString> = if let Some(checked) = block.task {
        Some(if checked { "☑" } else { "☐" }.into())
    } else {
        block.list_marker.clone().map(Into::into)
    };
    let content = if let Some(marker) = marker {
        div()
            .flex()
            .ml(px(block.list_depth as f32 * 24.0))
            .child(div().w(px(30.0)).flex_none().child(marker))
            .child(div().flex_1().min_w_0().child(content))
            .into_any_element()
    } else {
        content
    };
    if block.quote_depth > 0 {
        div()
            .pl(px(12.0 * block.quote_depth as f32))
            .border_l_2()
            .border_color(rgb(0x56606d))
            .text_color(rgb(0xb8c0cc))
            .child(content)
            .into_any_element()
    } else {
        content
    }
}

fn styled_fragment(
    block: &Block,
    range: std::ops::Range<usize>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
) -> StyledText {
    let highlights = fragment_highlights(block, &range, cursor, selection);
    StyledText::new(block.text[range].to_owned()).with_highlights(highlights)
}

fn fragment_highlights(
    block: &Block,
    range: &std::ops::Range<usize>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
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
                        background_color: span.code.then_some(rgb(0x242a32).into()),
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
            (
                range,
                HighlightStyle {
                    color: Some(rgb(0xf28c45).into()),
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
                background_color: Some(rgb(0x8a3b1c).into()),
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
                color: Some(rgb(0x111418).into()),
                background_color: Some(rgb(0xe6e9ed).into()),
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
    let mut ascent = if block.images.is_empty() {
        baseline
    } else {
        baseline.max(24.0)
    };
    let mut descent = 24.0 - baseline;
    for formula in &block.maths {
        if let Some(MathState::Ready(formula)) = math.get(&(formula.source.clone(), false)) {
            ascent = ascent.max(formula.ascent(FONT_SIZE) + 1.0);
            descent = descent.max(formula.descent(FONT_SIZE) + 1.0);
        }
    }
    (ascent, descent)
}

fn push_inline_text(
    children: &mut Vec<AnyElement>,
    block: &Block,
    range: std::ops::Range<usize>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    top: f32,
    height: f32,
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
    block: &Block,
    math: &HashMap<(String, bool), MathState>,
    text_baseline: f32,
    links: Links,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
) -> AnyElement {
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
                &mut children,
                block,
                start..range.start,
                cursor,
                selection,
                ascent - text_baseline,
                row_height,
            );
        }
        let offset = block.text[..range.start]
            .chars()
            .filter(|character| *character != '\n')
            .count();
        let active = cursor == Some(offset)
            || selection.is_some_and(|(from, to)| from <= offset && offset <= to);
        let child = match atom {
            InlineAtom::Image(image) => {
                let alt = image.alt.clone();
                let source = image.source.clone();
                if let Some(path) = links.image(&image.source) {
                    div()
                        .pt(px(ascent - 24.0))
                        .child(
                            img(path)
                                .h(px(24.0))
                                .max_w_full()
                                .with_fallback(move || div().child(alt.clone()).into_any_element()),
                        )
                        .into_any_element()
                } else {
                    div()
                        .px_1()
                        .bg(rgb(0x242a32))
                        .child(format!("![{alt}]({source})"))
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
                    .bg(rgb(0x3a1f24))
                    .text_color(rgb(0xffa7b2))
                    .child(format!("${}$：{}", formula.source, error))
                    .into_any_element(),
                _ => div()
                    .px_1()
                    .bg(rgb(0x242a32))
                    .child(format!("${}$", formula.source))
                    .into_any_element(),
            },
        };
        children.push(
            div()
                .flex_none()
                .h(px(row_height))
                .when(active, |element| element.bg(rgb(0x3c2a22)))
                .child(child)
                .into_any_element(),
        );
        start = range.end;
    }
    if start < block.text.len() {
        push_inline_text(
            &mut children,
            block,
            start..block.text.len(),
            cursor,
            selection,
            ascent - text_baseline,
            row_height,
        );
    }
    div()
        .mb_4()
        .flex()
        .flex_wrap()
        .items_start()
        .line_height(px(24.0))
        .children(children)
        .into_any_element()
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

fn display_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Whether a linked file can be edited as text: valid UTF-8 without NUL bytes near the start.
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
            return (length > 0).then_some(ReadingCursor {
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

fn search_cursor(
    blocks: &[Block],
    start: ReadingCursor,
    query: &str,
    forward: bool,
) -> Option<ReadingCursor> {
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
            return None;
        }
        if matches_query(blocks, cursor, &query) {
            return Some(cursor);
        }
        candidate = step_cursor(blocks, cursor, forward);
    }
}

fn matches_query(blocks: &[Block], start: ReadingCursor, query: &[char]) -> bool {
    let mut cursor = start;
    for (index, expected) in query.iter().enumerate() {
        if cursor_character(blocks, cursor) != Some(*expected) {
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
    use gpui::Modifiers;

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
    fn reveals_heading_and_block_fragments() {
        let mut app = RusidianApp::open(Some(Path::new("examples/tikz.md")));
        app.document.as_mut().unwrap().markdown =
            crate::markdown::parse("# Intro\n\ntext\n\n## Second Part\n\nquoted line ^abc123\n");
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
        assert_eq!(heading_key("Hello, World!"), heading_key("hello-world"));
        assert!(is_text_file(Path::new("README.md")));
        assert!(!is_text_file(Path::new("assets/app-icon.png")));
        assert!(is_external_link("https://example.com") && !is_external_link("note.md"));
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
            Some(ReadingCursor {
                block: 0,
                offset: 10
            })
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
            Some(ReadingCursor::default())
        );
        assert!(search_cursor(blocks, ReadingCursor::default(), "betaalpha", true).is_none());
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
        let highlights = fragment_highlights(block, &(0..block.text.len()), Some(3), Some((2, 4)));
        for pair in highlights.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "{highlights:?}");
        }
        let cursor = highlights
            .iter()
            .find(|(_, style)| style.color == Some(rgb(0x111418).into()))
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
                    for (highlight, _) in
                        fragment_highlights(block, &range, Some(cursor), Some((0, cursor)))
                    {
                        assert!(fragment.is_char_boundary(highlight.start));
                        assert!(fragment.is_char_boundary(highlight.end));
                    }
                }
            }
        }
    }
}
