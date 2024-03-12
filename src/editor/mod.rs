use std::{
    collections::HashMap,
    io::{stdout, Write},
    mem,
    sync::{Arc, Mutex, RwLock},
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
    buffer::{Buffer, SharedBuffer},
    command,
    config::{Config, KeyAction},
    dispatcher::Dispatcher,
    highlighter::Highlighter,
    log,
    lsp::{Diagnostic, InboundMessage, LspClient, ParsedNotification},
    plugin::{PluginRegistry, Runtime},
    theme::{self, Style, Theme},
    ui::{Component, FilePicker, Info, Picker},
};

use self::{
    action::{ActionEffect, GoToLinePosition},
    render::Change,
    window::Window,
};

pub use action::Action;
pub use render::{RenderBuffer, StyleInfo};
// pub use viewport::Viewport;

mod action;
mod render;
mod window;

pub static ACTION_DISPATCHER: Lazy<Dispatcher<PluginRequest, PluginResponse>> =
    Lazy::new(|| Dispatcher::new());

/// Maps file types to their specific LSP client
pub static LSP_CLIENTS: Lazy<RwLock<HashMap<String, LspClient>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub enum PluginRequest {
    Action(Action),
    EditorInfo(Option<i32>),
    OpenPicker(Option<String>, Option<i32>, Vec<serde_json::Value>),
}

pub struct PluginResponse(serde_json::Value);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Command,
    Search,
}

impl Mode {
    fn is_normal(&self) -> bool {
        self == &Mode::Normal
    }
}

// pub async fn run(config: Config, editor: &mut Editor) -> anyhow::Result<()> {
//     let theme_file = &Config::path("themes").join(&config.theme);
//     let theme = theme::parse_vscode_theme(&theme_file.to_string_lossy())?;
//
//     let size = terminal::size()?;
//
//     terminal::enable_raw_mode()?;
//
//     stdout()
//         .execute(event::EnableMouseCapture)?
//         .execute(terminal::EnterAlternateScreen)?
//         .execute(terminal::Clear(terminal::ClearType::All))?;
//
//     let mut runtime = Runtime::new();
//     let mut plugin_registry = PluginRegistry::new();
//     for (name, path) in &config.plugins {
//         let path = Config::path("plugins").join(path);
//         plugin_registry.add(name, path.to_string_lossy().as_ref());
//     }
//     plugin_registry.initialize(&mut runtime).await?;
//
//     let mut lsp = LspClient::start().await?;
//     lsp.initialize().await?;
//
//     let mut buffer = RenderBuffer::new(size.0 as usize, size.1 as usize, theme.style.clone());
//
//     let mut reader = EventStream::new();
//     let mut viewport = Viewport::new(
//         &theme,
//         editor.size.0 as usize,
//         editor.size.1 as usize - 2,
//         editor.vleft,
//         editor.vtop,
//     )?;
//     viewport.set_left(editor.vleft);
//     viewport.set_wrap(editor.wrap);
//
//     render(&theme, editor, &viewport, &mut buffer)?;
//
//     loop {
//         let mut delay = futures_timer::Delay::new(Duration::from_millis(10)).fuse();
//         let mut event = reader.next().fuse();
//
//         select! {
//             _ = delay => {
//                 // handle responses from lsp
//                 if let Some((msg, method)) = lsp.recv_response().await? {
//                     if let Some(action) = handle_lsp_message(editor, &msg, method) {
//                         // TODO: handle quit
//                         let current_buffer = buffer.clone();
//                         execute(&action, editor, &theme, &viewport, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
//                         redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
//                     }
//                 }
//
//                 if let Some(req) = ACTION_DISPATCHER.try_recv_request() {
//                     match req {
//                         PluginRequest::Action(action) => {
//                             let current_buffer = buffer.clone();
//                             execute(&action, editor, &theme, &viewport, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
//                             redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
//                         }
//                         PluginRequest::EditorInfo(id) => {
//                             let info = serde_json::to_value(editor.info())?;
//                             let key = if let Some(id) = id {
//                                 format!("editor:info:{}", id)
//                             } else {
//                                 "editor:info".to_string()
//                             };
//                             plugin_registry
//                                 .notify(&mut runtime, &key, info)
//                                 .await?;
//                         }
//                         PluginRequest::OpenPicker(title, id, items) => {
//                             let current_buffer = buffer.clone();
//                             let items = items.iter().map(|v| match v {
//                                 serde_json::Value::String(s) => s.clone(),
//                                 val => val.to_string(),
//                             }).collect();
//
//                             execute(&Action::OpenPicker(title, items, id), editor, &theme, &viewport, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
//                             redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
//                         }
//                     }
//                 }
//             }
//             maybe_event = event => {
//                 match maybe_event {
//                     Some(Ok(ev)) => {
//                         let current_buffer = buffer.clone();
//                         check_bounds(editor);
//
//                         if let event::Event::Resize(width, height) = ev {
//                             editor.size = (width, height);
//                             let max_y = height as usize - 2;
//                             if editor.cy > max_y - 1 {
//                                 editor.cy = max_y - 1;
//                             }
//                             buffer = RenderBuffer::new(
//                                 editor.size.0 as usize,
//                                 editor.size.1 as usize,
//                                 theme.style.clone(),
//                             );
//                             render(&theme, editor, &viewport, &mut buffer)?;
//                             continue;
//                         }
//
//                         if let Some(action) = handle_event(editor, &config, &ev) {
//                             if handle_key_action(&ev, &action, editor, &theme, &viewport, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await? {
//                                 log!("requested to quit");
//                                 break;
//                             }
//                         }
//
//                             redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
//                     },
//                     Some(Err(error)) => {
//                         log!("error: {error}");
//                     },
//                     None => {
//                     }
//                 }
//             }
//         }
//     }
//
//     Ok(())
// }

// fn handle_event(editor: &mut Editor, config: &Config, ev: &event::Event) -> Option<KeyAction> {
//     if let Some(ka) = editor.waiting_key_action.take() {
//         editor.waiting_command = None;
//         return editor.handle_waiting_command(ka, ev);
//     }
//
//     if let Some(current_dialog) = &mut editor.current_dialog {
//         return current_dialog.handle_event(ev);
//     }
//
//     match editor.mode {
//         Mode::Normal => handle_normal_event(editor, config, ev),
//         Mode::Insert => handle_insert_event(editor, config, ev),
//         Mode::Command => editor.handle_command_event(ev),
//         Mode::Search => editor.handle_search_event(ev),
//     }
// }

// async fn redraw(
//     theme: &Theme,
//     config: &Config,
//     editor: &mut Editor,
//     runtime: &mut Runtime,
//     plugin_registry: &mut PluginRegistry,
//     current_buffer: &RenderBuffer,
//     buffer: &mut RenderBuffer,
// ) -> anyhow::Result<()> {
//     stdout().execute(Hide)?;
//     draw_statusline(theme, editor, buffer);
//     draw_commandline(theme, editor, buffer);
//     draw_diagnostics(theme, config, editor, buffer);
//     draw_current_dialog(editor, buffer)?;
//     render_diff(
//         theme,
//         editor,
//         runtime,
//         plugin_registry,
//         buffer.diff(&current_buffer),
//     )
//     .await?;
//     draw_cursor(theme, editor, buffer)?;
//     stdout().execute(Show)?;
//
//     Ok(())
// }

// fn draw_current_dialog(editor: &Editor, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
//     if let Some(current_dialog) = &editor.current_dialog {
//         current_dialog.draw(buffer)?;
//     }
//
//     Ok(())
// }

// fn draw_diagnostics(
//     theme: &Theme,
//     config: &Config,
//     editor: &mut Editor,
//     buffer: &mut RenderBuffer,
// ) {
//     if !config.show_diagnostics {
//         return;
//     }
//
//     let fg = adjust_color_brightness(theme.style.fg, -20);
//     let bg = adjust_color_brightness(theme.style.bg, 10);
//
//     let hint_style = Style {
//         fg,
//         bg,
//         italic: true,
//         ..Default::default()
//     };
//
//     let mut diagnostics_per_line = HashMap::new();
//     for diag in editor.visible_diagnostics() {
//         let line = diagnostics_per_line
//             .entry(diag.range.start.line)
//             .or_insert_with(Vec::new);
//         line.push(diag);
//     }
//
//     for (l, diags) in diagnostics_per_line {
//         let line = editor.current_buffer().get(l);
//         let len = line.clone().map(|l| l.len()).unwrap_or(0);
//         let y = l - editor.vtop;
//         let x = len + 5;
//         let msg = format!("■ {}", diags[0].message.lines().next().unwrap());
//         buffer.set_text(x, y, &msg, &hint_style);
//     }
// }

