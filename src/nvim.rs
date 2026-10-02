use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use nvim_rs::{
    Handler, Neovim, Value, compat::tokio::Compat, create::tokio as create,
    uioptions::UiAttachOptions,
};
use std::{collections::HashMap, path::PathBuf, thread};
use tokio::{process::ChildStdin, sync::mpsc};

pub struct Client {
    pub events: Receiver<Event>,
    commands: mpsc::UnboundedSender<Command>,
}

/// A cheap handle that lets layout code request a new grid size.
#[derive(Clone)]
pub struct Resizer(mpsc::UnboundedSender<Command>);

impl Resizer {
    pub fn resize(&self, width: i64, height: i64) {
        let _ = self.0.send(Command::Resize(width, height));
    }
}

pub enum Event {
    Redraw(Vec<Value>),
    BufferLines {
        first: usize,
        last: Option<usize>,
        lines: Vec<String>,
        more: bool,
    },
    Warning(String),
    Error(String),
    CloseRefused(String),
    /// The cursor of the current window: zero-based line and byte column.
    Cursor {
        line: usize,
        column: usize,
    },
    Exited,
}

enum Command {
    Input(String),
    Resize(i64, i64),
    /// Zero-based line and byte column.
    SetCursor(usize, usize),
    QueryCursor,
    Close,
}

#[derive(Clone)]
struct EventHandler(Sender<Event>);

#[async_trait]
impl Handler for EventHandler {
    type Writer = Compat<ChildStdin>;

    async fn handle_notify(&self, name: String, args: Vec<Value>, _: Neovim<Self::Writer>) {
        match name.as_str() {
            "redraw" => {
                let _ = self.0.try_send(Event::Redraw(args));
            }
            "nvim_buf_lines_event" if args.get(1).is_some_and(|tick| !tick.is_nil()) => {
                let Some(first) = args.get(2).and_then(Value::as_u64) else {
                    return;
                };
                let Some(last) = args.get(3).and_then(Value::as_i64) else {
                    return;
                };
                let Some(lines) = args.get(4).and_then(Value::as_array) else {
                    return;
                };
                let lines = lines
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let more = args.get(5).and_then(Value::as_bool).unwrap_or(false);
                let _ = self.0.try_send(Event::BufferLines {
                    first: first as usize,
                    last: (last >= 0).then_some(last as usize),
                    lines,
                    more,
                });
            }
            _ => {}
        }
    }
}

