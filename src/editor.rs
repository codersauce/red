use std::{
    io::{stdout, Write},
    mem,
};

use crossterm::{
    cursor,
    event::{self, read, KeyModifiers},
    style::{self, Color, Stylize},
    terminal, ExecutableCommand, QueueableCommand,
};
use tree_sitter::{Parser, Query, QueryCursor};
use tree_sitter_rust::HIGHLIGHT_QUERY;

use crate::buffer::Buffer;

#[derive(Debug)]
enum Action {
    Undo,
    Quit,

    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,

    InsertCharAtCursorPos(char),
    NewLine,

    EnterMode(Mode),
    PageDown,
    PageUp,
    MoveToLineEnd,
    MoveToLineStart,
    DeleteCharAtCursorPos,
    DeleteCurrentLine,
    DeleteLineAt(usize),

    SetWaitingCmd(char),
    InsertLineAt(usize, Option<String>),
    MoveLineToViewportCenter,
    InsertLineBelowCursor,
    InsertLineAtCursor,
    MoveToBottom,
    MoveToTop,
    RemoveCharAt(u16, usize),
    UndoMultiple(Vec<Action>),
}

impl Action {}

#[derive(Debug, Clone, Copy)]
enum Mode {
    Normal,
    Insert,
}

#[derive(Debug)]
pub struct ColorInfo {
    start: usize,
    end: usize,
    color: Color,
}

pub struct Editor {
    buffer: Buffer,
    stdout: std::io::Stdout,
    size: (u16, u16),
    vtop: usize,
    vleft: u16,
    cx: u16,
    cy: u16,
    mode: Mode,
    waiting_command: Option<char>,
    undo_actions: Vec<Action>,
    insert_undo_actions: Vec<Action>,
}

impl Editor {
    pub fn new(buffer: Buffer) -> anyhow::Result<Self> {
        let mut stdout = stdout();
        terminal::enable_raw_mode()?;
        stdout
            .execute(terminal::EnterAlternateScreen)?
            .execute(terminal::Clear(terminal::ClearType::All))?;

        Ok(Editor {
            buffer,
            stdout,
            vtop: 0,
            vleft: 0,
            cx: 0,
            cy: 0,
            mode: Mode::Normal,
            size: terminal::size()?,
            waiting_command: None,
            undo_actions: vec![],
            insert_undo_actions: vec![],
        })
    }

    fn vwidth(&self) -> u16 {
        self.size.0
    }

    fn vheight(&self) -> u16 {
        self.size.1 - 2
    }

    fn line_length(&self) -> u16 {
        if let Some(line) = self.viewport_line(self.cy) {
            return line.len() as u16;
        }
        0
    }

    fn buffer_line(&self) -> usize {
        self.vtop + self.cy as usize
    }

    fn viewport_line(&self, n: u16) -> Option<String> {
        let buffer_line = self.vtop + n as usize;
        self.buffer.get(buffer_line)
    }

    fn set_cursor_style(&mut self) -> anyhow::Result<()> {
        self.stdout.queue(match self.waiting_command {
            Some(_) => cursor::SetCursorStyle::SteadyUnderScore,
            _ => match self.mode {
                Mode::Normal => cursor::SetCursorStyle::DefaultUserShape,
                Mode::Insert => cursor::SetCursorStyle::SteadyBar,
            },
        })?;

        Ok(())
    }

    pub fn draw(&mut self) -> anyhow::Result<()> {
        self.set_cursor_style()?;
        self.draw_viewport()?;
        self.draw_statusline()?;
        self.stdout.queue(cursor::MoveTo(self.cx, self.cy))?;
        self.stdout.flush()?;

        Ok(())
    }

    pub fn highlight(&self, code: &str) -> anyhow::Result<Vec<ColorInfo>> {
        let mut parser = Parser::new();
        let language = tree_sitter_rust::language();
        parser.set_language(language)?;

        let tree = parser.parse(&code, None).expect("parse works");
        let query = Query::new(language, HIGHLIGHT_QUERY)?;

        let mut colors = Vec::new();
        let mut cursor = QueryCursor::new();
        let matches = cursor.matches(&query, tree.root_node(), code.as_bytes());

        for mat in matches {
            for cap in mat.captures {
                let node = cap.node;
                let start = node.start_byte();
                let end = node.end_byte();
                let color = match query.capture_names()[cap.index as usize].as_str() {
                    "function" => Some(Color::Blue),
                    "string" => Some(Color::Green),
                    _ => None,
                };
                if let Some(color) = color {
                    colors.push(ColorInfo { start, end, color })
                }
            }
        }

        Ok(colors)
    }

