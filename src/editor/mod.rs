use std::{
    collections::HashMap,
    io::{stdout, Write},
    mem,
    time::Duration,
};

use crossterm::{
    cursor::{self, Hide, MoveTo, Show},
    event::{
        self, Event, EventStream, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    style::{self, Color},
    terminal::{self, Clear, ClearType},
    ExecutableCommand, QueueableCommand,
};
use futures::{future::FutureExt, select, StreamExt};
use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    buffer::Buffer,
    command,
    config::{Config, KeyAction},
    dispatcher::Dispatcher,
    log,
    lsp::{Diagnostic, InboundMessage, LspClient, ParsedNotification},
    plugin::{PluginRegistry, Runtime},
    theme::{Style, Theme},
    ui::{Component, FilePicker, Info, Picker},
};

use self::{action::GoToLinePosition, render::Change};

pub use action::Action;
pub use render::{RenderBuffer, StyleInfo};
pub use viewport::Viewport;

mod action;
mod render;
mod viewport;

pub static ACTION_DISPATCHER: Lazy<Dispatcher<PluginRequest, PluginResponse>> =
    Lazy::new(|| Dispatcher::new());

pub enum PluginRequest {
    Action(Action),
    EditorInfo(Option<i32>),
    OpenPicker(Option<String>, Option<i32>, Vec<serde_json::Value>),
}

pub struct PluginResponse(serde_json::Value);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    Search,
}

pub struct Editor {
    lsp: LspClient,
    config: Config,
    theme: Theme,
    plugin_registry: PluginRegistry,
    buffers: Vec<Buffer>,
    current_buffer_index: usize,
    stdout: std::io::Stdout,
    size: (u16, u16),
    vtop: usize,
    vleft: usize,
    cx: usize,
    cy: usize,
    vx: usize,
    mode: Mode,
    waiting_command: Option<String>,
    waiting_key_action: Option<KeyAction>,
    undo_actions: Vec<Action>,
    insert_undo_actions: Vec<Action>,
    command: String,
    search_term: String,
    last_error: Option<String>,
    current_dialog: Option<Box<dyn Component>>,
    repeater: Option<u16>,
    wrap: bool,
}

impl Editor {
    #[allow(unused)]
    pub fn with_size(
        lsp: LspClient,
        width: usize,
        height: usize,
        config: Config,
        theme: Theme,
        buffers: Vec<Buffer>,
    ) -> anyhow::Result<Self> {
        let mut stdout = stdout();
        let vx = buffers
            .get(0)
            .map(|b| b.len().to_string().len())
            .unwrap_or(0)
            + 2;
        let size = (width as u16, height as u16);

        let mut plugin_registry = PluginRegistry::new();

        Ok(Editor {
            lsp,
            config,
            theme,
            plugin_registry,
            buffers,
            current_buffer_index: 0,
            stdout,
            vtop: 0,
            vleft: 0,
            cx: 0,
            cy: 0,
            vx,
            mode: Mode::Normal,
            size,
            waiting_command: None,
            waiting_key_action: None,
            undo_actions: vec![],
            insert_undo_actions: vec![],
            command: String::new(),
            search_term: String::new(),
            last_error: None,
            current_dialog: None,
            repeater: None,
            wrap: true,
        })
    }

    pub fn new(
        lsp: LspClient,
        config: Config,
        theme: Theme,
        buffers: Vec<Buffer>,
    ) -> anyhow::Result<Self> {
        let size = terminal::size()?;
        Self::with_size(
            lsp,
            size.0 as usize,
            size.1 as usize,
            config,
            theme,
            buffers,
        )
    }

    pub fn vwidth(&self) -> usize {
        self.size.0 as usize
    }

    pub fn vheight(&self) -> usize {
        self.size.1 as usize - 2
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        (self.vx + self.cx, self.cy)
    }

    fn line_length(&self) -> usize {
        if let Some(line) = self.viewport_line(self.cy) {
            return line.len();
        }
        0
    }

    fn buffer_line(&self) -> usize {
        self.vtop + self.cy as usize
    }

    fn viewport_line(&self, n: usize) -> Option<String> {
        let buffer_line = self.vtop + n;
        self.current_buffer().get(buffer_line)
    }

    fn set_cursor_style(&mut self) -> anyhow::Result<()> {
        self.stdout.queue(match self.waiting_key_action {
            Some(_) => cursor::SetCursorStyle::SteadyUnderScore,
            _ => match self.mode {
                Mode::Normal => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Command => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Insert => cursor::SetCursorStyle::SteadyBar,
                Mode::Search => cursor::SetCursorStyle::DefaultUserShape,
            },
        })?;

        Ok(())
    }

    fn gutter_width(&self) -> usize {
        self.current_buffer().len().to_string().len() + 1
    }

    fn draw_gutter(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let width = self.gutter_width();
        if self.vx != self.gutter_width() + 1 {
            self.vx = self.gutter_width() + 1;
            self.render(buffer)?;
        }
        let fg = self
            .theme
            .gutter_style
            .fg
            .unwrap_or(self.theme.style.fg.expect("fg is defined for theme"));
        let bg = self
            .theme
            .gutter_style
            .bg
            .unwrap_or(self.theme.style.bg.expect("bg is defined for theme"));

        for n in 0..self.vheight() as usize {
            let line_number = n + 1 + self.vtop as usize;
            let text = if line_number <= self.current_buffer().len() {
                line_number.to_string()
            } else {
                " ".repeat(width)
            };

            buffer.set_text(
                0,
                n,
                &format!("{text:>width$} ", width = width,),
                &Style {
                    fg: Some(fg),
                    bg: Some(bg),
                    ..Default::default()
                },
            );
        }

        Ok(())
    }

    pub fn draw_cursor(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.set_cursor_style()?;
        self.check_bounds();

        // TODO: refactor this out to allow for dynamic setting of the cursor "target",
        // so we could transition from the editor to dialogs, to searches, etc.
        let cursor_pos = if let Some(current_dialog) = &self.current_dialog {
            current_dialog.cursor_position()
        } else if self.has_term() {
            Some((self.term().len() as u16 + 1, (self.size.1 - 1) as u16))
        } else {
            Some(((self.vx + self.cx) as u16, self.cy as u16))
        };

        if let Some((x, y)) = cursor_pos {
            self.stdout.queue(cursor::MoveTo(x, y))?;
        } else {
            self.stdout.queue(cursor::Hide)?;
        }
        self.draw_statusline(buffer);

        Ok(())
    }

    fn fill_line(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize, style: &Style) {
        let width = self.vwidth().saturating_sub(x);
        let line_fill = " ".repeat(width);
        buffer.set_text(x, y, &line_fill, style);
    }