impl Client {
    /// Start `nvim --embed` on `path`, in `directory` so relative `:edit` paths resolve there.
    pub fn start(
        path: PathBuf,
        directory: Option<PathBuf>,
        clean: bool,
        (width, height): (i64, i64),
    ) -> Self {
        let path = path.canonicalize().unwrap_or(path);
        let (event_sender, events) = async_channel::unbounded();
        let (commands, mut command_receiver) = mpsc::unbounded_channel();

        thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = event_sender.send_blocking(Event::Error(error.to_string()));
                    return;
                }
            };

            runtime.block_on(async move {
                let handler = EventHandler(event_sender.clone());
                let mut command = tokio::process::Command::new("nvim");
                command.kill_on_drop(true);
                if let Some(directory) = directory.filter(|directory| directory.is_dir()) {
                    command.current_dir(directory);
                }
                command.arg("--embed");
                if clean {
                    command.arg("--clean");
                }
                command.args([
                    "--cmd",
                    "lua local g=vim.api.nvim_create_augroup('RusidianStartupSwap',{clear=true}); vim.api.nvim_create_autocmd('SwapExists',{group=g,callback=function() vim.g.rusidian_swapname=vim.v.swapname; vim.v.swapchoice='q' end})",
                    "--",
                ]).arg(&path);
                let (nvim, io, _child) = match create::new_child_cmd(&mut command, handler).await {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = event_sender
                            .send(Event::Error(format!("无法启动 Neovim：{error}")))
                            .await;
                        return;
                    }
                };

                let mut options = UiAttachOptions::new();
                options.set_rgb(true).set_linegrid_external(true);
                if let Err(error) = nvim.ui_attach(width, height, &options).await {
                    let _ = event_sender
                        .send(Event::Error(format!("无法连接 Neovim UI：{error}")))
                        .await;
                    return;
                }
                let swap = nvim
                    .get_var("rusidian_swapname")
                    .await
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned));
                let _ = nvim
                    .exec_lua(
                        "pcall(vim.api.nvim_del_augroup_by_name, 'RusidianStartupSwap')",
                        Vec::new(),
                    )
                    .await;
                if let Some(swap) = swap {
                    let _ = event_sender
                        .send(Event::Error(format!(
                            "Neovim 检测到 swap 文件，已停止打开以保护未恢复内容：{swap}。请先用 Neovim 的恢复模式检查该文件：{}",
                            path.display()
                        )))
                        .await;
                    return;
                }
                let target = path.canonicalize().unwrap_or_else(|_| path.clone());
                let mut buffer = None;
                if let Ok(buffers) = nvim.list_bufs().await {
                    for candidate in buffers {
                        let Ok(name) = candidate.get_name().await else {
                            continue;
                        };
                        let name = PathBuf::from(name);
                        if name.canonicalize().unwrap_or(name) == target {
                            buffer = Some(candidate);
                            break;
                        }
                    }
                }
                let Some(buffer) = buffer else {
                    let _ = event_sender
                        .send(Event::Error(format!(
                            "Neovim 未打开请求的文件：{}",
                            path.display()
                        )))
                        .await;
                    return;
                };
                if let Err(error) = nvim.set_current_buf(&buffer).await {
                    let _ = event_sender
                        .send(Event::Error(format!("无法选择 Neovim buffer：{error}")))
                        .await;
                    return;
                }
                if let Err(error) = buffer.attach(true, Vec::new()).await {
                    let _ = event_sender
                        .send(Event::Error(format!("无法监听 Neovim buffer：{error}")))
                        .await;
                    return;
                }
                let local_maps = buffer.get_keymap("n").await.unwrap_or_default();
                let global_maps = nvim.get_keymap("n").await.unwrap_or_default();
                let mut warnings = Vec::new();
                if let Some(mapping) = escape_mapping(&local_maps)
                    .or_else(|| escape_mapping(&global_maps))
                {
                    warnings.push(format!(
                        "Neovim Normal 的 Esc 已映射为 {mapping}；Rusidian 当前会优先用 Esc 返回阅读视图"
                    ));
                }
                if !clean
                    && nvim
                        .exec_lua("return package.loaded['im_select'] ~= nil", Vec::new())
                        .await
                        .ok()
                        .and_then(|value| value.as_bool())
                        != Some(true)
                {
                    warnings.push(
                        "未检测到已加载的 im-select.nvim；如需在 Insert/Normal 间自动切换中英文输入源，可以安装该插件"
                            .into(),
                    );
                }
                if !warnings.is_empty() {
                    let _ = event_sender.send(Event::Warning(warnings.join("\n"))).await;
                }

                let exit_sender = event_sender.clone();
                tokio::spawn(async move {
                    let _ = io.await;
                    let _ = exit_sender.send(Event::Exited).await;
                });

                while let Some(command) = command_receiver.recv().await {
                    match command {
                        Command::Close => {
                            if let Err(error) = nvim.command("qall").await
                                && !error.is_channel_closed()
                            {
                                let _ = event_sender.send(Event::CloseRefused(format!(
                                    "Neovim 拒绝关闭：{error}。请用 :w 保存，或自行用 :q! 放弃修改。"
                                ))).await;
                            }
                        }
                        Command::Input(keys) => {
                            if let Err(error) = send_input(&nvim, &keys).await {
                                let _ = event_sender
                                    .send(Event::Error(format!("Neovim 输入失败：{error}")))
                                    .await;
                            }
                        }
                        Command::SetCursor(line, column) => {
                            // The buffer may have changed since the position was computed; a
                            // rejected position just leaves the cursor where it was.
                            if let Ok(window) = nvim.get_current_win().await {
                                let line = line as i64 + 1;
                                let lines = buffer.line_count().await.unwrap_or(line);
                                let _ = window.set_cursor((line.min(lines), column as i64)).await;
                            }
                        }
                        Command::QueryCursor => {
                            if let Ok(window) = nvim.get_current_win().await
                                && let Ok((line, column)) = window.get_cursor().await
                            {
                                let _ = event_sender
                                    .send(Event::Cursor {
                                        line: (line - 1).max(0) as usize,
                                        column: column.max(0) as usize,
                                    })
                                    .await;
                            }
                        }
                        Command::Resize(width, height) => {
                            if let Err(error) = nvim.ui_try_resize(width, height).await {
                                let _ = event_sender
                                    .send(Event::Error(format!("Neovim 调整尺寸失败：{error}")))
                                    .await;
                            }
                        }
                    }
                }

                let _ = nvim.quit_no_save().await;
            });
        });

        Self { events, commands }
    }

    pub fn input(&self, keys: impl Into<String>) {
        let _ = self.commands.send(Command::Input(keys.into()));
    }

    pub fn input_text(&self, text: &str) {
        self.input(text.replace('<', "<lt>"));
    }

    pub fn set_cursor(&self, line: usize, column: usize) {
        let _ = self.commands.send(Command::SetCursor(line, column));
    }

    pub fn query_cursor(&self) {
        let _ = self.commands.send(Command::QueryCursor);
    }

    pub fn close(&self) -> bool {
        self.commands.send(Command::Close).is_ok()
    }

    #[cfg(test)]
    pub fn resize(&self, width: i64, height: i64) {
        self.resizer().resize(width, height);
    }

    pub fn resizer(&self) -> Resizer {
        Resizer(self.commands.clone())
    }
}

