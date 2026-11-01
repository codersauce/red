//! Terminal chrome for code explore: sidebar, breadcrumb, skeleton dimming,
//! and symbol-follow highlights.

use std::path::Path;

use unicode_segmentation::UnicodeSegmentation as _;

use crate::color::Color;
use crate::explore::{sidebar_columns, DimZone, Hit, Jump, SyncInput, View, ViewDef, ViewRef};
use crate::highlighter::LanguageRegistry;
use crate::theme::{Style, Theme};
use crate::unicode_utils::{
    char_to_grapheme, display_width, trim_line_ending, truncate_display_width,
};

use super::{Action, Editor, RenderBuffer, Runtime};

pub(crate) fn explore_sidebar_columns(editor: &Editor, window: &crate::window::Window) -> usize {
    if !editor.explore.enabled || !window.active {
        return 0;
    }
    sidebar_columns(window.inner_width())
}

pub(crate) fn explore_chrome_rows(editor: &Editor, window: &crate::window::Window) -> usize {
    if !editor.explore.enabled || !window.active || window.inner_height() < 8 {
        return 0;
    }
    1
}

impl Editor {
    pub(crate) fn explore_sidebar_columns(&self, window: &crate::window::Window) -> usize {
        explore_sidebar_columns(self, window)
    }

    pub(crate) fn explore_chrome_rows(&self, window: &crate::window::Window) -> usize {
        explore_chrome_rows(self, window)
    }

    pub(crate) async fn execute_explore_command(
        &mut self,
        command: &str,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let command = command.trim();
        let message = if let Some(base) = command.strip_prefix("pr ") {
            self.explore_pr(Some(base.trim()))
        } else {
            match command {
                "toggle" => {
                    self.explore.enabled = !self.explore.enabled;
                    if !self.explore.enabled {
                        self.explore.clear_follow();
                        self.explore.help = false;
                    }
                    if self.explore.enabled {
                        "Code explore on. \\? lists keys. Double-click a name to follow it."
                            .to_string()
                    } else {
                        "Code explore off.".to_string()
                    }
                }
                "on" => {
                    self.explore.enabled = true;
                    "Code explore on. \\? lists keys.".to_string()
                }
                "off" | "close" => {
                    self.explore.enabled = false;
                    self.explore.clear_follow();
                    self.explore.help = false;
                    "Code explore off.".to_string()
                }
                "skeleton" | "skel" => {
                    self.explore.enabled = true;
                    self.explore.skeleton = !self.explore.skeleton;
                    if self.explore.skeleton {
                        "Skeleton on. Repo calls and syscall wrappers stay bright.".to_string()
                    } else {
                        "Skeleton off.".to_string()
                    }
                }
                "follow" => {
                    self.explore.enabled = true;
                    self.sync_explore_buffer();
                    let position = self.cursor_text_position();
                    match self.explore.toggle_follow(position.line, position.character) {
                        Ok(message) => message,
                        Err(message) => message,
                    }
                }
                "definition" | "def" => {
                    self.explore.enabled = true;
                    self.explore_jump_definition(buffer, runtime).await?
                }
                "type" => {
                    self.explore.enabled = true;
                    self.explore_jump_type(buffer, runtime).await?
                }
                "back" => self.explore_trail(false, buffer, runtime).await?,
                "forward" => self.explore_trail(true, buffer, runtime).await?,
                "next" => self.explore_step_reference(true, buffer, runtime).await?,
                "prev" | "previous" => self.explore_step_reference(false, buffer, runtime).await?,
                "index" | "scan" => self.explore_scan(false, None),
                "refresh" => {
                    self.explore.invalidate();
                    self.sync_explore_buffer();
                    "Re-read this file.".to_string()
                }
                "pr" => self.explore_pr(None),
                "help" | "?" => {
                    self.explore.enabled = true;
                    self.explore.help = !self.explore.help;
                    if self.explore.help {
                        "Key list is in the sidebar.".to_string()
                    } else {
                        "Key list closed.".to_string()
                    }
                }
                _ => "usage: explore [on|off|skeleton|follow|def|type|back|forward|next|prev|index|pr [base]|help]".to_string(),
            }
        };
        self.set_quiet_message(Some(message));
        self.render(buffer)?;
        Ok(())
    }

    pub(crate) fn paint_explore_chrome(
        &mut self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
    ) -> anyhow::Result<()> {
        if !self.explore.enabled || !window.active {
            return Ok(());
        }
        self.sync_explore_buffer();
        let position = self.cursor_text_position();
        let view = self.explore.view(position.line, position.character);
        let palette = Palette::from_theme(&self.theme);
        let ascii = self.config.window_borders_ascii;
        let mut hits = Vec::new();
        self.paint_explore_source(buffer, window, &view, &palette);
        if let Some(rect) = self.explore_sidebar_rect(window) {
            hits.extend(paint_sidebar(buffer, rect, &view, &palette, ascii));
        }
        if self.explore_chrome_rows(window) == 1 {
            let y = window.position.y + window.inner_height() - 1;
            hits.extend(paint_breadcrumb(
                buffer,
                window.position.x,
                y,
                window.inner_width(),
                &view,
                &palette,
                ascii,
            ));
        }
        self.explore.set_hits(hits);
        Ok(())
    }