    fn draw_line(&mut self, buffer: &mut RenderBuffer) {
        unimplemented!()
        // let line = self.viewport_line(self.cy).unwrap_or_default();
        // let style_info = self.highlight(&line).unwrap_or_default();
        // let default_style = self.theme.style.clone();
        //
        // let mut x = self.vx;
        // let mut iter = line.chars().enumerate().peekable();
        //
        // if line.is_empty() {
        //     self.fill_line(buffer, x, self.cy, &default_style);
        //     return;
        // }
        //
        // while let Some((pos, c)) = iter.next() {
        //     if c == '\n' || iter.peek().is_none() {
        //         if c != '\n' {
        //             buffer.set_char(x, self.cy, c, &default_style);
        //             x += 1;
        //         }
        //         self.fill_line(buffer, x, self.cy, &default_style);
        //         break;
        //     }
        //
        //     if x < self.vwidth() {
        //         if let Some(style) = determine_style_for_position(&style_info, pos) {
        //             buffer.set_char(x, self.cy, c, &style);
        //         } else {
        //             buffer.set_char(x, self.cy, c, &default_style);
        //         }
        //     }
        //     x += 1;
        // }
    }

    pub fn draw_viewport(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let mut viewport = self.current_buffer().viewport(
            &self.theme,
            self.vwidth(),
            self.vheight(),
            self.vleft,
            self.vtop,
        )?;
        viewport.set_wrap(self.wrap);
        viewport.set_left(self.vleft);

        viewport.draw(buffer, 0, 0)?;
        //
        // while y < vheight {
        //     self.fill_line(buffer, self.vx, y, &default_style);
        //     y += 1;
        // }

        // self.draw_gutter(buffer)?;

        Ok(())
    }

    pub fn draw_statusline(&mut self, buffer: &mut RenderBuffer) {
        let mode = format!(" {:?} ", self.mode).to_uppercase();
        let dirty = if self.current_buffer().is_dirty() {
            " [+] "
        } else {
            ""
        };
        let file = format!(" {}{}", self.current_buffer().name(), dirty);
        let pos = format!(" {}:{} ", self.vtop + self.cy + 1, self.cx + 1);

        let file_width = self.size.0 - mode.len() as u16 - pos.len() as u16 - 2;
        let y = self.size.1 as usize - 2;

        let transition_style = Style {
            fg: self.theme.statusline_style.outer_style.bg,
            bg: self.theme.statusline_style.inner_style.bg,
            ..Default::default()
        };

        buffer.set_text(0, y, &mode, &self.theme.statusline_style.outer_style);

        buffer.set_text(
            mode.len(),
            y,
            &self.theme.statusline_style.outer_chars[1].to_string(),
            &transition_style,
        );

        buffer.set_text(
            mode.len() + 1,
            y,
            &format!("{:<width$}", file, width = file_width as usize),
            &self.theme.statusline_style.inner_style,
        );

        buffer.set_text(
            mode.len() + 1 + file_width as usize,
            y,
            &self.theme.statusline_style.outer_chars[2].to_string(),
            &transition_style,
        );

        buffer.set_text(
            mode.len() + 2 + file_width as usize,
            y,
            &pos,
            &self.theme.statusline_style.outer_style,
        );
    }

    fn draw_commandline(&mut self, buffer: &mut RenderBuffer) {
        let style = &self.theme.style;
        let y = self.size.1 as usize - 1;

        if !self.has_term() {
            let wc = if let Some(ref waiting_command) = self.waiting_command {
                waiting_command.clone()
            } else if let Some(ref repeater) = self.repeater {
                format!("{}", repeater)
            } else {
                String::new()
            };
            let wc = format!("{:<width$}", wc, width = 10);

            if let Some(ref last_error) = self.last_error {
                let error = format!("{:width$}", last_error, width = self.size.0 as usize);
                buffer.set_text(0, self.size.1 as usize - 1, &error, style);
            } else {
                let clear_line = " ".repeat(self.size.0 as usize - 10);
                buffer.set_text(0, y, &clear_line, style);
            }

            buffer.set_text(self.size.0 as usize - 10, y, &wc, style);

            return;
        }

        let text = if self.is_command() {
            &self.command
        } else {
            &self.search_term
        };
        let prefix = if self.is_command() { ":" } else { "/" };
        let cmdline = format!(
            "{}{:width$}",
            prefix,
            text,
            width = self.size.0 as usize - self.command.len() - 1
        );
        buffer.set_text(0, self.size.1 as usize - 1, &cmdline, style);
    }

    fn draw_diagnostics(&mut self, buffer: &mut RenderBuffer) {
        if !self.config.show_diagnostics {
            return;
        }

        let fg = adjust_color_brightness(self.theme.style.fg, -20);
        let bg = adjust_color_brightness(self.theme.style.bg, 10);

        let hint_style = Style {
            fg,
            bg,
            italic: true,
            ..Default::default()
        };

        let mut diagnostics_per_line = HashMap::new();
        for diag in self.visible_diagnostics() {
            let line = diagnostics_per_line
                .entry(diag.range.start.line)
                .or_insert_with(Vec::new);
            line.push(diag);
        }

        for (l, diags) in diagnostics_per_line {
            let line = self.current_buffer().get(l);
            let len = line.clone().map(|l| l.len()).unwrap_or(0);
            let y = l - self.vtop;
            let x = self.gutter_width() + len + 5;
            let msg = format!("■ {}", diags[0].message.lines().next().unwrap());
            buffer.set_text(x, y, &msg, &hint_style);
        }
    }

    fn draw_current_dialog(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if let Some(current_dialog) = &self.current_dialog {
            current_dialog.draw(buffer)?;
        }

        Ok(())
    }

    fn is_normal(&self) -> bool {
        matches!(self.mode, Mode::Normal)
    }

    fn is_insert(&self) -> bool {
        matches!(self.mode, Mode::Insert)
    }

    fn is_command(&self) -> bool {
        matches!(self.mode, Mode::Command)
    }

    fn is_search(&self) -> bool {
        matches!(self.mode, Mode::Search)
    }

    fn has_term(&self) -> bool {
        self.is_command() || self.is_search()
    }

    fn term(&self) -> &str {
        if self.is_command() {
            &self.command
        } else {
            &self.search_term
        }
    }

    // TODO: in neovim, when you are at an x position and you move to a shorter line, the cursor
    //       goes back to the max x but returns to the previous x position if the line is longer
    fn check_bounds(&mut self) {
        let line_length = self.line_length();

        if self.cx >= line_length && self.is_normal() {
            if line_length > 0 {
                self.cx = self.line_length() - 1;
            } else if self.is_normal() {
                self.cx = 0;
            }
        }
        if self.cx >= self.vwidth() {
            self.cx = self.vwidth() - 1;
        }

        // check if cy is after the end of the buffer
        // the end of the buffer is less than vtop + cy
        let line_on_buffer = self.cy as usize + self.vtop;
        if line_on_buffer > self.current_buffer().len().saturating_sub(1) {
            self.cy = self.current_buffer().len() - self.vtop - 1;
        }
    }