// fn draw_commandline(theme: &Theme, editor: &mut Editor, buffer: &mut RenderBuffer) {
//     let style = &theme.style;
//     let y = editor.size.1 as usize - 1;
//
//     if !editor.has_term() {
//         let wc = if let Some(ref waiting_command) = editor.waiting_command {
//             waiting_command.clone()
//         } else if let Some(ref repeater) = editor.repeater {
//             format!("{}", repeater)
//         } else {
//             String::new()
//         };
//         let wc = format!("{:<width$}", wc, width = 10);
//
//         if let Some(ref last_error) = editor.last_error {
//             let error = format!("{:width$}", last_error, width = editor.size.0 as usize);
//             buffer.set_text(0, editor.size.1 as usize - 1, &error, style);
//         } else {
//             let clear_line = " ".repeat(editor.size.0 as usize - 10);
//             buffer.set_text(0, y, &clear_line, style);
//         }
//
//         buffer.set_text(editor.size.0 as usize - 10, y, &wc, style);
//
//         return;
//     }
//
//     let text = if editor.is_command() {
//         &editor.command
//     } else {
//         &editor.search_term
//     };
//     let prefix = if editor.is_command() { ":" } else { "/" };
//     let cmdline = format!(
//         "{}{:width$}",
//         prefix,
//         text,
//         width = editor.size.0 as usize - editor.command.len() - 1
//     );
//     buffer.set_text(0, editor.size.1 as usize - 1, &cmdline, style);
// }

// pub fn draw_statusline(theme: &Theme, editor: &Editor, buffer: &mut RenderBuffer) {
//     let mode = format!(" {:?} ", editor.mode).to_uppercase();
//     let dirty = if editor.current_buffer().is_dirty() {
//         " [+] "
//     } else {
//         ""
//     };
//     let file = format!(" {}{}", editor.current_buffer().name(), dirty);
//     let pos = format!(" {}:{} ", editor.vtop + editor.cy + 1, editor.cx + 1);
//
//     let file_width = editor
//         .size
//         .0
//         .saturating_sub(mode.len() as u16 + pos.len() as u16 + 2);
//     let y = editor.size.1 as usize - 2;
//
//     let transition_style = Style {
//         fg: theme.statusline_style.outer_style.bg,
//         bg: theme.statusline_style.inner_style.bg,
//         ..Default::default()
//     };
//
//     buffer.set_text(0, y, &mode, &theme.statusline_style.outer_style);
//
//     buffer.set_text(
//         mode.len(),
//         y,
//         &theme.statusline_style.outer_chars[1].to_string(),
//         &transition_style,
//     );
//
//     buffer.set_text(
//         mode.len() + 1,
//         y,
//         &format!("{:<width$}", file, width = file_width as usize),
//         &theme.statusline_style.inner_style,
//     );
//
//     buffer.set_text(
//         mode.len() + 1 + file_width as usize,
//         y,
//         &theme.statusline_style.outer_chars[2].to_string(),
//         &transition_style,
//     );
//
//     buffer.set_text(
//         mode.len() + 2 + file_width as usize,
//         y,
//         &pos,
//         &theme.statusline_style.outer_style,
//     );
// }

