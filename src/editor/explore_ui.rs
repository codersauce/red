//! Terminal chrome for code explore: sidebar, breadcrumb, skeleton dimming,
//! and symbol-follow highlights.

use std::path::Path;

use unicode_segmentation::UnicodeSegmentation as _;

use crate::color::{ensure_minimum_contrast, Color};
use crate::explore::{sidebar_columns, DimZone, Hit, Jump, SyncInput, View, ViewDef, ViewRef};
use crate::theme::{Style, SurfacePalette, Theme, MINIMUM_SELECTION_TEXT_CONTRAST};
use crate::unicode_utils::{
    char_to_grapheme, display_width, trim_line_ending, truncate_display_width,
};

use super::{Action, Editor, RenderBuffer, Runtime};

pub(super) struct ExploreIndexRun {
    receiver: std::sync::mpsc::Receiver<crate::explore::IndexEvent>,
    notification: crate::notification::NotificationId,
    review: bool,
}

pub(super) struct ExploreBufferParse {
    key: String,
    revision: u64,
    receiver: std::sync::mpsc::Receiver<crate::explore::ParsedBuffer>,
}

struct ExploreIndexPoll {
    review: bool,
    notification: crate::notification::NotificationId,
    progress: Option<(usize, usize)>,
    ready: Option<(Box<crate::explore::Index>, crate::explore::ScanStats)>,
    failed: Option<String>,
}

