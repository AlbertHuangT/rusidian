use crate::markdown::{Block, BlockKind, MarkdownDocument};
use crate::nvim::{Client as NvimClient, Event as NvimEvent, Grid as NvimGrid};
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, FontStyle, FontWeight, HighlightStyle, Image, ImageFormat, KeyBinding,
    KeyDownEvent, Keystroke, Menu, MenuItem, PathPromptOptions, Pixels, Point, SharedString,
    StyledText, UTF16Selection, Window, WindowBounds, WindowOptions, actions, canvas, div, img,
    point, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

const WINDOW_WIDTH: f32 = 960.0;
const WINDOW_HEIGHT: f32 = 640.0;

actions!(rusidian, [OpenFile, Quit, EnterSourceNormal]);

pub fn run(initial_path: Option<PathBuf>) {
    application().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenFile, None),
            KeyBinding::new("enter", EnterSourceNormal, Some("Reading")),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus([
            Menu::new("Rusidian").items([MenuItem::action("退出 Rusidian", Quit)]),
            Menu::new("文件").items([MenuItem::action("打开文件…", OpenFile)]),
        ]);

        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let app = cx.new(|cx| {
                    let mut app = RusidianApp::open(initial_path.as_deref());
                    app.focus_handle = Some(cx.focus_handle());
                    app.compile_tikz(cx);
                    app.start_nvim(cx);
                    app
                });
                let focus = app.read(cx).focus_handle.clone();
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
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
    error: Option<SharedString>,
    tikz: HashMap<usize, TikzState>,
    view: View,
    nvim: Option<NvimClient>,
    grid: NvimGrid,
    nvim_error: Option<SharedString>,
    nvim_warning: Option<SharedString>,
    nvim_size: (i64, i64),
    reading_cursor: ReadingCursor,
    reading_column: Option<usize>,
    reading_pending_g: bool,
    reading_count: Option<usize>,
    reading_find: Option<FindPending>,
    reading_selection: Option<ReadingSelection>,
    focus_handle: Option<FocusHandle>,
    marked_text: String,
    marked_selection: std::ops::Range<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Reading,
    Source,
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

#[derive(Clone, Copy)]
enum WordMotion {
    Next,
    Previous,
    End,
}

enum TikzState {
    Loading,
    Ready(Arc<Image>),
    Failed(SharedString),
}

impl RusidianApp {
    fn open(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return Self {
                document: None,
                error: None,
                tikz: HashMap::new(),
                view: View::Reading,
                nvim: None,
                grid: NvimGrid::default(),
                nvim_error: None,
                nvim_warning: None,
                nvim_size: (120, 40),
                reading_cursor: ReadingCursor::default(),
                reading_column: None,
                reading_pending_g: false,
                reading_count: None,
                reading_find: None,
                reading_selection: None,
                focus_handle: None,
                marked_text: String::new(),
                marked_selection: 0..0,
            };
        };

