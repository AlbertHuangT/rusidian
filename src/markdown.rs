use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::ops::Range;

#[derive(Debug)]
pub struct MarkdownDocument {
    pub blocks: Vec<Block>,
}

#[derive(Debug, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub spans: Vec<Span>,
    pub images: Vec<InlineImage>,
    pub maths: Vec<InlineMath>,
    pub links: Vec<LinkSpan>,
    pub list_marker: Option<String>,
    pub list_depth: usize,
    pub task: Option<bool>,
    pub quote_depth: usize,
    pub cells: Vec<Range<usize>>,
    pub table_alignments: Vec<Alignment>,
}

#[derive(Debug, PartialEq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    Code(Option<String>),
    Image(String),
    Html,
    Metadata,
    Rule,
    Table { header: bool },
    Math,
    Footnote(String),
    DefinitionTitle,
    Definition,
}

#[derive(Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
}

#[derive(Debug, PartialEq)]
pub struct InlineImage {
    pub range: Range<usize>,
    pub source: String,
    pub alt: String,
}

#[derive(Debug, PartialEq)]
pub struct InlineMath {
    pub range: Range<usize>,
    pub source: String,
}

#[derive(Debug, PartialEq)]
pub struct LinkSpan {
    pub range: Range<usize>,
    pub destination: String,
}

struct PendingImage {
    source: String,
    alt: String,
}

struct ListState {
    next: Option<u64>,
}

struct ItemState {
    marker: String,
    depth: usize,
    used: bool,
}

pub fn parse(source: &str) -> MarkdownDocument {
    parse_with_options(source, false)
}