pub(super) struct ExploreBufferSnapshot {
    key: String,
    revision: u64,
    language: Option<String>,
    source: String,
}

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

    #[inline(never)]
    pub(super) fn explore_command<'a>(
        &'a mut self,
        command: &'a str,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.execute_explore_command(command, buffer, runtime))
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
                    }
                    if self.explore.enabled {
                        "Code explore on. Space e ? opens keyboard shortcuts. Double-click a name to follow it."
                            .to_string()
                    } else {
                        "Code explore off.".to_string()
                    }
                }
                "on" => {
                    self.explore.enabled = true;
                    "Code explore on. Space e ? opens keyboard shortcuts.".to_string()
                }
                "off" | "close" => {
                    self.explore.enabled = false;
                    self.explore.clear_follow();
                    "Code explore off.".to_string()
                }
                "skeleton" | "skel" => {
                    self.explore.enabled = true;
                    self.explore.skeleton = !self.explore.skeleton;
                    if self.explore.skeleton {
                        "Dim on. Calls into this project, and library calls that touch the system, stay bright.".to_string()
                    } else {
                        "Dim off.".to_string()
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
                "index" | "scan" => self.start_explore_index(false, None),
                "refresh" => {
                    self.explore.invalidate();
                    self.sync_explore_buffer();
                    "Re-read this file.".to_string()
                }
                "pr" => self.explore_pr(None),
                "help" | "?" => {
                    self.explore.enabled = true;
                    self.open_explore_shortcuts(Some(runtime));
                    "Keyboard shortcuts.".to_string()
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
            hits.extend(paint_sidebar(
                buffer,
                rect,
                &view,
                &palette,
                ascii,
                explore_title_icon(self.config.picker.icons.style),
            ));
        }
        if self.explore_chrome_rows(window) == 1 {
            let y = window.position.y + window.inner_height() - 1;
            hits.extend(paint_breadcrumb(
                buffer,
                window.position.x,
                y,
                window.inner_width(),
                &view,
                ascii,
                &self.theme,
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
        if self.explore.is_current(&key, revision) || self.buffer_parse_covers(&key, revision) {
            return;
        }
        let bytes = self.current_buffer().byte_len();
        if bytes > crate::explore::MAX_SOURCE_BYTES {
            self.explore.install_analysis(
                key,
                revision,
                0,
                crate::explore::Analysis::empty("file is too large for the explore parse"),
            );
            return;
        }
        let source = self.current_buffer().contents();
        let language = self.current_language_id();
        if bytes > crate::explore::INLINE_PARSE_BYTES {
            self.queue_buffer_parse(key, revision, language, source);
            return;
        }
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
        if !jump.file.is_empty() && jump.file != self.explore_file_key() {
            if let Some(id) = jump.file.strip_prefix("buffer:") {
                let Ok(id) = id.parse::<u64>() else {
                    return Ok(format!("{label} is no longer an open buffer"));
                };
                let open = self
                    .buffer_manager
                    .iter()
                    .any(|buffer| buffer.id().as_u64() == id);
                if !open {
                    return Ok(format!("buffer {id} is no longer open"));
                }
                self.execute_with_tracking(&Action::OpenBufferById(id), buffer, runtime, false)
                    .await?;
            } else if crate::explore::normalize_path(&jump.file) != self.explore_file_key() {
                self.execute_with_tracking(
                    &Action::OpenFile(jump.file.clone()),
                    buffer,
                    runtime,
                    false,
                )
                .await?;
            }
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

    fn start_explore_index(&mut self, review: bool, base: Option<&str>) -> String {
        if self.explore_index.is_some() {
            return "An index is already running.".to_string();
        }
        let Some(root) = self.explore_root() else {
            return "No directory to index.".to_string();
        };
        let registry = self.highlighter.registry().as_ref().clone();
        let receiver =
            crate::explore::spawn_index(root, registry, review, base.map(str::to_string));
        let label = if review { "review" } else { "project" };
        let notification = self.notifications.begin_progress(
            crate::notification::NotificationSource::Editor,
            "explore-index",
            crate::notification::MessageContent::new(format!("Indexing {label}")),
            crate::notification::ProgressPriority::UserInitiated,
            crate::notification::NotificationTime::now(),
        );
        match notification {
            Ok(notification) => {
                self.explore_index = Some(ExploreIndexRun {
                    receiver,
                    notification,
                    review,
                });
                format!("Indexing {label}.")
            }
            Err(_) => "An index is already running.".to_string(),
        }
    }

    fn explore_pr(&mut self, base: Option<&str>) -> String {
        if self.explore_index.is_some() {
            return "An index is already running.".to_string();
        }
        if self.explore.pr_mode && base.is_none() {
            self.explore.clear_pr();
            return "Review scope off. Dim uses every indexed definition.".to_string();
        }
        self.explore.enabled = true;
        self.start_explore_index(true, base)
    }

    pub(super) fn service_explore_background(&mut self) -> bool {
        let mut render = self.poll_explore_index();
        render |= self.poll_buffer_parse();
        render
    }

    fn poll_explore_index(&mut self) -> bool {
        let Some(ExploreIndexPoll {
            review,
            notification,
            progress,
            ready,
            failed,
        }) = self.drain_explore_index()
        else {
            return false;
        };
        if let Some((done, total)) = progress {
            let label = if review { "review" } else { "project" };
            let text = format!("Indexing {label} · {done}/{total}");
            let percentage =
                (total > 0).then(|| ((done.saturating_mul(100)) / total).min(100) as u8);
            let _ = self.notifications.update_progress(
                notification,
                crate::notification::MessageContent::new(text.clone()),
                percentage,
                crate::notification::NotificationTime::now(),
            );
            self.set_quiet_message(Some(text));
        }
        if let Some((index, stats)) = ready {
            self.explore.index = *index;
            self.explore.pr_mode = review;
            self.ingest_open_buffers();
            let summary = Self::index_summary(review, &stats);
            let _ = self.notifications.finish_progress(
                notification,
                crate::notification::ProgressOutcome::Succeeded,
                crate::notification::MessageContent::new(summary.clone()),
                crate::notification::NotificationTime::now(),
            );
            self.set_quiet_message(Some(summary));
            self.explore_index = None;
            return true;
        }
        if let Some(message) = failed {
            let _ = self.notifications.finish_progress(
                notification,
                crate::notification::ProgressOutcome::Failed,
                crate::notification::MessageContent::new(message.clone()),
                crate::notification::NotificationTime::now(),
            );
            self.set_quiet_message(Some(message));
            self.explore_index = None;
            return true;
        }
        progress.is_some()
    }

    fn drain_explore_index(&self) -> Option<ExploreIndexPoll> {
        let run = self.explore_index.as_ref()?;
        let mut progress = None;
        let mut ready = None;
        let mut failed = None;
        loop {
            match run.receiver.try_recv() {
                Ok(crate::explore::IndexEvent::Progress { done, total }) => {
                    progress = Some((done, total));
                }
                Ok(crate::explore::IndexEvent::Ready { index, stats }) => {
                    ready = Some((index, stats));
                    break;
                }
                Ok(crate::explore::IndexEvent::Failed { message }) => {
                    failed = Some(message);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    failed = Some("Index stopped before it finished.".to_string());
                    break;
                }
            }
        }
        if progress.is_none() && ready.is_none() && failed.is_none() {
            return None;
        }
        Some(ExploreIndexPoll {
            review: run.review,
            notification: run.notification,
            progress,
            ready,
            failed,
        })
    }

    fn poll_buffer_parse(&mut self) -> bool {
        let parsed = {
            let Some(run) = &self.explore_buffer_parse else {
                return false;
            };
            match run.receiver.try_recv() {
                Ok(parsed) => Some(parsed),
                Err(std::sync::mpsc::TryRecvError::Empty) => return false,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
            }
        };
        self.explore_buffer_parse = None;
        let mut render = false;
        if let Some(parsed) = parsed {
            let current = self.explore_file_key();
            let revision = self.current_buffer().revision();
            if parsed.key == current && parsed.revision == revision {
                self.explore.install_analysis(
                    parsed.key,
                    parsed.revision,
                    parsed.hash,
                    parsed.analysis,
                );
                render = true;
            }
        }
        self.kick_buffer_parse();
        render
    }

    fn buffer_parse_covers(&self, key: &str, revision: u64) -> bool {
        self.explore_buffer_parse
            .as_ref()
            .is_some_and(|parse| parse.key == key && parse.revision == revision)
            || self
                .explore_buffer_parse_next
                .as_ref()
                .is_some_and(|parse| parse.key == key && parse.revision == revision)
    }

    fn queue_buffer_parse(
        &mut self,
        key: String,
        revision: u64,
        language: Option<String>,
        source: String,
    ) {
        self.explore_buffer_parse_next = Some(ExploreBufferSnapshot {
            key,
            revision,
            language,
            source,
        });
        if self.explore_buffer_parse.is_none() {
            self.kick_buffer_parse();
        }
    }

    fn kick_buffer_parse(&mut self) {
        if self.explore_buffer_parse.is_some() {
            return;
        }
        let Some(snapshot) = self.explore_buffer_parse_next.take() else {
            return;
        };
        let registry = self.highlighter.registry().as_ref().clone();
        let key = snapshot.key.clone();
        let revision = snapshot.revision;
        let receiver = crate::explore::spawn_buffer_parse(
            snapshot.key,
            snapshot.revision,
            snapshot.language,
            snapshot.source,
            registry,
        );
        self.explore_buffer_parse = Some(ExploreBufferParse {
            key,
            revision,
            receiver,
        });
    }

    fn explore_root(&self) -> Option<std::path::PathBuf> {
        let key = self.explore_file_key();
        if key.starts_with("buffer:") {
            return crate::explore::repo_root(Path::new("."));
        }
        self.explore.root_for(&key)
    }

    fn index_summary(review: bool, stats: &crate::explore::ScanStats) -> String {
        let scope = if review { "review" } else { "project" };
        format!(
            "Indexed {} {scope} {}{}. {}",
            stats.files,
            if stats.files == 1 { "file" } else { "files" },
            if stats.truncated { " (capped)" } else { "" },
            stats.message
        )
    }

    fn ingest_open_buffers(&mut self) {
        let opened = self
            .buffer_manager
            .iter()
            .filter_map(|buffer| {
                let file = buffer.file.clone()?;
                Some((
                    crate::explore::normalize_path(&file),
                    buffer.byte_len(),
                    buffer.contents(),
                    file,
                ))
            })
            .collect::<Vec<_>>();
        let registry = self.highlighter.registry();
        for (key, bytes, source, file) in opened {
            if bytes > crate::explore::INLINE_PARSE_BYTES {
                continue;
            }
            let Some(language) = self.highlighter.language_id_for_file(Some(&file)) else {
                continue;
            };
            self.explore
                .ingest_source(&key, &source, language, &registry);
        }
    }

    fn explore_sidebar_rect(&self, window: &crate::window::Window) -> Option<Rect> {
        let width = self.explore_sidebar_columns(window);
        if width == 0 {
            return None;
        }
        let height = window
            .inner_height()
            .saturating_sub(self.explore_chrome_rows(window));
        if height < 4 {
            return None;
        }
        Some(Rect {
            x: window.position.x + window.inner_width() - width,
            y: window.position.y,
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
        let tab_width = self.tab_stops_for_buffer_index(window.buffer_index);
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
                    tab_width.advance(display)
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
    let text = format!(" {count}");
    let width = display_width(&text);
    if line_width + width > content_width {
        return;
    }
    let style = Style {
        fg: theme.statusline_style.inner_style.fg.or(Some(palette.fg)),
        bg: theme.style.bg,
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
    title_fg: Color,
    title_bg: Color,
    border: Color,
    editor_bg: Color,
    sidebar_bg: Color,
    selected_bg: Color,
    selected_fg: Color,
    follow_bg: Color,
    follow_fg: Color,
}

impl Palette {
    fn from_theme(theme: &Theme) -> Self {
        let editor_bg = theme.style.bg.unwrap_or_default();
        let editor_fg = theme.style.fg.unwrap_or(Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        let pick = |keys: &[&str], fallback: Color| {
            keys.iter()
                .find_map(|key| theme.colors.get(*key).copied())
                .unwrap_or(fallback)
        };
        let sidebar_bg = pick(
            &["sideBar.background", "editor.background"],
            theme.ui_style.popup.bg.unwrap_or(editor_bg),
        );
        let fg = pick(
            &["sideBar.foreground", "editor.foreground"],
            theme.ui_style.popup.fg.unwrap_or(editor_fg),
        );
        let muted = theme.ui_style.muted.fg.unwrap_or(pick(
            &["descriptionForeground", "editorLineNumber.foreground"],
            fg,
        ));
        let border = pick(
            &[
                "sideBar.border",
                "panel.border",
                "editorLineNumber.foreground",
            ],
            muted,
        );
        let title_fg = pick(
            &[
                "sideBarSectionHeader.foreground",
                "sideBarTitle.foreground",
                "sideBar.foreground",
            ],
            fg,
        );
        let title_bg = pick(
            &["sideBarSectionHeader.background", "sideBar.background"],
            sidebar_bg,
        );
        let selected_bg = theme
            .ui_style
            .picker_selected_item
            .bg
            .unwrap_or(pick(&["list.activeSelectionBackground"], sidebar_bg));
        let selected_fg = theme
            .ui_style
            .picker_selected_item
            .fg
            .unwrap_or(pick(&["list.activeSelectionForeground"], fg));
        let follow_bg = theme
            .find_match_highlight_style
            .as_ref()
            .and_then(|style| style.bg)
            .or_else(|| theme.find_match_style.as_ref().and_then(|style| style.bg))
            .unwrap_or(selected_bg);
        let follow_fg = theme
            .find_match_highlight_style
            .as_ref()
            .and_then(|style| style.fg)
            .or_else(|| theme.find_match_style.as_ref().and_then(|style| style.fg))
            .unwrap_or(selected_fg);
        let roles = SurfacePalette::new(
            theme,
            &Style {
                fg: Some(fg),
                bg: Some(sidebar_bg),
                ..Style::default()
            },
        );
        let fg = roles.primary.fg.unwrap_or(fg);
        let muted = roles.muted.fg.unwrap_or(muted);
        let title_fg = ensure_minimum_contrast(title_fg, title_bg, MINIMUM_SELECTION_TEXT_CONTRAST);
        let selected_fg =
            ensure_minimum_contrast(selected_fg, selected_bg, MINIMUM_SELECTION_TEXT_CONTRAST);
        let follow_fg =
            ensure_minimum_contrast(follow_fg, follow_bg, MINIMUM_SELECTION_TEXT_CONTRAST);
        Self {
            fg,
            muted,
            title_fg,
            title_bg,
            border,
            editor_bg,
            sidebar_bg,
            selected_bg,
            selected_fg,
            follow_bg,
            follow_fg,
        }
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Body,
    Muted,
    Heading,
    Selected,
}

struct Row {
    text: String,
    tone: Tone,
    jump: Option<Jump>,
    right: Option<String>,
}

fn explore_title_icon(style: crate::config::PickerIconStyle) -> &'static str {
    match style {
        crate::config::PickerIconStyle::NerdFont => "",
        _ => ">",
    }
}

fn paint_sidebar(
    buffer: &mut RenderBuffer,
    rect: Rect,
    view: &View,
    palette: &Palette,
    ascii: bool,
    icon: &str,
) -> Vec<Hit> {
    if rect.width < 8 || rect.height < 4 {
        return Vec::new();
    }
    let body_w = rect.width.saturating_sub(1);
    let content_w = body_w.saturating_sub(1);
    if body_w == 0 || content_w == 0 {
        return Vec::new();
    }
    let surface = Style {
        fg: Some(palette.fg),
        bg: Some(palette.sidebar_bg),
        ..Style::default()
    };
    let divider = Style {
        fg: Some(palette.border),
        bg: Some(palette.editor_bg),
        ..Style::default()
    };
    let title = Style {
        fg: Some(palette.title_fg),
        bg: Some(palette.title_bg),
        bold: true,
        ..Style::default()
    };
    let body_x = rect.x + 1;
    let blank = " ".repeat(body_w);
    let rail = if ascii { "|" } else { "│" };
    for row in 0..rect.height {
        let y = rect.y + row;
        buffer.set_text(body_x, y, &blank, &surface);
        buffer.set_text(rect.x, y, rail, &divider);
    }
    let title_text = if icon.is_empty() {
        " Explore".to_string()
    } else {
        format!(" {icon} Explore")
    };
    buffer.set_text(body_x, rect.y, &pad(&title_text, body_w), &title);
    let rows = sidebar_rows(view, content_w, rect.height.saturating_sub(2));
    let mut hits = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let y = rect.y + 2 + index;
        paint_sidebar_row(buffer, body_x, y, body_w, row, palette);
        if let Some(jump) = &row.jump {
            hits.push(Hit {
                x0: body_x,
                x1: rect.x + rect.width,
                y,
                jump: jump.clone(),
            });
        }
    }
    hits
}

fn paint_sidebar_row(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    row: &Row,
    palette: &Palette,
) {
    let line_style = row_style(&row.tone, palette);
    let Some(right) = row.right.as_deref() else {
        buffer.set_text(x, y, &pad(&format!(" {}", row.text), width), &line_style);
        return;
    };
    let right_w = display_width(right);
    let left_room = width.saturating_sub(right_w);
    buffer.set_text(
        x,
        y,
        &pad(&format!(" {}", row.text), left_room),
        &line_style,
    );
    if right_w > 0 && right_w < width {
        buffer.set_text(x + left_room, y, right, &row_style(&Tone::Muted, palette));
    }
}

fn row_style(tone: &Tone, palette: &Palette) -> Style {
    let fg = match tone {
        Tone::Body | Tone::Heading => palette.fg,
        Tone::Muted => palette.muted,
        Tone::Selected => palette.selected_fg,
    };
    let bg = match tone {
        Tone::Selected => palette.selected_bg,
        _ => palette.sidebar_bg,
    };
    Style {
        fg: Some(fg),
        bg: Some(bg),
        bold: matches!(tone, Tone::Heading | Tone::Selected),
        ..Style::default()
    }
}

fn sidebar_rows(view: &View, width: usize, height: usize) -> Vec<Row> {
    let mut rows = body_rows(view, width);
    rows.push(blank());
    rows.extend(note_rows(view, width));
    rows.truncate(height);
    rows
}

fn body_rows(view: &View, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    if !view.note.is_empty() {
        rows.push(muted(&view.note, width));
    }
    rows.push(heading("Inside"));
    rows.push(body(
        &view
            .in_function
            .clone()
            .unwrap_or_else(|| "top of the file".to_string()),
        width,
    ));
    rows.push(muted(
        &format!("{} · {}", view.language, view.in_file),
        width,
    ));
    rows.push(blank());
    rows.push(heading("Here"));
    match &view.cursor_name {
        Some(name) => {
            let mut label = name.clone();
            if !view.cursor_role.is_empty() {
                label = format!("{name}  {}", plain_role(&view.cursor_role));
            }
            rows.push(body(&label, width));
        }
        None => rows.push(muted("No name under the cursor", width)),
    }
    if !view.cursor_syntax.is_empty() {
        rows.push(muted(&view.cursor_syntax, width));
    }
    if view.in_function_count > 0 {
        rows.push(body(
            &format!("{} in this function", view.in_function_count),
            width,
        ));
    }
    if let Some(following) = &view.following {
        rows.push(body(
            &format!("Following {following} ({})", view.follow_spans.len()),
            width,
        ));
    }
    rows.push(blank());
    rows.push(heading("Definition"));
    if let Some(def) = &view.definition {
        rows.extend(definition_rows(def, width));
    } else {
        rows.push(muted("No definition in this index", width));
    }
    rows.push(blank());
    rows.push(heading_count("Matches", view.references.len().min(40)));
    if view.references.is_empty() {
        rows.push(muted("No matches for this name", width));
    }
    for reference in view.references.iter().take(8) {
        rows.push(reference_row(reference, width));
    }
    if view.references.len() > 8 {
        rows.push(muted(
            &format!("+{} more", view.references.len() - 8),
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
        row(clip(&def.label, width), Tone::Body, Some(jump.clone())),
        row(
            clip(
                &format!("{file}:{}  {}  {}", def.line + 1, def.kind, def.refs),
                width,
            ),
            Tone::Muted,
            Some(jump),
        ),
        muted(&def.signature, width),
    ]
}

fn reference_row(reference: &ViewRef, width: usize) -> Row {
    let marker = if reference.current { "▸" } else { " " };
    row(
        clip(
            &format!("{marker}{:>4} {}", reference.line + 1, reference.detail),
            width,
        ),
        if reference.current {
            Tone::Selected
        } else {
            Tone::Body
        },
        Some(Jump {
            label: reference.label.clone(),
            file: reference.file.clone(),
            line: reference.line,
            column: reference.column,
        }),
    )
}

fn note_rows(view: &View, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    rows.extend(wrap_muted(&view.index_summary, width));
    if view.pr_mode && !view.pr_summary.is_empty() {
        rows.extend(wrap_muted(&view.pr_summary, width));
    }
    rows.extend(wrap_muted(&view.index_note, width));
    rows
}

fn paint_breadcrumb(
    buffer: &mut RenderBuffer,
    x: usize,
    y: usize,
    width: usize,
    view: &View,
    ascii: bool,
    theme: &Theme,
) -> Vec<Hit> {
    let style = theme.statusline_style.inner_style.clone();
    let fill_theme = Theme {
        style: Style {
            bg: style.bg,
            ..Style::default()
        },
        ..Theme::default()
    };
    buffer.fill_rect(x, y, width, 1, ' ', &style, &fill_theme);
    let gap = if ascii { " > " } else { " › " };
    let mut text = String::from(" ");
    for (index, segment) in location_segments(view).iter().enumerate() {
        if index > 0 {
            text.push_str(gap);
        }
        text.push_str(segment);
    }
    let mut tail = Vec::new();
    if !view.pr_summary.is_empty() {
        tail.push((format!(" {} ", view.pr_summary), style.clone()));
    }
    if view.skeleton {
        tail.push((
            " DIM ".to_string(),
            theme.statusline_style.outer_style.clone(),
        ));
    }
    if view.pr_mode {
        tail.push((
            " REVIEW ".to_string(),
            theme.statusline_style.outer_style.clone(),
        ));
    }
    if !view.trail.is_empty() {
        let counter = format!(" {}/{} ", view.trail_at + 1, view.trail.len());
        let mut counter_style = theme.ui_style.muted.clone();
        counter_style.bg = style.bg.or(counter_style.bg);
        counter_style.fg = theme
            .colors
            .get("descriptionForeground")
            .or_else(|| theme.colors.get("editorLineNumber.foreground"))
            .copied()
            .or(counter_style.fg)
            .or(style.fg);
        tail.push((counter, counter_style));
    }
    let tail_w = tail
        .iter()
        .map(|(label, _)| display_width(label))
        .sum::<usize>();
    let room = width.saturating_sub(tail_w);
    let shown = clip(&text, room);
    buffer.set_text(x, y, &pad(&shown, room), &style);
    if tail_w > 0 && tail_w < width {
        let mut cursor = x + width - tail_w;
        for (label, label_style) in &tail {
            buffer.set_text(cursor, y, label, label_style);
            cursor += display_width(label);
        }
    }
    Vec::new()
}

fn location_segments(view: &View) -> Vec<String> {
    let mut segments = Vec::new();
    if let Some(function) = &view.in_function {
        push_distinct(&mut segments, function);
    }
    if !view.cursor_syntax.is_empty() {
        push_distinct(&mut segments, &view.cursor_syntax);
    }
    if let Some(name) = &view.cursor_name {
        push_distinct(&mut segments, name);
    }
    segments
}

fn push_distinct(segments: &mut Vec<String>, next: &str) {
    if next.is_empty() || segments.last().is_some_and(|previous| previous == next) {
        return;
    }
    segments.push(next.to_string());
}

fn row(text: String, tone: Tone, jump: Option<Jump>) -> Row {
    Row {
        text,
        tone,
        jump,
        right: None,
    }
}

fn heading(text: &str) -> Row {
    row(text.to_string(), Tone::Heading, None)
}

fn heading_count(text: &str, count: usize) -> Row {
    Row {
        right: Some(count.to_string()),
        ..heading(text)
    }
}

fn body(text: &str, width: usize) -> Row {
    row(clip(text, width), Tone::Body, None)
}

fn muted(text: &str, width: usize) -> Row {
    row(clip(text, width), Tone::Muted, None)
}

fn blank() -> Row {
    row(String::new(), Tone::Body, None)
}

fn clip(text: &str, width: usize) -> String {
    truncate_display_width(text, width)
}

fn plain_role(role: &str) -> &str {
    match role {
        "definition" => "definition",
        "call" => "call",
        "type" => "type",
        "field" => "field",
        _ => "name",
    }
}

fn wrap_muted(text: &str, width: usize) -> Vec<Row> {
    if text.is_empty() {
        return Vec::new();
    }
    wrap_lines(text, width)
        .into_iter()
        .map(|line| muted(&line, width))
        .collect()
}

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![clip(text, width)];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for word in text.split_whitespace() {
        let word_w = display_width(word);
        if word_w > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_w = 0;
            }
            lines.push(clip(word, width));
            continue;
        }
        let extra = usize::from(!current.is_empty());
        if current_w + extra + word_w > width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_w += 1;
        }
        current.push_str(word);
        current_w += word_w;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
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

fn pad(text: &str, width: usize) -> String {
    let mut padded = truncate_display_width(text, width);
    let gap = width.saturating_sub(display_width(&padded));
    padded.extend(std::iter::repeat_n(' ', gap));
    padded
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
        assert!(text.contains("Explore"), "{text}");
        assert!(
            screen_text_row(&screen, 0).contains("Explore"),
            "the title shares the top row: {}",
            screen_text_row(&screen, 0)
        );
        assert!(text.contains("let template"), "{text}");
        assert!(text.contains("Definition"), "{text}");
        assert!(!text.contains("DEFINITION"), "{text}");
        assert!(!text.contains("binding"), "{text}");
        assert!(text.contains("variable") || text.contains("name"), "{text}");
        assert!(
            !text.contains('['),
            "explore draws no bracketed keys: {text}"
        );
        assert!(
            text.contains("Same spelling") || text.contains("language server"),
            "{text}"
        );
        assert!(text.contains('│'), "the editor edge is one divider: {text}");
        assert!(
            !text.contains('┌') && !text.contains('└'),
            "explore has no dialog frame: {text}"
        );
        assert!(
            text.contains("let  ") || text.contains("let 3") || text.contains("let  3"),
            "the definition count stays beside the kind: {text}"
        );
    }

    #[test]
    fn title_stays_on_the_top_row_when_a_window_bar_is_present() {
        let mut editor = editor_with(SOURCE);
        editor.explore.enabled = true;
        let window_id = editor
            .window_manager
            .active_stable_window_id()
            .expect("active window");
        editor
            .window_bar_manager
            .create("test-bar".into(), crate::plugin::WindowBarConfig::default());
        editor.window_bar_manager.update(
            "test-bar",
            window_id,
            vec![crate::plugin::WindowBarSegment {
                id: Some("chrome".into()),
                text: "X".repeat(400),
                style: crate::plugin::WindowBarStyle::default(),
                tooltip: None,
                action: None,
            }],
        );
        let mut screen = RenderBuffer::new(120, 36, &editor.theme.style);
        editor.render(&mut screen).unwrap();
        let top = screen_text_row(&screen, 0);
        let divider = top.find('│').expect("divider");
        assert!(top[..divider].contains('X'), "{top}");
        assert!(top[divider..].contains("Explore"), "{top}");
        assert!(!top[divider..].contains('X'), "{top}");
    }

    #[test]
    fn mocha_and_ascii_keep_the_title_and_a_single_divider() {
        let mut editor = editor_with(SOURCE);
        editor.theme = crate::theme::parse_vscode_theme("themes/mocha.json").unwrap();
        editor.config.window_borders_ascii = true;
        editor.config.picker.icons.style = crate::config::PickerIconStyle::Ascii;
        editor.explore.enabled = true;
        let mut screen = RenderBuffer::new(120, 36, &editor.theme.style);
        editor.render(&mut screen).unwrap();
        let top = screen_text_row(&screen, 0);
        assert!(top.contains('|'), "{top}");
        assert!(top.contains("> Explore"), "{top}");
        assert!(!top.contains('│') && !top.contains('┌'), "{top}");
        let title_bg = editor
            .theme
            .colors
            .get("sideBarSectionHeader.background")
            .copied();
        let explore_at = top.find("Explore").expect("title");
        let cell = &screen.cells[explore_at];
        assert_eq!(cell.style.bg, title_bg);
        assert!(cell.style.bold);
    }

    #[test]
    fn location_row_follows_the_cursor_and_counts_the_trail() {
        let mut editor = editor_with(SOURCE);
        editor.explore.enabled = true;
        let mut screen = RenderBuffer::new(120, 36, &editor.theme.style);

        place(&mut editor, 3, "template");
        editor.render(&mut screen).unwrap();
        let inside = screen_text(&screen);
        assert!(inside.contains("review › variable › template"), "{inside}");
        assert!(!inside.contains("1/1"), "{inside}");

        place(&mut editor, 0, "LaunchSpec");
        editor.render(&mut screen).unwrap();
        let top = screen_text(&screen);
        assert!(top.contains("struct › LaunchSpec"), "{top}");
        assert!(top.contains("top of the file"), "{top}");
        assert!(!top.contains("explore ›"), "{top}");
        assert!(!top.contains("1/1"), "{top}");

        place(&mut editor, 3, "LaunchSpec");
        editor.sync_explore_buffer();
        let from = editor.cursor_crumb();
        let position = editor.cursor_text_position();
        let to = editor
            .explore
            .definition_jump(position.line, position.character)
            .expect("definition");
        editor.explore.record_jump(from, to.clone());
        editor.cy = to.line;
        editor.cx = to.column;
        editor.sync_to_window();
        editor.render(&mut screen).unwrap();
        let jumped = screen_text(&screen);
        assert!(jumped.contains("struct › LaunchSpec"), "{jumped}");
        assert!(jumped.contains("2/2"), "{jumped}");
        assert!(!jumped.contains("LaunchSpec › LaunchSpec"), "{jumped}");
        assert!(jumped.contains("top of the file"), "{jumped}");

        let back = editor.explore.back().expect("trail back");
        editor.cy = back.line;
        editor.cx = back.column;
        editor.sync_to_window();
        editor.render(&mut screen).unwrap();
        let returned = screen_text(&screen);
        assert!(returned.contains("1/2"), "{returned}");
        assert!(returned.contains("review ›"), "{returned}");
    }

    fn place(editor: &mut Editor, line: usize, needle: &str) {
        let text = editor.current_buffer().get(line).unwrap_or_default();
        editor.cy = line;
        editor.cx = text.find(needle).unwrap_or(0);
        editor.sync_to_window();
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
        assert!(text.contains("DIM"), "{text}");
        assert!(!text.contains("×"), "counts are plain numbers: {text}");
        editor.config =
            crate::config::Config::load_user_toml("", std::path::Path::new("config.toml"), &[])
                .unwrap()
                .config;
        editor.open_explore_shortcuts(None);
        editor.render(&mut screen).unwrap();
        let help = screen_text(&screen);
        write_frame(&screen, "explore-help.html");
        assert!(help.contains("Keyboard shortcuts"), "{help}");
        assert!(help.contains("Explore"), "{help}");
        assert!(help.contains("Space e f"), "{help}");
        assert!(help.contains("Space e ?"), "{help}");
        assert!(!help.contains("[e "), "{help}");
        assert!(!help.contains("LSP"), "{help}");
    }

    fn write_frame(buffer: &RenderBuffer, name: &str) {
        let Ok(dir) = std::env::var("EXPLORE_SHOTS") else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = std::path::Path::new(&dir).join(name);
        let mut html = String::from(
            "<!doctype html><meta charset=\"utf-8\"><title>explore</title>\
<style>html,body{margin:0;background:#07090d}pre{margin:0;padding:18px 16px 20px;font-family:'JetBrainsMono Nerd Font','JetBrainsMono NF','JetBrains Mono','DejaVu Sans Mono',monospace;font-size:15px;line-height:1.35;white-space:pre}</style><pre>",
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

    fn screen_text_row(buffer: &RenderBuffer, y: usize) -> String {
        let start = y * buffer.width;
        buffer.cells[start..start + buffer.width]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[tokio::test]
    async fn trail_back_activates_the_unnamed_source_buffer() {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let unnamed = crate::buffer::Buffer::new(None, "let value = 1;\nvalue\n".to_string());
        let unnamed_id = unnamed.id().as_u64();
        let named =
            crate::buffer::Buffer::new(Some("other.rs".into()), "fn other() {}\n".to_string());
        let mut editor = Editor::with_size(
            lsp,
            80,
            24,
            config,
            crate::theme::Theme::default(),
            vec![named, unnamed],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut screen = RenderBuffer::new(80, 24, &editor.theme.style);
        let mut runtime = crate::plugin::Runtime::new();
        let message = editor
            .explore_go(
                crate::explore::Jump {
                    label: "value".into(),
                    file: format!("buffer:{unnamed_id}"),
                    line: 1,
                    column: 0,
                },
                false,
                &mut screen,
                &mut runtime,
            )
            .await
            .unwrap();
        assert!(message.starts_with('→'), "{message}");
        assert_eq!(editor.current_buffer().id().as_u64(), unnamed_id);
        assert_eq!(editor.buffer_line(), 1);

        let message = editor
            .explore_go(
                crate::explore::Jump {
                    label: "gone".into(),
                    file: "buffer:999999".into(),
                    line: 0,
                    column: 0,
                },
                false,
                &mut screen,
                &mut runtime,
            )
            .await
            .unwrap();
        assert_eq!(message, "buffer 999999 is no longer open");
        assert_eq!(editor.current_buffer().id().as_u64(), unnamed_id);
        assert_eq!(editor.buffer_line(), 1);
    }

    #[tokio::test]
    async fn project_index_lets_the_editor_take_input_while_it_runs() {
        use std::sync::{Arc, Barrier};
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().unwrap();
        let body = "fn item() { let value = 1; value }\n".repeat(4_000);
        for index in 0..6 {
            std::fs::write(directory.path().join(format!("file{index}.rs")), &body).unwrap();
        }
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        let buffer = crate::buffer::Buffer::new(
            Some(directory.path().join("file0.rs").display().to_string()),
            "fn cursor() {\n    let value = 1;\n}\n".into(),
        );
        let mut editor = Editor::with_size(
            lsp,
            80,
            24,
            config,
            crate::theme::Theme::default(),
            vec![buffer],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut screen = RenderBuffer::new(80, 24, &editor.theme.style);
        let mut runtime = crate::plugin::Runtime::new();
        let barrier = Arc::new(Barrier::new(2));
        crate::explore::set_index_pause_for_test(Some(barrier.clone()));
        struct ClearPause;
        impl Drop for ClearPause {
            fn drop(&mut self) {
                crate::explore::set_index_pause_for_test(None);
            }
        }
        let _clear = ClearPause;

        let started = Instant::now();
        editor
            .execute_explore_command("index", &mut screen, &mut runtime)
            .await
            .unwrap();
        let blocked = started.elapsed();
        assert!(
            blocked < Duration::from_millis(200),
            "index command blocked the editor for {blocked:?}"
        );
        assert_eq!(editor.last_error.as_deref(), Some("Indexing project."));
        editor
            .execute_explore_command("index", &mut screen, &mut runtime)
            .await
            .unwrap();
        assert_eq!(
            editor.last_error.as_deref(),
            Some("An index is already running.")
        );
        editor
            .execute(&Action::MoveDown, &mut screen, &mut runtime)
            .await
            .unwrap();
        assert_eq!(editor.buffer_line(), 1);
        editor.explore.enabled = false;
        editor.explore.invalidate();
        editor
            .execute(&Action::DeleteBuffer(true), &mut screen, &mut runtime)
            .await
            .unwrap();
        barrier.wait();
        let deadline = Instant::now() + Duration::from_secs(30);
        while editor.explore_index.is_some() {
            assert!(Instant::now() < deadline, "index did not finish");
            editor.service_explore_background();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(editor.explore.index.file_count() >= 6);
        editor
            .execute(&Action::MoveDown, &mut screen, &mut runtime)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn large_buffer_reparse_does_not_block_the_next_edit() {
        use std::time::{Duration, Instant};

        let line = "fn item() { let value = 1; value }\n";
        let source = line.repeat((crate::explore::INLINE_PARSE_BYTES / line.len()) + 40);
        let mut editor = editor_with(&source);
        editor.explore.enabled = true;
        let key = editor.explore_file_key();
        let revision = editor.current_buffer().revision();
        let started = Instant::now();
        editor.sync_explore_buffer();
        let blocked = started.elapsed();
        assert!(
            blocked < Duration::from_millis(100),
            "reparse blocked the editor for {blocked:?}"
        );
        assert!(!editor.explore.is_current(&key, revision));
        editor
            .execute(
                &Action::MoveDown,
                &mut RenderBuffer::new(120, 36, &editor.theme.style),
                &mut crate::plugin::Runtime::new(),
            )
            .await
            .unwrap();
        assert_eq!(editor.buffer_line(), 1);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !editor.explore.is_current(&key, revision) {
            assert!(
                Instant::now() < deadline,
                "background reparse did not finish"
            );
            editor.service_explore_background();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            editor.explore.view(0, 3).in_function.as_deref(),
            Some("item")
        );
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