        match std::fs::read_to_string(path) {
            Ok(content) => {
                let lines = source_lines(&content);
                Self {
                    document: Some(Document {
                        file: path.to_path_buf(),
                        name: path
                            .file_name()
                            .unwrap_or(path.as_os_str())
                            .to_string_lossy()
                            .into_owned()
                            .into(),
                        path: path.to_string_lossy().into_owned().into(),
                        lines,
                        markdown: crate::markdown::parse(&content),
                    }),
                    error: None,
                    tikz: HashMap::new(),
                    view: View::Reading,
                    nvim: None,
                    grid: NvimGrid::default(),
                    nvim_error: None,
                    nvim_warning: None,
                    nvim_size: (120, 40),
                    reading_cursor: ReadingCursor::default(),
                    reading_column: None,
                    reading_pending_g: false,
                    reading_count: None,
                    reading_find: None,
                    reading_selection: None,
                    focus_handle: None,
                    marked_text: String::new(),
                    marked_selection: 0..0,
                }
            }
            Err(error) => Self {
                document: None,
                error: Some(format!("无法打开 {}：{error}", path.display()).into()),
                tikz: HashMap::new(),
                view: View::Reading,
                nvim: None,
                grid: NvimGrid::default(),
                nvim_error: None,
                nvim_warning: None,
                nvim_size: (120, 40),
                reading_cursor: ReadingCursor::default(),
                reading_column: None,
                reading_pending_g: false,
                reading_count: None,
                reading_find: None,
                reading_selection: None,
                focus_handle: None,
                marked_text: String::new(),
                marked_selection: 0..0,
            },
        }
    }

    fn compile_tikz(&mut self, cx: &mut Context<Self>) {
        let Some(document) = &self.document else {
            return;
        };

        let jobs = document
            .markdown
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, block)| match &block.kind {
                BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz") => {
                    Some((index, block.text.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        for (index, source) in jobs {
            self.tikz.insert(index, TikzState::Loading);
            let expected = source.clone();
            let executor = cx.background_executor().clone();
            cx.spawn(async move |this, cx| {
                let result = executor
                    .spawn(async move { crate::tikz::compile(&source) })
                    .await;
                this.update(cx, |this, cx| {
                    let is_current = this
                        .document
                        .as_ref()
                        .and_then(|document| document.markdown.blocks.get(index))
                        .is_some_and(|block| block.text == expected);
                    if !is_current {
                        return;
                    }

                    let state = match result {
                        Ok(bytes) => {
                            TikzState::Ready(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))
                        }
                        Err(error) => TikzState::Failed(error.into()),
                    };
                    this.tikz.insert(index, state);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    fn start_nvim(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.document.as_ref().map(|document| document.file.clone()) else {
            return;
        };
        let client = NvimClient::start(path, false);
        let events = client.events.clone();
        self.nvim = Some(client);

        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let result = this.update(cx, |this, cx| match event {
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
                            this.tikz.clear();
                            if this.view == View::Reading {
                                this.compile_tikz(cx);
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
                });
                if result.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn choose_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("打开".into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(mut paths))) = selected.await else {
                return;
            };
            let Some(path) = paths.pop() else {
                return;
            };

            this.update_in(cx, |this, _, cx| {
                let focus_handle = this.focus_handle.clone();
                *this = Self::open(Some(&path));
                this.focus_handle = focus_handle;
                this.compile_tikz(cx);
                this.start_nvim(cx);
                cx.notify();
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
            document.markdown = crate::markdown::parse(&document.lines.join("\n"));
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

    fn take_reading_count(&mut self) -> usize {
        self.reading_count.take().unwrap_or(1)
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
        if self.view == View::Reading {
            let key = event.keystroke.key_char.as_deref();
            if event.keystroke.key == "escape" {
                self.reading_find = None;
                self.reading_pending_g = false;
                self.reading_count = None;
                if self.reading_selection.take().is_some() {
                    cx.notify();
                }
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
                    && let Some(blocks) = self
                        .document
                        .as_ref()
                        .map(|document| &document.markdown.blocks)
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(selection_text(
                        blocks,
                        selection,
                        self.reading_cursor,
                    )));
                    cx.notify();
                }
                self.reading_count = None;
                return;
            }
            if let Some(digit) = key.and_then(|key| key.parse::<usize>().ok())
                && (digit != 0 || self.reading_count.is_some())
            {
                self.reading_count = Some(append_count(self.reading_count.unwrap_or(0), digit));
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
            if key == Some("g") {
                if self.reading_pending_g {
                    self.move_reading_document_edge(false);
                    let count = self.take_reading_count();
                    for _ in 1..count {
                        self.move_reading_line(true);
                    }
                    self.reading_pending_g = false;
                    cx.notify();
                } else {
                    self.reading_pending_g = true;
                }
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
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape" && self.grid.is_normal() {
            self.view = View::Reading;
            self.compile_tikz(cx);
            cx.notify();
        } else if self.grid.accepts_text_input()
            && event.keystroke.key_char.is_some()
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.platform
        {
            // Printable text is committed through EntityInputHandler so IME composition is not duplicated.
        } else if let Some(nvim) = &self.nvim {
            nvim.input(nvim_key(&event.keystroke));
        }
    }

    fn render_source(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(error) = &self.nvim_error {
            return div()
                .flex_1()
                .p_8()
                .text_color(rgb(0xffa7b2))
                .child(error.clone())
                .into_any_element();
        }

        let view = cx.entity();
        let focus = self.focus_handle.clone();
        let marked_text = self.marked_text.clone();
        let warning = self.nvim_warning.clone();
        let (foreground, background) = self.grid.colors();

        div()
            .flex_1()
            .id("nvim-grid")
            .relative()
            .overflow_scroll()
            .p_4()
            .bg(rgb(background.unwrap_or(0x0c0f12)))
            .text_color(rgb(foreground.unwrap_or(0xe6e9ed)))
            .font_family("SFMono-Regular")
            .text_sm()
            .when_some(warning, |element, warning| {
                element.child(
                    div()
                        .mb_3()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(0x4a3518))
                        .text_color(rgb(0xffd38a))
                        .child(warning),
                )
            })
            .children(self.grid.styled_lines().map(|(mut line, styles, cursor)| {
                let mut highlights = styles
                    .into_iter()
                    .map(|(range, style)| {
                        (
                            range,
                            HighlightStyle {
                                color: style.foreground.map(|color| rgb(color).into()),
                                background_color: style.background.map(|color| rgb(color).into()),
                                font_weight: style.bold.then_some(FontWeight::BOLD),
                                font_style: style.italic.then_some(FontStyle::Italic),
                                ..Default::default()
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                if let Some(range) = cursor {
                    if marked_text.is_empty() {
                        highlights.push((
                            range,
                            HighlightStyle {
                                color: Some(rgb(background.unwrap_or(0x0c0f12)).into()),
                                background_color: Some(rgb(foreground.unwrap_or(0xe6e9ed)).into()),
                                ..Default::default()
                            },
                        ));
                    } else {
                        let start = range.start;
                        line.insert_str(start, &marked_text);
                        highlights.push((
                            start..start + marked_text.len(),
                            HighlightStyle {
                                background_color: Some(rgb(0x284d75).into()),
                                ..Default::default()
                            },
                        ));
                    }
                }
                let text = StyledText::new(line).with_highlights(highlights);
                div().whitespace_nowrap().child(text)
            }))
            .when_some(focus, |element, focus| {
                element.track_focus(&focus).child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, cx| {
                            window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx);
                        },
                    )
                    .absolute()
                    .size_full(),
                )
            })
            .into_any_element()
    }

    fn resize_nvim(&mut self, window: &Window) {
        let viewport = window.viewport_size();
        let width = ((f32::from(viewport.width) - 32.0) / 8.0).floor().max(20.0) as i64;
        let height = ((f32::from(viewport.height) - 84.0) / 18.0)
            .floor()
            .max(8.0) as i64;
        let size = (width, height);
        if size != self.nvim_size {
            self.nvim_size = size;
            if let Some(nvim) = &self.nvim {
                nvim.resize(width, height);
            }
        }
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
        if let Some(nvim) = &self.nvim {
            nvim.input(text);
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
        Some(Bounds::new(
            point(
                element_bounds.left() + px(16.0 + self.grid.cursor.1 as f32 * 8.0),
                element_bounds.top() + px(16.0 + self.grid.cursor.0 as f32 * 18.0),
            ),
            size(px(8.0), px(18.0)),
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
        self.view == View::Source && self.grid.accepts_text_input()
    }
}

impl Render for RusidianApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.view == View::Source {
            self.resize_nvim(window);
        }
        let title = self
            .document
            .as_ref()
            .map(|document| document.name.clone())
            .unwrap_or_else(|| "Rusidian".into());
        let reading_cursor = self.reading_cursor;

        let reading = if let Some(document) = &self.document {
            let selection = self.reading_selection.and_then(|selection| {
                selection_bounds(&document.markdown.blocks, selection, self.reading_cursor)
            });
            div()
                .flex_1()
                .id("document")
                .overflow_y_scroll()
                .p_8()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .mx_auto()
                        .w_full()
                        .max_w(px(820.0))
                        .text_base()
                        .children(document.markdown.blocks.iter().enumerate().map(
                            |(index, block)| {
                                render_block(
                                    block,
                                    self.tikz.get(&index),
                                    &document.file,
                                    (reading_cursor.block == index)
                                        .then_some(reading_cursor.offset),
                                    selection.and_then(|bounds| {
                                        selection_for_block(bounds, index, block_len(block))
                                    }),
                                )
                            },
                        )),
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
            self.render_source(cx)
        } else {
            reading
        };

        div()
            .key_context(if self.view == View::Source {
                "Source"
            } else {
                "Reading"
            })
            .on_action(cx.listener(Self::choose_file))
            .on_action(cx.listener(Self::enter_source_normal))
            .on_key_down(cx.listener(Self::key_down))
            .flex()
            .flex_col()
            .size_full()
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
    }
}

fn nvim_key(key: &Keystroke) -> String {
    let key_name = match key.key.as_str() {
        "enter" => "CR",
        "escape" => "Esc",
        "backspace" => "BS",
        "delete" => "Del",
        "tab" => "Tab",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "home" => "Home",
        "end" => "End",
        _ if !key.modifiers.modified() => {
            return key.key_char.clone().unwrap_or_else(|| key.key.clone());
        }
        other => other,
    };

    let mut modifiers = String::new();
    if key.modifiers.control {
        modifiers.push_str("C-");
    }
    if key.modifiers.alt {
        modifiers.push_str("M-");
    }
    if key.modifiers.shift {
        modifiers.push_str("S-");
    }
    if key.modifiers.platform {
        modifiers.push_str("D-");
    }
    format!("<{modifiers}{key_name}>")
}

fn source_lines(source: &str) -> Vec<String> {
    let lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn render_block(
    block: &Block,
    tikz: Option<&TikzState>,
    note: &Path,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
) -> AnyElement {
    let object_cursor = (cursor.is_some() || selection.is_some()) && is_object(block);
    let mut highlights = block
        .spans
        .iter()
        .map(|span| {
            (
                span.range.clone(),
                HighlightStyle {
                    font_weight: span.bold.then_some(FontWeight::BOLD),
                    font_style: span.italic.then_some(FontStyle::Italic),
                    background_color: span.code.then_some(rgb(0x242a32).into()),
                    ..Default::default()
                },
            )
        })
        .collect::<Vec<_>>();
    if !object_cursor
        && let Some((start, end)) = selection
        && let (Some(start), Some(end)) =
            (text_range(&block.text, start), text_range(&block.text, end))
    {
        highlights.push((
            start.start..end.end,
            HighlightStyle {
                background_color: Some(rgb(0x315b7d).into()),
                ..Default::default()
            },
        ));
    }
    if !object_cursor && let Some(range) = cursor.and_then(|offset| text_range(&block.text, offset))
    {
        highlights.push((
            range,
            HighlightStyle {
                color: Some(rgb(0x111418).into()),
                background_color: Some(rgb(0xe6e9ed).into()),
                ..Default::default()
            },
        ));
    }
    let text = StyledText::new(block.text.clone()).with_highlights(highlights);

    match &block.kind {
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
        BlockKind::Paragraph => div().mb_4().child(text).into_any_element(),
        BlockKind::Image(source) => {
            let source_label = source.clone();
            let alt = block.text.clone();
            let path = Path::new(source);
            if path.is_absolute() || source.contains("://") || source.starts_with("data:") {
                return div()
                    .mb_4()
                    .p_4()
                    .rounded_md()
                    .when(object_cursor, |element| {
                        element.border_2().border_color(rgb(0x88c0d0))
                    })
                    .bg(rgb(0x1c2229))
                    .child(format!("![{alt}]({source_label})"))
                    .into_any_element();
            }
            let path = note.parent().unwrap_or_else(|| Path::new(".")).join(path);
            div()
                .mb_4()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(0x88c0d0))
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
            Some(TikzState::Ready(image)) => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(0x88c0d0))
                })
                .bg(rgb(0xffffff))
                .child(img(image.clone()).max_w_full())
                .into_any_element(),
            Some(TikzState::Failed(error)) => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .when(object_cursor, |element| {
                    element.border_2().border_color(rgb(0x88c0d0))
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
                    element.border_2().border_color(rgb(0x88c0d0))
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
            .font_family("SFMono-Regular")
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
    }
}

fn is_object(block: &Block) -> bool {
    matches!(&block.kind, BlockKind::Image(_))
        || matches!(&block.kind, BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz"))
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
    loop {
        let Some(block) = blocks.get(position.block) else {
            break;
        };
        match &block.kind {
            BlockKind::Image(_) => output.push_str(&block.text),
            BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz") => {
                output.push_str("[TikZ]")
            }
            _ => {
                if let Some(character) = cursor_character(blocks, position) {
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
    fn opens_text_file_and_reports_missing_file() {
        let opened = RusidianApp::open(Some(Path::new("README.md")));
        assert!(!opened.document.unwrap().markdown.blocks.is_empty());

        let missing = RusidianApp::open(Some(Path::new("missing-rusidian-test-file.md")));
        assert!(missing.error.is_some());
    }

    #[test]
    fn translates_gpui_keys_for_neovim() {
        assert_eq!(nvim_key(&Keystroke::parse("a").unwrap()), "a");
        assert_eq!(nvim_key(&Keystroke::parse("ctrl-a").unwrap()), "<C-a>");
        assert_eq!(nvim_key(&Keystroke::parse("left").unwrap()), "<Left>");
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
    }
}