async fn send_input(nvim: &Neovim<Compat<ChildStdin>>, mut keys: &str) -> Result<(), String> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !keys.is_empty() {
            let consumed = nvim.input(keys).await.map_err(|error| error.to_string())?;
            keys = keys
                .get(consumed as usize..)
                .ok_or("Neovim 返回了无效的输入长度")?;
            if consumed == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| "Neovim 输入超时，部分文字可能未送达".to_owned())?
}

fn escape_mapping(maps: &[Vec<(Value, Value)>]) -> Option<String> {
    maps.iter().find_map(|map| {
        let field = |name: &str| {
            map.iter()
                .find(|(key, _)| key.as_str() == Some(name))
                .map(|(_, value)| value)
        };
        (field("lhs").and_then(Value::as_str) == Some("<Esc>")).then(|| {
            field("rhs")
                .and_then(Value::as_str)
                .unwrap_or("Lua 回调")
                .to_owned()
        })
    })
}

#[derive(Clone, Debug, Default)]
pub struct Grid {
    width: usize,
    height: usize,
    cells: Vec<Vec<Cell>>,
    highlights: HashMap<u64, Highlight>,
    foreground: Option<u32>,
    background: Option<u32>,
    pub cursor: (usize, usize),
    mode: String,
    mode_index: usize,
    cursor_shapes: Vec<CursorShape>,
    busy: bool,
}