pub fn parse_with_options(source: &str, strict_line_breaks: bool) -> MarkdownDocument {
    let mut blocks = Vec::new();
    let mut current = None;
    let mut bold = 0;
    let mut italic = 0;
    let mut strike = 0;
    let mut image = None;
    let mut link = None;
    let mut quote_depth = 0;
    let mut lists: Vec<ListState> = Vec::new();
    let mut items: Vec<ItemState> = Vec::new();
    let mut cell_start = None;
    let mut table_alignments = Vec::new();
    let mut footnote: Option<String> = None;

    for event in Parser::new_ext(source, Options::all()) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph if current.is_none() => {
                    current = Some(new_block(
                        footnote.as_ref().map_or(BlockKind::Paragraph, |label| {
                            BlockKind::Footnote(label.clone())
                        }),
                        quote_depth,
                        &mut items,
                    ));
                }
                Tag::Heading { level, .. } => {
                    current = Some(new_block(
                        BlockKind::Heading(heading_level(level)),
                        quote_depth,
                        &mut items,
                    ));
                }
                Tag::CodeBlock(kind) => {
                    let language = match kind {
                        CodeBlockKind::Fenced(language) => Some(language.into_string()),
                        CodeBlockKind::Indented => None,
                    };
                    current = Some(new_block(
                        BlockKind::Code(language),
                        quote_depth,
                        &mut items,
                    ));
                }
                Tag::HtmlBlock => {
                    current = Some(new_block(BlockKind::Html, quote_depth, &mut items));
                }
                Tag::MetadataBlock(_) => {
                    current = Some(new_block(BlockKind::Metadata, quote_depth, &mut items));
                }
                Tag::BlockQuote(_) => quote_depth += 1,
                Tag::List(start) => {
                    push_current(&mut blocks, &mut current);
                    lists.push(ListState { next: start });
                }
                Tag::Item => {
                    let marker = lists
                        .last()
                        .and_then(|list| list.next)
                        .map_or_else(|| "•".into(), |number| format!("{number}."));
                    items.push(ItemState {
                        marker,
                        depth: lists.len().saturating_sub(1),
                        used: false,
                    });
                }
                Tag::TableHead => {
                    let mut block =
                        new_block(BlockKind::Table { header: true }, quote_depth, &mut items);
                    block.table_alignments.clone_from(&table_alignments);
                    current = Some(block);
                }
                Tag::TableRow => {
                    let mut block =
                        new_block(BlockKind::Table { header: false }, quote_depth, &mut items);
                    block.table_alignments.clone_from(&table_alignments);
                    current = Some(block);
                }
                Tag::TableCell => {
                    cell_start = current.as_ref().map(|block| block.text.len());
                }
                Tag::Table(alignments) => table_alignments = alignments,
                Tag::FootnoteDefinition(label) => footnote = Some(label.into_string()),
                Tag::DefinitionListTitle => {
                    current = Some(new_block(
                        BlockKind::DefinitionTitle,
                        quote_depth,
                        &mut items,
                    ));
                }
                Tag::DefinitionListDefinition => {
                    current = Some(new_block(BlockKind::Definition, quote_depth, &mut items));
                }
                Tag::Strong => bold += 1,
                Tag::Emphasis => italic += 1,
                Tag::Strikethrough => strike += 1,
                Tag::Link { dest_url, .. } => link = Some(dest_url.into_string()),
                Tag::Image { dest_url, .. } => {
                    current.get_or_insert_with(|| {
                        new_block(BlockKind::Paragraph, quote_depth, &mut items)
                    });
                    image = Some(PendingImage {
                        source: dest_url.into_string(),
                        alt: String::new(),
                    });
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Strong => bold -= 1,
                TagEnd::Emphasis => italic -= 1,
                TagEnd::Strikethrough => strike -= 1,
                TagEnd::Link => link = None,
                TagEnd::Image => {
                    if let (Some(block), Some(image)) = (&mut current, image.take()) {
                        block.push_image(image);
                    }
                }
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::CodeBlock
                | TagEnd::HtmlBlock
                | TagEnd::MetadataBlock(_) => push_current(&mut blocks, &mut current),
                TagEnd::BlockQuote(_) => {
                    push_current(&mut blocks, &mut current);
                    quote_depth = quote_depth.saturating_sub(1);
                }
                TagEnd::List(_) => {
                    lists.pop();
                }
                TagEnd::Item => {
                    if current.is_none() && items.last().is_some_and(|item| !item.used) {
                        current = Some(new_block(BlockKind::Paragraph, quote_depth, &mut items));
                    }
                    push_current(&mut blocks, &mut current);
                    items.pop();
                    if let Some(number) = lists.last_mut().and_then(|list| list.next.as_mut()) {
                        *number += 1;
                    }
                }
                TagEnd::TableCell => {
                    if let (Some(block), Some(start)) = (&mut current, cell_start.take()) {
                        block.cells.push(start..block.text.len());
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => push_current(&mut blocks, &mut current),
                TagEnd::Table => table_alignments.clear(),
                TagEnd::FootnoteDefinition => {
                    push_current(&mut blocks, &mut current);
                    footnote = None;
                }
                TagEnd::DefinitionListTitle | TagEnd::DefinitionListDefinition => {
                    push_current(&mut blocks, &mut current);
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some(image) = &mut image {
                    image.alt.push_str(&text);
                } else {
                    current
                        .get_or_insert_with(|| {
                            new_block(BlockKind::Paragraph, quote_depth, &mut items)
                        })
                        .push(
                            &text,
                            bold > 0,
                            italic > 0,
                            false,
                            strike > 0,
                            link.as_deref(),
                        );
                }
            }
            Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some(image) = &mut image {
                    image.alt.push_str(&text);
                } else {
                    current
                        .get_or_insert_with(|| {
                            new_block(BlockKind::Paragraph, quote_depth, &mut items)
                        })
                        .push(
                            &text,
                            bold > 0,
                            italic > 0,
                            true,
                            strike > 0,
                            link.as_deref(),
                        );
                }
            }
            Event::InlineMath(text) => {
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote_depth, &mut items))
                    .push_math(text.into_string());
            }
            Event::DisplayMath(text) => {
                push_current(&mut blocks, &mut current);
                let mut block = new_block(BlockKind::Math, quote_depth, &mut items);
                block.push(&text, false, false, false, false, None);
                blocks.push(block);
            }
            Event::SoftBreak => {
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote_depth, &mut items))
                    .push(
                        if strict_line_breaks { " " } else { "\n" },
                        bold > 0,
                        italic > 0,
                        false,
                        strike > 0,
                        link.as_deref(),
                    );
            }
            Event::HardBreak => {
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote_depth, &mut items))
                    .push(
                        "\n",
                        bold > 0,
                        italic > 0,
                        false,
                        strike > 0,
                        link.as_deref(),
                    );
            }
            Event::Rule => blocks.push(new_block(BlockKind::Rule, quote_depth, &mut items)),
            Event::TaskListMarker(checked) => {
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote_depth, &mut items))
                    .task = Some(checked);
            }
            Event::FootnoteReference(label) => {
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote_depth, &mut items))
                    .push(&format!("[^{label}]"), false, false, false, false, None);
            }
        }
    }

    push_current(&mut blocks, &mut current);
    MarkdownDocument { blocks }
}

fn new_block(kind: BlockKind, quote_depth: usize, items: &mut [ItemState]) -> Block {
    let mut block = Block {
        kind,
        text: String::new(),
        spans: Vec::new(),
        images: Vec::new(),
        maths: Vec::new(),
        links: Vec::new(),
        list_marker: None,
        list_depth: 0,
        task: None,
        quote_depth,
        cells: Vec::new(),
        table_alignments: Vec::new(),
    };
    if let Some(item) = items.last_mut()
        && !item.used
    {
        block.list_marker = Some(item.marker.clone());
        block.list_depth = item.depth;
        item.used = true;
    }
    block
}

fn push_current(blocks: &mut Vec<Block>, current: &mut Option<Block>) {
    if let Some(mut block) = current.take() {
        if block.kind == BlockKind::Paragraph
            && block.text.is_empty()
            && block.list_marker.is_none()
        {
            return;
        }
        block.finish();
        blocks.push(block);
    }
}

