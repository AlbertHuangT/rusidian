use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
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
    /// The outermost list containing this block, numbered in document order. Blocks after an
    /// item's first one indent to its text.
    pub list: Option<usize>,
    /// Text of a tight list item (no blank lines between items), which sits close to the next.
    pub tight: bool,
    pub task: Option<bool>,
    pub quote_depth: usize,
    /// The innermost blockquote containing this block, numbered in document order.
    pub quote: Option<usize>,
    /// The outermost blockquote containing this block; blocks sharing it read as one quote.
    pub quote_root: Option<usize>,
    /// The callout kind (`note`, `tip`, ...) of the innermost blockquote, if it is a callout.
    pub callout: Option<String>,
    /// The callout's title, on the first block of the callout only.
    pub callout_title: Option<String>,
    /// For fenced code: whether the closing fence is present (an open fence runs to the end).
    pub fence_closed: bool,
    /// Obsidian's `^id` that block links point to; hidden from the text like in Obsidian.
    pub block_id: Option<String>,
    pub cells: Vec<Range<usize>>,
    pub table_alignments: Vec<Alignment>,
    /// Where each piece of `text` came from in the Markdown source, in text order.
    pub source_map: Vec<SourceSpan>,
}

/// A run of rendered text and the source bytes it was produced from.
#[derive(Debug, PartialEq)]
pub struct SourceSpan {
    pub text: Range<usize>,
    pub source: Range<usize>,
    /// The source repeats the text byte for byte, so offsets inside map one to one.
    pub exact: bool,
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
    Table {
        header: bool,
    },
    Math,
    /// A footnote definition's paragraph. `number` follows the order of first references and
    /// is only on the definition's first paragraph.
    Footnote {
        label: String,
        number: Option<usize>,
    },
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
    /// Obsidian `==highlight==`.
    pub highlight: bool,
    /// An Obsidian `#tag`.
    pub tag: bool,
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

#[derive(Clone, Copy, Default)]
struct Quote {
    depth: usize,
    id: Option<usize>,
    root: Option<usize>,
}

struct ListState {
    next: Option<u64>,
    id: usize,
}

struct ItemState {
    marker: String,
    depth: usize,
    used: bool,
    /// The outermost open list.
    list: usize,
}

#[cfg(test)]
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
    let mut quote = Quote::default();
    // Open blockquotes (innermost last) and the GFM alert kind of every quote by id.
    let mut quotes: Vec<usize> = Vec::new();
    let mut alerts: Vec<Option<BlockQuoteKind>> = Vec::new();
    let mut lists: Vec<ListState> = Vec::new();
    let mut list_count = 0;
    let mut items: Vec<ItemState> = Vec::new();
    let mut cell_start = None;
    let mut table_alignments = Vec::new();
    let mut footnote: Option<String> = None;
    // Footnote labels in the order they are first referenced.
    let mut footnotes: Vec<String> = Vec::new();

