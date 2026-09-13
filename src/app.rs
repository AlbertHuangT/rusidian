use crate::markdown::{Block, BlockKind, MarkdownDocument};
use crate::nvim::{Client as NvimClient, Event as NvimEvent, Grid as NvimGrid};
use gpui::{
    AnyElement, App, Bounds, Context, FontStyle, FontWeight, HighlightStyle, Image, ImageFormat,
    KeyBinding, KeyDownEvent, Keystroke, Menu, MenuItem, PathPromptOptions, SharedString,
    StyledText, Window, WindowBounds, WindowOptions, actions, div, img, prelude::*, px, rgb, size,
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
            |_, cx| {
                cx.new(|cx| {
                    let mut app = RusidianApp::open(initial_path.as_deref());
                    app.compile_tikz(cx);
                    app.start_nvim(cx);
                    app
                })
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
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Reading,
    Source,
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
            };
        };

        match std::fs::read_to_string(path) {
            Ok(content) => Self {
                document: Some(Document {
                    file: path.to_path_buf(),
                    name: path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                    path: path.to_string_lossy().into_owned().into(),
                    markdown: crate::markdown::parse(&content),
                }),
                error: None,
                tikz: HashMap::new(),
                view: View::Reading,
                nvim: None,
                grid: NvimGrid::default(),
                nvim_error: None,
            },
            Err(error) => Self {
                document: None,
                error: Some(format!("无法打开 {}：{error}", path.display()).into()),
                tikz: HashMap::new(),
                view: View::Reading,
                nvim: None,
                grid: NvimGrid::default(),
                nvim_error: None,
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
        let client = NvimClient::start(path);
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
                    NvimEvent::Error(error) => {
                        this.nvim_error = Some(error.into());
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
                *this = Self::open(Some(&path));
                this.compile_tikz(cx);
                this.start_nvim(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn enter_source_normal(
        &mut self,
        _: &EnterSourceNormal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.view = View::Source;
        if let Some(nvim) = &self.nvim {
            nvim.input("<Esc>");
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.view != View::Source {
            return;
        }
        if event.keystroke.key == "escape" && self.grid.is_normal() {
            self.view = View::Reading;
            cx.notify();
        } else if let Some(nvim) = &self.nvim {
            nvim.input(nvim_key(&event.keystroke));
        }
    }

    fn render_source(&self) -> AnyElement {
        if let Some(error) = &self.nvim_error {
            return div()
                .flex_1()
                .p_8()
                .text_color(rgb(0xffa7b2))
                .child(error.clone())
                .into_any_element();
        }

        div()
            .flex_1()
            .id("nvim-grid")
            .overflow_scroll()
            .p_4()
            .bg(rgb(0x0c0f12))
            .font_family("SFMono-Regular")
            .text_sm()
            .children(self.grid.lines().map(|(line, cursor)| {
                let text = StyledText::new(line).with_highlights(cursor.map(|range| {
                    (
                        range,
                        HighlightStyle {
                            color: Some(rgb(0x0c0f12).into()),
                            background_color: Some(rgb(0xe6e9ed).into()),
                            ..Default::default()
                        },
                    )
                }));
                div().whitespace_nowrap().child(text)
            }))
            .into_any_element()
    }
}

impl Render for RusidianApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .document
            .as_ref()
            .map(|document| document.name.clone())
            .unwrap_or_else(|| "Rusidian".into());

        let reading =
            if let Some(document) = &self.document {
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
                            .children(
                                document.markdown.blocks.iter().enumerate().map(
                                    |(index, block)| render_block(block, self.tikz.get(&index)),
                                ),
                            ),
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
            self.render_source()
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

fn render_block(block: &Block, tikz: Option<&TikzState>) -> AnyElement {
    let text =
        StyledText::new(block.text.clone()).with_highlights(block.spans.iter().map(|span| {
            (
                span.range.clone(),
                HighlightStyle {
                    font_weight: span.bold.then_some(FontWeight::BOLD),
                    font_style: span.italic.then_some(FontStyle::Italic),
                    ..Default::default()
                },
            )
        }));

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
        BlockKind::Code(Some(language)) if language.eq_ignore_ascii_case("tikz") => match tikz {
            Some(TikzState::Ready(image)) => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .bg(rgb(0xffffff))
                .child(img(image.clone()).max_w_full())
                .into_any_element(),
            Some(TikzState::Failed(error)) => div()
                .mb_4()
                .p_4()
                .rounded_md()
                .bg(rgb(0x3a1f24))
                .text_color(rgb(0xffa7b2))
                .child(error.clone())
                .into_any_element(),
            _ => div()
                .mb_4()
                .p_4()
                .rounded_md()
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
}