    fn print_char(
        &mut self,
        x: u16,
        y: u16,
        c: char,
        color: Option<&ColorInfo>,
    ) -> anyhow::Result<()> {
        self.stdout.queue(cursor::MoveTo(x, y))?;

        match color {
            Some(ci) => {
                self.stdout
                    .queue(style::PrintStyledContent(c.to_string().with(ci.color)))?;
            }
            None => {
                self.stdout.queue(style::Print(c.to_string()))?;
            }
        };

        Ok(())
    }

    pub fn draw_viewport(&mut self) -> anyhow::Result<()> {
        let vbuffer = self.buffer.viewport(self.vtop, self.vheight() as usize);
        let color_info = self.highlight(&vbuffer)?;
        let vwidth = self.vwidth();
        let vheight = self.vheight();

        let mut x = 0;
        let mut y = 0;
        let mut color = None;
        let mut iter = vbuffer.chars().enumerate().peekable();

        while let Some((pos, c)) = iter.next() {
            if c == '\n' || iter.peek().is_none() {
                if c != '\n' {
                    self.print_char(x, y, c, color)?;
                    x += 1;
                }
                self.stdout
                    .queue(style::Print(" ".repeat((vwidth - x) as usize)))?;
                x = 0;
                y += 1;
                if y > vheight {
                    break;
                }
                continue;
            }

            if let Some(col) = color_info.iter().find(|ci| ci.start == pos) {
                color = Some(col);
            }
            if let Some(_) = color_info.iter().find(|ci| ci.end == pos) {
                color = None;
            }

            self.print_char(x, y, c, color)?;
            x += 1;
        }

        while y < vheight {
            self.stdout.queue(cursor::MoveTo(0, y))?;
            self.stdout
                .queue(style::Print(" ".repeat(vwidth as usize)))?;
            y += 1;
        }

        Ok(())
    }

    pub fn draw_statusline(&mut self) -> anyhow::Result<()> {
        let mode = format!(" {:?} ", self.mode).to_uppercase();
        let file = format!(" {}", self.buffer.file.as_deref().unwrap_or("No Name"));
        let pos = format!(" {}:{} ", self.cx + 1, self.cy + 1);

        let file_width = self.size.0 - mode.len() as u16 - pos.len() as u16 - 2;

        self.stdout.queue(cursor::MoveTo(0, self.size.1 - 2))?;
        self.stdout.queue(style::PrintStyledContent(
            mode.with(Color::Rgb { r: 0, g: 0, b: 0 })
                .bold()
                .on(Color::Rgb {
                    r: 184,
                    g: 144,
                    b: 243,
                }),
        ))?;
        self.stdout.queue(style::PrintStyledContent(
            ""
                .with(Color::Rgb {
                    r: 184,
                    g: 144,
                    b: 243,
                })
                .on(Color::Rgb {
                    r: 67,
                    g: 70,
                    b: 89,
                }),
        ))?;
        self.stdout.queue(style::PrintStyledContent(
            format!("{:<width$}", file, width = file_width as usize)
                .with(Color::Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                })
                .bold()
                .on(Color::Rgb {
                    r: 67,
                    g: 70,
                    b: 89,
                }),
        ))?;
        self.stdout.queue(style::PrintStyledContent(
            ""
                .with(Color::Rgb {
                    r: 184,
                    g: 144,
                    b: 243,
                })
                .on(Color::Rgb {
                    r: 67,
                    g: 70,
                    b: 89,
                }),
        ))?;
        self.stdout.queue(style::PrintStyledContent(
            pos.with(Color::Rgb { r: 0, g: 0, b: 0 })
                .bold()
                .on(Color::Rgb {
                    r: 184,
                    g: 144,
                    b: 243,
                }),
        ))?;