    for (event, range) in Parser::new_ext(source, Options::all()).into_offset_iter() {
        let before = current.as_ref().map(|block: &Block| block.text.len());
        // Set when an event maps its own pieces to the source.
        let mut mapped = false;
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph if current.is_none() => {
                    let mut block = new_block(
                        footnote.as_ref().map_or(BlockKind::Paragraph, |label| {
                            BlockKind::Footnote {
                                label: label.clone(),
                                number: None,
                            }
                        }),
                        quote,
                        &mut items,
                    );
                    // Loose list items wrap their text in paragraphs; tight ones do not.
                    block.tight = false;
                    current = Some(block);
                }
                Tag::Heading { level, .. } => {
                    current = Some(new_block(
                        BlockKind::Heading(heading_level(level)),
                        quote,
                        &mut items,
                    ));
                }
                Tag::CodeBlock(kind) => {
                    let language = match kind {
                        CodeBlockKind::Fenced(language) => Some(language.into_string()),
                        CodeBlockKind::Indented => None,
                    };
                    current = Some(new_block(BlockKind::Code(language), quote, &mut items));
                }
                Tag::HtmlBlock => {
                    current = Some(new_block(BlockKind::Html, quote, &mut items));
                }
                Tag::MetadataBlock(_) => {
                    current = Some(new_block(BlockKind::Metadata, quote, &mut items));
                }
                Tag::BlockQuote(kind) => {
                    push_current(&mut blocks, &mut current);
                    quotes.push(alerts.len());
                    alerts.push(kind);
                    quote = Quote {
                        depth: quotes.len(),
                        id: quotes.last().copied(),
                        root: quotes.first().copied(),
                    };
                }
                Tag::List(start) => {
                    push_current(&mut blocks, &mut current);
                    lists.push(ListState {
                        next: start,
                        id: list_count,
                    });
                    list_count += 1;
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
                        list: lists.first().map_or(0, |list| list.id),
                    });
                }
                Tag::TableHead => {
                    let mut block = new_block(BlockKind::Table { header: true }, quote, &mut items);
                    block.table_alignments.clone_from(&table_alignments);
                    current = Some(block);
                }
                Tag::TableRow => {
                    let mut block =
                        new_block(BlockKind::Table { header: false }, quote, &mut items);
                    block.table_alignments.clone_from(&table_alignments);
                    current = Some(block);
                }
                Tag::TableCell => {
                    cell_start = current.as_ref().map(|block| block.text.len());
                }
                Tag::Table(alignments) => table_alignments = alignments,
                Tag::FootnoteDefinition(label) => footnote = Some(label.into_string()),
                Tag::DefinitionListTitle => {
                    current = Some(new_block(BlockKind::DefinitionTitle, quote, &mut items));
                }
                Tag::DefinitionListDefinition => {
                    current = Some(new_block(BlockKind::Definition, quote, &mut items));
                }
                Tag::Strong => bold += 1,
                Tag::Emphasis => italic += 1,
                Tag::Strikethrough => strike += 1,
                Tag::Link { dest_url, .. } => link = Some(dest_url.into_string()),
                Tag::Image { dest_url, .. } => {
                    current
                        .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items));
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
                TagEnd::CodeBlock => {
                    if let Some(block) = &mut current {
                        block.fence_closed = closes_fence(source.get(range.clone()).unwrap_or(""));
                    }
                    push_current(&mut blocks, &mut current);
                }
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::HtmlBlock
                | TagEnd::MetadataBlock(_) => push_current(&mut blocks, &mut current),
                TagEnd::BlockQuote(_) => {
                    push_current(&mut blocks, &mut current);
                    quotes.pop();
                    quote = Quote {
                        depth: quotes.len(),
                        id: quotes.last().copied(),
                        root: quotes.first().copied(),
                    };
                }
                TagEnd::List(_) => {
                    lists.pop();
                }
                TagEnd::Item => {
                    if current.is_none() && items.last().is_some_and(|item| !item.used) {
                        current = Some(new_block(BlockKind::Paragraph, quote, &mut items));
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
                    let block = current
                        .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items));
                    // Bare URLs and #tags are marked, as in Obsidian, except in code and links.
                    let marks = link.is_none()
                        && !matches!(
                            block.kind,
                            BlockKind::Code(_) | BlockKind::Html | BlockKind::Metadata
                        );
                    let pieces = obsidian_inline(&text);
                    // Pieces map to exact source ranges only when the text is the source verbatim.
                    let verbatim = source.get(range.clone()) == Some(&*text);
                    if pieces.len() > 1 && verbatim {
                        for (piece, kind) in pieces {
                            if kind == Inline::Comment {
                                continue;
                            }
                            let start = block.text.len();
                            block.push_text(
                                &text[piece.clone()],
                                (bold > 0, italic > 0, strike > 0),
                                link.as_deref(),
                                marks,
                            );
                            let pushed = start..block.text.len();
                            if kind == Inline::Highlight {
                                block.spans.push(Span {
                                    range: pushed.clone(),
                                    bold: false,
                                    italic: false,
                                    code: false,
                                    strike: false,
                                    highlight: true,
                                    tag: false,
                                });
                            }
                            block.map_source(
                                pushed,
                                source,
                                range.start + piece.start..range.start + piece.end,
                            );
                        }
                        mapped = true;
                    } else {
                        block.push_text(
                            &text,
                            (bold > 0, italic > 0, strike > 0),
                            link.as_deref(),
                            marks,
                        );
                    }
                }
            }
            Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some(image) = &mut image {
                    image.alt.push_str(&text);
                } else {
                    current
                        .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items))
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
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items))
                    .push_math(text.into_string());
            }
            Event::DisplayMath(text) => {
                push_current(&mut blocks, &mut current);
                let mut block = new_block(BlockKind::Math, quote, &mut items);
                block.push(&text, false, false, false, false, None);
                block.map_source(0..block.text.len(), source, range.clone());
                blocks.push(block);
            }
            Event::SoftBreak => {
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items))
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
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items))
                    .push(
                        "\n",
                        bold > 0,
                        italic > 0,
                        false,
                        strike > 0,
                        link.as_deref(),
                    );
            }
            Event::Rule => blocks.push(new_block(BlockKind::Rule, quote, &mut items)),
            Event::TaskListMarker(checked) => {
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items))
                    .task = Some(checked);
            }
            Event::FootnoteReference(label) => {
                let number = footnote_number(&mut footnotes, &label);
                // Shown as a link that `gf` follows to the definition.
                current
                    .get_or_insert_with(|| new_block(BlockKind::Paragraph, quote, &mut items))
                    .push(
                        &format!("[{number}]"),
                        false,
                        false,
                        false,
                        false,
                        Some(&footnote_link(&label)),
                    );
            }
        }
        if let Some(block) = current.as_mut().filter(|_| !mapped) {
            let start = before.unwrap_or(0);
            if block.text.len() > start {
                block.map_source(start..block.text.len(), source, range);
            }
        }
    }

    push_current(&mut blocks, &mut current);
    apply_callouts(&mut blocks, &alerts);
    number_footnotes(&mut blocks, &mut footnotes);
    attach_standalone_block_ids(&mut blocks);
    MarkdownDocument { blocks }
}