    pub(crate) async fn explore_activate_hit(
        &mut self,
        x: usize,
        y: usize,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<bool> {
        let Some(jump) = self.explore.hit_at(x, y).cloned() else {
            return Ok(false);
        };
        let message = self.explore_go(jump, true, buffer, runtime).await?;
        self.set_quiet_message(Some(message));
        self.render(buffer)?;
        Ok(true)
    }

    pub(crate) fn sync_explore_buffer(&mut self) {
        if !self.explore.enabled {
            return;
        }
        let key = self.explore_file_key();
        let revision = self.current_buffer().revision();
        if self.explore.is_current(&key, revision) {
            return;
        }
        let source = self.current_buffer().contents();
        let language = self.current_language_id();
        let registry = self.highlighter.registry();
        self.explore.sync(SyncInput {
            key: &key,
            revision,
            language: language.as_deref(),
            source: &source,
            registry: &registry,
        });
    }

    fn explore_file_key(&self) -> String {
        match self.current_buffer().file.as_deref() {
            Some(file) => crate::explore::normalize_path(file),
            None => format!("buffer:{}", self.current_buffer().id().as_u64()),
        }
    }

    fn cursor_crumb(&mut self) -> Jump {
        self.sync_explore_buffer();
        let position = self.cursor_text_position();
        let view = self.explore.view(position.line, position.character);
        let label = view
            .cursor_name
            .clone()
            .or_else(|| view.in_function.clone())
            .unwrap_or_else(|| view.in_file.clone());
        Jump {
            label,
            file: self.explore_file_key(),
            line: position.line,
            column: position.character,
        }
    }

    async fn explore_jump_definition(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<String> {
        self.sync_explore_buffer();
        let position = self.cursor_text_position();
        match self
            .explore
            .definition_jump(position.line, position.character)
        {
            Ok(jump) => self.explore_go(jump, true, buffer, runtime).await,
            Err(message) => Ok(message),
        }
    }

    async fn explore_jump_type(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<String> {
        self.sync_explore_buffer();
        let position = self.cursor_text_position();
        match self.explore.type_jump(position.line, position.character) {
            Ok(jump) => self.explore_go(jump, true, buffer, runtime).await,
            Err(message) => Ok(message),
        }
    }

    async fn explore_trail(
        &mut self,
        forward: bool,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<String> {
        let jump = if forward {
            self.explore.forward()
        } else {
            self.explore.back()
        };
        let Some(jump) = jump else {
            return Ok(if forward {
                "End of the review trail.".to_string()
            } else {
                "Start of the review trail.".to_string()
            });
        };
        let label = jump.label.clone();
        self.explore_go(jump, false, buffer, runtime).await?;
        Ok(format!("trail · {label}"))
    }

    async fn explore_step_reference(
        &mut self,
        forward: bool,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<String> {
        self.sync_explore_buffer();
        let position = self.cursor_text_position();
        match self
            .explore
            .step_reference(position.line, position.character, forward)
        {
            Ok(jump) => self.explore_go(jump, true, buffer, runtime).await,
            Err(message) => Ok(message),
        }
    }

    async fn explore_go(
        &mut self,
        jump: Jump,
        record: bool,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<String> {
        let label = jump.label.clone();
        if record {
            let from = self.cursor_crumb();
            self.explore.record_jump(from, jump.clone());
        }
        if !jump.file.is_empty()
            && !jump.file.starts_with("buffer:")
            && crate::explore::normalize_path(&jump.file) != self.explore_file_key()
        {
            self.execute_with_tracking(
                &Action::OpenFile(jump.file.clone()),
                buffer,
                runtime,
                false,
            )
            .await?;
        }
        let line_text = self.current_buffer().get(jump.line).unwrap_or_default();
        let column = char_to_grapheme(trim_line_ending(&line_text), jump.column);
        self.execute_with_tracking(
            &Action::MoveTo(column, jump.line + 1),
            buffer,
            runtime,
            true,
        )
        .await?;
        Ok(format!("→ {label}"))
    }

    fn explore_scan(&mut self, pr: bool, base: Option<&str>) -> String {
        let Some(root) = self.explore_root() else {
            return "No directory to index.".to_string();
        };
        let registry = self.highlighter.registry();
        let result = if pr {
            self.explore.scan_pr(&root, &registry, base)
        } else {
            Ok(self.explore.scan_workspace(&root, &registry))
        };
        self.ingest_open_buffers(&registry);
        match result {
            Ok(stats) => {
                let scope = if pr { "PR" } else { "repo" };
                format!(
                    "Indexed {} {scope} files{}. {}",
                    stats.files,
                    if stats.truncated { " (capped)" } else { "" },
                    stats.message
                )
            }
            Err(error) => error,
        }
    }

    fn explore_pr(&mut self, base: Option<&str>) -> String {
        if self.explore.pr_mode && base.is_none() {
            self.explore.clear_pr();
            return "PR scope off. Skeleton uses every indexed definition.".to_string();
        }
        self.explore.enabled = true;
        self.explore_scan(true, base)
    }

    fn explore_root(&self) -> Option<std::path::PathBuf> {
        let key = self.explore_file_key();
        if key.starts_with("buffer:") {
            return crate::explore::repo_root(Path::new("."));
        }
        self.explore.root_for(&key)
    }

    fn ingest_open_buffers(&mut self, registry: &LanguageRegistry) {
        let opened = self
            .buffer_manager
            .iter()
            .filter_map(|buffer| {
                let file = buffer.file.clone()?;
                Some((
                    crate::explore::normalize_path(&file),
                    buffer.contents(),
                    file,
                ))
            })
            .collect::<Vec<_>>();
        for (key, source, file) in opened {
            let Some(language) = self.highlighter.language_id_for_file(Some(&file)) else {
                continue;
            };
            self.explore
                .ingest_source(&key, &source, language, registry);
        }
    }

    fn explore_sidebar_rect(&self, window: &crate::window::Window) -> Option<Rect> {
        let width = self.explore_sidebar_columns(window);
        if width == 0 {
            return None;
        }
        let height = self.window_content_height(window);
        if height == 0 {
            return None;
        }
        Some(Rect {
            x: window.position.x + window.inner_width() - width,
            y: window.position.y + self.window_content_top(window),
            width,
            height,
        })
    }

    fn paint_explore_source(
        &self,
        buffer: &mut RenderBuffer,
        window: &crate::window::Window,
        view: &View,
        palette: &Palette,
    ) {
        if view.dim.is_none() && view.follow_spans.is_empty() && view.badges.is_empty() {
            return;
        }
        let layout = self.layout_for_window(window);
        let gutter = self.gutter_width_for_window(window);
        let origin_x = self.window_to_terminal_x(window, gutter + 1);
        let content_width = self.window_content_width(window);
        let tab_width = self.tab_width_for_buffer_index(window.buffer_index).max(1);
        let background = self
            .theme
            .style
            .bg
            .unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });
        for segment in &layout.rows {
            if segment.projection.is_some() {
                continue;
            }
            let Some(line) = self
                .buffer_manager
                .get(window.buffer_index)
                .and_then(|buffer| buffer.get(segment.line))
            else {
                continue;
            };
            let line = trim_line_ending(&line);
            let term_y = self.window_to_terminal_y(window, segment.row);
            let line_start = view.line_starts.get(segment.line).copied().unwrap_or(0);
            let mut byte = 0usize;
            let mut display = 0usize;
            for grapheme in line.graphemes(true) {
                let width = if grapheme == "\t" {
                    tab_width - (display % tab_width)
                } else {
                    display_width(grapheme).max(1)
                };
                let abs = line_start + byte;
                let kind = source_mark(view, abs);
                if kind != Mark::Plain
                    && display < segment.end_col
                    && display + width > segment.start_col
                {
                    let shown = display.max(segment.start_col);
                    let local = segment.visual_offset + shown.saturating_sub(segment.start_col);
                    if local < content_width {
                        paint_mark(buffer, origin_x + local, term_y, kind, palette, background);
                    }
                }
                byte += grapheme.len();
                display += width;
            }
            if let Some((_, count)) = view
                .badges
                .iter()
                .find(|(line_no, _)| *line_no == segment.line)
            {
                if segment.last_segment {
                    paint_badge(
                        buffer,
                        origin_x,
                        term_y,
                        content_width,
                        display,
                        *count,
                        palette,
                        &self.theme,
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Plain,
    Dim,
    Follow,
}

fn source_mark(view: &View, byte: usize) -> Mark {
    if view
        .follow_spans
        .iter()
        .any(|span| span.contains_byte(byte))
    {
        return Mark::Follow;
    }
    if let Some(dim) = &view.dim {
        if in_dim(dim, byte) {
            return Mark::Dim;
        }
    }
    Mark::Plain
}

fn in_dim(dim: &DimZone, byte: usize) -> bool {
    dim.zone.contains_byte(byte)
        && !dim.holes.iter().any(|span| span.contains_byte(byte))
        && !dim.keep.iter().any(|span| span.contains_byte(byte))
}

fn paint_mark(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    mark: Mark,
    palette: &Palette,
    background: Color,
) {
    let Some(cell) = cell_mut(buffer, x, y) else {
        return;
    };
    match mark {
        Mark::Plain => {}
        Mark::Dim => {
            let fg = cell.style.fg.unwrap_or(palette.fg);
            cell.style.fg = Some(mix(fg, background, 0.8));
        }
        Mark::Follow => {
            cell.style.fg = Some(palette.follow_fg);
            cell.style.bg = Some(palette.follow_bg);
            cell.style.bold = true;
            cell.style.underline = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_badge(
    buffer: &mut RenderBuffer,
    origin_x: usize,
    y: usize,
    content_width: usize,
    line_width: usize,
    count: usize,
    palette: &Palette,
    theme: &Theme,
) {
    let text = format!(" ×{count}");
    let width = display_width(&text);
    if line_width + width > content_width {
        return;
    }
    let style = Style {
        fg: Some(palette.accent),
        bg: theme.style.bg,
        bold: true,
        ..Style::default()
    };
    buffer.set_text(origin_x + line_width, y, &text, &style);
}

struct Rect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

struct Palette {
    fg: Color,
    muted: Color,
    heading: Color,
    accent: Color,
    border: Color,
    sidebar_bg: Color,
    title_bg: Color,
    title_fg: Color,
    selected_bg: Color,
    selected_fg: Color,
    follow_bg: Color,
    follow_fg: Color,
    crumb_bg: Color,
    warn: Color,
}

impl Palette {
    fn from_theme(theme: &Theme) -> Self {
        let bg = theme.style.bg.unwrap_or(Color::Rgb {
            r: 12,
            g: 14,
            b: 18,
        });
        let fg = theme.style.fg.unwrap_or(Color::Rgb {
            r: 230,
            g: 232,
            b: 235,
        });
        if bg.is_light() {
            Self {
                fg: Color::Rgb {
                    r: 28,
                    g: 32,
                    b: 38,
                },
                muted: Color::Rgb {
                    r: 90,
                    g: 96,
                    b: 108,
                },
                heading: Color::Rgb {
                    r: 140,
                    g: 72,
                    b: 10,
                },
                accent: Color::Rgb {
                    r: 12,
                    g: 110,
                    b: 78,
                },
                border: Color::Rgb {
                    r: 176,
                    g: 164,
                    b: 140,
                },
                sidebar_bg: mix(
                    bg,
                    Color::Rgb {
                        r: 244,
                        g: 240,
                        b: 230,
                    },
                    0.72,
                ),
                title_bg: Color::Rgb {
                    r: 42,
                    g: 48,
                    b: 64,
                },
                title_fg: Color::Rgb {
                    r: 255,
                    g: 236,
                    b: 210,
                },
                selected_bg: Color::Rgb {
                    r: 255,
                    g: 214,
                    b: 150,
                },
                selected_fg: Color::Rgb {
                    r: 32,
                    g: 24,
                    b: 12,
                },
                follow_bg: Color::Rgb {
                    r: 255,
                    g: 196,
                    b: 96,
                },
                follow_fg: Color::Rgb { r: 32, g: 22, b: 8 },
                crumb_bg: mix(
                    bg,
                    Color::Rgb {
                        r: 232,
                        g: 226,
                        b: 210,
                    },
                    0.8,
                ),
                warn: Color::Rgb {
                    r: 150,
                    g: 40,
                    b: 40,
                },
            }
        } else {
            Self {
                fg,
                muted: Color::Rgb {
                    r: 148,
                    g: 158,
                    b: 172,
                },
                heading: Color::Rgb {
                    r: 255,
                    g: 196,
                    b: 120,
                },
                accent: Color::Rgb {
                    r: 126,
                    g: 214,
                    b: 168,
                },
                border: Color::Rgb {
                    r: 78,
                    g: 96,
                    b: 120,
                },
                sidebar_bg: mix(
                    bg,
                    Color::Rgb {
                        r: 16,
                        g: 20,
                        b: 30,
                    },
                    0.65,
                ),
                title_bg: Color::Rgb {
                    r: 28,
                    g: 36,
                    b: 52,
                },
                title_fg: Color::Rgb {
                    r: 255,
                    g: 224,
                    b: 180,
                },
                selected_bg: Color::Rgb {
                    r: 58,
                    g: 74,
                    b: 104,
                },
                selected_fg: Color::Rgb {
                    r: 244,
                    g: 246,
                    b: 250,
                },
                follow_bg: Color::Rgb {
                    r: 112,
                    g: 78,
                    b: 24,
                },
                follow_fg: Color::Rgb {
                    r: 255,
                    g: 236,
                    b: 200,
                },
                crumb_bg: mix(
                    bg,
                    Color::Rgb {
                        r: 22,
                        g: 28,
                        b: 40,
                    },
                    0.7,
                ),
                warn: Color::Rgb {
                    r: 255,
                    g: 140,
                    b: 150,
                },
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Body,
    Muted,
    Heading,
    Accent,
    Selected,
}

struct Row {
    text: String,
    tone: Tone,
    jump: Option<Jump>,
}

fn paint_sidebar(
    buffer: &mut RenderBuffer,
    rect: Rect,
    view: &View,
    palette: &Palette,
    ascii: bool,
) -> Vec<Hit> {
    let style = |tone: &Tone| row_style(tone, palette);
    fill_rect(buffer, &rect, ' ', &style(&Tone::Body));
    let inner_w = rect.width.saturating_sub(2);
    if rect.width < 8 || rect.height < 4 || inner_w == 0 {
        return Vec::new();
    }
    let rows = sidebar_rows(view, inner_w, rect.height);
    let mut hits = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if index >= rect.height {
            break;
        }
        let y = rect.y + index;
        let tone = if index == 0 { Tone::Heading } else { row.tone };
        let background = if index == 0 {
            palette.title_bg
        } else {
            match &row.tone {
                Tone::Selected => palette.selected_bg,
                _ => palette.sidebar_bg,
            }
        };
        let mut line_style = style(&tone);
        line_style.bg = Some(background);
        if index == 0 {
            line_style.fg = Some(palette.title_fg);
            line_style.bold = true;
        }
        let text = format!(
            "{border}{body}{border}",
            border = if ascii { "|" } else { "│" },
            body = pad(&format!(" {}", row.text.trim_start()), inner_w)
        );
        buffer.set_text(
            rect.x,
            y,
            &truncate_display_width(&text, rect.width),
            &line_style,
        );
        if let Some(cell) = cell_mut(buffer, rect.x, y) {
            cell.style.fg = Some(palette.border);
        }
        if rect.width > 1 {
            if let Some(cell) = cell_mut(buffer, rect.x + rect.width - 1, y) {
                cell.style.fg = Some(palette.border);
            }
        }
        if let Some(jump) = &row.jump {
            hits.push(Hit {
                x0: rect.x + 1,
                x1: rect.x + rect.width - 1,
                y,
                jump: jump.clone(),
            });
        }
    }
    hits
}

fn row_style(tone: &Tone, palette: &Palette) -> Style {
    let fg = match tone {
        Tone::Body => palette.fg,
        Tone::Muted => palette.muted,
        Tone::Heading => palette.heading,
        Tone::Accent => palette.accent,
        Tone::Selected => palette.selected_fg,
    };
    let bg = match tone {
        Tone::Selected => palette.selected_bg,
        _ => palette.sidebar_bg,
    };
    Style {
        fg: Some(fg),
        bg: Some(bg),
        bold: matches!(tone, Tone::Heading | Tone::Accent | Tone::Selected),
        ..Style::default()
    }
}

fn sidebar_rows(view: &View, width: usize, height: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut title = String::from(" explore");
    if view.skeleton {
        title.push_str("  SKEL");
    }
    if view.pr_mode {
        title.push_str("  PR");
    }
    rows.push(Row {
        text: truncate_display_width(&title, width),
        tone: Tone::Heading,
        jump: None,
    });
    let footer = footer_rows(width);
    let note = if view.help {
        Vec::new()
    } else {
        note_rows(view, width)
    };
    let reserved = 1 + footer.len() + note.len();
    let body_height = height.saturating_sub(reserved);
    let mut body = if view.help {
        help_rows(width)
    } else {
        body_rows(view, width)
    };
    body.truncate(body_height);
    rows.extend(body);
    while rows.len() + footer.len() + note.len() < height {
        rows.push(Row {
            text: String::new(),
            tone: Tone::Body,
            jump: None,
        });
    }
    rows.extend(note);
    rows.extend(footer);
    rows.truncate(height);
    rows
}

fn body_rows(view: &View, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    if !view.note.is_empty() {
        rows.push(muted(&view.note, width));
    }
    rows.push(heading("IN"));
    rows.push(body(
        &view
            .in_function
            .clone()
            .unwrap_or_else(|| "top level".to_string()),
        width,
    ));
    rows.push(muted(
        &format!("{} · {}", view.language, view.in_file),
        width,
    ));
    rows.push(blank());
    rows.push(heading("AT CURSOR"));
    match &view.cursor_name {
        Some(name) => {
            let mut label = name.clone();
            if !view.cursor_role.is_empty() {
                label = format!("{name}  {}", view.cursor_role);
            }
            rows.push(body(&label, width));
        }
        None => rows.push(muted("no name", width)),
    }
    if !view.cursor_syntax.is_empty() {
        rows.push(muted(&view.cursor_syntax, width));
    }
    if view.in_function_count > 0 {
        rows.push(accent(
            &format!("{} in this function", view.in_function_count),
            width,
        ));
    }
    if let Some(following) = &view.following {
        rows.push(accent(
            &format!("following {following} · {}", view.follow_spans.len()),
            width,
        ));
    }
    rows.push(blank());
    rows.push(heading("DEFINITION"));
    if let Some(def) = &view.definition {
        rows.extend(definition_rows(def, width));
    } else {
        rows.push(muted("not in the local index", width));
    }
    rows.push(blank());
    rows.push(heading(&format!(
        "REFERENCES  {}",
        view.references.len().min(40)
    )));
    if view.references.is_empty() {
        rows.push(muted("none for this name", width));
    }
    for reference in view.references.iter().take(8) {
        rows.push(reference_row(reference, width));
    }
    if view.references.len() > 8 {
        rows.push(muted(
            &format!("+{} more  \\j next", view.references.len() - 8),
            width,
        ));
    }
    rows
}

fn definition_rows(def: &ViewDef, width: usize) -> Vec<Row> {
    let jump = Jump {
        label: def.label.clone(),
        file: def.file.clone(),
        line: def.line,
        column: def.column,
    };
    let file = Path::new(&def.file)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(def.file.as_str());
    vec![
        Row {
            text: truncate_display_width(&def.label, width),
            tone: Tone::Body,
            jump: Some(jump.clone()),
        },
        Row {
            text: truncate_display_width(
                &format!("{file}:{}  {}  ×{}", def.line + 1, def.kind, def.refs),
                width,
            ),
            tone: Tone::Accent,
            jump: Some(jump),
        },
        muted(&def.signature, width),
    ]
}

fn reference_row(reference: &ViewRef, width: usize) -> Row {
    let marker = if reference.current { "▸" } else { " " };
    Row {
        text: truncate_display_width(
            &format!("{marker}{:>4} {}", reference.line + 1, reference.detail),
            width,
        ),
        tone: if reference.current {
            Tone::Selected
        } else {
            Tone::Body
        },
        jump: Some(Jump {
            label: reference.label.clone(),
            file: reference.file.clone(),
            line: reference.line,
            column: reference.column,
        }),
    }
}

fn note_rows(view: &View, width: usize) -> Vec<Row> {
    let mut rows = vec![muted(&view.index_summary, width)];
    if view.pr_mode && !view.pr_summary.is_empty() {
        rows.push(muted(&view.pr_summary, width));
    }
    rows.push(muted("names, not types · gd = LSP", width));
    rows
}

fn footer_rows(width: usize) -> Vec<Row> {
    vec![
        muted("\\f follow   \\s skeleton", width),
        muted("\\d def      \\t type", width),
        muted("\\b back     \\? keys", width),
    ]
}

fn help_rows(width: usize) -> Vec<Row> {
    [
        "KEYS",
        "\\ e     toggle explore",
        "space E  same",
        "\\ f     follow this name",
        "\\ *     follow this name",
        "double-click follows too",
        "\\ s     skeleton dimming",
        "\\ d     definition",
        "\\ t     type, then back",
        "\\ j/k   next/prev ref",
        "\\ b/n   trail back/forward",
        "\\ r     re-read this file",
        "\\ R     index the repo",
        "\\ p     PR changed files",
        "\\ q     close explore",
        ":explore same commands",
        "",
        "Skeleton keeps calls into",
        "this repo, or this PR, and",
        "stdlib wrappers of syscalls.",
        "Everything else in the",
        "function fades.",
        "",
        "The index is tree-sitter.",
        "SCIP files are not loaded.",
        "gd still asks the LSP.",
    ]
    .into_iter()
    .map(|line| {
        if line == "KEYS" {
            heading(line)
        } else if line.is_empty() {
            blank()
        } else {
            muted(line, width)
        }
    })
    .collect()
}

fn paint_breadcrumb(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    view: &View,
    palette: &Palette,
    ascii: bool,
) -> Vec<Hit> {
    let style = Style {
        fg: Some(palette.fg),
        bg: Some(palette.crumb_bg),
        ..Style::default()
    };
    buffer.fill_rect(
        x,
        y,
        width,
        1,
        ' ',
        &style,
        &Theme {
            style: Style {
                bg: Some(palette.crumb_bg),
                ..Style::default()
            },
            ..Theme::default()
        },
    );
    // fill_rect blends against the passed theme background. The cells are
    // already the crumb color, so a direct write is enough for the text.
    let gap = if ascii { " > " } else { " › " };
    let mut hits = Vec::new();
    let mut text = String::from(" ");
    if view.trail.is_empty() {
        if let Some(function) = &view.in_function {
            text.push_str(function);
        } else {
            text.push_str("explore");
        }
        if !view.cursor_syntax.is_empty() {
            text.push_str(gap);
            text.push_str(&view.cursor_syntax);
        }
        if let Some(name) = &view.cursor_name {
            text.push_str(gap);
            text.push_str(name);
        }
    } else {
        let mut cursor = 1usize;
        for (index, crumb) in view.trail.iter().enumerate() {
            if index > 0 {
                text.push_str(gap);
                cursor += display_width(gap);
            }
            let start = cursor;
            text.push_str(&crumb.label);
            cursor += display_width(&crumb.label);
            hits.push(Hit {
                x0: x + start,
                x1: x + cursor,
                y,
                jump: crumb.clone(),
            });
            if cursor > width.saturating_sub(16) {
                break;
            }
        }
    }
    let mut pills = String::new();
    if view.skeleton {
        pills.push_str(" skel");
    }
    if view.pr_mode {
        pills.push_str(" pr");
    }
    if !view.pr_summary.is_empty() {
        pills.push(' ');
        pills.push_str(&view.pr_summary);
    }
    let pill_w = display_width(&pills);
    let room = width.saturating_sub(pill_w + 1);
    let shown = truncate_display_width(&text, room);
    let current = Style {
        fg: Some(palette.heading),
        bg: Some(palette.crumb_bg),
        bold: true,
        ..Style::default()
    };
    buffer.set_text(x, y, &pad(&shown, room), &current);
    if pill_w > 0 && pill_w < width {
        let pill_style = Style {
            fg: Some(if view.pr_mode {
                palette.warn
            } else {
                palette.accent
            }),
            bg: Some(palette.crumb_bg),
            bold: true,
            ..Style::default()
        };
        buffer.set_text(x + width - pill_w, y, &pills, &pill_style);
    }
    hits
}

fn heading(text: &str) -> Row {
    Row {
        text: text.to_string(),
        tone: Tone::Heading,
        jump: None,
    }
}

fn body(text: &str, width: usize) -> Row {
    Row {
        text: truncate_display_width(text, width),
        tone: Tone::Body,
        jump: None,
    }
}

fn muted(text: &str, width: usize) -> Row {
    Row {
        text: truncate_display_width(text, width),
        tone: Tone::Muted,
        jump: None,
    }
}

fn accent(text: &str, width: usize) -> Row {
    Row {
        text: truncate_display_width(text, width),
        tone: Tone::Accent,
        jump: None,
    }
}

fn blank() -> Row {
    Row {
        text: String::new(),
        tone: Tone::Body,
        jump: None,
    }
}

fn pad(text: &str, width: usize) -> String {
    let mut padded = truncate_display_width(text, width);
    let gap = width.saturating_sub(display_width(&padded));
    padded.extend(std::iter::repeat_n(' ', gap));
    padded
}

fn fill_rect(buffer: &mut RenderBuffer, rect: &Rect, ch: char, style: &Style) {
    let theme = Theme {
        style: Style {
            bg: style.bg,
            ..Style::default()
        },
        ..Theme::default()
    };
    buffer.fill_rect(rect.x, rect.y, rect.width, rect.height, ch, style, &theme);
}

fn cell_mut(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
) -> Option<&mut crate::editor::render_buffer::Cell> {
    if x >= buffer.width || y >= buffer.height {
        return None;
    }
    buffer.cells.get_mut(y * buffer.width + x)
}

fn mix(from: Color, to: Color, amount: f32) -> Color {
    let (ar, ag, ab) = channels(from);
    let (br, bg, bb) = channels(to);
    let amount = amount.clamp(0.0, 1.0);
    let mix_channel = |left: u8, right: u8| -> u8 {
        (f32::from(left) * (1.0 - amount) + f32::from(right) * amount).round() as u8
    };
    Color::Rgb {
        r: mix_channel(ar, br),
        g: mix_channel(ag, bg),
        b: mix_channel(ab, bb),
    }
}

fn channels(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb { r, g, b } => (r, g, b),
        Color::Rgba { r, g, b, a } => {
            let amount = f32::from(a) / 255.0;
            (
                (f32::from(r) * amount) as u8,
                (f32::from(g) * amount) as u8,
                (f32::from(b) * amount) as u8,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::lsp::LspManager;

    const SOURCE: &str = r#"struct LaunchSpec;
fn helper() -> LaunchSpec { LaunchSpec }
fn review(owned: bool) -> bool {
    let template: LaunchSpec = helper();
    let worker = template;
    std::fs::read_to_string("log").ok();
    if worker.starts_with("a") {
        helper();
    }
    template
}
"#;

    #[test]
    fn sidebar_names_the_function_and_the_symbol() {
        let mut editor = editor_with(SOURCE);
        editor.explore.enabled = true;
        editor.cx = 8;
        editor.cy = 3;
        editor.sync_to_window();
        let mut screen = RenderBuffer::new(120, 36, &editor.theme.style);
        editor.render(&mut screen).unwrap();
        let text = screen_text(&screen);
        write_frame(&screen, "explore-sidebar.html");
        assert!(text.contains("explore"), "{text}");
        assert!(text.contains("let template"), "{text}");
        assert!(text.contains("review") || text.contains("IN"), "{text}");
        assert!(
            text.contains("REFERENCES") || text.contains("AT CURSOR"),
            "{text}"
        );
        assert!(text.contains("follow"), "{text}");
    }

    #[test]
    fn follow_and_skeleton_change_the_painted_cells() {
        let mut editor = editor_with(SOURCE);
        editor.explore.enabled = true;
        editor.cx = 8;
        editor.cy = 3;
        editor.sync_to_window();
        let mut screen = RenderBuffer::new(120, 36, &editor.theme.style);
        editor.render(&mut screen).unwrap();
        let position = editor.cursor_text_position();
        editor
            .explore
            .toggle_follow(position.line, position.character)
            .unwrap();
        editor.explore.skeleton = true;
        editor.render(&mut screen).unwrap();
        let view = editor.explore.view(position.line, position.character);
        assert!(view.dim.is_some());
        assert!(view.following.is_some());
        let faded = fg_of(&screen, "worker").expect("worker");
        let kept = fg_of(&screen, "read_to_string").expect("syscall");
        assert_ne!(faded, kept, "skeleton should fade ordinary lines");
        let followed = cells_matching(&screen, "template");
        assert!(
            followed
                .iter()
                .any(|style| style.underline && style.bg.is_some()),
            "followed names should be marked"
        );
        let text = screen_text(&screen);
        write_frame(&screen, "explore-skeleton.html");
        assert!(text.contains("SKEL") || text.contains("skel"), "{text}");
        editor.explore.help = true;
        editor.render(&mut screen).unwrap();
        write_frame(&screen, "explore-help.html");
    }

    fn write_frame(buffer: &RenderBuffer, name: &str) {
        let Ok(dir) = std::env::var("EXPLORE_SHOTS") else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = std::path::Path::new(&dir).join(name);
        let mut html = String::from(
            "<!doctype html><meta charset=\"utf-8\"><title>explore</title>\
<style>html,body{margin:0;background:#07090d}pre{margin:0;padding:18px 16px 20px;font-family:'JetBrains Mono','DejaVu Sans Mono',monospace;font-size:15px;line-height:1.35;white-space:pre}</style><pre>",
        );
        for y in 0..buffer.height {
            let mut x = 0;
            while x < buffer.width {
                let cell = &buffer.cells[y * buffer.width + x];
                let style = &cell.style;
                let mut text = String::new();
                while x < buffer.width {
                    let next = &buffer.cells[y * buffer.width + x];
                    if next.style != *style {
                        break;
                    }
                    text.push_str(&next.text);
                    x += 1;
                }
                html.push_str("<span style=\"");
                if let Some(color) = style.fg {
                    html.push_str(&format!("color:{color};"));
                }
                if let Some(color) = style.bg {
                    html.push_str(&format!("background:{color};"));
                }
                if style.bold {
                    html.push_str("font-weight:700;");
                }
                if style.italic {
                    html.push_str("font-style:italic;");
                }
                if style.underline {
                    html.push_str("text-decoration:underline;");
                }
                html.push_str("\">");
                html.push_str(&escape(&text));
                html.push_str("</span>");
            }
            if y + 1 != buffer.height {
                html.push('\n');
            }
        }
        html.push_str("</pre>");
        let _ = std::fs::write(path, html);
    }

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn fg_of(buffer: &RenderBuffer, token: &str) -> Option<Color> {
        cells_matching(buffer, token)
            .into_iter()
            .find_map(|style| style.fg)
    }

    fn cells_matching(buffer: &RenderBuffer, token: &str) -> Vec<Style> {
        let mut styles = Vec::new();
        for y in 0..buffer.height {
            let mut row = String::new();
            for x in 0..buffer.width {
                row.push_str(&buffer.cells[y * buffer.width + x].text);
            }
            let mut search_from = 0;
            while let Some(found) = row[search_from..].find(token) {
                let index = search_from + found;
                let char_at = row[..index].chars().count();
                if let Some(cell) = buffer.cells.get(y * buffer.width + char_at) {
                    styles.push(cell.style.clone());
                }
                search_from = index + token.len();
            }
        }
        styles
    }

    fn editor_with(source: &str) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = crate::buffer::Buffer::new(Some("review.rs".into()), source.to_string());
        let theme = crate::theme::parse_vscode_theme_contents(include_str!(
            "../../themes/tokyo-night.json"
        ))
        .unwrap_or_else(|_| Theme::default());
        let mut editor = Editor::with_size(lsp, 120, 36, config, theme, vec![buffer]).unwrap();
        editor.test_disable_terminal_output();
        editor.splash_dismissed = true;
        editor
    }

    fn screen_text(buffer: &RenderBuffer) -> String {
        let mut lines = Vec::new();
        for y in 0..buffer.height {
            let mut line = String::new();
            for x in 0..buffer.width {
                line.push_str(&buffer.cells[y * buffer.width + x].text);
            }
            lines.push(line);
        }
        lines.join("\n")
    }
}
