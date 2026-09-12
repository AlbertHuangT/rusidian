use crate::markdown::{Block, BlockKind, MarkdownDocument};
use gpui::{
    AnyElement, App, Bounds, Context, FontStyle, FontWeight, HighlightStyle, Image, ImageFormat,
    KeyBinding, Menu, MenuItem, PathPromptOptions, SharedString, StyledText, Window, WindowBounds,
    WindowOptions, actions, div, img, prelude::*, px, rgb, size,
};
use gpui_platform::application;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

const WINDOW_WIDTH: f32 = 960.0;
const WINDOW_HEIGHT: f32 = 640.0;

actions!(rusidian, [OpenFile, Quit]);

pub fn run(initial_path: Option<PathBuf>) {
    application().run(move |cx: &mut App| {
        cx.bind_keys([KeyBinding::new("cmd-o", OpenFile, None)]);
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
                    app
                })
            },
        )
        .expect("failed to open Rusidian window");

        cx.activate(true);
    });
}

struct Document {
    name: SharedString,
    path: SharedString,
    markdown: MarkdownDocument,
}

struct RusidianApp {
    document: Option<Document>,
    error: Option<SharedString>,
    tikz: HashMap<usize, TikzState>,
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
            };
        };

        match std::fs::read_to_string(path) {
            Ok(content) => Self {
                document: Some(Document {
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
            },
            Err(error) => Self {
                document: None,
                error: Some(format!("无法打开 {}：{error}", path.display()).into()),
                tikz: HashMap::new(),
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
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

impl Render for RusidianApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .document
            .as_ref()
            .map(|document| document.name.clone())
            .unwrap_or_else(|| "Rusidian".into());

        let body =
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

        div()
            .key_context("Rusidian")
            .on_action(cx.listener(Self::choose_file))
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
}