/// A paragraph holding only `^id` names the block before it (how Obsidian marks lists, quotes
/// and tables); it is not shown itself.
fn attach_standalone_block_ids(blocks: &mut Vec<Block>) {
    let mut index = 1;
    while index < blocks.len() {
        let block = &blocks[index];
        if block.kind == BlockKind::Paragraph && block.text.is_empty() && block.block_id.is_some() {
            let id = blocks.remove(index).block_id;
            blocks[index - 1].block_id = id;
        } else {
            index += 1;
        }
    }
}

fn footnote_number(footnotes: &mut Vec<String>, label: &str) -> usize {
    match footnotes.iter().position(|known| known == label) {
        Some(index) => index + 1,
        None => {
            footnotes.push(label.to_owned());
            footnotes.len()
        }
    }
}

/// The in-note destination of a footnote reference; see [`footnote_label`].
fn footnote_link(label: &str) -> String {
    format!("#[^{label}]")
}

/// The label a link fragment from [`footnote_link`] points to.
pub fn footnote_label(fragment: &str) -> Option<&str> {
    fragment.strip_prefix("[^")?.strip_suffix(']')
}

/// Number each definition's first paragraph; unreferenced definitions follow the referenced ones.
fn number_footnotes(blocks: &mut [Block], footnotes: &mut Vec<String>) {
    let mut numbered = Vec::new();
    for block in blocks {
        if let BlockKind::Footnote { label, number } = &mut block.kind
            && !numbered.contains(label)
        {
            *number = Some(footnote_number(footnotes, label));
            numbered.push(label.clone());
        }
    }
}