        Ok(())
    }

    fn is_insert(&self) -> bool {
        matches!(self.mode, Mode::Insert)
    }

    // TODO: in neovim, when you are at an x position and you move to a shorter line, the cursor
    //       goes back to the max x but returns to the previous x position if the line is longer
    fn check_bounds(&mut self) {
        let line_length = self.line_length();

        if self.cx >= line_length && !self.is_insert() {
            if line_length > 0 {
                self.cx = self.line_length() - 1;
            } else if !self.is_insert() {
                self.cx = 0;
            }
        }
        if self.cx >= self.vwidth() {
            self.cx = self.vwidth() - 1;
        }

        // check if cy is after the end of the buffer
        // the end of the buffer is less than vtop + cy
        let line_on_buffer = self.cy as usize + self.vtop;
        if line_on_buffer > self.buffer.len() - 1 {
            self.cy = (self.buffer.len() as usize - self.vtop - 1) as u16;
        }
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        loop {
            self.check_bounds();
            self.draw()?;
            if let Some(action) = self.handle_event(read()?)? {
                if matches!(action, Action::Quit) {
                    break;
                }
                self.execute(&action);
            }
        }

        Ok(())
    }

    fn handle_event(&mut self, ev: event::Event) -> anyhow::Result<Option<Action>> {
        if let event::Event::Resize(width, height) = ev {
            self.size = (width, height);
            return Ok(None);
        }

        match self.mode {
            Mode::Normal => self.handle_normal_event(ev),
            Mode::Insert => self.handle_insert_event(ev),
        }
    }

    fn handle_normal_event(&mut self, ev: event::Event) -> anyhow::Result<Option<Action>> {
        // log!("Event: {:?}", ev);

        if let Some(cmd) = self.waiting_command {
            self.waiting_command = None;
            return self.handle_waiting_command(cmd, ev);
        }

        let action = match ev {
            event::Event::Key(event) => {
                let code = event.code;
                let modifiers = event.modifiers;

                match code {
                    event::KeyCode::Char('o') => Some(Action::InsertLineBelowCursor),
                    event::KeyCode::Char('O') => Some(Action::InsertLineAtCursor),
                    event::KeyCode::Char('q') => Some(Action::Quit),
                    event::KeyCode::Char('G') => Some(Action::MoveToBottom),
                    event::KeyCode::Char('g') => Some(Action::SetWaitingCmd('g')),
                    event::KeyCode::Char('u') => Some(Action::Undo),
                    event::KeyCode::Up | event::KeyCode::Char('k') => Some(Action::MoveUp),
                    event::KeyCode::Down | event::KeyCode::Char('j') => Some(Action::MoveDown),
                    event::KeyCode::Left | event::KeyCode::Char('h') => Some(Action::MoveLeft),
                    event::KeyCode::Right | event::KeyCode::Char('l') => Some(Action::MoveRight),
                    event::KeyCode::Char('i') => Some(Action::EnterMode(Mode::Insert)),
                    event::KeyCode::Char('0') | event::KeyCode::Home => {
                        Some(Action::MoveToLineStart)
                    }
                    event::KeyCode::Char('$') | event::KeyCode::End => Some(Action::MoveToLineEnd),
                    event::KeyCode::Char('b') => {
                        if matches!(modifiers, KeyModifiers::CONTROL) {
                            Some(Action::PageUp)
                        } else {
                            None
                        }
                    }
                    event::KeyCode::Char('f') => {
                        if matches!(modifiers, KeyModifiers::CONTROL) {
                            Some(Action::PageDown)
                        } else {
                            None
                        }
                    }
                    event::KeyCode::Char('x') => Some(Action::DeleteCharAtCursorPos),
                    event::KeyCode::Char('d') => Some(Action::SetWaitingCmd('d')),
                    event::KeyCode::Char('z') => Some(Action::SetWaitingCmd('z')),
                    _ => None,
                }
            }
            _ => None,
        };

        Ok(action)
    }

    // TODO: I don't think this handlers are ever gonna fail, so maybe just return Option<Action>
    // here?
    fn handle_waiting_command(
        &self,
        cmd: char,
        ev: event::Event,
    ) -> anyhow::Result<Option<Action>> {
        let action = match cmd {
            'd' => match ev {
                event::Event::Key(event) => match event.code {
                    event::KeyCode::Char('d') => Some(Action::DeleteCurrentLine),
                    _ => None,
                },
                _ => None,
            },
            'g' => match ev {
                event::Event::Key(event) => match event.code {
                    event::KeyCode::Char('g') => Some(Action::MoveToTop),
                    _ => None,
                },
                _ => None,
            },
            'z' => match ev {
                event::Event::Key(event) => match event.code {
                    event::KeyCode::Char('z') => Some(Action::MoveLineToViewportCenter),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };

        Ok(action)
    }

    fn handle_insert_event(&self, ev: event::Event) -> anyhow::Result<Option<Action>> {
        let action = match ev {
            event::Event::Key(event) => match event.code {
                event::KeyCode::Esc => Some(Action::EnterMode(Mode::Normal)),
                event::KeyCode::Enter => Some(Action::NewLine),
                event::KeyCode::Char(c) => Some(Action::InsertCharAtCursorPos(c)),
                _ => None,
            },
            _ => None,
        };

        Ok(action)
    }

    pub fn cleanup(&mut self) -> anyhow::Result<()> {
        self.stdout.execute(terminal::LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;

        Ok(())
    }

    fn current_line_contents(&self) -> Option<String> {
        self.buffer.get(self.buffer_line())
    }

    fn execute(&mut self, action: &Action) {
        match action {
            Action::Quit => {}
            Action::MoveUp => {
                if self.cy == 0 {
                    // scroll up
                    if self.vtop > 0 {
                        self.vtop -= 1;
                    }
                } else {
                    self.cy = self.cy.saturating_sub(1);
                }
            }
            Action::MoveDown => {
                self.cy += 1;
                if self.cy >= self.vheight() {
                    // scroll if possible
                    self.vtop += 1;
                    self.cy -= 1;
                }
            }
            Action::MoveLeft => {
                self.cx = self.cx.saturating_sub(1);
                if self.cx < self.vleft {
                    self.cx = self.vleft;
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
                }
            }
            Action::PageDown => {
                if self.buffer.len() > self.vtop + self.vheight() as usize {
                    self.vtop += self.vheight() as usize;
                }
            }
            Action::EnterMode(new_mode) => {
                // entering insert mode
                if !self.is_insert() && matches!(new_mode, Mode::Insert) {
                    self.insert_undo_actions = Vec::new();
                }
                if self.is_insert() && matches!(new_mode, Mode::Normal) {
                    if !self.insert_undo_actions.is_empty() {
                        let actions = mem::take(&mut self.insert_undo_actions);
                        self.undo_actions.push(Action::UndoMultiple(actions));
                    }
                }

                self.mode = *new_mode;
            }
            Action::InsertCharAtCursorPos(c) => {
                self.insert_undo_actions
                    .push(Action::RemoveCharAt(self.cx, self.buffer_line()));
                self.buffer.insert(self.cx, self.buffer_line(), *c);
                self.cx += 1;
            }
            Action::RemoveCharAt(x, y) => {
                self.buffer.remove(*x, *y);
            }
            Action::DeleteCharAtCursorPos => {
                self.buffer.remove(self.cx, self.buffer_line());
            }
            Action::NewLine => {
                self.cx = 0;
                self.cy += 1;
            }
            Action::SetWaitingCmd(cmd) => {
                self.waiting_command = Some(*cmd);
            }
            Action::DeleteCurrentLine => {
                let line = self.buffer_line();
                let contents = self.current_line_contents();

                self.buffer.remove_line(self.buffer_line());
                self.undo_actions.push(Action::InsertLineAt(line, contents));
            }
            Action::Undo => {
                if let Some(undo_action) = self.undo_actions.pop() {
                    self.execute(&undo_action);
                }
            }
            Action::UndoMultiple(actions) => {
                for action in actions.iter().rev() {
                    self.execute(action);
                }
            }
            Action::InsertLineAt(y, contents) => {
                if let Some(contents) = contents {
                    self.buffer.insert_line(*y, contents.to_string());
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
                    }
                } else if distance_to_center < 0 {
                    // if distance < 0 we need to scroll down
                    let distance_to_center = distance_to_center.abs() as usize;
                    let new_vtop = self.vtop.saturating_sub(distance_to_center);
                    let distance_to_go = self.vtop as usize + distance_to_center;
                    if self.buffer.len() > distance_to_go && new_vtop != self.vtop {
                        self.vtop = new_vtop;
                        self.cy = viewport_center;
                    }
                }
            }
            Action::InsertLineBelowCursor => {
                self.undo_actions
                    .push(Action::DeleteLineAt(self.buffer_line() + 1));

                self.buffer
                    .insert_line(self.buffer_line() + 1, String::new());
                self.cy += 1;
                self.cx = 0;
                self.mode = Mode::Insert;
            }
            Action::InsertLineAtCursor => {
                self.undo_actions
                    .push(Action::DeleteLineAt(self.buffer_line()));

                self.buffer.insert_line(self.buffer_line(), String::new());
                self.cx = 0;
                self.mode = Mode::Insert;
            }
            Action::MoveToTop => {
                self.vtop = 0;
                self.cy = 0;
            }
            Action::MoveToBottom => {
                if self.buffer.len() > self.vheight() as usize {
                    self.cy = self.vheight() - 1;
                    self.vtop = self.buffer.len() - self.vheight() as usize;
                } else {
                    self.cy = self.buffer.len() as u16 - 1u16;
                }
            }
            Action::DeleteLineAt(y) => self.buffer.remove_line(*y),
        }
    }
}