impl Block {
    fn push(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
        code: bool,
        strike: bool,
        link: Option<&str>,
    ) {
        let start = self.text.len();
        self.text.push_str(text);
        let range = start..self.text.len();
        if bold || italic || code || strike {
            self.spans.push(Span {
                range: range.clone(),
                bold,
                italic,
                code,
                strike,
            });
        }
        if let Some(destination) = link {
            self.links.push(LinkSpan {
                range,
                destination: destination.to_owned(),
            });
        }
    }

    fn push_image(&mut self, image: PendingImage) {
        let start = self.text.len();
        self.text.push('\u{fffc}');
        self.images.push(InlineImage {
            range: start..self.text.len(),
            source: image.source,
            alt: image.alt,
        });
    }

    fn push_math(&mut self, source: String) {
        let start = self.text.len();
        self.text.push('\u{fffc}');
        self.maths.push(InlineMath {
            range: start..self.text.len(),
            source,
        });
    }

    fn finish(&mut self) {
        if self.kind == BlockKind::Paragraph && self.text == "\u{fffc}" && self.images.len() == 1 {
            let image = self.images.pop().unwrap();
            self.kind = BlockKind::Image(image.source);
            self.text = image.alt;
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_display_math_order_and_list_markers() {
        let document = parse("before $$x$$ after");
        assert_eq!(
            document
                .blocks
                .iter()
                .map(|block| block.text.as_str())
                .collect::<Vec<_>>(),
            ["before ", "x", " after"]
        );
        let document = parse("- # Heading\n\n- ```rust\n  code\n  ```\n\n- [x] done\n");
        assert_eq!(document.blocks.len(), 3);
        assert!(
            document
                .blocks
                .iter()
                .all(|block| block.list_marker.as_deref() == Some("•"))
        );
        assert_eq!(document.blocks[2].task, Some(true));
        let empty_item = parse("-\n");
        assert_eq!(empty_item.blocks[0].list_marker.as_deref(), Some("•"));
    }

    #[test]
    fn parses_commonmark_and_gfm_blocks() {
        let document = parse(
            "# 标题\n\n普通 **粗体**、*斜体*、~~删除~~ 和 [链接](note.md)。\n\n- [x] 完成\n- [ ] 待办\n\n1. 第一\n2. 第二\n\n> 引用\n\n| A | B |\n| - | - |\n| 1 | 2 |\n\n---\n",
        );

        assert_eq!(document.blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(document.blocks[1].spans.len(), 3);
        assert_eq!(document.blocks[1].links[0].destination, "note.md");
        assert_eq!(document.blocks[2].task, Some(true));
        assert_eq!(document.blocks[2].list_marker.as_deref(), Some("•"));
        assert_eq!(document.blocks[4].list_marker.as_deref(), Some("1."));
        assert_eq!(document.blocks[6].quote_depth, 1);
        assert_eq!(document.blocks[7].kind, BlockKind::Table { header: true });
        assert_eq!(document.blocks[7].cells.len(), 2);
        assert_eq!(document.blocks[8].kind, BlockKind::Table { header: false });
        assert_eq!(document.blocks[9].kind, BlockKind::Rule);

        let images = parse("![图](assets/image.png)\n\n前 ![图](inline.png) 后\n");
        assert_eq!(
            images.blocks[0].kind,
            BlockKind::Image("assets/image.png".into())
        );
        assert_eq!(images.blocks[1].text, "前 \u{fffc} 后");
        assert_eq!(images.blocks[1].images[0].source, "inline.png");

        let raw = parse("`code` <span>raw</span> <!-- comment -->");
        assert!(raw.blocks[0].spans.iter().all(|span| span.code));

        let math = parse("行内 $x^2$。\n\n$$y^2$$\n");
        assert_eq!(math.blocks[0].text, "行内 \u{fffc}。");
        assert_eq!(math.blocks[0].maths[0].source, "x^2");
        assert_eq!(math.blocks[1].kind, BlockKind::Math);
        assert_eq!(math.blocks[1].text, "y^2");

        let extras = parse(
            "| 左 | 右 |\n| :--- | ---: |\n| A | B |\n\n术语\n: 定义\n\n引用[^1]\n\n[^1]: 脚注\n",
        );
        assert_eq!(
            extras.blocks[0].table_alignments,
            [Alignment::Left, Alignment::Right]
        );
        assert!(
            extras
                .blocks
                .iter()
                .any(|block| block.kind == BlockKind::DefinitionTitle)
        );
        assert!(
            extras
                .blocks
                .iter()
                .any(|block| matches!(&block.kind, BlockKind::Footnote(label) if label == "1"))
        );
        assert_eq!(parse("a\nb").blocks[0].text, "a\nb");
        assert_eq!(parse_with_options("a\nb", true).blocks[0].text, "a b");
        assert_eq!(parse_with_options("a  \nb", true).blocks[0].text, "a\nb");
    }
}
