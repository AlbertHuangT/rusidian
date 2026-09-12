use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    Code,
}

#[derive(Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
}

pub fn parse(source: &str) -> MarkdownDocument {
    let mut blocks = Vec::new();
    let mut current = None;
    let mut bold = 0;
    let mut italic = 0;

    for event in Parser::new_ext(source, Options::all()) {
        match event {
            Event::Start(Tag::Paragraph) => current = Some(Block::new(BlockKind::Paragraph)),
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some(Block::new(BlockKind::Heading(heading_level(level))))
            }
            Event::Start(Tag::CodeBlock(_)) => current = Some(Block::new(BlockKind::Code)),
            Event::Start(Tag::Strong) => bold += 1,
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Strong) => bold -= 1,
            Event::End(TagEnd::Emphasis) => italic -= 1,
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock) => {
                if let Some(block) = current.take() {
                    blocks.push(block);
                }
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                let block = current.get_or_insert_with(|| Block::new(BlockKind::Paragraph));
                block.push(&text, bold > 0, italic > 0);
            }
            Event::SoftBreak | Event::HardBreak => {
                current
                    .get_or_insert_with(|| Block::new(BlockKind::Paragraph))
                    .push("\n", bold > 0, italic > 0);
            }
            _ => {}
        }
    }

    if let Some(block) = current {
        blocks.push(block);
    }

    MarkdownDocument { blocks }
}

impl Block {
    fn new(kind: BlockKind) -> Self {
        Self {
            kind,
            text: String::new(),
            spans: Vec::new(),
        }
    }

    fn push(&mut self, text: &str, bold: bool, italic: bool) {
        let start = self.text.len();
        self.text.push_str(text);
        if bold || italic {
            self.spans.push(Span {
                range: start..self.text.len(),
                bold,
                italic,
            });
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
    fn parses_prototype_markdown() {
        let document = parse("# 标题\n\n普通 **粗体** 和 *斜体*。\n\n```rust\nfn main() {}\n```\n");

        assert_eq!(document.blocks.len(), 3);
        assert_eq!(document.blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(document.blocks[1].text, "普通 粗体 和 斜体。");
        assert_eq!(document.blocks[1].spans.len(), 2);
        assert_eq!(document.blocks[2].kind, BlockKind::Code);
    }
}