    async fn render_diff(
        &mut self,
        runtime: &mut Runtime,
        change_set: Vec<Change<'_>>,
    ) -> anyhow::Result<()> {
        // FIXME: find a better place for this, probably inside the modifying
        // functions on the Buffer struct
        if !change_set.is_empty() {
            self.plugin_registry
                .notify(
                    runtime,
                    "buffer:changed",
                    json!(self.current_buffer().contents()),
                )
                .await?;
        }

        for change in change_set {
            let x = change.x;
            let y = change.y;
            let cell = change.cell;
            self.stdout.queue(MoveTo(x as u16, y as u16))?;
            if let Some(bg) = cell.style.bg {
                self.stdout.queue(style::SetBackgroundColor(bg))?;
            } else {
                self.stdout
                    .queue(style::SetBackgroundColor(self.theme.style.bg.unwrap()))?;
            }
            if let Some(fg) = cell.style.fg {
                self.stdout.queue(style::SetForegroundColor(fg))?;
            } else {
                self.stdout
                    .queue(style::SetForegroundColor(self.theme.style.fg.unwrap()))?;
            }
            if cell.style.italic {
                self.stdout
                    .queue(style::SetAttribute(style::Attribute::Italic))?;
            } else {
                self.stdout
                    .queue(style::SetAttribute(style::Attribute::NoItalic))?;
            }
            self.stdout.queue(style::Print(cell.c))?;
        }

        self.set_cursor_style()?;
        self.stdout
            .queue(cursor::MoveTo((self.vx + self.cx) as u16, self.cy as u16))?
            .flush()?;

        Ok(())
    }

    // Draw the current render buffer to the terminal
    fn render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.draw_viewport(buffer)?;
        // self.draw_gutter(buffer)?;
        self.draw_statusline(buffer);

        self.stdout
            .queue(Clear(ClearType::All))?
            .queue(MoveTo(0, 0))?;

        let mut current_style = &self.theme.style;

        self.stdout
            .queue(style::SetBackgroundColor(current_style.bg.unwrap()))?;

        for cell in buffer.cells.iter() {
            if cell.style != *current_style {
                if let Some(bg) = cell.style.bg {
                    self.stdout.queue(style::SetBackgroundColor(bg))?;
                }
                if let Some(fg) = cell.style.fg {
                    self.stdout.queue(style::SetForegroundColor(fg))?;
                }
                if cell.style.italic {
                    self.stdout
                        .queue(style::SetAttribute(style::Attribute::Italic))?;
                } else {
                    self.stdout
                        .queue(style::SetAttribute(style::Attribute::NoItalic))?;
                }
                current_style = &cell.style;
            }

            self.stdout.queue(style::Print(cell.c))?;
        }

        self.draw_cursor(buffer)?;
        self.stdout.flush()?;

        Ok(())
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        terminal::enable_raw_mode()?;
        self.stdout
            .execute(event::EnableMouseCapture)?
            .execute(terminal::EnterAlternateScreen)?
            .execute(terminal::Clear(terminal::ClearType::All))?;

        let mut runtime = Runtime::new();
        for (name, path) in &self.config.plugins {
            let path = Config::path("plugins").join(path);
            self.plugin_registry
                .add(name, path.to_string_lossy().as_ref());
        }
        self.plugin_registry.initialize(&mut runtime).await?;

        let mut buffer = RenderBuffer::new(
            self.size.0 as usize,
            self.size.1 as usize,
            self.theme.style.clone(),
        );
        self.render(&mut buffer)?;

        let mut reader = EventStream::new();