fn new_block(kind: BlockKind, quote: Quote, items: &mut [ItemState]) -> Block {
    let mut block = Block {
        kind,
        text: String::new(),
        spans: Vec::new(),
        images: Vec::new(),
        maths: Vec::new(),
        links: Vec::new(),
        list_marker: None,
        list_depth: 0,
        list: None,
        tight: false,
        task: None,
        quote_depth: quote.depth,
        quote: quote.id,
        quote_root: quote.root,
        callout: None,
        callout_title: None,
        fence_closed: false,
        block_id: None,
        cells: Vec::new(),
        table_alignments: Vec::new(),
        source_map: Vec::new(),
    };
    if let Some(item) = items.last_mut() {
        block.list = Some(item.list);
        block.list_depth = item.depth;
        block.tight = block.kind == BlockKind::Paragraph;
        if !item.used {
            block.list_marker = Some(item.marker.clone());
            item.used = true;
        }
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

/// Bare `http://` and `https://` URLs in text, as GFM's autolink extension and Obsidian find
/// them. Trailing punctuation, unbalanced `)` and Chinese punctuation are not part of the URL.
fn bare_urls(text: &str) -> Vec<Range<usize>> {
    let mut urls = Vec::new();
    let mut search = 0;
    while let Some(found) = text[search..].find("http") {
        let start = search + found;
        let rest = &text[start..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            search = start + 4;
            continue;
        };
        let starts_word = text[..start].chars().next_back().is_none_or(|character| {
            character.is_whitespace() || !character.is_ascii() || "([{<\"'".contains(character)
        });
        let mut end = start
            + rest
                .find(|character: char| {
                    character.is_whitespace()
                        || "<>\"`".contains(character)
                        || is_wide_punctuation(character)
                })
                .unwrap_or(rest.len());
        while let Some(last) = text[start..end].chars().next_back() {
            let unbalanced = last == ')'
                && text[start..end].matches(')').count() > text[start..end].matches('(').count();
            if ".,:;!?'*_~".contains(last) || unbalanced {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        if starts_word && end > start + scheme {
            urls.push(start..end);
        }
        search = end.max(start + scheme);
    }
    urls
}

/// Obsidian tags: `#` starting a word, then letters, digits, `_`, `-` or `/`, not only digits.
fn tags(text: &str) -> Vec<Range<usize>> {
    text.match_indices('#')
        .filter_map(|(start, _)| {
            let starts_word = text[..start].chars().next_back().is_none_or(|character| {
                character.is_whitespace() || is_wide_punctuation(character)
            });
            let name = &text[start + 1..];
            let length = name
                .find(|character: char| !(character.is_alphanumeric() || "_-/".contains(character)))
                .unwrap_or(name.len());
            let name = &name[..length];
            (starts_word && !name.is_empty() && !name.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| start..start + 1 + length)
        })
        .collect()
}

/// Full-width (CJK) punctuation, which ends a bare URL written in Chinese text.
fn is_wide_punctuation(character: char) -> bool {
    matches!(
        character,
        '\u{3000}'..='\u{303f}'
            | '\u{ff01}'..='\u{ff0f}'
            | '\u{ff1a}'..='\u{ff20}'
            | '\u{ff3b}'..='\u{ff40}'
            | '\u{ff5b}'..='\u{ff65}'
    )
}

impl Block {
    /// Push text with its emphasis (bold, italic, strike). With `marks`, bare URLs become
    /// links and `#tags` are marked.
    fn push_text(
        &mut self,
        text: &str,
        (bold, italic, strike): (bool, bool, bool),
        link: Option<&str>,
        marks: bool,
    ) {
        // Marked pieces in order; `true` for a URL, `false` for a tag.
        let mut pieces: Vec<(Range<usize>, bool)> = Vec::new();
        if marks {
            let urls = bare_urls(text);
            let tags = tags(text).into_iter().filter(|tag| {
                !urls
                    .iter()
                    .any(|url| url.start < tag.end && tag.start < url.end)
            });
            pieces.extend(tags.map(|tag| (tag, false)));
            pieces.extend(urls.into_iter().map(|url| (url, true)));
            pieces.sort_by_key(|(range, _)| range.start);
        }
        let mut start = 0;
        for (range, url) in pieces {
            if start < range.start {
                self.push(&text[start..range.start], bold, italic, false, strike, link);
            }
            let piece = &text[range.clone()];
            if url {
                self.push(piece, bold, italic, false, strike, Some(piece));
            } else {
                let at = self.text.len();
                self.push(piece, bold, italic, false, strike, link);
                self.spans.push(Span {
                    range: at..self.text.len(),
                    bold: false,
                    italic: false,
                    code: false,
                    strike: false,
                    highlight: false,
                    tag: true,
                });
            }
            start = range.end;
        }
        if start < text.len() {
            self.push(&text[start..], bold, italic, false, strike, link);
        }
    }

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
                highlight: false,
                tag: false,
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
        // Fenced code, HTML and front matter end with the newline before their closing line.
        if matches!(
            self.kind,
            BlockKind::Code(_) | BlockKind::Html | BlockKind::Metadata
        ) && self.text.ends_with('\n')
        {
            self.truncate(self.text.len() - 1);
        }
        if !matches!(
            self.kind,
            BlockKind::Code(_) | BlockKind::Html | BlockKind::Metadata | BlockKind::Math
        ) && let Some((start, id)) = trailing_block_id(&self.text)
        {
            self.block_id = Some(id.to_owned());
            self.truncate(start);
        }
        if self.kind == BlockKind::Paragraph && self.text == "\u{fffc}" && self.images.len() == 1 {
            let image = self.images.pop().unwrap();
            self.kind = BlockKind::Image(image.source);
            self.text = image.alt;
            let source = self
                .source_map
                .first()
                .map_or(0..0, |span| span.source.clone());
            self.source_map = vec![SourceSpan {
                text: 0..self.text.len(),
                source,
                exact: false,
            }];
        }
    }
}

/// A trailing Obsidian block id: where its leading whitespace starts, and the id.
fn trailing_block_id(text: &str) -> Option<(usize, &str)> {
    let caret = text.rfind('^')?;
    let id = &text[caret + 1..];
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    let before = text[..caret].trim_end();
    // Alone, or after whitespace: `word^id` is not an id.
    (before.len() < caret || caret == 0).then_some((before.len(), id))
}

/// Whether a fenced code block's source ends with a closing fence matching its opening one.
fn closes_fence(block: &str) -> bool {
    let mut lines = block.trim_end_matches('\n').lines();
    let Some(open) = lines.next().map(str::trim_start) else {
        return false;
    };
    let marker = open
        .chars()
        .next()
        .filter(|marker| matches!(marker, '`' | '~'));
    let Some(marker) = marker else {
        return false;
    };
    let length = open
        .chars()
        .take_while(|character| *character == marker)
        .count();
    lines.next_back().is_some_and(|close| {
        let close = close.trim();
        close.len() >= length && close.chars().all(|character| character == marker)
    })
}

impl Block {
    /// The zero-based source lines this block's text came from.
    pub fn source_lines(&self, source: &str) -> Option<Range<usize>> {
        let start = self.source_map.first()?.source.start;
        let end = self.source_map.last()?.source.end;
        let line = |offset: usize| source[..offset.min(source.len())].matches('\n').count();
        Some(line(start)..line(end.saturating_sub(1).max(start)) + 1)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Inline {
    Plain,
    /// `==text==`, shown highlighted without the markers.
    Highlight,
    /// `%%text%%`, hidden in the reading view.
    Comment,
}

/// Split text into plain runs, Obsidian highlights and comments (markers excluded). Only pairs
/// within the same text run are recognized; a highlight must not start or end with a space.
fn obsidian_inline(text: &str) -> Vec<(Range<usize>, Inline)> {
    let mut pieces = Vec::new();
    let mut plain = 0;
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let marker = if rest.starts_with("==") {
            Some(("==", Inline::Highlight))
        } else if rest.starts_with("%%") {
            Some(("%%", Inline::Comment))
        } else {
            None
        };
        if let Some((marker, kind)) = marker
            && let Some(length) = rest[2..].find(marker)
        {
            let inner = &rest[2..2 + length];
            let valid = match kind {
                Inline::Highlight => {
                    !inner.is_empty()
                        && !inner.starts_with(char::is_whitespace)
                        && !inner.ends_with(char::is_whitespace)
                }
                _ => true,
            };
            if valid {
                if plain < index {
                    pieces.push((plain..index, Inline::Plain));
                }
                if !inner.is_empty() {
                    pieces.push((index + 2..index + 2 + length, kind));
                }
                index += 4 + length;
                plain = index;
                continue;
            }
        }
        index += rest.chars().next().map_or(1, char::len_utf8);
    }
    if plain < text.len() {
        pieces.push((plain..text.len(), Inline::Plain));
    }
    pieces
}

/// Turn blockquotes into callouts: GitHub alerts (`> [!NOTE]`, already parsed by pulldown-cmark)
/// and Obsidian callouts (`> [!tip]- Optional title` as the quote's first line).
fn apply_callouts(blocks: &mut [Block], alerts: &[Option<BlockQuoteKind>]) {
    for (id, alert) in alerts.iter().enumerate() {
        let Some(first) = blocks.iter().position(|block| block.quote == Some(id)) else {
            continue;
        };
        let callout = if let Some(kind) = alert {
            let kind = format!("{kind:?}").to_lowercase();
            Some((kind, None))
        } else if blocks[first].kind == BlockKind::Paragraph {
            parse_callout_header(&blocks[first].text).map(|(kind, title, header)| {
                blocks[first].remove_prefix(header);
                (kind, title)
            })
        } else {
            None
        };
        let Some((kind, title)) = callout else {
            continue;
        };
        blocks[first].callout_title = Some(title.unwrap_or_else(|| capitalize(&kind)));
        for block in blocks.iter_mut().filter(|block| block.quote == Some(id)) {
            block.callout = Some(kind.clone());
        }
    }
}

/// `[!kind]`, an optional fold marker and an optional title on the first line. Returns the kind,
/// the title and how many bytes of text the header line takes (including its newline).
fn parse_callout_header(text: &str) -> Option<(String, Option<String>, usize)> {
    let line_end = text.find('\n').unwrap_or(text.len());
    let line = &text[..line_end];
    let rest = line.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let kind = &rest[..close];
    if kind.is_empty()
        || !kind
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
    {
        return None;
    }
    let title = rest[close + 1..].trim_start_matches(['+', '-']).trim();
    let header = if line_end < text.len() {
        line_end + 1
    } else {
        line_end
    };
    Some((
        kind.to_lowercase(),
        (!title.is_empty()).then(|| title.to_owned()),
        header,
    ))
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

impl Block {
    /// Drop the first `count` bytes of text, keeping every range pointing at the same text.
    /// Drop the text from `end` on, keeping every range pointing at the same text.
    fn truncate(&mut self, end: usize) {
        self.text.truncate(end);
        for span in &mut self.spans {
            span.range.end = span.range.end.min(end);
        }
        self.spans.retain(|span| span.range.start < span.range.end);
        for link in &mut self.links {
            link.range.end = link.range.end.min(end);
        }
        self.links.retain(|link| link.range.start < link.range.end);
        self.images.retain(|image| image.range.end <= end);
        self.maths.retain(|math| math.range.end <= end);
        for span in &mut self.source_map {
            if span.text.end > end {
                if span.exact {
                    span.source.end -= span.text.end - end.max(span.text.start);
                }
                span.text.end = end.max(span.text.start);
            }
        }
        self.source_map.retain(|span| !span.text.is_empty());
    }

    fn remove_prefix(&mut self, count: usize) {
        self.text.replace_range(..count, "");
        let shift = |range: &mut Range<usize>| {
            range.start = range.start.saturating_sub(count);
            range.end = range.end.saturating_sub(count);
        };
        for span in &mut self.spans {
            shift(&mut span.range);
        }
        self.spans.retain(|span| !span.range.is_empty());
        for link in &mut self.links {
            shift(&mut link.range);
        }
        self.links.retain(|link| !link.range.is_empty());
        self.images.retain(|image| image.range.start >= count);
        for image in &mut self.images {
            shift(&mut image.range);
        }
        self.maths.retain(|math| math.range.start >= count);
        for math in &mut self.maths {
            shift(&mut math.range);
        }
        self.source_map.retain(|span| span.text.end > count);
        for span in &mut self.source_map {
            if span.text.start < count {
                if span.exact {
                    span.source.start += count - span.text.start;
                }
                span.text.start = count;
            }
            shift(&mut span.text);
        }
    }

    fn map_source(&mut self, text: Range<usize>, source: &str, range: Range<usize>) {
        let shown = &self.text[text.clone()];
        let raw = source.get(range.clone()).unwrap_or_default();
        let (source, exact) = if raw == shown {
            (range, true)
        } else if let Some(at) = raw.find(shown).filter(|_| !shown.is_empty()) {
            (range.start + at..range.start + at + shown.len(), true)
        } else {
            (range, false)
        };
        self.source_map.push(SourceSpan {
            text,
            source,
            exact,
        });
    }

    /// The source byte for the text byte `byte`; hidden markup and objects map to their start.
    pub fn source_offset(&self, byte: usize) -> Option<usize> {
        if let Some(span) = self
            .source_map
            .iter()
            .find(|span| span.text.contains(&byte))
        {
            return Some(if span.exact {
                span.source.start + (byte - span.text.start)
            } else {
                span.source.start
            });
        }
        self.source_map
            .iter()
            .find(|span| span.text.start >= byte)
            .or(self.source_map.last())
            .map(|span| span.source.start)
    }

    /// The text byte shown for source byte `offset`, or the next visible text after it when the
    /// offset is on hidden markup. `None` if the block ends before `offset`.
    pub fn text_offset(&self, offset: usize) -> Option<usize> {
        if let Some(span) = self
            .source_map
            .iter()
            .find(|span| span.source.contains(&offset))
        {
            if !span.exact {
                return Some(span.text.start);
            }
            let mut byte = (span.text.start + offset - span.source.start).min(span.text.end);
            while !self.text.is_char_boundary(byte) {
                byte -= 1;
            }
            return Some(byte);
        }
        self.source_map
            .iter()
            .find(|span| span.source.start > offset)
            .map(|span| span.text.start)
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

        let tight = parse("- a\n  - b\n- c\n\nafter\n");
        assert!(
            tight.blocks[..3]
                .iter()
                .all(|block| block.tight && block.list == Some(0))
        );
        assert_eq!(tight.blocks[1].list_depth, 1);
        assert!(tight.blocks[3].list.is_none() && !tight.blocks[3].tight);
        let loose = parse("1. first\n\n   more\n\n2. second\n");
        assert_eq!(loose.blocks.len(), 3);
        assert!(
            loose
                .blocks
                .iter()
                .all(|block| block.list.is_some() && !block.tight)
        );
        // A list in a quote and the list after it are different lists.
        let separate = parse("> - quoted\n\n- outside\n");
        assert_eq!(separate.blocks[0].list, Some(0));
        assert_eq!(separate.blocks[1].list, Some(1));
        // The item's second paragraph has no marker of its own.
        assert_eq!(loose.blocks[1].list_marker, None);
        assert_eq!(loose.blocks[2].list_marker.as_deref(), Some("2."));
    }

    #[test]
    fn marks_obsidian_tags() {
        let found = |text: &str| {
            tags(text)
                .into_iter()
                .map(|range| text[range].to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found("#todo and #project/rusidian-app, C# #123 #2026年 a#b"),
            ["#todo", "#project/rusidian-app", "#2026年"]
        );
        assert_eq!(found("标签：#中文标签，后文"), ["#中文标签"]);
        let document = parse("See #todo at https://x.org/#frag and `#code`\n");
        let tagged: Vec<_> = document.blocks[0]
            .spans
            .iter()
            .filter(|span| span.tag)
            .map(|span| &document.blocks[0].text[span.range.clone()])
            .collect();
        assert_eq!(tagged, ["#todo"]);
    }

    #[test]
    fn hides_obsidian_block_ids() {
        let document = parse(
            "Para text ^abc-1\n\n- item ^li\n\n| a |\n| - |\n| b |\n\n^table\n\nx^2 and caret ^ end\n",
        );
        let paragraph = &document.blocks[0];
        assert_eq!(paragraph.text, "Para text");
        assert_eq!(paragraph.block_id.as_deref(), Some("abc-1"));
        assert_eq!(paragraph.source_lines("Para text ^abc-1"), Some(0..1));
        assert_eq!(document.blocks[1].text, "item");
        assert_eq!(document.blocks[1].block_id.as_deref(), Some("li"));
        // A standalone id names the table (its last row) and is not shown.
        assert_eq!(document.blocks[3].block_id.as_deref(), Some("table"));
        assert_eq!(document.blocks[4].text, "x^2 and caret ^ end");
        assert_eq!(document.blocks.len(), 5);
        assert_eq!(trailing_block_id("^solo"), Some((0, "solo")));
        assert_eq!(trailing_block_id("word^id"), None);
    }

    #[test]
    fn links_bare_urls() {
        let found = |text: &str| {
            bare_urls(text)
                .into_iter()
                .map(|range| text[range].to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found("see https://example.com/a_b?x=1. And (http://x.org/wiki/A_(b)) too"),
            ["https://example.com/a_b?x=1", "http://x.org/wiki/A_(b)"]
        );
        assert_eq!(
            found("访问https://example.com/路径，然后"),
            ["https://example.com/路径"]
        );
        assert!(found("nohttps://x.org https:// httpx").is_empty());

        let document = parse(
            "Go to https://example.com, `https://code.example` or [a](https://b.org).\n\n```\nhttps://in.code\n```\n",
        );
        let paragraph = &document.blocks[0];
        let links: Vec<_> = paragraph
            .links
            .iter()
            .map(|link| {
                (
                    &paragraph.text[link.range.clone()],
                    link.destination.as_str(),
                )
            })
            .collect();
        assert_eq!(
            links,
            [
                ("https://example.com", "https://example.com"),
                ("a", "https://b.org")
            ]
        );
        assert_eq!(paragraph.source_offset(6), Some(6));
        assert!(document.blocks[1].links.is_empty());
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

        let code = parse("```rust\nfn main() {}\n```\n\n<div>\nraw\n</div>\n");
        assert_eq!(code.blocks[0].text, "fn main() {}");
        assert_eq!(code.blocks[1].text, "<div>\nraw\n</div>");

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
                .any(|block| matches!(&block.kind, BlockKind::Footnote { label, number: Some(1) } if label == "1"))
        );
        let footnotes =
            parse("A[^b] B[^a] again[^b]\n\n[^a]: First.\n\n[^b]: Second.\n\n[^c]: Unused.\n");
        let text = &footnotes.blocks[0];
        assert_eq!(text.text, "A[1] B[2] again[1]");
        assert_eq!(text.links[0].destination, "#[^b]");
        assert_eq!(footnote_label(&text.links[1].destination[1..]), Some("a"));
        let numbers: Vec<_> = footnotes
            .blocks
            .iter()
            .filter_map(|block| match &block.kind {
                BlockKind::Footnote { label, number } => Some((label.as_str(), *number)),
                _ => None,
            })
            .collect();
        assert_eq!(numbers, [("a", Some(2)), ("b", Some(1)), ("c", Some(3))]);
        assert_eq!(parse("a\nb").blocks[0].text, "a\nb");
        assert_eq!(parse_with_options("a\nb", true).blocks[0].text, "a b");
        assert_eq!(parse_with_options("a  \nb", true).blocks[0].text, "a\nb");

        let mapped = parse("# Title\n\nSome **bold** and `code` text.\n");
        let paragraph = &mapped.blocks[1];
        let source = "# Title\n\nSome **bold** and `code` text.\n";
        let bold = paragraph.text.find("bold").unwrap();
        assert_eq!(paragraph.source_offset(bold), source.find("bold"));
        let code = paragraph.text.find("code").unwrap();
        assert_eq!(
            paragraph.source_offset(code + 1),
            Some(source.find("code").unwrap() + 1)
        );
        // The `**` before "bold" is hidden; it maps forward to the bold text.
        assert_eq!(
            paragraph.text_offset(source.find("**").unwrap()),
            Some(bold)
        );
        assert_eq!(mapped.blocks[0].text_offset(0), Some(0));
        assert_eq!(mapped.blocks[0].source_offset(0), Some(2));

        let callouts = parse(
            "> [!NOTE]\n> GitHub alert.\n\n> [!tip]- Custom **title**\n> Body *text*.\n> > nested\n\n> plain quote\n",
        );
        let alert = &callouts.blocks[0];
        assert_eq!(alert.callout.as_deref(), Some("note"));
        assert_eq!(alert.callout_title.as_deref(), Some("Note"));
        assert_eq!(alert.text, "GitHub alert.");
        let tip = &callouts.blocks[1];
        assert_eq!(tip.callout.as_deref(), Some("tip"));
        assert_eq!(tip.callout_title.as_deref(), Some("Custom title"));
        assert_eq!(tip.text, "Body text.");
        assert_eq!(tip.spans[0].range, 5..9);
        let source = "> [!NOTE]\n> GitHub alert.\n\n> [!tip]- Custom **title**\n> Body *text*.\n> > nested\n\n> plain quote\n";
        assert_eq!(tip.source_offset(0), source.find("Body"));
        let nested = &callouts.blocks[2];
        assert_eq!((nested.callout.as_deref(), nested.quote_depth), (None, 2));
        assert_eq!(callouts.blocks[3].callout, None);
        // Separate quotes stay apart; a nested quote belongs to its outer one.
        let roots: Vec<_> = callouts
            .blocks
            .iter()
            .map(|block| block.quote_root)
            .collect();
        assert_eq!(roots, [Some(0), Some(1), Some(1), Some(3)]);
        assert_ne!(nested.quote, nested.quote_root);
        assert_eq!(
            parse_callout_header("[!warning]"),
            Some(("warning".into(), None, 10))
        );
        assert_eq!(parse_callout_header("[not callout]"), None);

        let obsidian = parse("a ==marked text== b %%hidden%% c == d");
        let paragraph = &obsidian.blocks[0];
        assert_eq!(paragraph.text, "a marked text b  c == d");
        let marked = paragraph.spans.iter().find(|span| span.highlight).unwrap();
        assert_eq!(&paragraph.text[marked.range.clone()], "marked text");
        assert_eq!(paragraph.source_offset(2), Some(4));
        assert_eq!(paragraph.text_offset(0), Some(0));
        assert_eq!(
            obsidian_inline("x == y == z"),
            [(0..11, Inline::Plain)],
            "spaced pairs are comparisons, not highlights"
        );

        let fences = parse("```tikz\na\n```\n\n~~~~\nb\n~~~~\n\n```tikz\nopen\n");
        assert!(fences.blocks[0].fence_closed && fences.blocks[1].fence_closed);
        assert!(!fences.blocks[2].fence_closed);
        let source = "# T\n\n```tikz\none\ntwo\n```\n";
        assert_eq!(parse(source).blocks[1].source_lines(source), Some(3..5));

        let wiki = parse("[[目标笔记|显示名称]]");
        assert_eq!(wiki.blocks[0].text, "显示名称");
        assert_eq!(wiki.blocks[0].links[0].destination, "目标笔记");
    }
}