#[derive(Clone, Debug, Default)]
struct Cell {
    text: String,
    highlight: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Highlight {
    pub foreground: Option<u32>,
    pub background: Option<u32>,
    pub special: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub reverse: bool,
}

/// Concrete colors for one highlight after applying defaults and `reverse`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedHighlight {
    pub foreground: u32,
    /// `None` means the grid's default background, which the container already paints.
    pub background: Option<u32>,
    pub special: u32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// Consecutive cells that can be drawn as one string starting at an exact column.
#[derive(Clone, Debug, PartialEq)]
pub struct GridRun {
    pub column: usize,
    /// Width in grid cells.
    pub width: usize,
    pub text: String,
    pub highlight: ResolvedHighlight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CursorShape {
    #[default]
    Block,
    /// Height as a fraction of the cell.
    Horizontal(f32),
    /// Width as a fraction of the cell.
    Vertical(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridCursor {
    pub row: usize,
    pub column: usize,
    pub width: usize,
    pub text: String,
    pub shape: CursorShape,
}

pub const DEFAULT_FOREGROUND: u32 = 0xe6e9ed;
pub const DEFAULT_BACKGROUND: u32 = 0x0c0f12;

#[cfg(test)]
type StyledLine = (String, Option<std::ops::Range<usize>>);

impl Grid {
    pub fn apply_redraw(&mut self, events: &[Value]) -> bool {
        let mut flush = false;
        for event in events {
            let Some(parts) = event.as_array() else {
                continue;
            };
            let Some(name) = parts.first().and_then(Value::as_str) else {
                continue;
            };
            for call in &parts[1..] {
                let Some(args) = call.as_array() else {
                    continue;
                };
                match name {
                    "grid_resize" => self.resize(args),
                    "grid_clear" => self.clear(args),
                    "grid_line" => self.line(args),
                    "grid_cursor_goto" => self.cursor(args),
                    "grid_scroll" => self.scroll(args),
                    "mode_info_set" => self.set_mode_info(args),
                    "mode_change" => self.set_mode(args),
                    "default_colors_set" => self.set_default_colors(args),
                    "hl_attr_define" => self.define_highlight(args),
                    "busy_start" => self.busy = true,
                    "busy_stop" => self.busy = false,
                    "flush" => flush = true,
                    _ => {}
                }
            }
        }
        flush
    }

    #[cfg(test)]
    pub fn lines(&self) -> impl Iterator<Item = StyledLine> + '_ {
        self.cells.iter().enumerate().map(|(row, cells)| {
            let content_end = cells
                .iter()
                .rposition(|cell| cell.text != " ")
                .map_or(0, |column| column + 1);
            let text = cells[..content_end]
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>();
            let cursor = (row == self.cursor.0 && self.cursor.1 < content_end).then(|| {
                let start = cells[..self.cursor.1]
                    .iter()
                    .map(|cell| cell.text.len())
                    .sum::<usize>();
                start..start + cells[self.cursor.1].text.len().max(1)
            });
            (text, cursor)
        })
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Runs for one row. ASCII cells with the same highlight are merged; every other cell gets
    /// its own run so wide and fallback-font glyphs cannot shift the following columns.
    pub fn row_runs(&self, row: usize) -> Vec<GridRun> {
        let Some(cells) = self.cells.get(row) else {
            return Vec::new();
        };
        let mut runs: Vec<(GridRun, u64, bool)> = Vec::new();
        let mut column = 0;
        while column < cells.len() {
            let cell = &cells[column];
            if cell.text.is_empty() {
                column += 1;
                continue;
            }
            let width = if cells
                .get(column + 1)
                .is_some_and(|next| next.text.is_empty())
            {
                2
            } else {
                1
            };
            let simple = width == 1 && cell.text.is_ascii();
            match runs.last_mut() {
                Some((run, highlight, true))
                    if simple
                        && *highlight == cell.highlight
                        && run.column + run.width == column =>
                {
                    run.text.push_str(&cell.text);
                    run.width += 1;
                }
                _ => runs.push((
                    GridRun {
                        column,
                        width,
                        text: cell.text.clone(),
                        highlight: self.resolve(cell.highlight),
                    },
                    cell.highlight,
                    simple,
                )),
            }
            column += width;
        }
        runs.into_iter()
            .map(|(run, _, _)| run)
            .filter(|run| {
                !run.text.chars().all(|character| character == ' ')
                    || run.highlight.background.is_some()
                    || run.highlight.underline
                    || run.highlight.strikethrough
            })
            .collect()
    }

    pub fn resolve(&self, id: u64) -> ResolvedHighlight {
        let highlight = self.highlights.get(&id).copied().unwrap_or_default();
        let (default_foreground, default_background) = self.colors();
        let foreground = highlight.foreground.unwrap_or(default_foreground);
        let background = highlight.background;
        let (foreground, background) = if highlight.reverse {
            (background.unwrap_or(default_background), Some(foreground))
        } else {
            (foreground, background)
        };
        ResolvedHighlight {
            foreground,
            background,
            special: highlight.special.unwrap_or(foreground),
            bold: highlight.bold,
            italic: highlight.italic,
            underline: highlight.underline,
            strikethrough: highlight.strikethrough,
        }
    }

    /// The visible cursor, or `None` while Neovim reports itself busy.
    pub fn visible_cursor(&self) -> Option<GridCursor> {
        if self.busy {
            return None;
        }
        let (row, column) = self.cursor;
        let cells = self.cells.get(row)?;
        let cell = cells.get(column)?;
        let width = if cells
            .get(column + 1)
            .is_some_and(|next| next.text.is_empty())
        {
            2
        } else {
            1
        };
        Some(GridCursor {
            row,
            column,
            width,
            text: cell.text.clone(),
            shape: self
                .cursor_shapes
                .get(self.mode_index)
                .copied()
                .unwrap_or_else(|| self.fallback_cursor_shape()),
        })
    }

    fn fallback_cursor_shape(&self) -> CursorShape {
        if self.mode.starts_with("insert") || self.mode == "cmdline_insert" {
            CursorShape::Vertical(0.25)
        } else if self.mode.starts_with("replace") {
            CursorShape::Horizontal(0.2)
        } else {
            CursorShape::Block
        }
    }

    pub fn colors(&self) -> (u32, u32) {
        (
            self.foreground.unwrap_or(DEFAULT_FOREGROUND),
            self.background.unwrap_or(DEFAULT_BACKGROUND),
        )
    }

    pub fn mode(&self) -> &str {
        &self.mode
    }

    pub fn is_normal(&self) -> bool {
        self.mode.starts_with("normal")
    }

    pub fn accepts_text_input(&self) -> bool {
        self.mode.starts_with("insert")
            || self.mode.starts_with("replace")
            || self.mode.starts_with("cmdline")
    }

    #[cfg(test)]
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn set_mode(&mut self, args: &[Value]) {
        if let Some(mode) = args.first().and_then(Value::as_str) {
            self.mode = mode.to_owned();
        }
        if let Some(index) = args.get(1).and_then(Value::as_u64) {
            self.mode_index = index as usize;
        }
    }

    fn set_mode_info(&mut self, args: &[Value]) {
        let enabled = args.first().and_then(Value::as_bool).unwrap_or(true);
        let Some(modes) = args.get(1).and_then(Value::as_array) else {
            return;
        };
        self.cursor_shapes = modes
            .iter()
            .map(|mode| {
                if !enabled {
                    return CursorShape::Block;
                }
                let field = |name: &str| {
                    mode.as_map()?
                        .iter()
                        .find(|(key, _)| key.as_str() == Some(name))
                        .map(|(_, value)| value)
                };
                let percentage = field("cell_percentage")
                    .and_then(Value::as_u64)
                    .filter(|percentage| (1..=100).contains(percentage))
                    .map_or(0.25, |percentage| percentage as f32 / 100.0);
                match field("cursor_shape").and_then(Value::as_str) {
                    Some("horizontal") => CursorShape::Horizontal(percentage),
                    Some("vertical") => CursorShape::Vertical(percentage),
                    _ => CursorShape::Block,
                }
            })
            .collect();
    }

    fn resize(&mut self, args: &[Value]) {
        if args.first().and_then(Value::as_i64) != Some(1) {
            return;
        }
        let Some(width) = args.get(1).and_then(Value::as_u64) else {
            return;
        };
        let Some(height) = args.get(2).and_then(Value::as_u64) else {
            return;
        };
        self.width = width as usize;
        self.height = height as usize;
        let blank = Cell {
            text: " ".into(),
            highlight: 0,
        };
        // Neovim redraws after a resize, but keep the overlapping area so the frame does not flash.
        self.cells
            .resize(self.height, vec![blank.clone(); self.width]);
        for row in &mut self.cells {
            row.resize(self.width, blank.clone());
        }
    }

    fn clear(&mut self, args: &[Value]) {
        if args.first().and_then(Value::as_i64) == Some(1) {
            self.cells = vec![
                vec![
                    Cell {
                        text: " ".into(),
                        highlight: 0
                    };
                    self.width
                ];
                self.height
            ];
        }
    }

    fn cursor(&mut self, args: &[Value]) {
        if args.first().and_then(Value::as_i64) != Some(1) {
            return;
        }
        if let (Some(row), Some(column)) = (
            args.get(1).and_then(Value::as_u64),
            args.get(2).and_then(Value::as_u64),
        ) {
            self.cursor = (row as usize, column as usize);
        }
    }

    fn line(&mut self, args: &[Value]) {
        if args.first().and_then(Value::as_i64) != Some(1) {
            return;
        }
        let Some(row) = args.get(1).and_then(Value::as_u64).map(|row| row as usize) else {
            return;
        };
        let Some(mut column) = args
            .get(2)
            .and_then(Value::as_u64)
            .map(|column| column as usize)
        else {
            return;
        };
        let Some(cells) = args.get(3).and_then(Value::as_array) else {
            return;
        };
        let Some(line) = self.cells.get_mut(row) else {
            return;
        };

        let mut highlight = 0;
        for cell in cells {
            let Some(cell) = cell.as_array() else {
                continue;
            };
            let Some(text) = cell.first().and_then(Value::as_str) else {
                continue;
            };
            if let Some(id) = cell.get(1).and_then(Value::as_u64) {
                highlight = id;
            }
            let repeat = cell.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
            for _ in 0..repeat {
                if let Some(slot) = line.get_mut(column) {
                    *slot = Cell {
                        text: text.to_owned(),
                        highlight,
                    };
                }
                column += 1;
            }
        }
    }

    fn scroll(&mut self, args: &[Value]) {
        if args.first().and_then(Value::as_i64) != Some(1) {
            return;
        }
        let Some(top) = args
            .get(1)
            .and_then(Value::as_u64)
            .map(|value| value as usize)
        else {
            return;
        };
        let Some(bottom) = args
            .get(2)
            .and_then(Value::as_u64)
            .map(|value| value as usize)
        else {
            return;
        };
        let Some(left) = args
            .get(3)
            .and_then(Value::as_u64)
            .map(|value| value as usize)
        else {
            return;
        };
        let Some(right) = args
            .get(4)
            .and_then(Value::as_u64)
            .map(|value| value as usize)
        else {
            return;
        };
        let Some(rows) = args.get(5).and_then(Value::as_i64) else {
            return;
        };
        let Some(columns) = args.get(6).and_then(Value::as_i64) else {
            return;
        };
        let old = self.cells.clone();

        for row in top..bottom.min(self.height) {
            for column in left..right.min(self.width) {
                let source_row = row as i64 + rows;
                let source_column = column as i64 + columns;
                // Rows scrolled in from outside the region are left for Neovim to redraw.
                if source_row >= top as i64
                    && source_row < bottom as i64
                    && source_column >= left as i64
                    && source_column < right as i64
                {
                    self.cells[row][column] =
                        old[source_row as usize][source_column as usize].clone();
                }
            }
        }
    }

    fn set_default_colors(&mut self, args: &[Value]) {
        let color = |index: usize| {
            args.get(index)
                .and_then(Value::as_i64)
                .filter(|value| *value >= 0)
                .map(|value| value as u32)
        };
        self.foreground = color(0);
        self.background = color(1);
    }

    fn define_highlight(&mut self, args: &[Value]) {
        let Some(id) = args.first().and_then(Value::as_u64) else {
            return;
        };
        let Some(attributes) = args.get(1).and_then(Value::as_map) else {
            return;
        };
        let value = |name: &str| {
            attributes
                .iter()
                .find(|(key, _)| key.as_str() == Some(name))
                .map(|(_, value)| value)
        };
        let flag = |name: &str| value(name).and_then(Value::as_bool).unwrap_or(false);
        let color = |name: &str| {
            value(name)
                .and_then(Value::as_u64)
                .map(|value| value as u32)
        };
        self.highlights.insert(
            id,
            Highlight {
                foreground: color("foreground"),
                background: color("background"),
                special: color("special"),
                bold: flag("bold"),
                italic: flag("italic"),
                underline: flag("underline")
                    || flag("undercurl")
                    || flag("underdouble")
                    || flag("underdotted")
                    || flag("underdashed"),
                strikethrough: flag("strikethrough"),
                reverse: flag("reverse"),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::Duration};

    #[test]
    fn applies_linegrid_redraw() {
        let mut grid = Grid::default();
        let events = vec![
            Value::Array(vec![
                "default_colors_set".into(),
                Value::Array(vec![0xe6e9ed.into(), 0x0c0f12.into(), 0.into()]),
            ]),
            Value::Array(vec![
                "hl_attr_define".into(),
                Value::Array(vec![
                    1.into(),
                    Value::Map(vec![
                        ("foreground".into(), 0x88c0d0.into()),
                        ("bold".into(), true.into()),
                    ]),
                    Value::Map(Vec::new()),
                    Value::Array(Vec::new()),
                ]),
            ]),
            Value::Array(vec![
                "grid_resize".into(),
                Value::Array(vec![1.into(), 5.into(), 2.into()]),
            ]),
            Value::Array(vec![
                "grid_line".into(),
                Value::Array(vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    Value::Array(vec![
                        Value::Array(vec!["A".into(), 1.into()]),
                        Value::Array(vec![" ".into(), 0.into(), 2.into()]),
                        Value::Array(vec!["B".into(), 0.into()]),
                    ]),
                    false.into(),
                ]),
            ]),
            Value::Array(vec!["flush".into(), Value::Array(vec![])]),
            Value::Array(vec![
                "mode_change".into(),
                Value::Array(vec!["normal".into(), 0.into()]),
            ]),
        ];

        assert!(grid.apply_redraw(&events));
        assert_eq!(grid.lines().next().unwrap().0, "A  B");
        assert_eq!(grid.colors(), (0xe6e9ed, 0x0c0f12));
        assert_eq!(
            grid.row_runs(0),
            [
                GridRun {
                    column: 0,
                    width: 1,
                    text: "A".into(),
                    highlight: ResolvedHighlight {
                        foreground: 0x88c0d0,
                        background: None,
                        special: 0x88c0d0,
                        bold: true,
                        italic: false,
                        underline: false,
                        strikethrough: false,
                    },
                },
                GridRun {
                    column: 1,
                    width: 4,
                    text: "  B ".into(),
                    highlight: grid.resolve(0),
                },
            ]
        );
        assert!(grid.is_normal());

        grid.apply_redraw(&[Value::Array(vec![
            "grid_line".into(),
            Value::Array(vec![
                1.into(),
                1.into(),
                0.into(),
                Value::Array(vec![Value::Array(vec!["Z".into(), 0.into()])]),
                false.into(),
            ]),
        ])]);
        grid.apply_redraw(&[Value::Array(vec![
            "grid_scroll".into(),
            Value::Array(vec![
                1.into(),
                0.into(),
                2.into(),
                0.into(),
                5.into(),
                1.into(),
                0.into(),
            ]),
        ])]);
        assert_eq!(grid.lines().next().unwrap().0, "Z");
    }

    #[test]
    fn positions_wide_cells_and_resolves_reverse_video() {
        let mut grid = Grid::default();
        grid.apply_redraw(&[
            Value::Array(vec![
                "hl_attr_define".into(),
                Value::Array(vec![
                    7.into(),
                    Value::Map(vec![("reverse".into(), true.into())]),
                ]),
            ]),
            Value::Array(vec![
                "grid_resize".into(),
                Value::Array(vec![1.into(), 6.into(), 1.into()]),
            ]),
            Value::Array(vec![
                "grid_line".into(),
                Value::Array(vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    Value::Array(vec![
                        Value::Array(vec!["中".into(), 0.into()]),
                        Value::Array(vec!["".into(), 0.into()]),
                        Value::Array(vec!["a".into(), 0.into()]),
                        Value::Array(vec!["b".into(), 7.into()]),
                    ]),
                    false.into(),
                ]),
            ]),
            Value::Array(vec![
                "mode_info_set".into(),
                Value::Array(vec![
                    true.into(),
                    Value::Array(vec![
                        Value::Map(vec![("cursor_shape".into(), "block".into())]),
                        Value::Map(vec![
                            ("cursor_shape".into(), "vertical".into()),
                            ("cell_percentage".into(), 25.into()),
                        ]),
                    ]),
                ]),
            ]),
            Value::Array(vec![
                "mode_change".into(),
                Value::Array(vec!["insert".into(), 1.into()]),
            ]),
        ]);
        let runs = grid.row_runs(0);
        assert_eq!(
            runs.iter()
                .map(|run| (run.column, run.width, run.text.as_str()))
                .collect::<Vec<_>>(),
            [(0, 2, "中"), (2, 1, "a"), (3, 1, "b")]
        );
        assert_eq!(runs[2].highlight.foreground, DEFAULT_BACKGROUND);
        assert_eq!(runs[2].highlight.background, Some(DEFAULT_FOREGROUND));
        let cursor = grid.visible_cursor().unwrap();
        assert_eq!((cursor.width, cursor.text.as_str()), (2, "中"));
        assert_eq!(cursor.shape, CursorShape::Vertical(0.25));
        grid.apply_redraw(&[Value::Array(vec![
            "busy_start".into(),
            Value::Array(vec![]),
        ])]);
        assert!(grid.visible_cursor().is_none());
    }

    #[test]
    fn detects_normal_escape_mapping() {
        let maps = vec![vec![
            ("lhs".into(), "<Esc>".into()),
            ("rhs".into(), "<Cmd>nohlsearch<CR>".into()),
        ]];
        assert_eq!(
            escape_mapping(&maps).as_deref(),
            Some("<Cmd>nohlsearch<CR>")
        );
        assert!(escape_mapping(&[]).is_none());
    }

    #[test]
    #[ignore = "requires the external Neovim installation"]
    fn starts_neovim_and_receives_redraw() {
        let client = Client::start(PathBuf::from("examples/tikz.md"), None, true, (120, 40));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut grid = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut grid = Grid::default();
                    let mut saw_buffer = false;
                    loop {
                        match client.events.recv().await.unwrap() {
                            Event::Redraw(events) => {
                                grid.apply_redraw(&events);
                                if grid.width > 0 && saw_buffer {
                                    break grid;
                                }
                            }
                            Event::BufferLines { lines, .. } => {
                                saw_buffer = lines.iter().any(|line| line.contains("TikZ preview"));
                                if grid.width > 0 && saw_buffer {
                                    break grid;
                                }
                            }
                            Event::Warning(_) | Event::Cursor { .. } => {}
                            Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                            Event::Exited => panic!("Neovim exited unexpectedly"),
                        }
                    }
                })
                .await
            })
            .expect("Neovim did not redraw within five seconds");