// #[async_recursion::async_recursion]
// async fn execute(
//     action: &Action,
//     mut editor: &mut Editor,
//     theme: &Theme,
//     viewport: &Viewport,
//     config: &Config,
//     mut buffer: &mut RenderBuffer,
//     mut lsp: &mut LspClient,
//     mut runtime: &mut Runtime,
//     mut plugin_registry: &mut PluginRegistry,
// ) -> anyhow::Result<bool> {
//     editor.last_error = None;
//     match action {
//         Action::Quit(force) => {
//             if *force {
//                 return Ok(true);
//             }
//             let modified_buffers = editor.modified_buffers();
//             if modified_buffers.is_empty() {
//                 return Ok(true);
//             }
//             editor.last_error = Some(format!(
//                 "The following buffers have unwritten changes: {}",
//                 modified_buffers.join(", ")
//             ));
//             return Ok(false);
//         }
//         Action::MoveUp => {
//             if editor.cy == 0 {
//                 // scroll up
//                 if editor.vtop > 0 {
//                     editor.vtop -= 1;
//                     viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//                 }
//             } else {
//                 editor.cy = editor.cy.saturating_sub(1);
//                 draw_cursor(theme, editor, buffer)?;
//             }
//         }
//         Action::MoveDown => {
//             if editor.vtop + editor.cy < editor.current_buffer().len() - 1 {
//                 editor.cy += 1;
//                 if editor.cy >= editor.vheight() {
//                     // scroll if possible
//                     editor.vtop += 1;
//                     editor.cy -= 1;
//                     viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//                 }
//             } else {
//                 draw_cursor(theme, editor, buffer)?;
//             }
//         }
//         Action::MoveLeft => {
//             editor.cx = editor.cx.saturating_sub(1);
//             if editor.cx < editor.vleft {
//                 editor.cx = editor.vleft;
//             } else {
//             }
//         }
//         Action::MoveRight => {
//             editor.cx += 1;
//         }
//         Action::MoveToLineStart => {
//             editor.cx = 0;
//         }
//         Action::MoveToLineEnd => {
//             editor.cx = editor.line_length().saturating_sub(1);
//         }
//         Action::PageUp => {
//             if editor.vtop > 0 {
//                 editor.vtop = editor.vtop.saturating_sub(editor.vheight() as usize);
//                 viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//             }
//         }
//         Action::PageDown => {
//             if editor.current_buffer().len() > editor.vtop + editor.vheight() as usize {
//                 editor.vtop += editor.vheight() as usize;
//                 viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//             }
//         }
//         Action::EnterMode(new_mode) => {
//             // TODO: with the introduction of new modes, maybe this transtion
//             // needs to be widened to anything -> insert and anything -> normal
//             if editor.is_normal() && matches!(new_mode, Mode::Insert) {
//                 editor.insert_undo_actions = Vec::new();
//             }
//             if editor.is_insert() && matches!(new_mode, Mode::Normal) {
//                 if !editor.insert_undo_actions.is_empty() {
//                     let actions = mem::take(&mut editor.insert_undo_actions);
//                     editor.undo_actions.push(Action::UndoMultiple(actions));
//                 }
//             }
//             if editor.has_term() {
//                 draw_commandline(theme, editor, buffer);
//             }
//
//             if matches!(new_mode, Mode::Search) {
//                 editor.search_term = String::new();
//             }
//
//             editor.mode = *new_mode;
//             draw_statusline(theme, editor, buffer);
//         }
//         Action::InsertCharAtCursorPos(c) => {
//             editor
//                 .insert_undo_actions
//                 .push(Action::DeleteCharAt(editor.cx, editor.buffer_line()));
//             let line = editor.buffer_line();
//             let cx = editor.cx;
//
//             editor.current_buffer_mut().insert(cx, line, *c);
//             notify_change(lsp, editor).await?;
//             editor.cx += 1;
//             viewport.draw_line(buffer, &editor.current_buffer().lines, 0, 0, line)?;
//         }
//         Action::DeleteCharAt(x, y) => {
//             editor.current_buffer_mut().remove(*x, *y);
//             notify_change(lsp, editor).await?;
//             viewport.draw_line(
//                 buffer,
//                 &editor.current_buffer().lines,
//                 0,
//                 0,
//                 editor.buffer_line(),
//             )?;
//         }
//         Action::DeleteCharAtCursorPos => {
//             let cx = editor.cx;
//             let line = editor.buffer_line();
//
//             editor.current_buffer_mut().remove(cx, line);
//             notify_change(lsp, editor).await?;
//             viewport.draw_line(
//                 buffer,
//                 &editor.current_buffer().lines,
//                 0,
//                 0,
//                 editor.buffer_line(),
//             )?;
//         }
//         Action::ReplaceLineAt(y, contents) => {
//             editor
//                 .current_buffer_mut()
//                 .replace_line(*y, contents.to_string());
//             notify_change(lsp, editor).await?;
//             viewport.draw_line(
//                 buffer,
//                 &editor.current_buffer().lines,
//                 0,
//                 0,
//                 editor.buffer_line(),
//             )?;
//         }
//         Action::InsertNewLine => {
//             editor.insert_undo_actions.extend(vec![
//                 Action::MoveTo(editor.cx, editor.buffer_line() + 1),
//                 Action::DeleteLineAt(editor.buffer_line() + 1),
//                 Action::ReplaceLineAt(
//                     editor.buffer_line(),
//                     editor.current_line_contents().unwrap_or_default(),
//                 ),
//             ]);
//             let spaces = editor.current_line_indentation();
//
//             let current_line = editor.current_line_contents().unwrap_or_default();
//             let before_cursor = current_line[..editor.cx].to_string();
//             let after_cursor = current_line[editor.cx..].to_string();
//
//             let line = editor.buffer_line();
//             editor
//                 .current_buffer_mut()
//                 .replace_line(line, before_cursor);
//             notify_change(lsp, editor).await?;
//
//             editor.cx = spaces;
//             editor.cy += 1;
//
//             let new_line = format!("{}{}", " ".repeat(spaces), &after_cursor);
//             let line = editor.buffer_line();
//
//             editor.current_buffer_mut().insert_line(line, new_line);
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::SetWaitingKeyAction(key_action) => {
//             editor.waiting_key_action = Some(*(key_action.clone()));
//         }
//         Action::DeleteCurrentLine => {
//             let line = editor.buffer_line();
//             let contents = editor.current_line_contents();
//
//             editor.current_buffer_mut().remove_line(line);
//             notify_change(lsp, editor).await?;
//             editor
//                 .undo_actions
//                 .push(Action::InsertLineAt(line, contents));
//
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::Undo => {
//             if let Some(undo_action) = editor.undo_actions.pop() {
//                 execute(
//                     &undo_action,
//                     &mut editor,
//                     &theme,
//                     &viewport,
//                     &config,
//                     &mut buffer,
//                     &mut lsp,
//                     &mut runtime,
//                     &mut plugin_registry,
//                 )
//                 .await?;
//             }
//         }
//         Action::UndoMultiple(actions) => {
//             for action in actions.iter().rev() {
//                 execute(
//                     &action,
//                     &mut editor,
//                     &theme,
//                     &viewport,
//                     &config,
//                     &mut buffer,
//                     &mut lsp,
//                     &mut runtime,
//                     &mut plugin_registry,
//                 )
//                 .await?;
//             }
//         }
//         Action::InsertLineAt(y, contents) => {
//             if let Some(contents) = contents {
//                 editor
//                     .current_buffer_mut()
//                     .insert_line(*y, contents.to_string());
//                 notify_change(lsp, editor).await?;
//                 viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//             }
//         }
//         Action::MoveLineToViewportCenter => {
//             let viewport_center = editor.vheight() / 2;
//             let distance_to_center = editor.cy as isize - viewport_center as isize;
//
//             if distance_to_center > 0 {
//                 // if distance > 0 we need to scroll up
//                 let distance_to_center = distance_to_center.abs() as usize;
//                 if editor.vtop > distance_to_center {
//                     let new_vtop = editor.vtop + distance_to_center;
//                     editor.vtop = new_vtop;
//                     editor.cy = viewport_center;
//                     viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//                 }
//             } else if distance_to_center < 0 {
//                 // if distance < 0 we need to scroll down
//                 let distance_to_center = distance_to_center.abs() as usize;
//                 let new_vtop = editor.vtop.saturating_sub(distance_to_center);
//                 let distance_to_go = editor.vtop as usize + distance_to_center;
//                 if editor.current_buffer().len() > distance_to_go && new_vtop != editor.vtop {
//                     editor.vtop = new_vtop;
//                     editor.cy = viewport_center;
//                     viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//                 }
//             }
//         }
//         Action::InsertLineBelowCursor => {
//             editor
//                 .undo_actions
//                 .push(Action::DeleteLineAt(editor.buffer_line() + 1));
//
//             let leading_spaces = editor.current_line_indentation();
//             let line = editor.buffer_line();
//             editor
//                 .current_buffer_mut()
//                 .insert_line(line + 1, " ".repeat(leading_spaces));
//             notify_change(lsp, editor).await?;
//             editor.cy += 1;
//             editor.cx = leading_spaces;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::InsertLineAtCursor => {
//             editor
//                 .undo_actions
//                 .push(Action::DeleteLineAt(editor.buffer_line()));
//
//             // if the current line is empty, let's use the indentation from the line above
//             let leading_spaces = if let Some(line) = editor.current_line_contents() {
//                 if line.is_empty() {
//                     editor.previous_line_indentation()
//                 } else {
//                     editor.current_line_indentation()
//                 }
//             } else {
//                 editor.previous_line_indentation()
//             };
//
//             let line = editor.buffer_line();
//             editor
//                 .current_buffer_mut()
//                 .insert_line(line, " ".repeat(leading_spaces));
//             notify_change(lsp, editor).await?;
//             editor.cx = leading_spaces;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::MoveToTop => {
//             editor.vtop = 0;
//             editor.cy = 0;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::MoveToBottom => {
//             if editor.current_buffer().len() > editor.vheight() as usize {
//                 editor.cy = editor.vheight() - 1;
//                 editor.vtop = editor.current_buffer().len() - editor.vheight() as usize;
//                 viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//             } else {
//                 editor.cy = editor.current_buffer().len() - 1;
//             }
//         }
//         Action::DeleteLineAt(y) => {
//             editor.current_buffer_mut().remove_line(*y);
//             notify_change(lsp, editor).await?;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::DeletePreviousChar => {
//             if editor.cx > 0 {
//                 editor.cx -= 1;
//                 let cx = editor.cx;
//                 let line = editor.buffer_line();
//                 editor.current_buffer_mut().remove(cx, line);
//                 notify_change(lsp, editor).await?;
//                 viewport.draw_line(
//                     buffer,
//                     &editor.current_buffer().lines,
//                     0,
//                     0,
//                     editor.buffer_line(),
//                 )?;
//             }
//         }
//         Action::DumpBuffer => {
//             log!("{buffer}", buffer = buffer.dump());
//         }
//         Action::Command(cmd) => {
//             log!("Handling command: {cmd}");
//
//             for action in editor.handle_command(cmd) {
//                 editor.last_error = None;
//                 if execute(
//                     &action,
//                     &mut editor,
//                     &theme,
//                     &viewport,
//                     &config,
//                     &mut buffer,
//                     &mut lsp,
//                     &mut runtime,
//                     &mut plugin_registry,
//                 )
//                 .await?
//                 {
//                     return Ok(true);
//                 }
//             }
//         }
//         Action::PluginCommand(cmd) => {
//             plugin_registry.execute(runtime, cmd).await?;
//         }
//         Action::GoToLine(line) => {
//             go_to_line(
//                 editor,
//                 theme,
//                 viewport,
//                 config,
//                 buffer,
//                 lsp,
//                 runtime,
//                 plugin_registry,
//                 *line,
//                 GoToLinePosition::Center,
//             )
//             .await?;
//         }
//         Action::GoToDefinition => {
//             if let Some(file) = editor.current_buffer().file.clone() {
//                 lsp.goto_definition(&file, editor.cx, editor.cy + editor.vtop)
//                     .await?;
//             }
//         }
//         Action::Hover => {
//             if let Some(file) = editor.current_buffer().file.clone() {
//                 lsp.hover(&file, editor.cx, editor.cy + editor.vtop).await?;
//             }
//         }
//         Action::MoveTo(x, y) => {
//             go_to_line(
//                 editor,
//                 theme,
//                 viewport,
//                 config,
//                 buffer,
//                 lsp,
//                 runtime,
//                 plugin_registry,
//                 *y,
//                 GoToLinePosition::Center,
//             )
//             .await?;
//             editor.cx = std::cmp::min(*x, editor.line_length().saturating_sub(1));
//         }
//         Action::SetCursor(x, y) => {
//             editor.cx = *x;
//             editor.cy = *y;
//         }
//         Action::ScrollUp => {
//             let scroll_lines = config.mouse_scroll_lines.unwrap_or(3);
//             if editor.vtop > scroll_lines {
//                 editor.vtop -= scroll_lines;
//                 let desired_cy = editor.cy + scroll_lines;
//                 if desired_cy <= editor.vheight() {
//                     editor.cy = desired_cy;
//                 }
//                 viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//             }
//         }
//         Action::ScrollDown => {
//             if editor.current_buffer().len() > editor.vtop + editor.vheight() as usize {
//                 editor.vtop += config.mouse_scroll_lines.unwrap_or(3);
//                 let desired_cy = editor
//                     .cy
//                     .saturating_sub(config.mouse_scroll_lines.unwrap_or(3));
//                 editor.cy = desired_cy;
//                 viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//             }
//         }
//         Action::MoveToNextWord => {
//             let next_word = editor
//                 .current_buffer()
//                 .find_next_word((editor.cx, editor.buffer_line()));
//
//             if let Some((x, y)) = next_word {
//                 editor.cx = x;
//                 go_to_line(
//                     editor,
//                     theme,
//                     viewport,
//                     config,
//                     buffer,
//                     lsp,
//                     runtime,
//                     plugin_registry,
//                     y + 1,
//                     GoToLinePosition::Top,
//                 )
//                 .await?;
//                 draw_cursor(theme, editor, buffer)?;
//             }
//         }
//         Action::MoveToPreviousWord => {
//             let previous_word = editor
//                 .current_buffer()
//                 .find_prev_word((editor.cx, editor.buffer_line()));
//
//             if let Some((x, y)) = previous_word {
//                 editor.cx = x;
//                 go_to_line(
//                     editor,
//                     theme,
//                     viewport,
//                     config,
//                     buffer,
//                     lsp,
//                     runtime,
//                     plugin_registry,
//                     y + 1,
//                     GoToLinePosition::Top,
//                 )
//                 .await?;
//                 draw_cursor(theme, editor, buffer)?;
//             }
//         }
//         Action::MoveLineToViewportBottom => {
//             let line = editor.buffer_line();
//             if line > editor.vtop + editor.vheight() {
//                 editor.vtop = line - editor.vheight();
//                 editor.cy = editor.vheight() - 1;
//
//                 viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//             }
//         }
//         Action::InsertTab => {
//             // TODO: Tab configuration
//             let tabsize = 4;
//             let cx = editor.cx;
//             let line = editor.buffer_line();
//             editor
//                 .current_buffer_mut()
//                 .insert_str(cx, line, &" ".repeat(tabsize));
//             notify_change(lsp, editor).await?;
//             editor.cx += tabsize;
//             viewport.draw_line(
//                 buffer,
//                 &editor.current_buffer().lines,
//                 0,
//                 0,
//                 editor.buffer_line(),
//             )?;
//         }
//         Action::Save => match editor.current_buffer_mut().save() {
//             Ok(msg) => {
//                 // TODO: use last_message instead of last_error
//                 editor.last_error = Some(msg);
//             }
//             Err(e) => {
//                 editor.last_error = Some(e.to_string());
//             }
//         },
//         Action::FindPrevious => {
//             if let Some((x, y)) = editor
//                 .current_buffer()
//                 .find_prev(&editor.search_term, (editor.cx, editor.vtop + editor.cy))
//             {
//                 editor.cx = x;
//                 go_to_line(
//                     editor,
//                     theme,
//                     viewport,
//                     config,
//                     buffer,
//                     lsp,
//                     runtime,
//                     plugin_registry,
//                     y + 1,
//                     GoToLinePosition::Center,
//                 )
//                 .await?;
//             }
//         }
//         Action::FindNext => {
//             if let Some((x, y)) = editor
//                 .current_buffer()
//                 .find_next(&editor.search_term, (editor.cx, editor.vtop + editor.cy))
//             {
//                 editor.cx = x;
//                 go_to_line(
//                     editor,
//                     theme,
//                     viewport,
//                     config,
//                     buffer,
//                     lsp,
//                     runtime,
//                     plugin_registry,
//                     y + 1,
//                     GoToLinePosition::Center,
//                 )
//                 .await?;
//             }
//         }
//         Action::DeleteWord => {
//             let cx = editor.cx;
//             let line = editor.buffer_line();
//             editor.current_buffer_mut().delete_word((cx, line));
//             notify_change(lsp, editor).await?;
//             viewport.draw_line(
//                 buffer,
//                 &editor.current_buffer().lines,
//                 0,
//                 0,
//                 editor.buffer_line(),
//             )?;
//         }
//         Action::NextBuffer => {
//             let new_index = if editor.current_buffer < editor.buffers.len() - 1 {
//                 editor.current_buffer + 1
//             } else {
//                 0
//             };
//             editor.set_current_buffer(theme, viewport, buffer, new_index)?;
//         }
//         Action::PreviousBuffer => {
//             let new_index = if editor.current_buffer > 0 {
//                 editor.current_buffer - 1
//             } else {
//                 editor.buffers.len() - 1
//             };
//             editor.set_current_buffer(theme, viewport, buffer, new_index)?;
//         }
//         Action::OpenBuffer(name) => {
//             if let Some(index) = editor.buffers.iter().position(|b| b.name() == *name) {
//                 editor.set_current_buffer(theme, viewport, buffer, index)?;
//             }
//         }
//         Action::OpenFile(path) => {
//             let new_buffer = match Buffer::from_file(&mut lsp, Some(path.to_string())).await {
//                 Ok(buffer) => buffer,
//                 Err(e) => {
//                     editor.last_error = Some(e.to_string());
//                     return Ok(false);
//                 }
//             };
//             editor.buffers.push(new_buffer);
//             editor.set_current_buffer(theme, viewport, buffer, editor.buffers.len() - 1)?;
//             render(&theme, editor, viewport, buffer)?;
//         }
//         Action::FilePicker => {
//             let file_picker = FilePicker::new(&editor, std::env::current_dir()?)?;
//             file_picker.draw(buffer)?;
//
//             editor.current_dialog = Some(Box::new(file_picker));
//         }
//         Action::ShowDialog => {
//             if let Some(dialog) = &mut editor.current_dialog {
//                 dialog.draw(buffer)?;
//             }
//         }
//         Action::CloseDialog => {
//             editor.current_dialog = None;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::RefreshDiagnostics => {
//             draw_diagnostics(theme, config, editor, buffer);
//         }
//         Action::Print(msg) => {
//             editor.last_error = Some(msg.clone());
//         }
//         Action::OpenPicker(title, items, id) => {
//             let picker = Picker::new(title.clone(), &editor, &items, *id);
//             picker.draw(buffer)?;
//
//             editor.current_dialog = Some(Box::new(picker));
//         }
//         Action::Picked(item, id) => {
//             log!("picked: {item} - {id:?}");
//             if let Some(id) = id {
//                 plugin_registry
//                     .notify(
//                         runtime,
//                         &format!("picker:selected:{}", id),
//                         serde_json::Value::String(item.clone()),
//                     )
//                     .await?;
//             }
//         }
//         Action::Suspend => {
//             stdout().execute(terminal::LeaveAlternateScreen)?;
//             let pid = Pid::from_raw(0);
//             let _ = signal::kill(pid, Signal::SIGSTOP);
//             stdout().execute(terminal::EnterAlternateScreen)?;
//             render(&theme, editor, viewport, buffer)?;
//         }
//         Action::ToggleWrap => {
//             editor.wrap = !editor.wrap;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::DecreaseLeft => {
//             editor.wrap = false;
//             editor.vleft = editor.vleft.saturating_sub(1);
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//         Action::IncreaseLeft => {
//             editor.wrap = false;
//             editor.vleft = editor.vleft + 1;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//     }
//
//     Ok(false)
// }

// TODO: in neovim, when you are at an x position and you move to a shorter line, the cursor
//       goes back to the max x but returns to the previous x position if the line is longer
// fn check_bounds(editor: &mut Editor) {
//     let line_length = editor.line_length();
//
//     if editor.cx >= line_length && editor.is_normal() {
//         if line_length > 0 {
//             editor.cx = editor.line_length() - 1;
//         } else if editor.is_normal() {
//             editor.cx = 0;
//         }
//     }
//     if editor.cx >= editor.vwidth() {
//         editor.cx = editor.vwidth() - 1;
//     }
//
//     // check if cy is after the end of the buffer
//     // the end of the buffer is less than vtop + cy
//     let line_on_buffer = editor.cy as usize + editor.vtop;
//     if line_on_buffer > editor.current_buffer().len().saturating_sub(1) {
//         editor.cy = editor.current_buffer().len() - editor.vtop - 1;
//     }
// }

// fn handle_lsp_message(
//     editor: &mut Editor,
//     msg: &InboundMessage,
//     method: Option<String>,
// ) -> Option<Action> {
//     match msg {
//         InboundMessage::Message(msg) => {
//             if let Some(method) = method {
//                 if method == "textDocument/definition" {
//                     let result = match msg.result {
//                         serde_json::Value::Array(ref arr) => arr[0].as_object().unwrap(),
//                         serde_json::Value::Object(ref obj) => obj,
//                         _ => return None,
//                     };
//
//                     if let Some(range) = result.get("range") {
//                         if let Some(start) = range.get("start") {
//                             if let Some(line) = start.get("line") {
//                                 if let Some(character) = start.get("character") {
//                                     let line = line.as_u64().unwrap() as usize;
//                                     let character = character.as_u64().unwrap() as usize;
//                                     return Some(Action::MoveTo(character, line + 1));
//                                 }
//                             }
//                         }
//                     }
//                 }
//                 if method == "textDocument/hover" {
//                     log!("hover response: {msg:?}");
//                     let result = match msg.result {
//                         serde_json::Value::Array(ref arr) => arr[0].as_object().unwrap(),
//                         serde_json::Value::Object(ref obj) => obj,
//                         _ => return None,
//                     };
//
//                     if let Some(contents) = result.get("contents") {
//                         if let Some(contents) = contents.as_object() {
//                             if let Some(serde_json::Value::String(value)) = contents.get("value") {
//                                 let info = Info::new(
//                                     editor.cx,
//                                     editor.cy,
//                                     editor.size.0 as usize,
//                                     editor.size.1 as usize,
//                                     value.clone(),
//                                 );
//                                 editor.current_dialog = Some(Box::new(info));
//                                 return Some(Action::ShowDialog);
//                             }
//                         }
//                     }
//                 }
//             }
//             None
//         }
//         InboundMessage::Notification(msg) => match msg {
//             ParsedNotification::PublishDiagnostics(msg) => {
//                 _ = editor.current_buffer_mut().offer_diagnostics(&msg);
//                 Some(Action::RefreshDiagnostics)
//             }
//         },
//         InboundMessage::UnknownNotification(msg) => {
//             log!("got an unhandled notification: {msg:#?}");
//             None
//         }
//         InboundMessage::Error(error_msg) => {
//             log!("got an error: {error_msg:?}");
//             None
//         }
//         InboundMessage::ProcessingError(error_msg) => {
//             editor.last_error = Some(error_msg.to_string());
//             None
//         }
//     }
// }

// Draw the current render buffer to the terminal
// fn render(
//     theme: &Theme,
//     editor: &mut Editor,
//     viewport: &Viewport,
//     buffer: &mut RenderBuffer,
// ) -> anyhow::Result<()> {
//     viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//     draw_statusline(theme, editor, buffer);
//
//     stdout().queue(Clear(ClearType::All))?.queue(MoveTo(0, 0))?;
//     stdout().queue(style::SetBackgroundColor(theme.style.bg.unwrap()))?;
//
//     let mut current_style = &theme.style;
//     for cell in buffer.cells.iter() {
//         if cell.style != *current_style {
//             if let Some(bg) = cell.style.bg {
//                 stdout().queue(style::SetBackgroundColor(bg))?;
//             }
//             if let Some(fg) = cell.style.fg {
//                 stdout().queue(style::SetForegroundColor(fg))?;
//             }
//             if cell.style.italic {
//                 stdout().queue(style::SetAttribute(style::Attribute::Italic))?;
//             } else {
//                 stdout().queue(style::SetAttribute(style::Attribute::NoItalic))?;
//             }
//             current_style = &cell.style;
//         }
//
//         stdout().queue(style::Print(cell.c))?;
//     }
//
//     draw_cursor(theme, editor, buffer)?;
//     stdout().flush()?;
//
//     Ok(())
// }

// async fn notify_change(lsp: &mut LspClient, editor: &mut Editor) -> anyhow::Result<()> {
//     let file = editor.current_buffer().file.clone();
//     if let Some(file) = &file {
//         lsp.did_change(&file, &editor.current_buffer().contents())
//             .await?;
//     }
//     Ok(())
// }

// async fn go_to_line(
//     editor: &mut Editor,
//     theme: &Theme,
//     viewport: &Viewport<'_>,
//     config: &Config,
//     buffer: &mut RenderBuffer,
//     lsp: &mut LspClient,
//     runtime: &mut Runtime,
//     plugin_registry: &mut PluginRegistry,
//     line: usize,
//     pos: GoToLinePosition,
// ) -> anyhow::Result<()> {
//     if line == 0 {
//         execute(
//             &Action::MoveToTop,
//             editor,
//             theme,
//             viewport,
//             config,
//             buffer,
//             lsp,
//             runtime,
//             plugin_registry,
//         )
//         .await?;
//         return Ok(());
//     }
//
//     if line <= editor.current_buffer().len() {
//         let y = line - 1;
//
//         if editor.is_within_viewport(y) {
//             editor.cy = y - editor.vtop;
//         } else if editor.is_within_first_page(y) {
//             editor.vtop = 0;
//             editor.cy = y;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         } else if editor.is_within_last_page(y) {
//             editor.vtop = editor.current_buffer().len() - editor.vheight();
//             editor.cy = y - editor.vtop;
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         } else {
//             if matches!(pos, GoToLinePosition::Bottom) {
//                 editor.vtop = y - editor.vheight();
//                 editor.cy = editor.buffer_line() - editor.vtop;
//             } else {
//                 editor.vtop = y;
//                 editor.cy = 0;
//                 if matches!(pos, GoToLinePosition::Center) {
//                     execute(
//                         &Action::MoveToTop,
//                         editor,
//                         theme,
//                         viewport,
//                         config,
//                         buffer,
//                         lsp,
//                         runtime,
//                         plugin_registry,
//                     )
//                     .await?;
//                 }
//             }
//
//             // FIXME: this is wasteful when move to viewport center worked
//             // but we have to account for the case where it didn't and also
//             viewport.draw(buffer, &editor.current_buffer().lines, 0, 0)?;
//         }
//     }
//
//     Ok(())
// }

// fn handle_insert_event(
//     editor: &mut Editor,
//     config: &Config,
//     ev: &event::Event,
// ) -> Option<KeyAction> {
//     let insert = config.keys.insert.clone();
//     if let Some(ka) = event_to_key_action(editor, &insert, &ev) {
//         return Some(ka);
//     }
//
//     match ev {
//         Event::Key(event) => match event.code {
//             KeyCode::Char(c) => KeyAction::Single(Action::InsertCharAtCursorPos(c)).into(),
//             _ => None,
//         },
//         _ => None,
//     }
// }

// #[async_recursion::async_recursion]
// async fn handle_key_action(
//     ev: &event::Event,
//     action: &KeyAction,
//     editor: &mut Editor,
//     theme: &Theme,
//     viewport: &Viewport<'_>,
//     config: &Config,
//     buffer: &mut RenderBuffer,
//     lsp: &mut LspClient,
//     runtime: &mut Runtime,
//     plugin_registry: &mut PluginRegistry,
// ) -> anyhow::Result<bool> {
//     log!("Action: {action:?}");
//     let quit = match action {
//         KeyAction::Single(action) => {
//             execute(
//                 action,
//                 editor,
//                 theme,
//                 viewport,
//                 config,
//                 buffer,
//                 lsp,
//                 runtime,
//                 plugin_registry,
//             )
//             .await?
//         }
//         KeyAction::Multiple(actions) => {
//             let mut quit = false;
//             for action in actions {
//                 if execute(
//                     action,
//                     editor,
//                     theme,
//                     viewport,
//                     config,
//                     buffer,
//                     lsp,
//                     runtime,
//                     plugin_registry,
//                 )
//                 .await?
//                 {
//                     quit = true;
//                     break;
//                 }
//             }
//             quit
//         }
//         KeyAction::Nested(actions) => {
//             if let Event::Key(KeyEvent {
//                 code: KeyCode::Char(c),
//                 ..
//             }) = ev
//             {
//                 editor.waiting_command = Some(format!("{c}"));
//             }
//             editor.waiting_key_action = Some(KeyAction::Nested(actions.clone()));
//             false
//         }
//         KeyAction::Repeating(times, action) => {
//             editor.repeater = None;
//             let mut quit = false;
//             for _ in 0..*times as usize {
//                 if handle_key_action(
//                     ev,
//                     action,
//                     editor,
//                     theme,
//                     viewport,
//                     config,
//                     buffer,
//                     lsp,
//                     runtime,
//                     plugin_registry,
//                 )
//                 .await?
//                 {
//                     quit = true;
//                     break;
//                 }
//             }
//             quit
//         }
//     };
//
//     Ok(quit)
// }

pub struct Editor {
    config: Config,
    theme: Theme,
    highlighter: Arc<Mutex<Highlighter>>,

    buffers: Vec<SharedBuffer>,
    windows: Vec<Window>,
    focused_window: usize,

    pub width: usize,
    pub height: usize,

    waiting_command: Option<String>,
    waiting_key_action: Option<KeyAction>,

    mode: Mode,
    command: String,
    search_term: String,

    undo_actions: Vec<Action>,
    insert_undo_actions: Vec<Action>,
    repeater: Option<u16>,

    last_error: Option<String>,
    last_message: Option<String>,
    current_dialog: Option<Box<dyn Component>>,
}

impl Editor {
    pub fn new(config: Config, theme: Theme, buffers: Vec<Buffer>) -> anyhow::Result<Self> {
        let width = terminal::size()?.0 as usize;
        let height = terminal::size()?.1 as usize;
        let highlighter = Arc::new(Mutex::new(Highlighter::new(theme.clone())?));
        let buffers: Vec<SharedBuffer> = buffers.into_iter().map(Into::into).collect();

        let windows = vec![Window::new(
            0,
            0,
            width,
            height - 2,
            buffers.get(0).unwrap().clone(),
            theme.style.clone(),
            theme.gutter_style.clone(),
            &highlighter,
        )];

        Ok(Editor {
            config,
            theme,
            highlighter,

            buffers,
            windows,
            focused_window: 0,

            width,
            height,

            waiting_command: None,
            waiting_key_action: None,

            mode: Mode::Normal,
            command: String::new(),
            search_term: String::new(),

            undo_actions: vec![],
            insert_undo_actions: vec![],
            repeater: None,

            last_error: None,
            last_message: None,
            current_dialog: None,
        })
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        terminal::enable_raw_mode()?;

        stdout()
            .execute(event::EnableMouseCapture)?
            .execute(terminal::EnterAlternateScreen)?
            .execute(terminal::Clear(terminal::ClearType::All))?;

        let mut runtime = Runtime::new();
        let mut plugin_registry = PluginRegistry::new();
        for (name, path) in &self.config.plugins {
            let path = Config::path("plugins").join(path);
            plugin_registry.add(&name, &path);
        }
        plugin_registry.initialize(&mut runtime).await?;

        let mut lsp = LspClient::start().await?;
        lsp.initialize().await?;

        LSP_CLIENTS.write().unwrap().insert("rust".to_string(), lsp);

        let mut buffer = RenderBuffer::new(self.width, self.height, self.theme.style.clone());
        let mut reader = EventStream::new();

        self.render(&mut buffer)?;

        loop {
            let mut delay = futures_timer::Delay::new(Duration::from_millis(10)).fuse();
            let mut event = reader.next().fuse();

            select! {
                _ = delay => {
                    // handle responses from lsp
                    // if let Some((msg, method)) = lsp.recv_response().await? {
                    //     if let Some(action) = handle_lsp_message(editor, &msg, method) {
                    //         // TODO: handle quit
                    //         let current_buffer = buffer.clone();
                    //         execute(&action, editor, &theme, &viewport, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
                    //         redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
                    //     }
                    // }

                    // if let Some(req) = ACTION_DISPATCHER.try_recv_request() {
                    //     match req {
                    //         PluginRequest::Action(action) => {
                    //             let current_buffer = buffer.clone();
                    //             execute(&action, editor, &theme, &viewport, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
                    //             redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
                    //         }
                    //         PluginRequest::EditorInfo(id) => {
                    //             let info = serde_json::to_value(editor.info())?;
                    //             let key = if let Some(id) = id {
                    //                 format!("editor:info:{}", id)
                    //             } else {
                    //                 "editor:info".to_string()
                    //             };
                    //             plugin_registry
                    //                 .notify(&mut runtime, &key, info)
                    //                 .await?;
                    //         }
                    //         PluginRequest::OpenPicker(title, id, items) => {
                    //             let current_buffer = buffer.clone();
                    //             let items = items.iter().map(|v| match v {
                    //                 serde_json::Value::String(s) => s.clone(),
                    //                 val => val.to_string(),
                    //             }).collect();
                    //
                    //             execute(&Action::OpenPicker(title, items, id), editor, &theme, &viewport, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
                    //             redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
                    //         }
                    //     }
                    // }

                }
                maybe_event = event => {
                    match maybe_event {
                        Some(Ok(ev)) => {
                            let current_buffer = buffer.clone();

                            if let event::Event::Resize(width, height) = ev {
                                log!("resize: {width}x{height}");
                                self.resize(width, height);
                                buffer = RenderBuffer::new(
                                    self.width,
                                    self.height,
                                    self.theme.style.clone(),
                                );
                                self.render(&mut buffer)?;
                                continue;
                            }

                            self.check_bounds();

                            if let Some(mut action) = self.handle_event(&ev) {
                                let mut quit = false;
                                loop {
                                    match self.handle_key_action(&ev, &action, &mut buffer).await? {
                                        ActionEffect::None => {},
                                        ActionEffect::Message(msg) => {
                                            self.last_message = Some(msg);
                                        }
                                        ActionEffect::Error(error) => {
                                            self.last_error = Some(error);
                                        }
                                        ActionEffect::RedrawCursor => {
                                            self.draw_cursor()?;
                                        },
                                        ActionEffect::RedrawLine => {
                                            self.current_window().draw_current_line(&mut buffer)?;
                                            self.draw_cursor()?;
                                        }
                                        ActionEffect::RedrawWindow => {
                                            self.current_window().draw(&mut buffer)?;
                                            self.draw_cursor()?;
                                        }
                                        ActionEffect::RedrawAll => {
                                            self.render(&mut buffer)?;
                                        }
                                        ActionEffect::NewBuffer(new_buffer) => {
                                            self.buffers.push(new_buffer);
                                            self.render(&mut buffer)?;
                                        }
                                        ActionEffect::Actions(actions) => {
                                            action = KeyAction::Multiple(actions);
                                            continue;
                                        }
                                        ActionEffect::Quit => {
                                            log!("requested to quit");
                                            quit = true;
                                        }
                                    };
                                    break;
                                }

                                if quit {
                                    break;
                                }
                            }

                            self.redraw(&current_buffer, &mut buffer).await?;
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

    fn handle_event(&mut self, ev: &event::Event) -> Option<KeyAction> {
        if let Some(key_action) = self.waiting_key_action.take() {
            self.waiting_command = None;
            return self.handle_waiting_command(key_action, ev);
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

    fn handle_waiting_command(
        &mut self,
        key_action: KeyAction,
        ev: &event::Event,
    ) -> Option<KeyAction> {
        let KeyAction::Nested(nested_mappings) = key_action else {
            panic!("expected nested mappings");
        };

        self.event_to_key_action(&nested_mappings, &ev)
    }

    fn handle_normal_event(&self, ev: &event::Event) -> Option<KeyAction> {
        let normal = self.config.keys.normal.clone();
        self.event_to_key_action(&normal, &ev)
    }

    fn handle_insert_event(&self, ev: &event::Event) -> Option<KeyAction> {
        if let Some(key_action) = self.event_to_key_action(&self.config.keys.insert, &ev) {
            return Some(key_action);
        }

        match ev {
            Event::Key(event) => match event.code {
                KeyCode::Char(c) => KeyAction::Single(Action::InsertCharAtCursorPos(c)).into(),
                _ => None,
            },
            _ => None,
        }
    }

    fn event_to_key_action(
        &self,
        mappings: &HashMap<String, KeyAction>,
        ev: &Event,
    ) -> Option<KeyAction> {
        // TODO: handle repeater
        // if self.handle_repeater(ev) {
        //     return None;
        // }

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
                    MouseEventKind::Down(MouseButton::Left) => Some(KeyAction::Single(
                        Action::Click((*column) as usize, (*row) as usize),
                    )),
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

    #[async_recursion::async_recursion]
    async fn handle_key_action(
        &mut self,
        ev: &event::Event,
        action: &KeyAction,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<ActionEffect> {
        let effect = match action {
            KeyAction::Single(action) => self.execute(action, buffer).await?,
            KeyAction::Multiple(actions) => {
                let mut effect = ActionEffect::None;
                for action in actions {
                    effect = self.execute(action, buffer).await?.max(effect);
                }
                effect
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
                ActionEffect::None
            }
            KeyAction::Repeating(times, action) => {
                self.repeater = None;
                let mut effect = ActionEffect::None;
                for _ in 0..*times as usize {
                    effect = self
                        .handle_key_action(ev, action, buffer)
                        .await?
                        .max(effect);
                }
                effect
            }
        };

        log!("Action: {action:?} -> {effect:?}");
        Ok(effect)
    }

    #[async_recursion::async_recursion]
    async fn execute(
        &mut self,
        action: &Action,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<ActionEffect> {
        log!("execute: {action:?}");
        self.last_error = None;
        self.last_message = None;
        let effect = match action {
            Action::Quit(force) => {
                if *force {
                    return Ok(ActionEffect::Quit);
                }
                let modified_buffers = self.modified_buffers();
                if modified_buffers.is_empty() {
                    return Ok(ActionEffect::Quit);
                }
                self.last_error = Some(format!(
                    "The following buffers have unwritten changes: {}",
                    modified_buffers.join(", ")
                ));
                ActionEffect::None
            }
            Action::ToggleWrap => self.current_window_mut().toggle_wrap(),
            // Action::Undo => todo!("Action::Undo"),
            // Action::UndoMultiple(actions) => todo!("Action::UndoMultiple"),

            // window management
            Action::Split => self.split_horizontal(),
            Action::NextWindow => self.next_window(),

            // cursor movement
            Action::MoveDown => self.current_window_mut().move_down(),
            Action::MoveUp => self.current_window_mut().move_up(),
            Action::MoveLeft => self.current_window_mut().move_left(),
            Action::MoveRight => self.current_window_mut().move_right(),
            Action::MoveToLineStart => self.current_window_mut().move_to_line_start(),
            Action::MoveToLineEnd => self.current_window_mut().move_to_line_end(),
            Action::MoveTo(x, y) => self.current_window_mut().move_to(*x, *y),
            Action::PageUp => self.current_window_mut().page_up(),
            Action::PageDown => self.current_window_mut().page_down(),

            // word movement
            Action::MoveToNextWord => self.current_window_mut().move_to_next_word(),
            Action::MoveToPreviousWord => self.current_window_mut().move_to_previous_word(),

            // window movement
            Action::MoveToTop => self.current_window_mut().move_to_top(),
            Action::MoveToBottom => self.current_window_mut().move_to_bottom(),
            Action::MoveLineToViewportCenter => self.current_window_mut().move_line_to_middle(),
            Action::MoveLineToViewportBottom => self.current_window_mut().move_line_to_bottom(),
            Action::GoToLine(line) => self
                .current_window_mut()
                .go_to_line(*line, GoToLinePosition::Center),

            // mouse actions
            Action::Click(x, y) => self.current_window_mut().click(*x, *y),
            Action::ScrollUp => {
                let lines = self.config.mouse_scroll_lines.unwrap_or(3);
                self.current_window_mut().scroll_up(lines)
            }
            Action::ScrollDown => {
                let lines = self.config.mouse_scroll_lines.unwrap_or(3);
                self.current_window_mut().scroll_down(lines)
            }

            // mode changes
            Action::EnterMode(new_mode) => {
                if self.is_normal() && matches!(new_mode, Mode::Normal) {
                    // TODO: implement undo
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

                ActionEffect::None
            }

            // line changes
            Action::InsertLineBelowCursor => self.current_window_mut().insert_line_below_cursor(),
            Action::InsertLineAtCursor => self.current_window_mut().insert_line_at_cursor(),
            Action::InsertCharAtCursorPos(c) => self.current_window_mut().insert_char_at_cursor(*c),
            Action::InsertNewLine => self.current_window_mut().insert_new_line(),
            Action::InsertTab => self.current_window_mut().insert_tab(),
            // Action::InsertLineAt(y, contents) => todo!("Action::InsertLineAt"),
            Action::DeletePreviousChar => self.current_window_mut().delete_previous_char(),
            Action::DeleteCharAtCursorPos => self.current_window_mut().delete_char_at_cursor(),
            Action::DeleteCharAt(x, y) => self.current_window_mut().delete_char_at(*x, *y),
            Action::DeleteWord => self.current_window_mut().delete_word(),
            Action::DeleteCurrentLine => self.current_window_mut().delete_current_line(),
            Action::DeleteLineAt(y) => self.current_window_mut().delete_line_at(*y),

            // Action::ReplaceLineAt(y, contents) => todo!("Action::ReplaceLineAt"),

            // buffer actions
            Action::OpenFile(path) => self.current_window_mut().open_file(path),
            Action::Save => self.current_window_mut().save_buffer(),
            Action::NextBuffer => {
                let current_buffer = &self.current_window().buffer;
                let current_buffer_pos = self
                    .buffers
                    .iter()
                    .position(|b| b == current_buffer)
                    .unwrap();
                let pos = if current_buffer_pos + 1 < self.buffers.len() {
                    current_buffer_pos + 1
                } else {
                    0
                };
                let buffer = self.buffers.get(pos).unwrap().clone();
                self.current_window_mut().set_buffer(buffer);
                ActionEffect::RedrawAll
            }
            Action::PreviousBuffer => {
                let current_buffer = &self.current_window().buffer;
                let current_buffer_pos = self
                    .buffers
                    .iter()
                    .position(|b| b == current_buffer)
                    .unwrap();
                let pos = if current_buffer_pos > 0 {
                    current_buffer_pos - 1
                } else {
                    self.buffers.len() - 1
                };
                let buffer = self.buffers.get(pos).unwrap().clone();
                self.current_window_mut().set_buffer(buffer);
                ActionEffect::RedrawAll
            }

            // search and replace
            Action::FindNext => {
                let search_term = self.search_term.clone();
                self.current_window_mut().find_next(&search_term)
            }
            Action::FindPrevious => {
                let search_term = self.search_term.clone();
                self.current_window_mut().find_previous(&search_term)
            }

            // dialogs
            Action::FilePicker => {
                let file_picker = FilePicker::new(self, std::env::current_dir()?)?;
                self.current_dialog = Some(Box::new(file_picker));
                ActionEffect::None
            }
            Action::ShowDialog => {
                if let Some(dialog) = &self.current_dialog {
                    dialog.draw(buffer)?;
                }
                ActionEffect::RedrawWindow
            }
            Action::CloseDialog => {
                self.current_dialog = None;
                ActionEffect::RedrawWindow
            }

            // command actions
            Action::Command(cmd) => ActionEffect::Actions(self.handle_command(cmd)),

            // editor actions
            Action::Suspend => {
                stdout().execute(terminal::LeaveAlternateScreen)?;
                let pid = Pid::from_raw(0);
                let _ = signal::kill(pid, Signal::SIGSTOP);
                stdout().execute(terminal::EnterAlternateScreen)?;
                ActionEffect::RedrawAll
            }

            action => {
                crate::log!("{action:?}");
                ActionEffect::None
            }
        };

        Ok(effect)
    }

    fn render(&mut self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.draw_windows(buffer)?;
        self.draw_statusline(buffer);
        self.draw_commandline(buffer);

        stdout().queue(Clear(ClearType::All))?.queue(MoveTo(0, 0))?;
        stdout().queue(style::SetBackgroundColor(self.theme.style.bg.unwrap()))?;

        let mut current_style = &self.theme.style;
        for cell in buffer.cells.iter() {
            if cell.style != *current_style {
                if let Some(bg) = cell.style.bg {
                    stdout().queue(style::SetBackgroundColor(bg))?;
                }
                if let Some(fg) = cell.style.fg {
                    stdout().queue(style::SetForegroundColor(fg))?;
                }
                if cell.style.italic {
                    stdout().queue(style::SetAttribute(style::Attribute::Italic))?;
                } else {
                    stdout().queue(style::SetAttribute(style::Attribute::NoItalic))?;
                }
                current_style = &cell.style;
            }

            stdout().queue(style::Print(cell.c))?;
        }

        self.draw_cursor()?;
        stdout().flush()?;

        Ok(())
    }

    async fn redraw(
        &mut self,
        current_buffer: &RenderBuffer,
        buffer: &mut RenderBuffer,
    ) -> anyhow::Result<()> {
        stdout().execute(Hide)?;

        self.draw_statusline(buffer);
        self.draw_commandline(buffer);
        // self.draw_diagnostics(buffer);
        self.draw_current_dialog(buffer)?;

        self.render_diff(buffer.diff(&current_buffer)).await?;
        self.draw_cursor()?;

        stdout().execute(Show)?;

        Ok(())
    }

    async fn render_diff(&self, change_set: Vec<Change<'_>>) -> anyhow::Result<()> {
        // FIXME: find a better place for this, probably inside the modifying
        // functions on the Buffer struct
        // if !change_set.is_empty() {
        //     plugin_registry
        //         .notify(
        //             runtime,
        //             "buffer:changed",
        //             json!(editor.current_buffer().contents()),
        //         )
        //         .await?;
        // }

        for change in change_set {
            let x = change.x;
            let y = change.y;
            let cell = change.cell;
            stdout().queue(MoveTo(x as u16, y as u16))?;
            if let Some(bg) = cell.style.bg {
                stdout().queue(style::SetBackgroundColor(bg))?;
            } else {
                stdout().queue(style::SetBackgroundColor(self.theme.style.bg.unwrap()))?;
            }
            if let Some(fg) = cell.style.fg {
                stdout().queue(style::SetForegroundColor(fg))?;
            } else {
                stdout().queue(style::SetForegroundColor(self.theme.style.fg.unwrap()))?;
            }
            if cell.style.italic {
                stdout().queue(style::SetAttribute(style::Attribute::Italic))?;
            } else {
                stdout().queue(style::SetAttribute(style::Attribute::NoItalic))?;
            }
            stdout().queue(style::Print(cell.c))?;
        }

        self.set_cursor_style()?;
        let (cx, cy) = self
            .cursor_position()
            .expect("editor cursor should be visible here");
        stdout().queue(cursor::MoveTo(cx, cy))?.flush()?;

        Ok(())
    }

    fn draw_windows(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        for (i, window) in self.windows.iter().enumerate() {
            log!("draw window");
            window.draw(buffer)?;
            if i < self.windows.len() - 1 {
                self.draw_divider(buffer, &window)?;
            }
        }

        Ok(())
    }

    fn draw_divider(&self, buffer: &mut RenderBuffer, window: &Window) -> anyhow::Result<()> {
        let x = window.x + window.width;
        let y = window.y;
        let height = window.height;
        // TODO: let style = self.theme.divider_style.clone();

        let style = Style {
            fg: Some(Color::Rgb {
                r: 0x20,
                g: 0x20,
                b: 0x20,
            }),
            bg: None,
            ..Default::default()
        };

        for i in 0..height {
            buffer.set_text(x, y + i, "│", &style);
        }

        Ok(())
    }

    fn split_horizontal(&mut self) -> ActionEffect {
        let num_windows = self.windows.len() + 1;
        let num_dividers = num_windows - 1;
        let width = (self.width - num_dividers) / num_windows;
        let height = self.height;

        self.windows.push(Window::new(
            width + 1,
            0,
            width / 2,
            height,
            self.current_window().buffer.clone(),
            self.theme.style.clone(),
            self.theme.gutter_style.clone(),
            &self.highlighter.clone(),
        ));

        for n in 0..self.windows.len() {
            let x = n * width + n;
            let mut width = width;
            if n == self.windows.len() - 1 {
                width = self.width - x;
            }
            self.windows
                .get_mut(n)
                .unwrap()
                .resize_move(x, 0, width, height);
        }

        ActionEffect::RedrawAll
    }

    fn next_window(&mut self) -> ActionEffect {
        if self.focused_window + 1 < self.windows.len() {
            self.focused_window += 1;
        } else {
            self.focused_window = 0;
        }

        ActionEffect::RedrawAll
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.width = width as usize;
        self.height = height as usize;

        for window in &mut self.windows {
            window.resize(self.width, self.height);
        }
    }

    pub fn draw_cursor(&mut self) -> anyhow::Result<()> {
        self.set_cursor_style()?;
        self.check_bounds();

        if let Some((x, y)) = self.cursor_position() {
            stdout().queue(cursor::MoveTo(x, y))?;
        } else {
            stdout().queue(cursor::Hide)?;
        }

        Ok(())
    }

    pub fn draw_statusline(&self, buffer: &mut RenderBuffer) {
        let mode = format!(" {:?} ", self.mode).to_uppercase();
        let dirty = if self.current_window().is_dirty() {
            " [+] "
        } else {
            ""
        };
        let file = format!(" {}{}", self.current_window().buffer_name(), dirty);
        let cursor_pos = self.current_window().cursor_location();
        let pos = format!(" {}:{} ", cursor_pos.1 + 1, cursor_pos.0 + 1);

        let file_width = self.width.saturating_sub(mode.len() + pos.len() + 2);
        let y = self.height - 2;

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

    fn draw_commandline(&self, buffer: &mut RenderBuffer) {
        let y = self.height - 1;

        if !self.has_term() {
            let wc = if let Some(ref waiting_command) = self.waiting_command {
                waiting_command.clone()
            } else if let Some(ref repeater) = self.repeater {
                format!("{}", repeater)
            } else {
                String::new()
            };
            let wc = format!("{:<width$}", wc, width = 10);

            // TODO: different styling for error and message, and what to do if we have both?
            if let Some(ref last_error) = self.last_error {
                let error = format!("{:width$}", last_error, width = self.width);
                buffer.set_text(0, y, &error, &self.theme.style);
            } else if let Some(ref message) = self.last_message {
                let message = format!("{:width$}", message, width = self.width);
                buffer.set_text(0, y, &message, &self.theme.style);
            } else {
                let clear_line = " ".repeat(self.width - 10);
                buffer.set_text(0, y, &clear_line, &self.theme.style);
            }

            buffer.set_text(self.width - 10, y, &wc, &self.theme.style);

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
            width = self.width - self.command.len() - 1
        );
        buffer.set_text(0, y, &cmdline, &self.theme.style);
    }

    fn draw_current_dialog(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        if let Some(current_dialog) = &self.current_dialog {
            current_dialog.draw(buffer)?;
        }

        Ok(())
    }

    fn current_window(&self) -> &Window {
        &self.windows[self.focused_window]
    }

    fn current_window_mut(&mut self) -> &mut Window {
        &mut self.windows[self.focused_window]
    }

    fn cursor_position(&self) -> Option<(u16, u16)> {
        // TODO: refactor this out to allow for dynamic setting of the cursor "target",
        // so we could transition from the editor to dialogs, to searches, etc.
        if let Some(current_dialog) = &self.current_dialog {
            current_dialog.cursor_position()
        } else if self.has_term() {
            Some((self.term().len() as u16 + 1, self.height as u16 - 1))
        } else {
            Some(self.current_window().cursor_position())
        }
    }

    // fn line_length(&self) -> usize {
    //     if let Some(line) = self.viewport_line(self.cy) {
    //         return line.len();
    //     }
    //     0
    // }
    //
    // fn buffer_line(&self) -> usize {
    //     self.vtop + self.cy as usize
    // }
    //
    // fn viewport_line(&self, n: usize) -> Option<String> {
    //     let buffer_line = self.vtop + n;
    //     self.current_buffer().get(buffer_line)
    // }

    fn set_cursor_style(&self) -> anyhow::Result<()> {
        stdout().queue(match self.waiting_key_action {
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

    // fn fill_line(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize, style: &Style) {
    //     let width = self.vwidth().saturating_sub(x);
    //     let line_fill = " ".repeat(width);
    //     buffer.set_text(x, y, &line_fill, style);
    // }

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
        let mode = self.mode;
        self.current_window_mut().check_bounds(&mode);
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
        self.last_message = None;

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

    // fn handle_waiting_command(&mut self, ka: KeyAction, ev: &event::Event) -> Option<KeyAction> {
    //     let KeyAction::Nested(nested_mappings) = ka else {
    //         panic!("expected nested mappings");
    //     };
    //
    //     event_to_key_action(self, &nested_mappings, &ev)
    // }

    pub fn cleanup(&mut self) -> anyhow::Result<()> {
        stdout()
            .execute(terminal::LeaveAlternateScreen)?
            .execute(event::DisableMouseCapture)?;
        terminal::disable_raw_mode()?;

        Ok(())
    }

    // fn current_line_contents(&self) -> Option<String> {
    //     self.current_buffer().get(self.buffer_line())
    // }

    // fn previous_line_indentation(&self) -> usize {
    //     if self.buffer_line() > 0 {
    //         self.current_buffer()
    //             .get(self.buffer_line() - 1)
    //             .unwrap_or_default()
    //             .chars()
    //             .position(|c| !c.is_whitespace())
    //             .unwrap_or(0)
    //     } else {
    //         0
    //     }
    // }

    // fn current_line_indentation(&self) -> usize {
    //     self.current_line_contents()
    //         .unwrap_or_default()
    //         .chars()
    //         .position(|c| !c.is_whitespace())
    //         .unwrap_or(0)
    // }

    // fn set_current_buffer(
    //     &mut self,
    //     theme: &Theme,
    //     viewport: &Viewport,
    //     render_buffer: &mut RenderBuffer,
    //     index: usize,
    // ) -> anyhow::Result<()> {
    //     let vtop = self.vtop;
    //     let pos = (self.cx, self.cy);
    //
    //     let buffer = self.current_buffer_mut();
    //     buffer.vtop = vtop;
    //     buffer.pos = pos;
    //
    //     self.current_buffer = index;
    //
    //     let (cx, cy) = self.current_buffer().pos;
    //     let vtop = self.current_buffer().vtop;
    //
    //     log!(
    //         "new vtop = {vtop}, new pos = ({cx}, {cy})",
    //         vtop = vtop,
    //         cx = cx,
    //         cy = cy
    //     );
    //     self.cx = cx;
    //     self.cy = cy;
    //     self.vtop = vtop;
    //
    //     viewport.draw(render_buffer, &self.current_buffer().lines, 0, 0)?;
    //
    //     Ok(())
    // }

    // fn is_within_viewport(&self, y: usize) -> bool {
    //     (self.vtop..self.vtop + self.vheight()).contains(&y)
    // }
    //
    // fn is_within_last_page(&self, y: usize) -> bool {
    //     y > self.current_buffer().len() - self.vheight()
    // }
    //
    // fn is_within_first_page(&self, y: usize) -> bool {
    //     y < self.vheight()
    // }
    //
    // fn visible_diagnostics(&self) -> Vec<&Diagnostic> {
    //     self.current_buffer()
    //         .diagnostics_for_lines(self.vtop, self.vtop + self.vheight())
    // }
    //
    // fn current_buffer(&self) -> &Buffer {
    //     &self.buffers[self.current_buffer]
    // }
    //
    // fn current_buffer_mut(&mut self) -> &mut Buffer {
    //     &mut self.buffers[self.current_buffer]
    // }

    fn modified_buffers(&self) -> Vec<String> {
        self.buffers
            .iter()
            .filter_map(|b| {
                let buffer = b.lock_read().expect("lock is poisoned");
                if buffer.is_dirty() {
                    let name = buffer.name().to_owned();
                    Some(name)
                } else {
                    None
                }
            })
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

impl From<&SharedBuffer> for BufferInfo {
    fn from(buffer: &SharedBuffer) -> Self {
        let buffer = buffer.lock_read().expect("lock is poisoned");
        Self {
            name: buffer.name().to_string(),
            dirty: buffer.is_dirty(),
        }
    }
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