        loop {
            let mut delay = futures_timer::Delay::new(Duration::from_millis(10)).fuse();
            let mut event = reader.next().fuse();

            select! {
                _ = delay => {
                    // handle responses from lsp
                    if let Some((msg, method)) = self.lsp.recv_response().await? {
                        if let Some(action) = self.handle_lsp_message(&msg, method) {
                            // TODO: handle quit
                            let current_buffer = buffer.clone();
                            self.execute(&action, &mut buffer, &mut runtime).await?;
                            self.redraw(&mut runtime, &current_buffer, &mut buffer).await?;
                        }
                    }

                    if let Some(req) = ACTION_DISPATCHER.try_recv_request() {
                        match req {
                            PluginRequest::Action(action) => {
                                let current_buffer = buffer.clone();
                                self.execute(&action, &mut buffer, &mut runtime).await?;
                                self.redraw(&mut runtime, &current_buffer, &mut buffer).await?;
                            }
                            PluginRequest::EditorInfo(id) => {
                                let info = serde_json::to_value(self.info())?;
                                let key = if let Some(id) = id {
                                    format!("editor:info:{}", id)
                                } else {
                                    "editor:info".to_string()
                                };
                                self.plugin_registry
                                    .notify(&mut runtime, &key, info)
                                    .await?;
                            }
                            PluginRequest::OpenPicker(title, id, items) => {
                                let current_buffer = buffer.clone();
                                let items = items.iter().map(|v| match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    val => val.to_string(),
                                }).collect();
                                self.execute(&Action::OpenPicker(title, items, id), &mut buffer, &mut runtime).await?;
                                self.redraw(&mut runtime, &current_buffer, &mut buffer).await?;
                            }
                        }
                    }
                }
                maybe_event = event => {
                    match maybe_event {
                        Some(Ok(ev)) => {
                            let current_buffer = buffer.clone();
                            self.check_bounds();

                            if let event::Event::Resize(width, height) = ev {
                                self.size = (width, height);
                                let max_y = height as usize - 2;
                                if self.cy > max_y - 1 {
                                    self.cy = max_y - 1;
                                }
                                buffer = RenderBuffer::new(
                                    self.size.0 as usize,
                                    self.size.1 as usize,
                                    self.theme.style.clone(),
                                );
                                self.render(&mut buffer)?;
                                continue;
                            }

                            if let Some(action) = self.handle_event(&ev) {
                                if self.handle_key_action(&ev, &action, &mut buffer, &mut runtime).await? {
                                    log!("requested to quit");
                                    break;
                                }
                            }

                            self.redraw(&mut runtime, &current_buffer, &mut buffer).await?;
                        },
                        Some(Err(error)) => {
                            log!("error: {error}");
                        },
                        None => {
                        }
                    }
                }
            }
        }

        Ok(())
    }

    #[async_recursion::async_recursion]
    async fn handle_key_action(
        &mut self,
        ev: &event::Event,
        action: &KeyAction,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        log!("Action: {action:?}");
        let quit = match action {
            KeyAction::Single(action) => self.execute(&action, buffer, runtime).await?,
            KeyAction::Multiple(actions) => {
                let mut quit = false;
                for action in actions {
                    if self.execute(&action, buffer, runtime).await? {
                        quit = true;
                        break;
                    }
                }
                quit
            }
            KeyAction::Nested(actions) => {
                if let Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                }) = ev
                {
                    self.waiting_command = Some(format!("{c}"));
                }
                self.waiting_key_action = Some(KeyAction::Nested(actions.clone()));
                false
            }
            KeyAction::Repeating(times, action) => {
                self.repeater = None;
                let mut quit = false;
                for _ in 0..*times as usize {
                    if self.handle_key_action(ev, action, buffer, runtime).await? {
                        quit = true;
                        break;
                    }
                }
                quit
            }
        };

        Ok(quit)
    }

    fn handle_lsp_message(
        &mut self,
        msg: &InboundMessage,
        method: Option<String>,
    ) -> Option<Action> {
        match msg {
            InboundMessage::Message(msg) => {
                if let Some(method) = method {
                    if method == "textDocument/definition" {
                        let result = match msg.result {
                            serde_json::Value::Array(ref arr) => arr[0].as_object().unwrap(),
                            serde_json::Value::Object(ref obj) => obj,
                            _ => return None,
                        };

                        if let Some(range) = result.get("range") {
                            if let Some(start) = range.get("start") {
                                if let Some(line) = start.get("line") {
                                    if let Some(character) = start.get("character") {
                                        let line = line.as_u64().unwrap() as usize;
                                        let character = character.as_u64().unwrap() as usize;
                                        return Some(Action::MoveTo(character, line + 1));
                                    }
                                }
                            }
                        }
                    }
                    if method == "textDocument/hover" {
                        log!("hover response: {msg:?}");
                        let result = match msg.result {
                            serde_json::Value::Array(ref arr) => arr[0].as_object().unwrap(),
                            serde_json::Value::Object(ref obj) => obj,
                            _ => return None,
                        };

                        if let Some(contents) = result.get("contents") {
                            if let Some(contents) = contents.as_object() {
                                if let Some(serde_json::Value::String(value)) =
                                    contents.get("value")
                                {
                                    let info = Info::new(self, value.clone());
                                    self.current_dialog = Some(Box::new(info));
                                    return Some(Action::ShowDialog);
                                }
                            }
                        }
                    }
                }
                None
            }
            InboundMessage::Notification(msg) => match msg {
                ParsedNotification::PublishDiagnostics(msg) => {
                    _ = self.current_buffer_mut().offer_diagnostics(&msg);
                    Some(Action::RefreshDiagnostics)
                }
            },
            InboundMessage::UnknownNotification(msg) => {
                log!("got an unhandled notification: {msg:#?}");
                None
            }
            InboundMessage::Error(error_msg) => {
                log!("got an error: {error_msg:?}");
                None
            }
            InboundMessage::ProcessingError(error_msg) => {
                self.last_error = Some(error_msg.to_string());
                None
            }
        }
    }

    async fn redraw(
        &mut self,
        runtime: &mut Runtime,
        current_buffer: &RenderBuffer,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        self.stdout.execute(Hide)?;
        self.draw_statusline(buffer);
        self.draw_commandline(buffer);
        self.draw_diagnostics(buffer);
        self.draw_current_dialog(buffer)?;
        self.render_diff(runtime, buffer.diff(&current_buffer))
            .await?;
        self.draw_cursor(buffer)?;
        self.stdout.execute(Show)?;
        Ok(())
    }

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if let Some(ka) = self.waiting_key_action.take() {
            self.waiting_command = None;
            return self.handle_waiting_command(ka, ev);
        }

        if let Some(current_dialog) = &mut self.current_dialog {
            return current_dialog.handle_event(ev);
        }

        match self.mode {
            Mode::Normal => self.handle_normal_event(ev),
            Mode::Insert => self.handle_insert_event(ev),
            Mode::Command => self.handle_command_event(ev),
            Mode::Search => self.handle_search_event(ev),
        }
    }

    fn handle_repeater(&mut self, ev: &event::Event) -> bool {
        if let Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            ..
        }) = ev
        {
            if !c.is_numeric() {
                return false;
            }

            if let Some(repeater) = self.repeater {
                let new_repeater = format!("{}{}", repeater, c).parse::<u16>().unwrap();
                self.repeater = Some(new_repeater);
            } else {
                self.repeater = Some(c.to_string().parse::<u16>().unwrap());
            }

            return true;
        }

        false
    }

    fn handle_command(&mut self, cmd: &str) -> Vec<Action> {
        self.command = String::new();
        self.waiting_command = None;
        self.repeater = None;
        self.last_error = None;

        if let Ok(line) = cmd.parse::<usize>() {
            return vec![Action::GoToLine(line)];
        }

        let commands = &["quit", "write", "buffer-next", "buffer-prev", "edit"];
        let parsed = command::parse(commands, cmd);

        log!("parsed: {parsed:?}");

        let Some(parsed) = parsed else {
            self.last_error = Some(format!("unknown command {cmd:?}"));
            return vec![];
        };

        let mut actions = vec![];
        for cmd in &parsed.commands {
            if cmd == "quit" {
                actions.push(Action::Quit(parsed.is_forced()));
            }

            if cmd == "write" {
                actions.push(Action::Save);
            }

            if cmd == "buffer-next" {
                actions.push(Action::NextBuffer);
            }

            if cmd == "buffer-prev" {
                actions.push(Action::PreviousBuffer);
            }

            if cmd == "edit" {
                if let Some(file) = parsed.args.get(0) {
                    actions.push(Action::OpenFile(file.clone()));
                }
            }
        }
        actions
    }

    fn handle_command_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(ref event) => {
                let code = event.code;
                let _modifiers = event.modifiers;

                match code {
                    KeyCode::Esc => {
                        return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                    }
                    KeyCode::Backspace => {
                        if self.command.len() < 2 {
                            self.command = String::new();
                        } else {
                            self.command = self.command[..self.command.len() - 1].to_string();
                        }
                    }
                    KeyCode::Enter => {
                        if self.command.trim().is_empty() {
                            return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                        }
                        return Some(KeyAction::Multiple(vec![
                            Action::EnterMode(Mode::Normal),
                            Action::Command(self.command.clone()),
                        ]));
                    }
                    KeyCode::Char(c) => {
                        self.command = format!("{}{c}", self.command);
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        None
    }

    fn handle_search_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        match ev {
            Event::Key(ref event) => {
                let code = event.code;
                let _modifiers = event.modifiers;

                match code {
                    KeyCode::Esc => {
                        self.search_term = String::new();
                        return Some(KeyAction::Single(Action::EnterMode(Mode::Normal)));
                    }
                    KeyCode::Backspace => {
                        if self.search_term.len() < 2 {
                            self.search_term = String::new();
                        } else {
                            self.search_term =
                                self.search_term[..self.search_term.len() - 1].to_string();
                        }
                    }
                    KeyCode::Enter => {
                        return Some(KeyAction::Multiple(vec![
                            Action::EnterMode(Mode::Normal),
                            Action::FindNext,
                        ]));
                    }
                    KeyCode::Char(c) => {
                        self.search_term = format!("{}{c}", self.search_term);
                        // TODO: real-time search
                        // return Some(KeyAction::Search);
                    }
                    _ => {}
                }
            }
            _ => {}
        }

        None
    }

    fn handle_waiting_command(&mut self, ka: KeyAction, ev: &event::Event) -> Option<KeyAction> {
        let KeyAction::Nested(nested_mappings) = ka else {
            panic!("expected nested mappings");
        };

        self.event_to_key_action(&nested_mappings, &ev)
    }

    fn handle_insert_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let insert = self.config.keys.insert.clone();
        if let Some(ka) = self.event_to_key_action(&insert, &ev) {
            return Some(ka);
        }

        match ev {
            Event::Key(event) => match event.code {
                KeyCode::Char(c) => KeyAction::Single(Action::InsertCharAtCursorPos(c)).into(),
                _ => None,
            },
            _ => None,
        }
    }

    fn handle_normal_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        let normal = self.config.keys.normal.clone();
        self.event_to_key_action(&normal, &ev)
    }

    pub fn cleanup(&mut self) -> anyhow::Result<()> {
        self.stdout
            .execute(terminal::LeaveAlternateScreen)?
            .execute(event::DisableMouseCapture)?;
        terminal::disable_raw_mode()?;

        Ok(())
    }

    fn current_line_contents(&self) -> Option<String> {
        self.current_buffer().get(self.buffer_line())
    }

    fn previous_line_indentation(&self) -> usize {
        if self.buffer_line() > 0 {
            self.current_buffer()
                .get(self.buffer_line() - 1)
                .unwrap_or_default()
                .chars()
                .position(|c| !c.is_whitespace())
                .unwrap_or(0)
        } else {
            0
        }
    }

    fn current_line_indentation(&self) -> usize {
        self.current_line_contents()
            .unwrap_or_default()
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    #[async_recursion::async_recursion]
    async fn execute(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        self.last_error = None;
        match action {
            Action::Quit(force) => {
                if *force {
                    return Ok(true);
                }
                let modified_buffers = self.modified_buffers();
                if modified_buffers.is_empty() {
                    return Ok(true);
                }
                self.last_error = Some(format!(
                    "The following buffers have unwritten changes: {}",
                    modified_buffers.join(", ")
                ));
                return Ok(false);
            }
            Action::MoveUp => {
                if self.cy == 0 {
                    // scroll up
                    if self.vtop > 0 {
                        self.vtop -= 1;
                        self.draw_viewport(buffer)?;
                    }
                } else {
                    self.cy = self.cy.saturating_sub(1);
                    self.draw_cursor(buffer)?;
                }
            }
            Action::MoveDown => {
                if self.vtop + self.cy < self.current_buffer().len() - 1 {
                    self.cy += 1;
                    if self.cy >= self.vheight() {
                        // scroll if possible
                        self.vtop += 1;
                        self.cy -= 1;
                        self.draw_viewport(buffer)?;
                    }
                } else {
                    self.draw_cursor(buffer)?;
                }
            }
            Action::MoveLeft => {
                self.cx = self.cx.saturating_sub(1);
                if self.cx < self.vleft {
                    self.cx = self.vleft;
                } else {
                }
            }
            Action::MoveRight => {
                self.cx += 1;
            }
            Action::MoveToLineStart => {
                self.cx = 0;
            }
            Action::MoveToLineEnd => {
                self.cx = self.line_length().saturating_sub(1);
            }
            Action::PageUp => {
                if self.vtop > 0 {
                    self.vtop = self.vtop.saturating_sub(self.vheight() as usize);
                    self.draw_viewport(buffer)?;
                }
            }
            Action::PageDown => {
                if self.current_buffer().len() > self.vtop + self.vheight() as usize {
                    self.vtop += self.vheight() as usize;
                    self.draw_viewport(buffer)?;
                }
            }
            Action::EnterMode(new_mode) => {
                // TODO: with the introduction of new modes, maybe this transtion
                // needs to be widened to anything -> insert and anything -> normal
                if self.is_normal() && matches!(new_mode, Mode::Insert) {
                    self.insert_undo_actions = Vec::new();
                }
                if self.is_insert() && matches!(new_mode, Mode::Normal) {
                    if !self.insert_undo_actions.is_empty() {
                        let actions = mem::take(&mut self.insert_undo_actions);
                        self.undo_actions.push(Action::UndoMultiple(actions));
                    }
                }
                if self.has_term() {
                    self.draw_commandline(buffer);
                }

                if matches!(new_mode, Mode::Search) {
                    self.search_term = String::new();
                }

                self.mode = *new_mode;
                self.draw_statusline(buffer);
            }
            Action::InsertCharAtCursorPos(c) => {
                self.insert_undo_actions
                    .push(Action::DeleteCharAt(self.cx, self.buffer_line()));
                let line = self.buffer_line();
                let cx = self.cx;

                self.current_buffer_mut().insert(cx, line, *c);
                self.notify_change().await?;
                self.cx += 1;
                self.draw_line(buffer);
            }
            Action::DeleteCharAt(x, y) => {
                self.current_buffer_mut().remove(*x, *y);
                self.notify_change().await?;
                self.draw_line(buffer);
            }
            Action::DeleteCharAtCursorPos => {
                let cx = self.cx;
                let line = self.buffer_line();

                self.current_buffer_mut().remove(cx, line);
                self.notify_change().await?;
                self.draw_line(buffer);
            }
            Action::ReplaceLineAt(y, contents) => {
                self.current_buffer_mut()
                    .replace_line(*y, contents.to_string());
                self.notify_change().await?;
                self.draw_line(buffer);
            }
            Action::InsertNewLine => {
                self.insert_undo_actions.extend(vec![
                    Action::MoveTo(self.cx, self.buffer_line() + 1),
                    Action::DeleteLineAt(self.buffer_line() + 1),
                    Action::ReplaceLineAt(
                        self.buffer_line(),
                        self.current_line_contents().unwrap_or_default(),
                    ),
                ]);
                let spaces = self.current_line_indentation();

                let current_line = self.current_line_contents().unwrap_or_default();
                let before_cursor = current_line[..self.cx].to_string();
                let after_cursor = current_line[self.cx..].to_string();

                let line = self.buffer_line();
                self.current_buffer_mut().replace_line(line, before_cursor);
                self.notify_change().await?;

                self.cx = spaces;
                self.cy += 1;

                let new_line = format!("{}{}", " ".repeat(spaces), &after_cursor);
                let line = self.buffer_line();

                self.current_buffer_mut().insert_line(line, new_line);
                self.draw_viewport(buffer)?;
            }
            Action::SetWaitingKeyAction(key_action) => {
                self.waiting_key_action = Some(*(key_action.clone()));
            }
            Action::DeleteCurrentLine => {
                let line = self.buffer_line();
                let contents = self.current_line_contents();

                self.current_buffer_mut().remove_line(line);
                self.notify_change().await?;
                self.undo_actions.push(Action::InsertLineAt(line, contents));
                self.draw_viewport(buffer)?;
            }
            Action::Undo => {
                if let Some(undo_action) = self.undo_actions.pop() {
                    self.execute(&undo_action, buffer, runtime).await?;
                }
            }
            Action::UndoMultiple(actions) => {
                for action in actions.iter().rev() {
                    self.execute(action, buffer, runtime).await?;
                }
            }
            Action::InsertLineAt(y, contents) => {
                if let Some(contents) = contents {
                    self.current_buffer_mut()
                        .insert_line(*y, contents.to_string());
                    self.notify_change().await?;
                    self.draw_viewport(buffer)?;
                }
            }
            Action::MoveLineToViewportCenter => {
                let viewport_center = self.vheight() / 2;
                let distance_to_center = self.cy as isize - viewport_center as isize;

                if distance_to_center > 0 {
                    // if distance > 0 we need to scroll up
                    let distance_to_center = distance_to_center.abs() as usize;
                    if self.vtop > distance_to_center {
                        let new_vtop = self.vtop + distance_to_center;
                        self.vtop = new_vtop;
                        self.cy = viewport_center;
                        self.draw_viewport(buffer)?;
                    }
                } else if distance_to_center < 0 {
                    // if distance < 0 we need to scroll down
                    let distance_to_center = distance_to_center.abs() as usize;
                    let new_vtop = self.vtop.saturating_sub(distance_to_center);
                    let distance_to_go = self.vtop as usize + distance_to_center;
                    if self.current_buffer().len() > distance_to_go && new_vtop != self.vtop {
                        self.vtop = new_vtop;
                        self.cy = viewport_center;
                        self.draw_viewport(buffer)?;
                    }
                }
            }
            Action::InsertLineBelowCursor => {
                self.undo_actions
                    .push(Action::DeleteLineAt(self.buffer_line() + 1));

                let leading_spaces = self.current_line_indentation();
                let line = self.buffer_line();
                self.current_buffer_mut()
                    .insert_line(line + 1, " ".repeat(leading_spaces));
                self.notify_change().await?;
                self.cy += 1;
                self.cx = leading_spaces;
                self.draw_viewport(buffer)?;
            }
            Action::InsertLineAtCursor => {
                self.undo_actions
                    .push(Action::DeleteLineAt(self.buffer_line()));

                // if the current line is empty, let's use the indentation from the line above
                let leading_spaces = if let Some(line) = self.current_line_contents() {
                    if line.is_empty() {
                        self.previous_line_indentation()
                    } else {
                        self.current_line_indentation()
                    }
                } else {
                    self.previous_line_indentation()
                };

                let line = self.buffer_line();
                self.current_buffer_mut()
                    .insert_line(line, " ".repeat(leading_spaces));
                self.notify_change().await?;
                self.cx = leading_spaces;
                self.draw_viewport(buffer)?;
            }
            Action::MoveToTop => {
                self.vtop = 0;
                self.cy = 0;
                self.draw_viewport(buffer)?;
            }
            Action::MoveToBottom => {
                if self.current_buffer().len() > self.vheight() as usize {
                    self.cy = self.vheight() - 1;
                    self.vtop = self.current_buffer().len() - self.vheight() as usize;
                    self.draw_viewport(buffer)?;
                } else {
                    self.cy = self.current_buffer().len() - 1;
                }
            }
            Action::DeleteLineAt(y) => {
                self.current_buffer_mut().remove_line(*y);
                self.notify_change().await?;
                self.draw_viewport(buffer)?;
            }
            Action::DeletePreviousChar => {
                if self.cx > 0 {
                    self.cx -= 1;
                    let cx = self.cx;
                    let line = self.buffer_line();
                    self.current_buffer_mut().remove(cx, line);
                    self.notify_change().await?;
                    self.draw_line(buffer);
                }
            }
            Action::DumpBuffer => {
                log!("{buffer}", buffer = buffer.dump());
            }
            Action::Command(cmd) => {
                log!("Handling command: {cmd}");

                for action in self.handle_command(cmd) {
                    self.last_error = None;
                    if self.execute(&action, buffer, runtime).await? {
                        return Ok(true);
                    }
                }
            }
            Action::PluginCommand(cmd) => {
                self.plugin_registry.execute(runtime, cmd).await?;
            }
            Action::GoToLine(line) => {
                self.go_to_line(*line, buffer, runtime, GoToLinePosition::Center)
                    .await?
            }
            Action::GoToDefinition => {
                if let Some(file) = self.current_buffer().file.clone() {
                    self.lsp
                        .goto_definition(&file, self.cx, self.cy + self.vtop)
                        .await?;
                }
            }
            Action::Hover => {
                if let Some(file) = self.current_buffer().file.clone() {
                    self.lsp.hover(&file, self.cx, self.cy + self.vtop).await?;
                }
            }
            Action::MoveTo(x, y) => {
                self.go_to_line(*y, buffer, runtime, GoToLinePosition::Center)
                    .await?;
                self.cx = std::cmp::min(*x, self.line_length().saturating_sub(1));
            }
            Action::SetCursor(x, y) => {
                self.cx = *x;
                self.cy = *y;
            }
            Action::ScrollUp => {
                let scroll_lines = self.config.mouse_scroll_lines.unwrap_or(3);
                if self.vtop > scroll_lines {
                    self.vtop -= scroll_lines;
                    let desired_cy = self.cy + scroll_lines;
                    if desired_cy <= self.vheight() {
                        self.cy = desired_cy;
                    }
                    self.draw_viewport(buffer)?;
                }
            }
            Action::ScrollDown => {
                if self.current_buffer().len() > self.vtop + self.vheight() as usize {
                    self.vtop += self.config.mouse_scroll_lines.unwrap_or(3);
                    let desired_cy = self
                        .cy
                        .saturating_sub(self.config.mouse_scroll_lines.unwrap_or(3));
                    self.cy = desired_cy;
                    self.draw_viewport(buffer)?;
                }
            }
            Action::MoveToNextWord => {
                let next_word = self
                    .current_buffer()
                    .find_next_word((self.cx, self.buffer_line()));

                if let Some((x, y)) = next_word {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Top)
                        .await?;
                    self.draw_cursor(buffer)?;
                }
            }
            Action::MoveToPreviousWord => {
                let previous_word = self
                    .current_buffer()
                    .find_prev_word((self.cx, self.buffer_line()));

                if let Some((x, y)) = previous_word {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Top)
                        .await?;
                    self.draw_cursor(buffer)?;
                }
            }
            Action::MoveLineToViewportBottom => {
                let line = self.buffer_line();
                if line > self.vtop + self.vheight() {
                    self.vtop = line - self.vheight();
                    self.cy = self.vheight() - 1;
                    self.draw_viewport(buffer)?;
                }
            }
            Action::InsertTab => {
                // TODO: Tab configuration
                let tabsize = 4;
                let cx = self.cx;
                let line = self.buffer_line();
                self.current_buffer_mut()
                    .insert_str(cx, line, &" ".repeat(tabsize));
                self.notify_change().await?;
                self.cx += tabsize;
                self.draw_line(buffer);
            }
            Action::Save => match self.current_buffer_mut().save() {
                Ok(msg) => {
                    // TODO: use last_message instead of last_error
                    self.last_error = Some(msg);
                }
                Err(e) => {
                    self.last_error = Some(e.to_string());
                }
            },
            Action::FindPrevious => {
                if let Some((x, y)) = self
                    .current_buffer()
                    .find_prev(&self.search_term, (self.cx, self.vtop + self.cy))
                {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Center)
                        .await?;
                }
            }
            Action::FindNext => {
                if let Some((x, y)) = self
                    .current_buffer()
                    .find_next(&self.search_term, (self.cx, self.vtop + self.cy))
                {
                    self.cx = x;
                    self.go_to_line(y + 1, buffer, runtime, GoToLinePosition::Center)
                        .await?;
                }
            }
            Action::DeleteWord => {
                let cx = self.cx;
                let line = self.buffer_line();
                self.current_buffer_mut().delete_word((cx, line));
                self.notify_change().await?;
                self.draw_line(buffer);
            }
            Action::NextBuffer => {
                let new_index = if self.current_buffer_index < self.buffers.len() - 1 {
                    self.current_buffer_index + 1
                } else {
                    0
                };
                self.set_current_buffer(buffer, new_index)?;
            }
            Action::PreviousBuffer => {
                let new_index = if self.current_buffer_index > 0 {
                    self.current_buffer_index - 1
                } else {
                    self.buffers.len() - 1
                };
                self.set_current_buffer(buffer, new_index)?;
            }
            Action::OpenBuffer(name) => {
                if let Some(index) = self.buffers.iter().position(|b| b.name() == *name) {
                    self.set_current_buffer(buffer, index)?;
                }
            }
            Action::OpenFile(path) => {
                let new_buffer =
                    match Buffer::from_file(&mut self.lsp, Some(path.to_string())).await {
                        Ok(buffer) => buffer,
                        Err(e) => {
                            self.last_error = Some(e.to_string());
                            return Ok(false);
                        }
                    };
                self.buffers.push(new_buffer);
                self.set_current_buffer(buffer, self.buffers.len() - 1)?;
                self.render(buffer)?;
            }
            Action::FilePicker => {
                let file_picker = FilePicker::new(&self, std::env::current_dir()?)?;
                file_picker.draw(buffer)?;

                self.current_dialog = Some(Box::new(file_picker));
            }
            Action::ShowDialog => {
                if let Some(dialog) = &mut self.current_dialog {
                    dialog.draw(buffer)?;
                }
            }
            Action::CloseDialog => {
                self.current_dialog = None;
                self.draw_viewport(buffer)?;
            }
            Action::RefreshDiagnostics => {
                self.draw_diagnostics(buffer);
            }
            Action::Print(msg) => {
                self.last_error = Some(msg.clone());
            }
            Action::OpenPicker(title, items, id) => {
                let picker = Picker::new(title.clone(), &self, items, *id);
                picker.draw(buffer)?;

                self.current_dialog = Some(Box::new(picker));
            }
            Action::Picked(item, id) => {
                log!("picked: {item} - {id:?}");
                if let Some(id) = id {
                    self.plugin_registry
                        .notify(
                            runtime,
                            &format!("picker:selected:{}", id),
                            serde_json::Value::String(item.clone()),
                        )
                        .await?;
                }
            }
            Action::Suspend => {
                self.stdout.execute(terminal::LeaveAlternateScreen)?;
                let pid = Pid::from_raw(0);
                let _ = signal::kill(pid, Signal::SIGSTOP);
                self.stdout.execute(terminal::EnterAlternateScreen)?;
                self.render(buffer)?;
            }
            Action::ToggleWrap => {
                self.wrap = !self.wrap;
                self.draw_viewport(buffer)?;
            }
            Action::DecreaseLeft => {
                self.wrap = false;
                self.vleft = self.vleft.saturating_sub(1);
                self.draw_viewport(buffer)?;
            }
            Action::IncreaseLeft => {
                self.wrap = false;
                self.vleft = self.vleft + 1;
                self.draw_viewport(buffer)?;
            }
        }

        Ok(false)
    }

    async fn notify_change(&mut self) -> anyhow::Result<()> {
        let file = self.current_buffer().file.clone();
        if let Some(file) = &file {
            self.lsp
                .did_change(&file, &self.current_buffer().contents())
                .await?;
        }
        Ok(())
    }

    fn set_current_buffer(
        &mut self,
        render_buffer: &mut RenderBuffer,
        index: usize,
    ) -> anyhow::Result<()> {
        let vtop = self.vtop;
        let pos = (self.cx, self.cy);

        let buffer = self.current_buffer_mut();
        buffer.vtop = vtop;
        buffer.pos = pos;

        self.current_buffer_index = index;

        let (cx, cy) = self.current_buffer().pos;
        let vtop = self.current_buffer().vtop;

        log!(
            "new vtop = {vtop}, new pos = ({cx}, {cy})",
            vtop = vtop,
            cx = cx,
            cy = cy
        );
        self.cx = cx;
        self.cy = cy;
        self.vtop = vtop;

        self.draw_viewport(render_buffer)
    }

    async fn go_to_line(
        &mut self,
        line: usize,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
        pos: GoToLinePosition,
    ) -> anyhow::Result<()> {
        if line == 0 {
            self.execute(&Action::MoveToTop, buffer, runtime).await?;
            return Ok(());
        }

        if line <= self.current_buffer().len() {
            let y = line - 1;

            if self.is_within_viewport(y) {
                self.cy = y - self.vtop;
            } else if self.is_within_first_page(y) {
                self.vtop = 0;
                self.cy = y;
                self.draw_viewport(buffer)?;
            } else if self.is_within_last_page(y) {
                self.vtop = self.current_buffer().len() - self.vheight();
                self.cy = y - self.vtop;
                self.draw_viewport(buffer)?;
            } else {
                if matches!(pos, GoToLinePosition::Bottom) {
                    self.vtop = y - self.vheight();
                    self.cy = self.buffer_line() - self.vtop;
                } else {
                    self.vtop = y;
                    self.cy = 0;
                    if matches!(pos, GoToLinePosition::Center) {
                        self.execute(&Action::MoveLineToViewportCenter, buffer, runtime)
                            .await?;
                    }
                }

                // FIXME: this is wasteful when move to viewport center worked
                // but we have to account for the case where it didn't and also
                self.draw_viewport(buffer)?;
            }
        }

        Ok(())
    }

    fn is_within_viewport(&self, y: usize) -> bool {
        (self.vtop..self.vtop + self.vheight()).contains(&y)
    }

    fn is_within_last_page(&self, y: usize) -> bool {
        y > self.current_buffer().len() - self.vheight()
    }

    fn is_within_first_page(&self, y: usize) -> bool {
        y < self.vheight()
    }

    fn event_to_key_action(
        &mut self,
        mappings: &HashMap<String, KeyAction>,
        ev: &Event,
    ) -> Option<KeyAction> {
        if self.handle_repeater(ev) {
            return None;
        }

        let key_action = match ev {
            event::Event::Key(KeyEvent {
                code, modifiers, ..
            }) => {
                let key = match code {
                    KeyCode::Char(c) => format!("{c}"),
                    _ => format!("{code:?}"),
                };

                let key = match *modifiers {
                    KeyModifiers::CONTROL => format!("Ctrl-{key}"),
                    KeyModifiers::ALT => format!("Alt-{key}"),
                    _ => key,
                };

                mappings.get(&key).cloned()
            }
            event::Event::Mouse(mev) => match mev {
                MouseEvent {
                    kind, column, row, ..
                } => match kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        let x = (*column as usize).saturating_sub(self.gutter_width() + 1);
                        Some(KeyAction::Single(Action::MoveTo(
                            x,
                            self.vtop + *row as usize + 1,
                        )))
                    }
                    MouseEventKind::ScrollUp => Some(KeyAction::Single(Action::ScrollUp)),
                    MouseEventKind::ScrollDown => Some(KeyAction::Single(Action::ScrollDown)),
                    _ => None,
                },
            },
            _ => None,
        };

        if let Some(ref ka) = key_action {
            if let Some(ref repeater) = self.repeater {
                return Some(KeyAction::Repeating(repeater.clone(), Box::new(ka.clone())));
            }
        }

        key_action
    }

    fn visible_diagnostics(&self) -> Vec<&Diagnostic> {
        self.current_buffer()
            .diagnostics_for_lines(self.vtop, self.vtop + self.vheight())
    }

    fn current_buffer(&self) -> &Buffer {
        &self.buffers[self.current_buffer_index]
    }

    fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current_buffer_index]
    }

    fn modified_buffers(&self) -> Vec<&str> {
        self.buffers
            .iter()
            .filter(|b| b.is_dirty())
            .map(|b| b.name())
            .collect()
    }

    fn info(&self) -> EditorInfo {
        self.into()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EditorInfo {
    buffers: Vec<BufferInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BufferInfo {
    name: String,
    dirty: bool,
}

impl From<&Editor> for EditorInfo {
    fn from(editor: &Editor) -> Self {
        let buffers = editor.buffers.iter().map(|b| b.into()).collect();
        Self { buffers }
    }
}

impl From<&Buffer> for BufferInfo {
    fn from(buffer: &Buffer) -> Self {
        Self {
            name: buffer.name().to_string(),
            dirty: buffer.is_dirty(),
        }
    }
}

fn determine_style_for_position(style_info: &Vec<StyleInfo>, pos: usize) -> Option<Style> {
    if let Some(s) = style_info.iter().find(|si| si.contains(pos)) {
        return Some(s.style.clone());
    }

    None
}

fn adjust_color_brightness(color: Option<Color>, percentage: i32) -> Option<Color> {
    let Some(color) = color else {
        println!("None");
        return None;
    };

    if let Color::Rgb { r, g, b } = color {
        let adjust = |component: u8| -> u8 {
            let delta = (255.0 * (percentage as f32 / 100.0)) as i32;
            let new_component = component as i32 + delta;
            if new_component > 255 {
                255
            } else if new_component < 0 {
                0
            } else {
                new_component as u8
            }
        };

        let r = adjust(r);
        let g = adjust(g);
        let b = adjust(b);

        let new_color = Color::Rgb { r, g, b };

        Some(new_color)
    } else {
        Some(color)
    }
}

#[cfg(test)]
mod test {
    use crossterm::style::Color;

    use super::*;

    #[test]
    fn test_set_char() {
        let mut buffer = RenderBuffer::new(10, 10, Style::default());
        buffer.set_char(
            0,
            0,
            'a',
            &Style {
                fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
                bg: Some(Color::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                bold: false,
                italic: false,
            },
        );

        assert_eq!(buffer.cells[0].c, 'a');
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn test_set_char_outside_buffer() {
        let mut buffer = RenderBuffer::new(2, 2, Style::default());
        buffer.set_char(
            2,
            2,
            'a',
            &Style {
                fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
                bg: Some(Color::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                bold: false,
                italic: false,
            },
        );
    }

    #[test]
    fn test_set_text() {
        let mut buffer = RenderBuffer::new(3, 15, Style::default());
        buffer.set_text(
            2,
            2,
            "Hello, world!",
            &Style {
                fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
                bg: Some(Color::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                bold: false,
                italic: true,
            },
        );

        let start = 2 * 3 + 2;
        assert_eq!(buffer.cells[start].c, 'H');
        assert_eq!(
            buffer.cells[start].style.fg,
            Some(Color::Rgb { r: 0, g: 0, b: 0 })
        );
        assert_eq!(
            buffer.cells[start].style.bg,
            Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255
            })
        );
        assert_eq!(buffer.cells[start].style.italic, true);
        assert_eq!(buffer.cells[start + 1].c, 'e');
        assert_eq!(buffer.cells[start + 2].c, 'l');
        assert_eq!(buffer.cells[start + 3].c, 'l');
        assert_eq!(buffer.cells[start + 4].c, 'o');
        assert_eq!(buffer.cells[start + 5].c, ',');
        assert_eq!(buffer.cells[start + 6].c, ' ');
        assert_eq!(buffer.cells[start + 7].c, 'w');
        assert_eq!(buffer.cells[start + 8].c, 'o');
        assert_eq!(buffer.cells[start + 9].c, 'r');
        assert_eq!(buffer.cells[start + 10].c, 'l');
        assert_eq!(buffer.cells[start + 11].c, 'd');
        assert_eq!(buffer.cells[start + 12].c, '!');
    }

    #[test]
    fn test_diff() {
        let buffer1 = RenderBuffer::new(3, 3, Style::default());
        let mut buffer2 = RenderBuffer::new(3, 3, Style::default());

        buffer2.set_char(
            0,
            0,
            'a',
            &Style {
                fg: Some(Color::Rgb { r: 0, g: 0, b: 0 }),
                bg: Some(Color::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                bold: false,
                italic: false,
            },
        );

        let diff = buffer2.diff(&buffer1);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].x, 0);
        assert_eq!(diff[0].y, 0);
        assert_eq!(diff[0].cell.c, 'a');
    }

    #[test]
    #[ignore]
    fn test_draw_viewport() {
        todo!("pass lsp to with_size");
        // let contents = "hello\nworld!";

        // let config = Config::default();
        // let theme = Theme::default();
        // let buffer = Buffer::new(None, contents.to_string());
        // log!("buffer: {buffer:?}");
        // let mut render_buffer = RenderBuffer::new(10, 10, Style::default());
        //
        // let mut editor = Editor::with_size(10, 10, config, theme, buffer).unwrap();
        // editor.draw_viewport(&mut render_buffer).unwrap();
        //
        // println!("{}", render_buffer.dump());
        //
        // assert_eq!(render_buffer.cells[0].c, ' ');
        // assert_eq!(render_buffer.cells[1].c, '1');
        // assert_eq!(render_buffer.cells[2].c, ' ');
        // assert_eq!(render_buffer.cells[3].c, 'h');
        // assert_eq!(render_buffer.cells[4].c, 'e');
        // assert_eq!(render_buffer.cells[5].c, 'l');
        // assert_eq!(render_buffer.cells[6].c, 'l');
        // assert_eq!(render_buffer.cells[7].c, 'o');
        // assert_eq!(render_buffer.cells[8].c, ' ');
        // assert_eq!(render_buffer.cells[9].c, ' ');
    }

    #[test]
    fn test_buffer_diff() {
        let contents1 = vec![" 1:2 ".to_string()];
        let contents2 = vec![" 1:3 ".to_string()];

        let buffer1 = RenderBuffer::new_with_contents(5, 1, Style::default(), contents1);
        let buffer2 = RenderBuffer::new_with_contents(5, 1, Style::default(), contents2);
        let diff = buffer2.diff(&buffer1);

        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].x, 3);
        assert_eq!(diff[0].y, 0);
        assert_eq!(diff[0].cell.c, '3');
        //
        // let contents1 = vec![
        //     "fn main() {".to_string(),
        //     "    println!(\"Hello, world!\");".to_string(),
        //     "".to_string(),
        //     "}".to_string(),
        // ];
        // let contents2 = vec![
        //     "    println!(\"Hello, world!\");".to_string(),
        //     "".to_string(),
        //     "}".to_string(),
        //     "".to_string(),
        // ];
        // let buffer1 = RenderBuffer::new_with_contents(50, 4, Style::default(), contents1);
        // let buffer2 = RenderBuffer::new_with_contents(50, 4, Style::default(), contents2);
        //
        // let diff = buffer2.diff(&buffer1);
        // println!("{}", buffer1.dump());
    }
}
