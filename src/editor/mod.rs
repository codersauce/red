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
    theme::{self, Style, Theme},
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Command,
    Search,
}

pub async fn run(config: Config, editor: &mut Editor) -> anyhow::Result<()> {
    let theme_file = &Config::path("themes").join(&config.theme);
    let theme = theme::parse_vscode_theme(&theme_file.to_string_lossy())?;

    let size = terminal::size()?;

    terminal::enable_raw_mode()?;

    stdout()
        .execute(event::EnableMouseCapture)?
        .execute(terminal::EnterAlternateScreen)?
        .execute(terminal::Clear(terminal::ClearType::All))?;

    let mut runtime = Runtime::new();
    let mut plugin_registry = PluginRegistry::new();
    for (name, path) in &config.plugins {
        let path = Config::path("plugins").join(path);
        plugin_registry.add(name, path.to_string_lossy().as_ref());
    }
    plugin_registry.initialize(&mut runtime).await?;

    let mut lsp = LspClient::start().await?;
    lsp.initialize().await?;

    let mut buffer = RenderBuffer::new(size.0 as usize, size.1 as usize, theme.style.clone());
    render(&theme, editor, &mut buffer)?;

    let mut reader = EventStream::new();

    loop {
        let mut delay = futures_timer::Delay::new(Duration::from_millis(10)).fuse();
        let mut event = reader.next().fuse();

        select! {
            _ = delay => {
                // handle responses from lsp
                if let Some((msg, method)) = lsp.recv_response().await? {
                    if let Some(action) = handle_lsp_message(editor, &msg, method) {
                        // TODO: handle quit
                        let current_buffer = buffer.clone();
                        execute(&action, editor, &theme, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
                        redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
                    }
                }

                if let Some(req) = ACTION_DISPATCHER.try_recv_request() {
                    match req {
                        PluginRequest::Action(action) => {
                            let current_buffer = buffer.clone();
                            execute(&action, editor, &theme, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
                            redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
                        }
                        PluginRequest::EditorInfo(id) => {
                            let info = serde_json::to_value(editor.info())?;
                            let key = if let Some(id) = id {
                                format!("editor:info:{}", id)
                            } else {
                                "editor:info".to_string()
                            };
                            plugin_registry
                                .notify(&mut runtime, &key, info)
                                .await?;
                        }
                        PluginRequest::OpenPicker(title, id, items) => {
                            let current_buffer = buffer.clone();
                            let items = items.iter().map(|v| match v {
                                serde_json::Value::String(s) => s.clone(),
                                val => val.to_string(),
                            }).collect();

                            execute(&Action::OpenPicker(title, items, id), editor, &theme, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await?;
                            redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
                        }
                    }
                }
            }
            maybe_event = event => {
                match maybe_event {
                    Some(Ok(ev)) => {
                        let current_buffer = buffer.clone();
                        check_bounds(editor);

                        if let event::Event::Resize(width, height) = ev {
                            editor.size = (width, height);
                            let max_y = height as usize - 2;
                            if editor.cy > max_y - 1 {
                                editor.cy = max_y - 1;
                            }
                            buffer = RenderBuffer::new(
                                editor.size.0 as usize,
                                editor.size.1 as usize,
                                theme.style.clone(),
                            );
                            render(&theme, editor, &mut buffer)?;
                            continue;
                        }

                        if let Some(action) = handle_event(editor, &config, &ev) {
                            if handle_key_action(&ev, &action, editor, &theme, &config, &mut buffer, &mut lsp, &mut runtime, &mut plugin_registry).await? {
                                log!("requested to quit");
                                break;
                            }
                        }

                            redraw(&theme, &config, editor, &mut runtime, &mut plugin_registry, &current_buffer, &mut buffer).await?;
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

fn handle_event(editor: &mut Editor, config: &Config, ev: &event::Event) -> Option<KeyAction> {
    if let Some(ka) = editor.waiting_key_action.take() {
        editor.waiting_command = None;
        return editor.handle_waiting_command(ka, ev);
    }

    if let Some(current_dialog) = &mut editor.current_dialog {
        return current_dialog.handle_event(ev);
    }

    match editor.mode {
        Mode::Normal => handle_normal_event(editor, config, ev),
        Mode::Insert => handle_insert_event(editor, config, ev),
        Mode::Command => editor.handle_command_event(ev),
        Mode::Search => editor.handle_search_event(ev),
    }
}

async fn redraw(
    theme: &Theme,
    config: &Config,
    editor: &mut Editor,
    runtime: &mut Runtime,
    plugin_registry: &mut PluginRegistry,
    current_buffer: &RenderBuffer,
    buffer: &mut RenderBuffer,
) -> anyhow::Result<()> {
    stdout().execute(Hide)?;
    draw_statusline(theme, editor, buffer);
    draw_commandline(theme, editor, buffer);
    draw_diagnostics(theme, config, editor, buffer);
    draw_current_dialog(editor, buffer)?;
    render_diff(
        theme,
        editor,
        runtime,
        plugin_registry,
        buffer.diff(&current_buffer),
    )
    .await?;
    draw_cursor(theme, editor, buffer)?;
    stdout().execute(Show)?;

    Ok(())
}

pub fn draw_cursor(
    theme: &Theme,
    editor: &mut Editor,
    buffer: &mut RenderBuffer,
) -> anyhow::Result<()> {
    editor.set_cursor_style()?;
    editor.check_bounds();

    // TODO: refactor this out to allow for dynamic setting of the cursor "target",
    // so we could transition from the editor to dialogs, to searches, etc.
    let cursor_pos = if let Some(current_dialog) = &editor.current_dialog {
        current_dialog.cursor_position()
    } else if editor.has_term() {
        Some((editor.term().len() as u16 + 1, (editor.size.1 - 1) as u16))
    } else {
        Some((editor.cx as u16, editor.cy as u16))
    };

    if let Some((x, y)) = cursor_pos {
        stdout().queue(cursor::MoveTo(x, y))?;
    } else {
        stdout().queue(cursor::Hide)?;
    }
    draw_statusline(theme, editor, buffer);

    Ok(())
}

async fn render_diff(
    theme: &Theme,
    editor: &mut Editor,
    runtime: &mut Runtime,
    plugin_registry: &mut PluginRegistry,
    change_set: Vec<Change<'_>>,
) -> anyhow::Result<()> {
    // FIXME: find a better place for this, probably inside the modifying
    // functions on the Buffer struct
    if !change_set.is_empty() {
        plugin_registry
            .notify(
                runtime,
                "buffer:changed",
                json!(editor.current_buffer().contents()),
            )
            .await?;
    }

    for change in change_set {
        let x = change.x;
        let y = change.y;
        let cell = change.cell;
        stdout().queue(MoveTo(x as u16, y as u16))?;
        if let Some(bg) = cell.style.bg {
            stdout().queue(style::SetBackgroundColor(bg))?;
        } else {
            stdout().queue(style::SetBackgroundColor(theme.style.bg.unwrap()))?;
        }
        if let Some(fg) = cell.style.fg {
            stdout().queue(style::SetForegroundColor(fg))?;
        } else {
            stdout().queue(style::SetForegroundColor(theme.style.fg.unwrap()))?;
        }
        if cell.style.italic {
            stdout().queue(style::SetAttribute(style::Attribute::Italic))?;
        } else {
            stdout().queue(style::SetAttribute(style::Attribute::NoItalic))?;
        }
        stdout().queue(style::Print(cell.c))?;
    }

    editor.set_cursor_style()?;
    stdout()
        .queue(cursor::MoveTo((editor.cx) as u16, editor.cy as u16))?
        .flush()?;

    Ok(())
}

fn draw_current_dialog(editor: &Editor, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
    if let Some(current_dialog) = &editor.current_dialog {
        current_dialog.draw(buffer)?;
    }

    Ok(())
}

fn draw_diagnostics(
    theme: &Theme,
    config: &Config,
    editor: &mut Editor,
    buffer: &mut RenderBuffer,
) {
    if !config.show_diagnostics {
        return;
    }

    let fg = adjust_color_brightness(theme.style.fg, -20);
    let bg = adjust_color_brightness(theme.style.bg, 10);

    let hint_style = Style {
        fg,
        bg,
        italic: true,
        ..Default::default()
    };

    let mut diagnostics_per_line = HashMap::new();
    for diag in editor.visible_diagnostics() {
        let line = diagnostics_per_line
            .entry(diag.range.start.line)
            .or_insert_with(Vec::new);
        line.push(diag);
    }

    for (l, diags) in diagnostics_per_line {
        let line = editor.current_buffer().get(l);
        let len = line.clone().map(|l| l.len()).unwrap_or(0);
        let y = l - editor.vtop;
        let x = len + 5;
        let msg = format!("■ {}", diags[0].message.lines().next().unwrap());
        buffer.set_text(x, y, &msg, &hint_style);
    }
}

fn draw_commandline(theme: &Theme, editor: &mut Editor, buffer: &mut RenderBuffer) {
    let style = &theme.style;
    let y = editor.size.1 as usize - 1;

    if !editor.has_term() {
        let wc = if let Some(ref waiting_command) = editor.waiting_command {
            waiting_command.clone()
        } else if let Some(ref repeater) = editor.repeater {
            format!("{}", repeater)
        } else {
            String::new()
        };
        let wc = format!("{:<width$}", wc, width = 10);

        if let Some(ref last_error) = editor.last_error {
            let error = format!("{:width$}", last_error, width = editor.size.0 as usize);
            buffer.set_text(0, editor.size.1 as usize - 1, &error, style);
        } else {
            let clear_line = " ".repeat(editor.size.0 as usize - 10);
            buffer.set_text(0, y, &clear_line, style);
        }

        buffer.set_text(editor.size.0 as usize - 10, y, &wc, style);

        return;
    }

    let text = if editor.is_command() {
        &editor.command
    } else {
        &editor.search_term
    };
    let prefix = if editor.is_command() { ":" } else { "/" };
    let cmdline = format!(
        "{}{:width$}",
        prefix,
        text,
        width = editor.size.0 as usize - editor.command.len() - 1
    );
    buffer.set_text(0, editor.size.1 as usize - 1, &cmdline, style);
}

pub fn draw_statusline(theme: &Theme, editor: &Editor, buffer: &mut RenderBuffer) {
    let mode = format!(" {:?} ", editor.mode).to_uppercase();
    let dirty = if editor.current_buffer().is_dirty() {
        " [+] "
    } else {
        ""
    };
    let file = format!(" {}{}", editor.current_buffer().name(), dirty);
    let pos = format!(" {}:{} ", editor.vtop + editor.cy + 1, editor.cx + 1);

    let file_width = editor
        .size
        .0
        .saturating_sub(mode.len() as u16 + pos.len() as u16 + 2);
    let y = editor.size.1 as usize - 2;

    let transition_style = Style {
        fg: theme.statusline_style.outer_style.bg,
        bg: theme.statusline_style.inner_style.bg,
        ..Default::default()
    };

    buffer.set_text(0, y, &mode, &theme.statusline_style.outer_style);

    buffer.set_text(
        mode.len(),
        y,
        &theme.statusline_style.outer_chars[1].to_string(),
        &transition_style,
    );

    buffer.set_text(
        mode.len() + 1,
        y,
        &format!("{:<width$}", file, width = file_width as usize),
        &theme.statusline_style.inner_style,
    );

    buffer.set_text(
        mode.len() + 1 + file_width as usize,
        y,
        &theme.statusline_style.outer_chars[2].to_string(),
        &transition_style,
    );

    buffer.set_text(
        mode.len() + 2 + file_width as usize,
        y,
        &pos,
        &theme.statusline_style.outer_style,
    );
}

#[async_recursion::async_recursion]
async fn execute(
    action: &Action,
    mut editor: &mut Editor,
    theme: &Theme,
    config: &Config,
    mut buffer: &mut RenderBuffer,
    mut lsp: &mut LspClient,
    mut runtime: &mut Runtime,
    mut plugin_registry: &mut PluginRegistry,
) -> anyhow::Result<bool> {
    editor.last_error = None;
    match action {
        Action::Quit(force) => {
            if *force {
                return Ok(true);
            }
            let modified_buffers = editor.modified_buffers();
            if modified_buffers.is_empty() {
                return Ok(true);
            }
            editor.last_error = Some(format!(
                "The following buffers have unwritten changes: {}",
                modified_buffers.join(", ")
            ));
            return Ok(false);
        }
        Action::MoveUp => {
            if editor.cy == 0 {
                // scroll up
                if editor.vtop > 0 {
                    editor.vtop -= 1;
                    draw_viewport(theme, editor, buffer)?;
                }
            } else {
                editor.cy = editor.cy.saturating_sub(1);
                draw_cursor(theme, editor, buffer)?;
            }
        }
        Action::MoveDown => {
            if editor.vtop + editor.cy < editor.current_buffer().len() - 1 {
                editor.cy += 1;
                if editor.cy >= editor.vheight() {
                    // scroll if possible
                    editor.vtop += 1;
                    editor.cy -= 1;
                    draw_viewport(theme, editor, buffer)?;
                }
            } else {
                draw_cursor(theme, editor, buffer)?;
            }
        }
        Action::MoveLeft => {
            editor.cx = editor.cx.saturating_sub(1);
            if editor.cx < editor.vleft {
                editor.cx = editor.vleft;
            } else {
            }
        }
        Action::MoveRight => {
            editor.cx += 1;
        }
        Action::MoveToLineStart => {
            editor.cx = 0;
        }
        Action::MoveToLineEnd => {
            editor.cx = editor.line_length().saturating_sub(1);
        }
        Action::PageUp => {
            if editor.vtop > 0 {
                editor.vtop = editor.vtop.saturating_sub(editor.vheight() as usize);
                draw_viewport(theme, editor, buffer)?;
            }
        }
        Action::PageDown => {
            if editor.current_buffer().len() > editor.vtop + editor.vheight() as usize {
                editor.vtop += editor.vheight() as usize;
                draw_viewport(theme, editor, buffer)?;
            }
        }
        Action::EnterMode(new_mode) => {
            // TODO: with the introduction of new modes, maybe this transtion
            // needs to be widened to anything -> insert and anything -> normal
            if editor.is_normal() && matches!(new_mode, Mode::Insert) {
                editor.insert_undo_actions = Vec::new();
            }
            if editor.is_insert() && matches!(new_mode, Mode::Normal) {
                if !editor.insert_undo_actions.is_empty() {
                    let actions = mem::take(&mut editor.insert_undo_actions);
                    editor.undo_actions.push(Action::UndoMultiple(actions));
                }
            }
            if editor.has_term() {
                draw_commandline(theme, editor, buffer);
            }

            if matches!(new_mode, Mode::Search) {
                editor.search_term = String::new();
            }

            editor.mode = *new_mode;
            draw_statusline(theme, editor, buffer);
        }
        Action::InsertCharAtCursorPos(c) => {
            editor
                .insert_undo_actions
                .push(Action::DeleteCharAt(editor.cx, editor.buffer_line()));
            let line = editor.buffer_line();
            let cx = editor.cx;

            editor.current_buffer_mut().insert(cx, line, *c);
            notify_change(lsp, editor).await?;
            editor.cx += 1;
            draw_line(editor, buffer);
        }
        Action::DeleteCharAt(x, y) => {
            editor.current_buffer_mut().remove(*x, *y);
            notify_change(lsp, editor).await?;
            draw_line(editor, buffer);
        }
        Action::DeleteCharAtCursorPos => {
            let cx = editor.cx;
            let line = editor.buffer_line();

            editor.current_buffer_mut().remove(cx, line);
            notify_change(lsp, editor).await?;
            draw_line(editor, buffer);
        }
        Action::ReplaceLineAt(y, contents) => {
            editor
                .current_buffer_mut()
                .replace_line(*y, contents.to_string());
            notify_change(lsp, editor).await?;
            draw_line(editor, buffer);
        }
        Action::InsertNewLine => {
            editor.insert_undo_actions.extend(vec![
                Action::MoveTo(editor.cx, editor.buffer_line() + 1),
                Action::DeleteLineAt(editor.buffer_line() + 1),
                Action::ReplaceLineAt(
                    editor.buffer_line(),
                    editor.current_line_contents().unwrap_or_default(),
                ),
            ]);
            let spaces = editor.current_line_indentation();

            let current_line = editor.current_line_contents().unwrap_or_default();
            let before_cursor = current_line[..editor.cx].to_string();
            let after_cursor = current_line[editor.cx..].to_string();

            let line = editor.buffer_line();
            editor
                .current_buffer_mut()
                .replace_line(line, before_cursor);
            notify_change(lsp, editor).await?;

            editor.cx = spaces;
            editor.cy += 1;

            let new_line = format!("{}{}", " ".repeat(spaces), &after_cursor);
            let line = editor.buffer_line();

            editor.current_buffer_mut().insert_line(line, new_line);
            draw_viewport(theme, editor, buffer)?;
        }
        Action::SetWaitingKeyAction(key_action) => {
            editor.waiting_key_action = Some(*(key_action.clone()));
        }
        Action::DeleteCurrentLine => {
            let line = editor.buffer_line();
            let contents = editor.current_line_contents();

            editor.current_buffer_mut().remove_line(line);
            notify_change(lsp, editor).await?;
            editor
                .undo_actions
                .push(Action::InsertLineAt(line, contents));

            draw_viewport(theme, editor, buffer)?;
        }
        Action::Undo => {
            if let Some(undo_action) = editor.undo_actions.pop() {
                execute(
                    &undo_action,
                    &mut editor,
                    &theme,
                    &config,
                    &mut buffer,
                    &mut lsp,
                    &mut runtime,
                    &mut plugin_registry,
                )
                .await?;
            }
        }
        Action::UndoMultiple(actions) => {
            for action in actions.iter().rev() {
                execute(
                    &action,
                    &mut editor,
                    &theme,
                    &config,
                    &mut buffer,
                    &mut lsp,
                    &mut runtime,
                    &mut plugin_registry,
                )
                .await?;
            }
        }
        Action::InsertLineAt(y, contents) => {
            if let Some(contents) = contents {
                editor
                    .current_buffer_mut()
                    .insert_line(*y, contents.to_string());
                notify_change(lsp, editor).await?;
                draw_viewport(theme, editor, buffer)?;
            }
        }
        Action::MoveLineToViewportCenter => {
            let viewport_center = editor.vheight() / 2;
            let distance_to_center = editor.cy as isize - viewport_center as isize;

            if distance_to_center > 0 {
                // if distance > 0 we need to scroll up
                let distance_to_center = distance_to_center.abs() as usize;
                if editor.vtop > distance_to_center {
                    let new_vtop = editor.vtop + distance_to_center;
                    editor.vtop = new_vtop;
                    editor.cy = viewport_center;
                    draw_viewport(theme, editor, buffer)?;
                }
            } else if distance_to_center < 0 {
                // if distance < 0 we need to scroll down
                let distance_to_center = distance_to_center.abs() as usize;
                let new_vtop = editor.vtop.saturating_sub(distance_to_center);
                let distance_to_go = editor.vtop as usize + distance_to_center;
                if editor.current_buffer().len() > distance_to_go && new_vtop != editor.vtop {
                    editor.vtop = new_vtop;
                    editor.cy = viewport_center;
                    draw_viewport(theme, editor, buffer)?;
                }
            }
        }
        Action::InsertLineBelowCursor => {
            editor
                .undo_actions
                .push(Action::DeleteLineAt(editor.buffer_line() + 1));

            let leading_spaces = editor.current_line_indentation();
            let line = editor.buffer_line();
            editor
                .current_buffer_mut()
                .insert_line(line + 1, " ".repeat(leading_spaces));
            notify_change(lsp, editor).await?;
            editor.cy += 1;
            editor.cx = leading_spaces;
            draw_viewport(theme, editor, buffer)?;
        }
        Action::InsertLineAtCursor => {
            editor
                .undo_actions
                .push(Action::DeleteLineAt(editor.buffer_line()));

            // if the current line is empty, let's use the indentation from the line above
            let leading_spaces = if let Some(line) = editor.current_line_contents() {
                if line.is_empty() {
                    editor.previous_line_indentation()
                } else {
                    editor.current_line_indentation()
                }
            } else {
                editor.previous_line_indentation()
            };

            let line = editor.buffer_line();
            editor
                .current_buffer_mut()
                .insert_line(line, " ".repeat(leading_spaces));
            notify_change(lsp, editor).await?;
            editor.cx = leading_spaces;
            draw_viewport(theme, editor, buffer)?;
        }
        Action::MoveToTop => {
            editor.vtop = 0;
            editor.cy = 0;
            draw_viewport(theme, editor, buffer)?;
        }
        Action::MoveToBottom => {
            if editor.current_buffer().len() > editor.vheight() as usize {
                editor.cy = editor.vheight() - 1;
                editor.vtop = editor.current_buffer().len() - editor.vheight() as usize;
                draw_viewport(theme, editor, buffer)?;
            } else {
                editor.cy = editor.current_buffer().len() - 1;
            }
        }
        Action::DeleteLineAt(y) => {
            editor.current_buffer_mut().remove_line(*y);
            notify_change(lsp, editor).await?;
            draw_viewport(theme, editor, buffer)?;
        }
        Action::DeletePreviousChar => {
            if editor.cx > 0 {
                editor.cx -= 1;
                let cx = editor.cx;
                let line = editor.buffer_line();
                editor.current_buffer_mut().remove(cx, line);
                notify_change(lsp, editor).await?;
                draw_line(editor, buffer);
            }
        }
        Action::DumpBuffer => {
            log!("{buffer}", buffer = buffer.dump());
        }
        Action::Command(cmd) => {
            log!("Handling command: {cmd}");

            for action in editor.handle_command(cmd) {
                editor.last_error = None;
                if execute(
                    &action,
                    &mut editor,
                    &theme,
                    &config,
                    &mut buffer,
                    &mut lsp,
                    &mut runtime,
                    &mut plugin_registry,
                )
                .await?
                {
                    return Ok(true);
                }
            }
        }
        Action::PluginCommand(cmd) => {
            plugin_registry.execute(runtime, cmd).await?;
        }
        Action::GoToLine(line) => {
            go_to_line(
                editor,
                theme,
                config,
                buffer,
                lsp,
                runtime,
                plugin_registry,
                *line,
                GoToLinePosition::Center,
            )
            .await?;
        }
        Action::GoToDefinition => {
            if let Some(file) = editor.current_buffer().file.clone() {
                lsp.goto_definition(&file, editor.cx, editor.cy + editor.vtop)
                    .await?;
            }
        }
        Action::Hover => {
            if let Some(file) = editor.current_buffer().file.clone() {
                lsp.hover(&file, editor.cx, editor.cy + editor.vtop).await?;
            }
        }
        Action::MoveTo(x, y) => {
            go_to_line(
                editor,
                theme,
                config,
                buffer,
                lsp,
                runtime,
                plugin_registry,
                *y,
                GoToLinePosition::Center,
            )
            .await?;
            editor.cx = std::cmp::min(*x, editor.line_length().saturating_sub(1));
        }
        Action::SetCursor(x, y) => {
            editor.cx = *x;
            editor.cy = *y;
        }
        Action::ScrollUp => {
            let scroll_lines = config.mouse_scroll_lines.unwrap_or(3);
            if editor.vtop > scroll_lines {
                editor.vtop -= scroll_lines;
                let desired_cy = editor.cy + scroll_lines;
                if desired_cy <= editor.vheight() {
                    editor.cy = desired_cy;
                }
                draw_viewport(theme, editor, buffer)?;
            }
        }
        Action::ScrollDown => {
            if editor.current_buffer().len() > editor.vtop + editor.vheight() as usize {
                editor.vtop += config.mouse_scroll_lines.unwrap_or(3);
                let desired_cy = editor
                    .cy
                    .saturating_sub(config.mouse_scroll_lines.unwrap_or(3));
                editor.cy = desired_cy;
                draw_viewport(theme, editor, buffer)?;
            }
        }
        Action::MoveToNextWord => {
            let next_word = editor
                .current_buffer()
                .find_next_word((editor.cx, editor.buffer_line()));

            if let Some((x, y)) = next_word {
                editor.cx = x;
                go_to_line(
                    editor,
                    theme,
                    config,
                    buffer,
                    lsp,
                    runtime,
                    plugin_registry,
                    y + 1,
                    GoToLinePosition::Top,
                )
                .await?;
                draw_cursor(theme, editor, buffer)?;
            }
        }
        Action::MoveToPreviousWord => {
            let previous_word = editor
                .current_buffer()
                .find_prev_word((editor.cx, editor.buffer_line()));

            if let Some((x, y)) = previous_word {
                editor.cx = x;
                go_to_line(
                    editor,
                    theme,
                    config,
                    buffer,
                    lsp,
                    runtime,
                    plugin_registry,
                    y + 1,
                    GoToLinePosition::Top,
                )
                .await?;
                draw_cursor(theme, editor, buffer)?;
            }
        }
        Action::MoveLineToViewportBottom => {
            let line = editor.buffer_line();
            if line > editor.vtop + editor.vheight() {
                editor.vtop = line - editor.vheight();
                editor.cy = editor.vheight() - 1;

                draw_viewport(theme, editor, buffer)?;
            }
        }
        Action::InsertTab => {
            // TODO: Tab configuration
            let tabsize = 4;
            let cx = editor.cx;
            let line = editor.buffer_line();
            editor
                .current_buffer_mut()
                .insert_str(cx, line, &" ".repeat(tabsize));
            notify_change(lsp, editor).await?;
            editor.cx += tabsize;
            draw_line(editor, buffer);
        }
        Action::Save => match editor.current_buffer_mut().save() {
            Ok(msg) => {
                // TODO: use last_message instead of last_error
                editor.last_error = Some(msg);
            }
            Err(e) => {
                editor.last_error = Some(e.to_string());
            }
        },
        Action::FindPrevious => {
            if let Some((x, y)) = editor
                .current_buffer()
                .find_prev(&editor.search_term, (editor.cx, editor.vtop + editor.cy))
            {
                editor.cx = x;
                go_to_line(
                    editor,
                    theme,
                    config,
                    buffer,
                    lsp,
                    runtime,
                    plugin_registry,
                    y + 1,
                    GoToLinePosition::Center,
                )
                .await?;
            }
        }
        Action::FindNext => {
            if let Some((x, y)) = editor
                .current_buffer()
                .find_next(&editor.search_term, (editor.cx, editor.vtop + editor.cy))
            {
                editor.cx = x;
                go_to_line(
                    editor,
                    theme,
                    config,
                    buffer,
                    lsp,
                    runtime,
                    plugin_registry,
                    y + 1,
                    GoToLinePosition::Center,
                )
                .await?;
            }
        }
        Action::DeleteWord => {
            let cx = editor.cx;
            let line = editor.buffer_line();
            editor.current_buffer_mut().delete_word((cx, line));
            notify_change(lsp, editor).await?;
            draw_line(editor, buffer);
        }
        Action::NextBuffer => {
            let new_index = if editor.current_buffer_index < editor.buffers.len() - 1 {
                editor.current_buffer_index + 1
            } else {
                0
            };
            editor.set_current_buffer(theme, buffer, new_index)?;
        }
        Action::PreviousBuffer => {
            let new_index = if editor.current_buffer_index > 0 {
                editor.current_buffer_index - 1
            } else {
                editor.buffers.len() - 1
            };
            editor.set_current_buffer(theme, buffer, new_index)?;
        }
        Action::OpenBuffer(name) => {
            if let Some(index) = editor.buffers.iter().position(|b| b.name() == *name) {
                editor.set_current_buffer(theme, buffer, index)?;
            }
        }
        Action::OpenFile(path) => {
            let new_buffer = match Buffer::from_file(&mut lsp, Some(path.to_string())).await {
                Ok(buffer) => buffer,
                Err(e) => {
                    editor.last_error = Some(e.to_string());
                    return Ok(false);
                }
            };
            editor.buffers.push(new_buffer);
            editor.set_current_buffer(theme, buffer, editor.buffers.len() - 1)?;
            render(&theme, editor, buffer)?;
        }
        Action::FilePicker => {
            let file_picker = FilePicker::new(&editor, std::env::current_dir()?)?;
            file_picker.draw(buffer)?;

            editor.current_dialog = Some(Box::new(file_picker));
        }
        Action::ShowDialog => {
            if let Some(dialog) = &mut editor.current_dialog {
                dialog.draw(buffer)?;
            }
        }
        Action::CloseDialog => {
            editor.current_dialog = None;
            draw_viewport(theme, editor, buffer)?;
        }
        Action::RefreshDiagnostics => {
            draw_diagnostics(theme, config, editor, buffer);
        }
        Action::Print(msg) => {
            editor.last_error = Some(msg.clone());
        }
        Action::OpenPicker(title, items, id) => {
            let picker = Picker::new(title.clone(), &editor, &items, *id);
            picker.draw(buffer)?;

            editor.current_dialog = Some(Box::new(picker));
        }
        Action::Picked(item, id) => {
            log!("picked: {item} - {id:?}");
            if let Some(id) = id {
                plugin_registry
                    .notify(
                        runtime,
                        &format!("picker:selected:{}", id),
                        serde_json::Value::String(item.clone()),
                    )
                    .await?;
            }
        }
        Action::Suspend => {
            stdout().execute(terminal::LeaveAlternateScreen)?;
            let pid = Pid::from_raw(0);
            let _ = signal::kill(pid, Signal::SIGSTOP);
            stdout().execute(terminal::EnterAlternateScreen)?;
            render(&theme, editor, buffer)?;
        }
        Action::ToggleWrap => {
            editor.wrap = !editor.wrap;
            draw_viewport(theme, editor, buffer)?;
        }
        Action::DecreaseLeft => {
            editor.wrap = false;
            editor.vleft = editor.vleft.saturating_sub(1);
            draw_viewport(theme, editor, buffer)?;
        }
        Action::IncreaseLeft => {
            editor.wrap = false;
            editor.vleft = editor.vleft + 1;
            draw_viewport(theme, editor, buffer)?;
        }
    }

    Ok(false)
}

// TODO: in neovim, when you are at an x position and you move to a shorter line, the cursor
//       goes back to the max x but returns to the previous x position if the line is longer
fn check_bounds(editor: &mut Editor) {
    let line_length = editor.line_length();

    if editor.cx >= line_length && editor.is_normal() {
        if line_length > 0 {
            editor.cx = editor.line_length() - 1;
        } else if editor.is_normal() {
            editor.cx = 0;
        }
    }
    if editor.cx >= editor.vwidth() {
        editor.cx = editor.vwidth() - 1;
    }

    // check if cy is after the end of the buffer
    // the end of the buffer is less than vtop + cy
    let line_on_buffer = editor.cy as usize + editor.vtop;
    if line_on_buffer > editor.current_buffer().len().saturating_sub(1) {
        editor.cy = editor.current_buffer().len() - editor.vtop - 1;
    }
}

fn handle_lsp_message(
    editor: &mut Editor,
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
                            if let Some(serde_json::Value::String(value)) = contents.get("value") {
                                let info = Info::new(
                                    editor.cx,
                                    editor.cy,
                                    editor.size.0 as usize,
                                    editor.size.1 as usize,
                                    value.clone(),
                                );
                                editor.current_dialog = Some(Box::new(info));
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
                _ = editor.current_buffer_mut().offer_diagnostics(&msg);
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
            editor.last_error = Some(error_msg.to_string());
            None
        }
    }
}

// Draw the current render buffer to the terminal
fn render(theme: &Theme, editor: &mut Editor, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
    draw_viewport(theme, editor, buffer)?;
    draw_statusline(theme, editor, buffer);

    stdout().queue(Clear(ClearType::All))?.queue(MoveTo(0, 0))?;
    stdout().queue(style::SetBackgroundColor(theme.style.bg.unwrap()))?;

    let mut current_style = &theme.style;
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

    draw_cursor(theme, editor, buffer)?;
    stdout().flush()?;

    Ok(())
}

pub fn draw_viewport(
    theme: &Theme,
    editor: &Editor,
    render_buffer: &mut RenderBuffer,
) -> anyhow::Result<()> {
    let mut viewport = editor.current_buffer().viewport(
        theme,
        editor.size.0 as usize,
        editor.size.1 as usize,
        editor.vleft,
        editor.vtop,
    )?;
    viewport.set_left(editor.vleft);
    viewport.set_wrap(editor.wrap);
    viewport.draw(render_buffer, 0, 0)?;

    Ok(())
}

async fn notify_change(lsp: &mut LspClient, editor: &mut Editor) -> anyhow::Result<()> {
    let file = editor.current_buffer().file.clone();
    if let Some(file) = &file {
        lsp.did_change(&file, &editor.current_buffer().contents())
            .await?;
    }
    Ok(())
}

async fn go_to_line(
    editor: &mut Editor,
    theme: &Theme,
    config: &Config,
    buffer: &mut RenderBuffer,
    lsp: &mut LspClient,
    runtime: &mut Runtime,
    plugin_registry: &mut PluginRegistry,
    line: usize,
    pos: GoToLinePosition,
) -> anyhow::Result<()> {
    if line == 0 {
        execute(
            &Action::MoveToTop,
            editor,
            theme,
            config,
            buffer,
            lsp,
            runtime,
            plugin_registry,
        )
        .await?;
        return Ok(());
    }

    if line <= editor.current_buffer().len() {
        let y = line - 1;

        if editor.is_within_viewport(y) {
            editor.cy = y - editor.vtop;
        } else if editor.is_within_first_page(y) {
            editor.vtop = 0;
            editor.cy = y;
            draw_viewport(theme, editor, buffer)?;
        } else if editor.is_within_last_page(y) {
            editor.vtop = editor.current_buffer().len() - editor.vheight();
            editor.cy = y - editor.vtop;
            draw_viewport(theme, editor, buffer)?;
        } else {
            if matches!(pos, GoToLinePosition::Bottom) {
                editor.vtop = y - editor.vheight();
                editor.cy = editor.buffer_line() - editor.vtop;
            } else {
                editor.vtop = y;
                editor.cy = 0;
                if matches!(pos, GoToLinePosition::Center) {
                    execute(
                        &Action::MoveToTop,
                        editor,
                        theme,
                        config,
                        buffer,
                        lsp,
                        runtime,
                        plugin_registry,
                    )
                    .await?;
                }
            }

            // FIXME: this is wasteful when move to viewport center worked
            // but we have to account for the case where it didn't and also
            draw_viewport(theme, editor, buffer)?;
        }
    }

    Ok(())
}

fn handle_insert_event(
    editor: &mut Editor,
    config: &Config,
    ev: &event::Event,
) -> Option<KeyAction> {
    let insert = config.keys.insert.clone();
    if let Some(ka) = event_to_key_action(editor, &insert, &ev) {
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

#[async_recursion::async_recursion]
async fn handle_key_action(
    ev: &event::Event,
    action: &KeyAction,
    editor: &mut Editor,
    theme: &Theme,
    config: &Config,
    buffer: &mut RenderBuffer,
    lsp: &mut LspClient,
    runtime: &mut Runtime,
    plugin_registry: &mut PluginRegistry,
) -> anyhow::Result<bool> {
    log!("Action: {action:?}");
    let quit = match action {
        KeyAction::Single(action) => {
            execute(
                &action,
                editor,
                &theme,
                &config,
                buffer,
                lsp,
                runtime,
                plugin_registry,
            )
            .await?
        }
        KeyAction::Multiple(actions) => {
            let mut quit = false;
            for action in actions {
                if execute(
                    &action,
                    editor,
                    &theme,
                    &config,
                    buffer,
                    lsp,
                    runtime,
                    plugin_registry,
                )
                .await?
                {
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
                editor.waiting_command = Some(format!("{c}"));
            }
            editor.waiting_key_action = Some(KeyAction::Nested(actions.clone()));
            false
        }
        KeyAction::Repeating(times, action) => {
            editor.repeater = None;
            let mut quit = false;
            for _ in 0..*times as usize {
                if handle_key_action(
                    ev,
                    action,
                    editor,
                    theme,
                    config,
                    buffer,
                    lsp,
                    runtime,
                    plugin_registry,
                )
                .await?
                {
                    quit = true;
                    break;
                }
            }
            quit
        }
    };

    Ok(quit)
}

fn event_to_key_action(
    editor: &mut Editor,
    mappings: &HashMap<String, KeyAction>,
    ev: &Event,
) -> Option<KeyAction> {
    if editor.handle_repeater(ev) {
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
                MouseEventKind::Down(MouseButton::Left) => Some(KeyAction::Single(Action::MoveTo(
                    (*column) as usize,
                    editor.vtop + *row as usize + 1,
                ))),
                MouseEventKind::ScrollUp => Some(KeyAction::Single(Action::ScrollUp)),
                MouseEventKind::ScrollDown => Some(KeyAction::Single(Action::ScrollDown)),
                _ => None,
            },
        },
        _ => None,
    };

    if let Some(ref ka) = key_action {
        if let Some(ref repeater) = editor.repeater {
            return Some(KeyAction::Repeating(repeater.clone(), Box::new(ka.clone())));
        }
    }

    key_action
}

fn handle_normal_event(
    editor: &mut Editor,
    config: &Config,
    ev: &event::Event,
) -> Option<KeyAction> {
    let normal = config.keys.normal.clone();
    event_to_key_action(editor, &normal, &ev)
}

fn draw_line(editor: &mut Editor, render_buffer: &mut RenderBuffer) {
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

#[derive(Default)]
pub struct Editor {
    buffers: Vec<Buffer>,
    current_buffer_index: usize,
    size: (u16, u16),
    vtop: usize,
    vleft: usize,
    cx: usize,
    cy: usize,
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
    pub fn with_size(width: usize, height: usize, buffers: Vec<Buffer>) -> anyhow::Result<Self> {
        let mut stdout = stdout();
        let vx = buffers
            .get(0)
            .map(|b| b.len().to_string().len())
            .unwrap_or(0)
            + 2;

        let mut plugin_registry = PluginRegistry::new();

        Ok(Editor {
            buffers,
            size: (width as u16, height as u16),
            current_buffer_index: 0,
            vtop: 0,
            vleft: 0,
            cx: 0,
            cy: 0,
            mode: Mode::Normal,
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

    pub fn new(buffers: Vec<Buffer>) -> anyhow::Result<Self> {
        let size = terminal::size()?;
        Self::with_size(size.0 as usize, size.1 as usize, buffers)
    }

    pub fn vwidth(&self) -> usize {
        self.size.0 as usize
    }

    pub fn vheight(&self) -> usize {
        self.size.1 as usize - 2
    }

    pub fn cursor_position(&self) -> (usize, usize) {
        (self.cx, self.cy)
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

    fn fill_line(&mut self, buffer: &mut RenderBuffer, x: usize, y: usize, style: &Style) {
        let width = self.vwidth().saturating_sub(x);
        let line_fill = " ".repeat(width);
        buffer.set_text(x, y, &line_fill, style);
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

        event_to_key_action(self, &nested_mappings, &ev)
    }

    pub fn cleanup(&mut self) -> anyhow::Result<()> {
        stdout()
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

    fn set_current_buffer(
        &mut self,
        theme: &Theme,
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

        draw_viewport(theme, self, render_buffer)?;

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