        assert!(grid.lines().any(|(line, _)| line.contains("TikZ preview")));
        assert!(grid.is_normal());

        client.resize(80, 24);
        runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while grid.size() != (80, 24) {
                        match client.events.recv().await.unwrap() {
                            Event::Redraw(events) => {
                                grid.apply_redraw(&events);
                            }
                            Event::BufferLines { .. } => {}
                            Event::Warning(_) | Event::Cursor { .. } => {}
                            Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                            Event::Exited => panic!("Neovim exited unexpectedly"),
                        }
                    }
                })
                .await
            })
            .expect("Neovim did not resize within five seconds");

        client.input("i");
        let mode = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    let mut mode = String::new();
                    while !mode.starts_with("insert") {
                        match client.events.recv().await.unwrap() {
                            Event::Redraw(events) => {
                                grid.apply_redraw(&events);
                                mode.clone_from(&grid.mode);
                            }
                            Event::BufferLines { .. } => {}
                            Event::Warning(_) | Event::Cursor { .. } => {}
                            Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                            Event::Exited => panic!("Neovim exited unexpectedly"),
                        }
                    }
                    mode
                })
                .await
            })
            .expect("Neovim did not enter Insert within five seconds");
        assert!(mode.starts_with("insert"));

        client.input("中文");
        runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !grid.lines().any(|(line, _)| line.contains("中文")) {
                        match client.events.recv().await.unwrap() {
                            Event::Redraw(events) => {
                                grid.apply_redraw(&events);
                            }
                            Event::BufferLines { .. } => {}
                            Event::Warning(_) | Event::Cursor { .. } => {}
                            Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                            Event::Exited => panic!("Neovim exited unexpectedly"),
                        }
                    }
                })
                .await
            })
            .expect("Neovim did not display committed Chinese text within five seconds");

        client.input("<Esc>");
        runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !grid.is_normal() {
                        match client.events.recv().await.unwrap() {
                            Event::Redraw(events) => {
                                grid.apply_redraw(&events);
                            }
                            Event::BufferLines { .. } => {}
                            Event::Warning(_) | Event::Cursor { .. } => {}
                            Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                            Event::Exited => panic!("Neovim exited unexpectedly"),
                        }
                    }
                })
                .await
            })
            .expect("Neovim did not return to Normal within five seconds");

        // Exercise literal key notation and input larger than Neovim's input queue.
        let literal = format!("<Esc>{}", "中文-".repeat(2048));
        client.input("A");
        client.input_text(&literal);
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match client.events.recv().await.unwrap() {
                        Event::BufferLines { lines, .. }
                            if lines.iter().any(|line| line.ends_with(&literal)) =>
                        {
                            break;
                        }
                        Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                        Event::Exited => panic!("Neovim exited before receiving all input"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("literal input was truncated or interpreted as keys");
        });
        client.input("<Esc>");
        assert!(client.close());
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match client.events.recv().await.unwrap() {
                        Event::CloseRefused(_) => break,
                        Event::Exited => panic!("closing discarded unsaved edits"),
                        Event::Error(error) => panic!("{error}"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("Neovim did not reject closing a modified buffer");
        });
        client.input(":qall!<CR>");
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(client.events.recv().await.unwrap(), Event::Exited) {}
            })
            .await
            .expect("Neovim exit was not reported");
        });
    }

    #[test]
    #[ignore = "requires the external Neovim installation"]
    fn refuses_swap_conflicts_without_overwriting_the_file() {
        let path = std::env::temp_dir().join(format!(
            "rusidian-swap-test-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "original\n").unwrap();
        let owner = Client::start(path.clone(), None, true, (120, 40));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match owner.events.recv().await.unwrap() {
                        Event::BufferLines { lines, .. }
                            if lines.iter().any(|line| line == "original") =>
                        {
                            break;
                        }
                        Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                        Event::Exited => panic!("owner Neovim exited unexpectedly"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("owner Neovim did not open the file");
        });
        owner.input("A unsaved<Esc>");
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match owner.events.recv().await.unwrap() {
                        Event::BufferLines { lines, .. }
                            if lines.iter().any(|line| line.contains("unsaved")) =>
                        {
                            break;
                        }
                        Event::Error(error) | Event::CloseRefused(error) => panic!("{error}"),
                        Event::Exited => panic!("owner Neovim exited unexpectedly"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("owner Neovim did not modify the buffer");
        });
        owner.input(":preserve<CR>");
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(100)).await });

        let contender = Client::start(path.clone(), None, true, (120, 40));
        let error = runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Event::Error(error) = contender.events.recv().await.unwrap() {
                            break error;
                        }
                    }
                })
                .await
            })
            .expect("second Neovim did not report the swap conflict");
        assert!(error.contains("swap 文件"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");

        owner.input(":qall!<CR>");
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !matches!(owner.events.recv().await.unwrap(), Event::Exited) {}
            })
            .await
            .expect("owner Neovim did not exit");
        });
        fs::remove_file(path).unwrap();
    }
}
