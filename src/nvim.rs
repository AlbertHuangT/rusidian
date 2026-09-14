use async_channel::{Receiver, Sender};
use async_trait::async_trait;
use nvim_rs::{
    Handler, Neovim, Value, compat::tokio::Compat, create::tokio as create,
    uioptions::UiAttachOptions,
};
use std::{ops::Range, path::PathBuf, thread};
use tokio::{process::ChildStdin, sync::mpsc};

pub struct Client {
    pub events: Receiver<Event>,
    commands: mpsc::UnboundedSender<Command>,
}

pub enum Event {
    Redraw(Vec<Value>),
    BufferLines {
        first: usize,
        last: Option<usize>,
        lines: Vec<String>,
        more: bool,
    },
    Error(String),
}

enum Command {
    Input(String),
    Resize(i64, i64),
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
    pub fn start(path: PathBuf, clean: bool) -> Self {
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
                command.arg("--embed");
                if clean {
                    command.arg("--clean");
                }
                command.arg(path);
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
                if let Err(error) = nvim.ui_attach(120, 40, &options).await {
                    let _ = event_sender
                        .send(Event::Error(format!("无法连接 Neovim UI：{error}")))
                        .await;
                    return;
                }
                let buffer = match nvim.get_current_buf().await {
                    Ok(buffer) => buffer,
                    Err(error) => {
                        let _ = event_sender
                            .send(Event::Error(format!("无法读取 Neovim buffer：{error}")))
                            .await;
                        return;
                    }
                };
                if let Err(error) = buffer.attach(true, Vec::new()).await {
                    let _ = event_sender
                        .send(Event::Error(format!("无法监听 Neovim buffer：{error}")))
                        .await;
                    return;
                }

                tokio::spawn(async move {
                    let _ = io.await;
                });

                while let Some(command) = command_receiver.recv().await {
                    match command {
                        Command::Input(keys) => {
                            if let Err(error) = nvim.input(&keys).await {
                                let _ = event_sender
                                    .send(Event::Error(format!("Neovim 输入失败：{error}")))
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

    pub fn resize(&self, width: i64, height: i64) {
        let _ = self.commands.send(Command::Resize(width, height));
    }
}

#[derive(Clone, Debug, Default)]
pub struct Grid {
    width: usize,
    height: usize,
    cells: Vec<Vec<String>>,
    pub cursor: (usize, usize),
    mode: String,
}

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
                    "mode_change" => self.set_mode(args),
                    "flush" => flush = true,
                    _ => {}
                }
            }
        }
        flush
    }

    pub fn lines(&self) -> impl Iterator<Item = (String, Option<Range<usize>>)> + '_ {
        self.cells.iter().enumerate().map(|(row, cells)| {
            let content_end = cells
                .iter()
                .rposition(|cell| cell != " ")
                .map_or(0, |column| column + 1);
            let end = if row == self.cursor.0 {
                content_end.max(self.cursor.1.saturating_add(1).min(cells.len()))
            } else {
                content_end
            };
            let text = cells[..end].concat();
            let cursor = (row == self.cursor.0 && self.cursor.1 < end).then(|| {
                let start = cells[..self.cursor.1]
                    .iter()
                    .map(String::len)
                    .sum::<usize>();
                let len = cells[self.cursor.1].len().max(1);
                start..(start + len).min(text.len())
            });
            (text, cursor)
        })
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
        self.cells = vec![vec![" ".into(); self.width]; self.height];
    }

    fn clear(&mut self, args: &[Value]) {
        if args.first().and_then(Value::as_i64) == Some(1) {
            self.cells = vec![vec![" ".into(); self.width]; self.height];
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

        for cell in cells {
            let Some(cell) = cell.as_array() else {
                continue;
            };
            let Some(text) = cell.first().and_then(Value::as_str) else {
                continue;
            };
            let repeat = cell.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
            for _ in 0..repeat {
                if let Some(slot) = line.get_mut(column) {
                    *slot = text.to_owned();
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
                self.cells[row][column] = if source_row >= top as i64
                    && source_row < bottom as i64
                    && source_column >= left as i64
                    && source_column < right as i64
                {
                    old[source_row as usize][source_column as usize].clone()
                } else {
                    " ".into()
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn applies_linegrid_redraw() {
        let mut grid = Grid::default();
        let events = vec![
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
                        Value::Array(vec!["A".into(), 0.into()]),
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
    #[ignore = "requires the external Neovim installation"]
    fn starts_neovim_and_receives_redraw() {
        let client = Client::start(PathBuf::from("examples/tikz.md"), true);
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
                            Event::Error(error) => panic!("{error}"),
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
                            Event::Error(error) => panic!("{error}"),
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
                            Event::Error(error) => panic!("{error}"),
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
                            Event::Error(error) => panic!("{error}"),
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
                            Event::Error(error) => panic!("{error}"),
                        }
                    }
                })
                .await
            })
            .expect("Neovim did not return to Normal within five seconds");
    }
}
