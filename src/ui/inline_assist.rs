//! Cursor-anchored prompt and result controls for bounded inline code edits.

use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, Mode, RenderBuffer},
    keyboard::is_word_backspace,
    text_layout::{LayoutOptions, TextLayout},
    theme::{Style, Theme},
    unicode_utils::{grapheme_len, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    first_prompt_line,
    geometry::{anchored_popup_geometry, anchored_popup_geometry_avoiding_rows},
    spinner_frame, wrap_text, ActionBar, ActionPriority, Component, OverlayLayout, PromptBuffer,
    ScreenRect, UiAction, SPINNER_FRAME_INTERVAL_MS,
};

const MAX_WIDTH: usize = 72;
const MAX_PROMPT_ROWS: usize = 6;
const MAX_ERROR_ROWS: usize = 4;
const CLOSE_CHOICES: [&str; 3] = ["Delete", "Edit", "Save draft"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineAssistPopupState {
    Prompt { initial: String, refining: bool },
    Working,
    Ready { stale: bool },
    WiderReady { stale: bool, summary: String },
    AnswerRetained(String),
    Applied { edited: bool, comments: usize },
    NeedsAgent(String),
    Declined(String),
    Failed(String),
}

pub struct InlineAssistPopup {
    state: InlineAssistPopupState,
    prompt: PromptBuffer,
    title: String,
    close_choice: Option<usize>,
    navigation: Option<(uuid::Uuid, usize, usize)>,
    layout: OverlayLayout,
    dialog: Dialog,
    style: Style,
    theme: Theme,
    spinner_started: Instant,
    spinner_frame: u64,
}

impl InlineAssistPopup {
    fn draw_actions(&self, buffer: &mut RenderBuffer, x: usize, y: usize, width: usize) {
        ActionBar::new(&self.surface_actions()).render(
            buffer,
            x,
            y,
            width,
            &self.theme,
            &self.style,
        );
    }
    pub fn new(editor: &Editor, scope: impl Into<String>, state: InlineAssistPopupState) -> Self {
        let local_anchor = editor.cursor_position();
        let anchor = editor.render_cursor_position().unwrap_or(local_anchor);
        let viewport_y_offset = anchor.1.saturating_sub(local_anchor.1);
        Self::new_in_layout(
            editor,
            scope,
            state,
            OverlayLayout {
                viewport: ScreenRect {
                    x: 0,
                    y: 0,
                    width: editor.vwidth(),
                    height: editor.vheight().saturating_add(viewport_y_offset),
                },
                anchor,
                avoid_rows: None,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn new_avoiding_rows(
        editor: &Editor,
        scope: impl Into<String>,
        state: InlineAssistPopupState,
        avoid_rows: Option<(usize, usize)>,
    ) -> Self {
        let local_anchor = editor.cursor_position();
        let anchor = editor.render_cursor_position().unwrap_or(local_anchor);
        let viewport_y_offset = anchor.1.saturating_sub(local_anchor.1);
        Self::new_in_layout(
            editor,
            scope,
            state,
            OverlayLayout {
                viewport: ScreenRect {
                    x: 0,
                    y: 0,
                    width: editor.vwidth(),
                    height: editor.vheight().saturating_add(viewport_y_offset),
                },
                anchor,
                avoid_rows,
            },
        )
    }

    pub(crate) fn new_in_layout(
        editor: &Editor,
        scope: impl Into<String>,
        state: InlineAssistPopupState,
        layout: OverlayLayout,
    ) -> Self {
        let scope = scope.into();
        let initial = match &state {
            InlineAssistPopupState::Prompt { initial, .. } => initial.clone(),
            _ => String::new(),
        };
        let prompt = PromptBuffer::with_history(&initial, editor.inline_prompt_history());
        let navigation = (!matches!(state, InlineAssistPopupState::Prompt { .. }))
            .then(|| editor.current_inline_navigation())
            .flatten();
        let width = Self::content_width(layout.viewport.width);
        let desired_height = Self::content_height(&state, &prompt, width);
        let (x, y, height) = Self::geometry(layout, width, desired_height);
        let style = editor.theme.ui_style.dialog.clone();
        let title = navigation.map_or_else(
            || format!("Inline assist · {scope}"),
            |(_, ordinal, count)| format!("Inline assist · {scope} · inline {ordinal} of {count}"),
        );
        let dialog = Dialog::new(
            Some(title.clone()),
            x,
            y,
            width,
            height,
            &style,
            BorderStyle::Rounded,
            &editor.theme,
        )
        .with_surface_theme(&editor.theme, SurfaceRole::Dialog);
        Self {
            state,
            prompt,
            title,
            close_choice: None,
            navigation,
            layout,
            dialog,
            style,
            theme: editor.theme.clone(),
            spinner_started: Instant::now(),
            spinner_frame: 0,
        }
    }

    fn content_width(viewport_width: usize) -> usize {
        viewport_width.saturating_sub(2).min(MAX_WIDTH)
    }

    fn prompt_inset(width: usize) -> usize {
        if width >= 3 {
            2
        } else {
            0
        }
    }

    fn prompt_layout_options(width: usize) -> LayoutOptions {
        LayoutOptions::word(width.saturating_sub(Self::prompt_inset(width)))
    }

    fn content_height(
        state: &InlineAssistPopupState,
        prompt: &PromptBuffer,
        width: usize,
    ) -> usize {
        match state {
            InlineAssistPopupState::Prompt { .. } => prompt
                .layout(Self::prompt_layout_options(width))
                .rows()
                .len()
                .clamp(1, MAX_PROMPT_ROWS)
                .saturating_add(1),
            InlineAssistPopupState::Working => 2,
            InlineAssistPopupState::Ready { .. } => 3,
            InlineAssistPopupState::Applied { .. } => 2,
            InlineAssistPopupState::AnswerRetained(message)
            | InlineAssistPopupState::WiderReady {
                summary: message, ..
            }
            | InlineAssistPopupState::NeedsAgent(message)
            | InlineAssistPopupState::Declined(message)
            | InlineAssistPopupState::Failed(message) => wrap_text(message, width.max(1))
                .rows
                .len()
                .clamp(1, MAX_ERROR_ROWS)
                .saturating_add(2),
        }
    }

    fn geometry(layout: OverlayLayout, width: usize, height: usize) -> (usize, usize, usize) {
        let viewport = layout.viewport;
        let anchor = (
            layout
                .anchor
                .0
                .saturating_sub(viewport.x)
                .min(viewport.width.saturating_sub(1)),
            layout
                .anchor
                .1
                .saturating_sub(viewport.y)
                .min(viewport.height.saturating_sub(1)),
        );
        let avoid_rows = layout.avoid_rows.and_then(|(start, end)| {
            let viewport_end = viewport.y.saturating_add(viewport.height.saturating_sub(1));
            let start = start.max(viewport.y);
            let end = end.min(viewport_end);
            (start <= end).then_some((
                start.saturating_sub(viewport.y),
                end.saturating_sub(viewport.y),
            ))
        });
        let (x, y, available_height) = avoid_rows.map_or_else(
            || anchored_popup_geometry(anchor, viewport.width, viewport.height, width, height),
            |avoid_rows| {
                anchored_popup_geometry_avoiding_rows(
                    anchor,
                    avoid_rows,
                    viewport.width,
                    viewport.height,
                    width,
                    height,
                )
            },
        );
        // A whole function may fill the viewport. Keep the popup usable even
        // when it is impossible to avoid every line in the edit scope.
        let (x, y, height) = if available_height < height.min(2) {
            anchored_popup_geometry(anchor, viewport.width, viewport.height, width, height)
        } else {
            (x, y, available_height)
        };
        (
            viewport.x.saturating_add(x),
            viewport.y.saturating_add(y),
            height,
        )
    }

    fn insert(&mut self, text: &str) {
        self.prompt.insert(&first_prompt_line(text));
    }

    fn refresh_action() -> Option<KeyAction> {
        Some(KeyAction::Single(Action::Refresh))
    }

    fn reflow(&mut self) {
        let width = Self::content_width(self.layout.viewport.width);
        let desired_height = if self.close_choice.is_some() {
            5
        } else {
            Self::content_height(&self.state, &self.prompt, width)
        };
        let (x, y, height) = Self::geometry(self.layout, width, desired_height);
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
        self.dialog.set_title(Some(if self.close_choice.is_some() {
            "Unsent inline prompt".into()
        } else {
            self.title.clone()
        }));
    }

    fn close_choice_rows(&self) -> (usize, usize, usize) {
        let intro = usize::from(self.dialog.height >= 4);
        let help = usize::from(self.dialog.height >= 5);
        let count = self
            .dialog
            .height
            .saturating_sub(intro + help)
            .min(CLOSE_CHOICES.len());
        let scroll = self
            .close_choice
            .unwrap_or(0)
            .saturating_sub(count.saturating_sub(1));
        (self.dialog.y + 1 + intro, count, scroll)
    }

    fn choose_close_action(&mut self, choice: usize) -> Option<KeyAction> {
        match choice {
            0 => Some(KeyAction::Single(Action::DiscardInlineAssistDraft)),
            1 => {
                self.close_choice = None;
                self.prompt_changed()
            }
            2 => Some(KeyAction::Single(Action::SaveInlineAssistDraft)),
            _ => None,
        }
    }

    fn handle_close_choice(&mut self, event: &Event) -> Option<KeyAction> {
        if let Event::Mouse(mouse) = event {
            let (top, count, scroll) = self.close_choice_rows();
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && (self.dialog.x + 1..self.dialog.x + 1 + self.dialog.width)
                    .contains(&usize::from(mouse.column))
                && (top..top + count).contains(&usize::from(mouse.row))
            {
                return self.choose_close_action(scroll + usize::from(mouse.row) - top);
            }
            return None;
        }
        let Event::Key(key) = event else {
            return None;
        };
        match key.code {
            KeyCode::Char('d') if key.modifiers.is_empty() => self.choose_close_action(0),
            KeyCode::Esc | KeyCode::Char('e') => self.choose_close_action(1),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.choose_close_action(1)
            }
            KeyCode::Char('s') if key.modifiers.is_empty() => self.choose_close_action(2),
            KeyCode::Enter => self.choose_close_action(self.close_choice.unwrap_or(0)),
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                self.close_choice = Some((self.close_choice.unwrap_or(0) + 2) % 3);
                Self::refresh_action()
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                self.close_choice = Some((self.close_choice.unwrap_or(0) + 1) % 3);
                Self::refresh_action()
            }
            _ => None,
        }
    }

    fn draw_close_choice(&self, buffer: &mut RenderBuffer) {
        let x = self.dialog.x + 1;
        let width = self.dialog.width;
        if self.dialog.height >= 4 {
            buffer.set_text(
                x,
                self.dialog.y + 1,
                &truncate_display_width("What should happen to this draft?", width),
                &self.style,
            );
        }
        let selected_style = self.theme.selected_style(
            &self.style,
            &self.theme.ui_style.picker_selected_item,
            crate::theme::SelectionForegroundPriority::Selection,
        );
        let (top, count, scroll) = self.close_choice_rows();
        for (offset, label) in CLOSE_CHOICES.iter().enumerate().skip(scroll).take(count) {
            let selected = self.close_choice == Some(offset);
            let style = if selected {
                &selected_style
            } else {
                &self.style
            };
            let label = format!("{} {label}", if selected { "›" } else { " " });
            let label = crate::unicode_utils::fit_display_width(&label, width);
            buffer.set_text(x, top + offset - scroll, &label, style);
        }
        if self.dialog.height >= 5 {
            self.draw_actions(buffer, x, self.dialog.y + self.dialog.height, width);
        }
    }

    fn prompt_changed(&mut self) -> Option<KeyAction> {
        self.reflow();
        Self::refresh_action()
    }

    fn prompt_layout(&self) -> TextLayout {
        self.prompt
            .layout(Self::prompt_layout_options(self.dialog.width))
    }

    fn prompt_body_height(&self) -> usize {
        self.dialog
            .height
            .saturating_sub(usize::from(self.dialog.height > 1))
    }

    fn prompt_scroll(&self, layout: &TextLayout) -> usize {
        layout
            .position(self.prompt.cursor())
            .map_or(0, |position| position.row)
            .saturating_sub(self.prompt_body_height().saturating_sub(1))
    }

    fn place_prompt_cursor(&mut self, column: usize, row: usize) -> Option<KeyAction> {
        let left = self.dialog.x + 1;
        let top = self.dialog.y + 1;
        if !(left..left + self.dialog.width).contains(&column)
            || !(top..top + self.prompt_body_height()).contains(&row)
        {
            return None;
        }
        let layout = self.prompt_layout();
        let row = self.prompt_scroll(&layout) + row - top;
        let column = column.saturating_sub(left + Self::prompt_inset(self.dialog.width));
        let offset = layout.nearest_offset_on_row(row, column)?;
        self.prompt.set_cursor(offset);
        Self::refresh_action()
    }

    fn inside(&self, column: usize, row: usize) -> bool {
        (self.dialog.x
            ..self
                .dialog
                .x
                .saturating_add(self.dialog.width)
                .saturating_add(2))
            .contains(&column)
            && (self.dialog.y
                ..self
                    .dialog
                    .y
                    .saturating_add(self.dialog.height)
                    .saturating_add(2))
                .contains(&row)
    }

    fn advance_spinner(&mut self, now: Instant) -> bool {
        if !matches!(self.state, InlineAssistPopupState::Working) {
            return false;
        }
        let frame = now
            .saturating_duration_since(self.spinner_started)
            .as_millis() as u64
            / SPINNER_FRAME_INTERVAL_MS;
        if frame == self.spinner_frame {
            return false;
        }
        self.spinner_frame = frame;
        true
    }
}

impl Component for InlineAssistPopup {
    fn is_inline_draft_confirmation(&self) -> bool {
        self.close_choice.is_some()
    }

    fn request_inline_assist_close(&mut self) -> Option<Action> {
        if !matches!(self.state, InlineAssistPopupState::Prompt { .. }) {
            return None;
        }
        if self.prompt.text().trim().is_empty() {
            return Some(Action::DiscardInlineAssistDraft);
        }
        self.close_choice = Some(0);
        self.reflow();
        Some(Action::Refresh)
    }

    fn inline_assist_state(&self) -> Option<InlineAssistPopupState> {
        Some(match &self.state {
            InlineAssistPopupState::Prompt { refining, .. } => InlineAssistPopupState::Prompt {
                initial: self.prompt.text(),
                refining: *refining,
            },
            state => state.clone(),
        })
    }

    fn surface_actions(&self) -> Vec<UiAction> {
        let essential =
            |id, key, label| UiAction::new(id, key, label).with_priority(ActionPriority::Essential);
        if self.close_choice.is_some() {
            return vec![
                essential("choose", "Enter", "choose"),
                UiAction::new("delete", "d", "delete"),
                UiAction::new("edit", "e", "edit"),
                UiAction::new("save", "s", "save draft"),
                UiAction::new("cancel", "Esc", "edit"),
            ];
        }
        let mut actions = match &self.state {
            InlineAssistPopupState::Prompt { .. } => vec![
                essential("apply", "Enter", "ask"),
                essential("close", "Esc", "close"),
                UiAction::new("move", "↑↓", "move through prompt"),
                UiAction::new("previous-prompt", "Ctrl-p", "previous prompt")
                    .with_enabled(!self.prompt.history().is_empty()),
                UiAction::new("next-prompt", "Ctrl-n", "next prompt")
                    .with_enabled(!self.prompt.history().is_empty()),
                UiAction::new("cancel", "Ctrl-c", "close"),
            ],
            InlineAssistPopupState::Working => vec![
                essential("hide", "Esc", "hide"),
                UiAction::new("cancel", "Ctrl-c", "cancel request"),
                UiAction::new("history", "H", "history"),
            ],
            InlineAssistPopupState::Ready { .. } => {
                let mut actions = vec![essential("view", "Enter", "review diff")];
                actions.extend([
                    UiAction::new("discard", "d", "decline"),
                    UiAction::new("refine", "r", "recheck"),
                    UiAction::new("agent", "A", "Agent"),
                    essential("hide", "Esc", "hide"),
                ]);
                actions
            }
            InlineAssistPopupState::WiderReady { stale, .. } => vec![
                essential(
                    "review",
                    "Enter",
                    if *stale {
                        "view stale diff"
                    } else {
                        "review wider diff"
                    },
                ),
                UiAction::new("discard", "d", "decline"),
                UiAction::new("refine", "r", "recheck"),
                UiAction::new("agent", "A", "Agent"),
                essential("hide", "Esc", "hide"),
            ],
            InlineAssistPopupState::AnswerRetained(_) => vec![
                essential("view", "v", "full answer"),
                UiAction::new("refine", "r", "recheck"),
                UiAction::new("agent", "A", "Agent"),
                essential("hide", "Esc", "hide"),
            ],
            InlineAssistPopupState::Applied { edited, .. } => vec![
                essential("close", "Esc", "close"),
                UiAction::new(
                    "view",
                    "v",
                    if *edited {
                        "view changes"
                    } else {
                        "full answer"
                    },
                ),
                UiAction::new("pin", "p", "pin annotations"),
                UiAction::new("undo", "u", if *edited { "undo" } else { "dismiss" }),
                UiAction::new("refine", "r", "refine"),
                UiAction::new("agent", "A", "agent"),
            ],
            InlineAssistPopupState::NeedsAgent(_) => vec![
                essential("agent", "A", "continue in Agent"),
                UiAction::new("view", "v", "full answer"),
                UiAction::new("pin", "p", "pin annotations"),
                UiAction::new("refine", "r", "refine"),
                essential("close", "Esc", "close"),
            ],
            InlineAssistPopupState::Declined(_) | InlineAssistPopupState::Failed(_) => vec![
                essential("retry", "r", "retry/refine"),
                UiAction::new("view", "v", "view result"),
                UiAction::new("agent", "A", "Agent"),
                essential("hide", "Esc", "hide"),
            ],
        };
        if self.navigation.is_some() {
            actions.push(UiAction::new("previous-inline", "[", "previous inline"));
            actions.push(UiAction::new("next-inline", "]", "next inline"));
        }
        actions
    }
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        if self.close_choice.is_some() {
            self.draw_close_choice(buffer);
            return Ok(());
        }
        let x = self.dialog.x.saturating_add(1);
        let y = self.dialog.y.saturating_add(1);
        let width = self.dialog.width;
        match &self.state {
            InlineAssistPopupState::Prompt { .. } => {
                let show_help = self.dialog.height > 1;
                let body_height = self.prompt_body_height();
                if body_height > 0 {
                    let layout = self.prompt_layout();
                    let scroll = self.prompt_scroll(&layout);
                    let inset = Self::prompt_inset(width);
                    for (offset, row) in layout
                        .rows()
                        .iter()
                        .skip(scroll)
                        .take(body_height)
                        .enumerate()
                    {
                        let marker = if scroll.saturating_add(offset) == 0 {
                            ">"
                        } else {
                            "│"
                        };
                        if inset > 0 {
                            buffer.set_text(x, y.saturating_add(offset), marker, &self.style);
                        }
                        buffer.set_text(
                            x.saturating_add(inset),
                            y.saturating_add(offset),
                            &row.text,
                            &self.style,
                        );
                    }
                }
                if show_help {
                    self.draw_actions(
                        buffer,
                        x,
                        y.saturating_add(self.dialog.height.saturating_sub(1)),
                        width,
                    );
                }
            }
            InlineAssistPopupState::Working => {
                let message = format!(
                    "{} Preparing inline result…",
                    spinner_frame(self.spinner_frame.saturating_mul(SPINNER_FRAME_INTERVAL_MS))
                );
                if self.dialog.height > 0 {
                    buffer.set_text(x, y, &truncate_display_width(&message, width), &self.style);
                }
                if self.dialog.height > 1 {
                    self.draw_actions(buffer, x, y.saturating_add(1), width);
                }
            }
            InlineAssistPopupState::Ready { stale } => {
                let message = if *stale {
                    "Source changed · result retained"
                } else {
                    "Result ready · not applied"
                };
                if self.dialog.height > 0 {
                    buffer.set_text(x, y, &truncate_display_width(message, width), &self.style);
                }
                if self.dialog.height > 1 {
                    self.draw_actions(buffer, x, y.saturating_add(self.dialog.height - 1), width);
                }
            }
            InlineAssistPopupState::Applied { edited, comments } => {
                let message = match (*edited, *comments) {
                    (true, 0) => "Applied to buffer (unsaved)".to_string(),
                    (true, count) => format!("Applied unsaved edit · {count} comment(s)"),
                    (false, 0) => "No changes or comments needed".to_string(),
                    (false, count) => format!("Added {count} inline comment(s) · code unchanged"),
                };
                if self.dialog.height > 0 {
                    buffer.set_text(x, y, &truncate_display_width(&message, width), &self.style);
                }
                if self.dialog.height > 1 {
                    self.draw_actions(buffer, x, y.saturating_add(1), width);
                }
            }
            InlineAssistPopupState::AnswerRetained(message)
            | InlineAssistPopupState::WiderReady {
                summary: message, ..
            }
            | InlineAssistPopupState::NeedsAgent(message)
            | InlineAssistPopupState::Declined(message)
            | InlineAssistPopupState::Failed(message) => {
                if self.dialog.height > 0 {
                    buffer.set_text(
                        x,
                        y,
                        &truncate_display_width(
                            match self.state {
                                InlineAssistPopupState::AnswerRetained(_) => "Answer retained",
                                InlineAssistPopupState::WiderReady { stale: true, .. } => {
                                    "Source changed · review only"
                                }
                                InlineAssistPopupState::WiderReady { .. } => {
                                    "Review required · source unchanged"
                                }
                                InlineAssistPopupState::NeedsAgent(_) => "Needs a broader edit",
                                InlineAssistPopupState::Declined(_) => {
                                    "Edit declined · source unchanged"
                                }
                                _ => "Inline assist failed",
                            },
                            width,
                        ),
                        &self.style,
                    );
                }
                let message_height = self.dialog.height.saturating_sub(2);
                for (offset, row) in wrap_text(message, width.max(1))
                    .rows
                    .iter()
                    .take(message_height)
                    .enumerate()
                {
                    buffer.set_text(x, y.saturating_add(1 + offset), row, &self.style);
                }
                if self.dialog.height > 1 {
                    self.draw_actions(
                        buffer,
                        x,
                        y.saturating_add(self.dialog.height.saturating_sub(1)),
                        width,
                    );
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> anyhow::Result<bool> {
        Ok(self.advance_spinner(Instant::now()))
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        self.layout.viewport.width = viewport_width;
        self.layout.viewport.height = viewport_height;
        self.reflow();
        true
    }

    fn update_overlay_layout(&mut self, layout: OverlayLayout) -> bool {
        self.layout = layout;
        self.reflow();
        true
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.style = theme.ui_style.dialog.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
        self.theme = theme.clone();
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        if matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release) {
            return None;
        }
        if self.close_choice.is_some() {
            return self.handle_close_choice(event);
        }
        if let (Some((id, _, _)), Event::Key(key)) = (self.navigation, event) {
            if matches!(key.code, KeyCode::Char('[' | ']')) && key.modifiers.is_empty() {
                return Some(KeyAction::Single(
                    Action::NavigateOverlappingInlineComment {
                        id,
                        backwards: key.code == KeyCode::Char('['),
                        open: true,
                    },
                ));
            }
        }
        if let Event::Mouse(mouse) = event {
            if matches!(mouse.kind, MouseEventKind::Down(_))
                && !self.inside(mouse.column as usize, mouse.row as usize)
            {
                return Some(KeyAction::Single(Action::HideInlineAssist));
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && matches!(self.state, InlineAssistPopupState::Prompt { .. })
            {
                return self.place_prompt_cursor(mouse.column as usize, mouse.row as usize);
            }
            return None;
        }
        match &self.state {
            InlineAssistPopupState::Prompt { .. } => match event {
                Event::Paste(text) => {
                    self.insert(text);
                    self.prompt_changed()
                }
                Event::Key(key) => match (key.code, key.modifiers) {
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        Some(KeyAction::Single(Action::HideInlineAssist))
                    }
                    (KeyCode::Esc, _) => Some(KeyAction::Single(Action::HideInlineAssist)),
                    (KeyCode::Enter, _) => {
                        let prompt = self.prompt.text().trim().to_string();
                        (!prompt.is_empty())
                            .then_some(KeyAction::Single(Action::SubmitInlineAssist(prompt)))
                    }
                    (KeyCode::Up | KeyCode::Down, _) => {
                        self.prompt.handle_event_with_layout_options(
                            event,
                            Self::prompt_layout_options(self.dialog.width),
                        );
                        Self::refresh_action()
                    }
                    (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                        self.prompt.history_previous();
                        self.prompt_changed()
                    }
                    (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                        self.prompt.history_next();
                        self.prompt_changed()
                    }
                    (KeyCode::Left, _) => {
                        self.prompt
                            .set_cursor(self.prompt.cursor().saturating_sub(1));
                        Self::refresh_action()
                    }
                    (KeyCode::Right, _) => {
                        self.prompt
                            .set_cursor(self.prompt.cursor().saturating_add(1));
                        Self::refresh_action()
                    }
                    (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                        self.prompt.set_cursor(0);
                        Self::refresh_action()
                    }
                    (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                        self.prompt.set_cursor(grapheme_len(&self.prompt.text()));
                        Self::refresh_action()
                    }
                    (KeyCode::Backspace, _) => {
                        if is_word_backspace(*key) {
                            self.prompt.delete_previous_word();
                        } else {
                            self.prompt.backspace();
                        }
                        self.prompt_changed()
                    }
                    (KeyCode::Delete, _) => {
                        self.prompt.delete();
                        self.prompt_changed()
                    }
                    (KeyCode::Char(character), modifiers)
                        if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.insert(&character.to_string());
                        self.prompt_changed()
                    }
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::Working => match event {
                Event::Key(key) => match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => Some(KeyAction::Single(Action::HideInlineAssist)),
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        Some(KeyAction::Single(Action::CancelInlineAssist))
                    }
                    (KeyCode::Char('H'), _) => Some(KeyAction::Single(Action::OpenInlineHistory)),
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::Ready { .. } => match event {
                Event::Key(key) => match key.code {
                    KeyCode::Enter | KeyCode::Char('v') => {
                        Some(KeyAction::Single(Action::ViewInlineAssistAnswer))
                    }
                    KeyCode::Char('d') => {
                        Some(KeyAction::Single(Action::RejectPendingInlineAssist))
                    }
                    KeyCode::Char('r') => Some(KeyAction::Single(Action::RefineInlineAssist)),
                    KeyCode::Char('A') => Some(KeyAction::Single(Action::EscalateInlineAssist)),
                    KeyCode::Esc => Some(KeyAction::Single(Action::HideInlineAssist)),
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::WiderReady { .. } => match event {
                Event::Key(key) => match key.code {
                    KeyCode::Enter | KeyCode::Char('v') => {
                        Some(KeyAction::Single(Action::ViewInlineAssistAnswer))
                    }
                    KeyCode::Char('d') => {
                        Some(KeyAction::Single(Action::RejectPendingInlineAssist))
                    }
                    KeyCode::Char('r') => Some(KeyAction::Single(Action::RefineInlineAssist)),
                    KeyCode::Char('A') => Some(KeyAction::Single(Action::EscalateInlineAssist)),
                    KeyCode::Esc => Some(KeyAction::Single(Action::HideInlineAssist)),
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::Applied { .. } => match event {
                Event::Key(key) => match key.code {
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char('k') => {
                        Some(KeyAction::Single(Action::KeepInlineAssist))
                    }
                    KeyCode::Char('u') => Some(KeyAction::Single(Action::UndoInlineAssist)),
                    KeyCode::Char('r') => Some(KeyAction::Single(Action::RefineInlineAssist)),
                    KeyCode::Char('v') => Some(KeyAction::Single(Action::ViewInlineAssistAnswer)),
                    KeyCode::Char('p' | 's') => {
                        Some(KeyAction::Single(Action::RestoreInlineAssistAnnotations))
                    }
                    KeyCode::Char('A') => Some(KeyAction::Single(Action::EscalateInlineAssist)),
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::NeedsAgent(_) => match event {
                Event::Key(key) => match key.code {
                    KeyCode::Char('A') | KeyCode::Enter => {
                        Some(KeyAction::Single(Action::EscalateInlineAssist))
                    }
                    KeyCode::Char('v') => Some(KeyAction::Single(Action::ViewInlineAssistAnswer)),
                    KeyCode::Char('p' | 's') => {
                        Some(KeyAction::Single(Action::RestoreInlineAssistAnnotations))
                    }
                    KeyCode::Char('r') => Some(KeyAction::Single(Action::RefineInlineAssist)),
                    KeyCode::Esc | KeyCode::Char('q') => {
                        Some(KeyAction::Single(Action::KeepInlineAssist))
                    }
                    _ => None,
                },
                _ => None,
            },
            InlineAssistPopupState::AnswerRetained(_)
            | InlineAssistPopupState::Declined(_)
            | InlineAssistPopupState::Failed(_) => match event {
                Event::Key(key) => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        Some(KeyAction::Single(Action::HideInlineAssist))
                    }
                    KeyCode::Enter | KeyCode::Char('r') => {
                        Some(KeyAction::Single(Action::RefineInlineAssist))
                    }
                    KeyCode::Char('v') => Some(KeyAction::Single(Action::ViewInlineAssistAnswer)),
                    KeyCode::Char('A') => Some(KeyAction::Single(Action::EscalateInlineAssist)),
                    _ => None,
                },
                _ => None,
            },
        }
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        if self.close_choice.is_some()
            || !matches!(self.state, InlineAssistPopupState::Prompt { .. })
        {
            return None;
        }
        if self.prompt_body_height() == 0 || self.dialog.width == 0 {
            return None;
        }
        let layout = self.prompt_layout();
        let position = layout.position(self.prompt.cursor())?;
        let scroll = self.prompt_scroll(&layout);
        Some((
            self.dialog
                .x
                .saturating_add(1 + Self::prompt_inset(self.dialog.width))
                .saturating_add(position.column)
                .min(
                    self.layout
                        .viewport
                        .x
                        .saturating_add(self.layout.viewport.width.saturating_sub(1)),
                ),
            self.dialog
                .y
                .saturating_add(1)
                .saturating_add(position.row.saturating_sub(scroll))
                .min(
                    self.layout
                        .viewport
                        .y
                        .saturating_add(self.layout.viewport.height.saturating_sub(1)),
                ),
        ))
    }

    fn cursor_mode(&self) -> Option<Mode> {
        (self.close_choice.is_none() && matches!(self.state, InlineAssistPopupState::Prompt { .. }))
            .then_some(Mode::Insert)
    }

    fn is_sensitive_input(&self) -> bool {
        matches!(self.state, InlineAssistPopupState::Prompt { .. })
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyModifiers};

    use super::*;
    use crate::{buffer::Buffer, config::Config, lsp::LspManager};

    fn editor() -> Editor {
        let config = Config::default();
        Editor::with_size(
            Box::new(LspManager::new(config.lsp.clone())),
            60,
            14,
            config,
            Theme::default(),
            vec![Buffer::new(None, "fn main() {}\n".to_string())],
        )
        .unwrap()
    }

    #[test]
    fn prompt_submits_bounded_action_and_cancel_is_explicit() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new(
            &editor,
            "line 1",
            InlineAssistPopupState::Prompt {
                initial: String::new(),
                refining: false,
            },
        );
        assert!(popup.is_sensitive_input());
        popup.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        assert_eq!(
            popup.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Some(KeyAction::Single(Action::SubmitInlineAssist(
                "x".to_string()
            )))
        );
        assert_eq!(
            popup.handle_event(&Event::Key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)
            )),
            Some(KeyAction::Single(Action::HideInlineAssist))
        );
        assert_eq!(
            popup.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            ))),
            Some(KeyAction::Single(Action::HideInlineAssist))
        );
    }

    #[test]
    fn inline_prompt_close_defaults_to_delete_and_edit_preserves_the_prompt_editor() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new(
            &editor,
            "line 1",
            InlineAssistPopupState::Prompt {
                initial: "draft".into(),
                refining: false,
            },
        );
        popup.prompt.set_cursor(2);
        popup.prompt.insert("!");
        let text = popup.prompt.text();
        let cursor = popup.prompt.cursor();
        assert_eq!(popup.request_inline_assist_close(), Some(Action::Refresh));
        assert_eq!(popup.close_choice, Some(0));
        assert!(popup.cursor_position().is_none());
        assert_eq!(
            popup.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))),
            Some(KeyAction::Single(Action::DiscardInlineAssistDraft))
        );
        assert_eq!(
            popup.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('e'),
                KeyModifiers::NONE
            ))),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(popup.prompt.text(), text);
        assert_eq!(popup.prompt.cursor(), cursor);
        assert!(popup.prompt.undo());
        assert_eq!(popup.prompt.text(), "draft");
        popup.request_inline_assist_close();
        popup.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::BackTab,
            KeyModifiers::SHIFT,
        )));
        assert_eq!(popup.close_choice, Some(2));
        assert_eq!(
            popup.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))),
            Some(KeyAction::Single(Action::SaveInlineAssistDraft))
        );
        let (top, count, scroll) = popup.close_choice_rows();
        assert_eq!((count, scroll), (3, 0));
        assert_eq!(
            popup.handle_event(&Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: (popup.dialog.x + 1) as u16,
                row: (top + 1) as u16,
                modifiers: KeyModifiers::NONE,
            })),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert!(popup.close_choice.is_none());
        popup.prompt.set_text("  ");
        assert_eq!(
            popup.request_inline_assist_close(),
            Some(Action::DiscardInlineAssistDraft)
        );
        assert!(popup.close_choice.is_none());
        popup.state = InlineAssistPopupState::Working;
        assert_eq!(popup.request_inline_assist_close(), None);
    }

    #[test]
    fn inline_popup_spinner_uses_elapsed_time_not_number_of_ticks() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new(&editor, "line 1", InlineAssistPopupState::Working);
        let since = popup.spinner_started;
        let interval = std::time::Duration::from_millis(SPINNER_FRAME_INTERVAL_MS);
        assert!(!popup.advance_spinner(since + interval - std::time::Duration::from_millis(1)));
        assert!(popup.advance_spinner(since + interval));
        assert_eq!(popup.spinner_frame, 1);
        assert!(!popup.advance_spinner(since + interval));
        assert!(popup.advance_spinner(since + interval * 3));
        assert_eq!(popup.spinner_frame, 3);
        popup.state = InlineAssistPopupState::Ready { stale: false };
        assert!(!popup.advance_spinner(since + interval * 4));
    }

    #[test]
    fn inline_prompt_recall_preserves_the_unsent_draft() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new(
            &editor,
            "line 1",
            InlineAssistPopupState::Prompt {
                initial: "unsent draft".into(),
                refining: false,
            },
        );
        popup.prompt =
            PromptBuffer::with_history("unsent draft", vec!["newest".into(), "older".into()]);
        for (key, modifiers, expected) in [
            (KeyCode::Char('p'), KeyModifiers::CONTROL, "newest"),
            (KeyCode::Char('p'), KeyModifiers::CONTROL, "older"),
            (KeyCode::Char('n'), KeyModifiers::CONTROL, "newest"),
            (KeyCode::Char('n'), KeyModifiers::CONTROL, "unsent draft"),
        ] {
            assert_eq!(
                popup.handle_event(&Event::Key(KeyEvent::new(key, modifiers))),
                Some(KeyAction::Single(Action::Refresh))
            );
            assert_eq!(popup.prompt.text(), expected);
        }
        popup.handle_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(
            popup.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))),
            Some(KeyAction::Single(Action::SubmitInlineAssist(
                "newest".into()
            )))
        );
    }

    #[test]
    fn applied_state_exposes_keep_undo_refine_and_escalate() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new(
            &editor,
            "selection",
            InlineAssistPopupState::Applied {
                edited: true,
                comments: 0,
            },
        );
        for (key, action) in [
            (KeyCode::Enter, Action::KeepInlineAssist),
            (KeyCode::Char('u'), Action::UndoInlineAssist),
            (KeyCode::Char('r'), Action::RefineInlineAssist),
            (KeyCode::Char('v'), Action::ViewInlineAssistAnswer),
            (KeyCode::Char('A'), Action::EscalateInlineAssist),
        ] {
            assert_eq!(
                popup.handle_event(&Event::Key(KeyEvent::new(key, KeyModifiers::NONE))),
                Some(KeyAction::Single(action))
            );
        }
    }

    #[test]
    fn word_backspace_edits_inline_assist_without_submitting() {
        let editor = editor();
        for modifiers in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let mut popup = InlineAssistPopup::new(
                &editor,
                "line 1",
                InlineAssistPopupState::Prompt {
                    initial: "first second".into(),
                    refining: false,
                },
            );
            assert_eq!(
                popup.handle_event(&Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Backspace,
                    modifiers,
                    KeyEventKind::Release,
                ))),
                None
            );
            assert_eq!(popup.prompt.text(), "first second");
            assert_eq!(
                popup.handle_event(&Event::Key(KeyEvent::new(KeyCode::Backspace, modifiers,))),
                Some(KeyAction::Single(Action::Refresh))
            );
            assert_eq!(popup.prompt.text(), "first ");
        }
    }

    #[test]
    fn broader_scope_has_actionable_controls_and_large_targets_keep_the_popup_usable() {
        let editor = editor();
        let mut popup = InlineAssistPopup::new_avoiding_rows(
            &editor,
            "function",
            InlineAssistPopupState::NeedsAgent("Update another function.".into()),
            Some((0, editor.vheight().saturating_sub(1))),
        );
        assert!(popup.dialog.height >= 2);
        for (key, action) in [
            (KeyCode::Enter, Action::EscalateInlineAssist),
            (KeyCode::Char('v'), Action::ViewInlineAssistAnswer),
            (KeyCode::Esc, Action::KeepInlineAssist),
        ] {
            assert_eq!(
                popup.handle_event(&Event::Key(KeyEvent::new(key, KeyModifiers::NONE))),
                Some(KeyAction::Single(action))
            );
        }
    }

    #[test]
    fn popup_avoids_the_rendered_target_rows() {
        let editor = editor();
        let avoid_rows = (4, 7);
        let popup = InlineAssistPopup::new_avoiding_rows(
            &editor,
            "lines 5–8 selection",
            InlineAssistPopupState::Applied {
                edited: true,
                comments: 0,
            },
            Some(avoid_rows),
        );
        let popup_last_row = popup
            .dialog
            .y
            .saturating_add(popup.dialog.height)
            .saturating_add(1);

        assert!(popup_last_row < avoid_rows.0 || popup.dialog.y > avoid_rows.1);
    }

    #[test]
    fn prompt_soft_wraps_grows_and_stays_inside_its_window() {
        let editor = editor();
        let viewport = ScreenRect {
            x: 30,
            y: 2,
            width: 30,
            height: 12,
        };
        let avoid_rows = (7, 7);
        let mut popup = InlineAssistPopup::new_in_layout(
            &editor,
            "line 8",
            InlineAssistPopupState::Prompt {
                initial: String::new(),
                refining: false,
            },
            OverlayLayout {
                viewport,
                anchor: (40, 7),
                avoid_rows: Some(avoid_rows),
            },
        );
        let initial_height = popup.dialog.height;

        popup.handle_event(&Event::Paste(format!(
            "{}TAIL",
            "expand this request ".repeat(12)
        )));

        let popup_last_column = popup
            .dialog
            .x
            .saturating_add(popup.dialog.width)
            .saturating_add(1);
        let popup_last_row = popup
            .dialog
            .y
            .saturating_add(popup.dialog.height)
            .saturating_add(1);
        assert!(popup.dialog.height > initial_height);
        assert!(popup.dialog.height <= MAX_PROMPT_ROWS + 1);
        assert!(popup.dialog.x >= viewport.x);
        assert!(popup_last_column < viewport.x + viewport.width);
        assert!(popup.dialog.y >= viewport.y);
        assert!(popup_last_row < viewport.y + viewport.height);
        assert!(popup_last_row < avoid_rows.0 || popup.dialog.y > avoid_rows.1);
        let cursor = popup.cursor_position().unwrap();
        assert!((viewport.x..viewport.x + viewport.width).contains(&cursor.0));
        assert!((viewport.y..viewport.y + viewport.height).contains(&cursor.1));
        let mut buffer = RenderBuffer::new(60, 14, &Style::default());
        popup.draw(&mut buffer).unwrap();
        let rendered = buffer.cells.iter().map(|cell| cell.c).collect::<String>();
        assert!(rendered.contains("TAIL"));
    }

    fn prompt_in_viewport(text: &str, width: usize) -> InlineAssistPopup {
        let viewport = ScreenRect {
            x: 7,
            y: 2,
            width,
            height: 20,
        };
        InlineAssistPopup::new_in_layout(
            &editor(),
            "line 1",
            InlineAssistPopupState::Prompt {
                initial: text.into(),
                refining: false,
            },
            OverlayLayout {
                viewport,
                anchor: (7, 3),
                avoid_rows: None,
            },
        )
    }

    fn prompt_key(popup: &mut InlineAssistPopup, code: KeyCode) -> Option<KeyAction> {
        popup.handle_event(&Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn prompt_click(popup: &mut InlineAssistPopup, x: usize, y: usize) -> Option<KeyAction> {
        popup.handle_event(&Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x as u16,
            row: y as u16,
            modifiers: KeyModifiers::NONE,
        }))
    }

    #[test]
    fn inline_prompt_word_wrap_render_navigation_and_click_share_source_positions() {
        let mut popup = prompt_in_viewport("one two three", 12);
        popup.prompt.set_cursor(0);
        let rows = popup
            .prompt_layout()
            .rows()
            .iter()
            .map(|row| row.text.clone())
            .collect::<Vec<_>>();
        assert_eq!(rows, ["one two", "three"]);
        let x = popup.dialog.x + 3;
        let y = popup.dialog.y + 1;
        let mut frame = RenderBuffer::new(100, 30, &Style::default());
        popup.draw(&mut frame).unwrap();
        for (offset, expected) in rows.iter().enumerate() {
            let start = (y + offset) * frame.width + x;
            let actual = frame.cells[start..start + expected.len()]
                .iter()
                .map(|cell| cell.c)
                .collect::<String>();
            assert_eq!(&actual, expected);
        }
        assert_eq!(popup.cursor_position(), Some((x, y)));
        prompt_key(&mut popup, KeyCode::Down);
        assert_eq!(popup.prompt.cursor(), 8);
        assert_eq!(popup.cursor_position(), Some((x, y + 1)));
        prompt_click(&mut popup, x + 2, y + 1);
        assert_eq!(popup.prompt.cursor(), 10);
        popup.handle_event(&Event::Paste("X".into()));
        assert_eq!(popup.prompt.text(), "one two thXree");
        assert_eq!(
            prompt_key(&mut popup, KeyCode::Enter),
            Some(KeyAction::Single(Action::SubmitInlineAssist(
                "one two thXree".into()
            )))
        );
        assert!(popup.prompt.undo());
        assert_eq!(popup.prompt.text(), "one two three");
    }

    #[test]
    fn inline_prompt_vertical_motion_keeps_the_preferred_column_and_draft() {
        let text = "abcdef\nx\nabcdef";
        let mut popup = prompt_in_viewport(text, 12);
        popup.prompt = PromptBuffer::with_history(text, vec!["older prompt".into()]);
        popup.prompt.set_cursor(5);
        for (key, offset) in [
            (KeyCode::Down, 8),
            (KeyCode::Down, 14),
            (KeyCode::Up, 8),
            (KeyCode::Up, 5),
            (KeyCode::Up, 5),
        ] {
            prompt_key(&mut popup, key);
            assert_eq!(popup.prompt.cursor(), offset);
            assert_eq!(popup.prompt.text(), text);
        }
    }

    #[test]
    fn inline_prompt_reflow_and_scrolled_clicks_preserve_unicode_and_source() {
        let text = "one   two\n\n  漢👩‍💻e\u{301}\tend  abcdefghijklmnop ".repeat(3);
        let mut popup = prompt_in_viewport(&text, 30);
        let revision = popup.prompt.buffer().revision();
        for width in [3, 4, 5, 9, 12, 20, 30, 72] {
            popup.resize(width, 20);
            for offset in [0, grapheme_len(&text) / 2, grapheme_len(&text)] {
                popup.prompt.set_cursor(offset);
                let layout = popup.prompt_layout();
                let options = InlineAssistPopup::prompt_layout_options(popup.dialog.width);
                assert_eq!(layout, TextLayout::new(&text, options));
                assert!(layout
                    .rows()
                    .iter()
                    .all(|row| crate::unicode_utils::display_width(&row.text) <= options.width));
                let (cursor_x, cursor_y) = popup.cursor_position().unwrap();
                assert!(
                    (popup.dialog.x + 1..popup.dialog.x + 1 + popup.dialog.width)
                        .contains(&cursor_x)
                );
                assert!(
                    (popup.dialog.y + 1..popup.dialog.y + 1 + popup.prompt_body_height())
                        .contains(&cursor_y)
                );
                let first = popup.prompt_scroll(&layout);
                let expected = layout.nearest_offset_on_row(first, 0).unwrap();
                let x = popup.dialog.x + 1 + InlineAssistPopup::prompt_inset(popup.dialog.width);
                let y = popup.dialog.y + 1;
                prompt_click(&mut popup, x, y);
                assert_eq!(popup.prompt.cursor(), expected);
                assert_eq!(popup.prompt.text(), text);
                assert_eq!(popup.prompt.buffer().revision(), revision);
            }
        }
        let x = popup.dialog.x + 1;
        let footer_y = popup.dialog.y + popup.dialog.height;
        let cursor = popup.prompt.cursor();
        assert_eq!(prompt_click(&mut popup, x, footer_y), None);
        assert_eq!(popup.prompt.cursor(), cursor);
        assert_eq!(
            prompt_click(&mut popup, 0, 0),
            Some(KeyAction::Single(Action::HideInlineAssist))
        );
    }

    #[test]
    fn applied_popup_uses_the_owning_split_coordinates() {
        let editor = editor();
        let viewport = ScreenRect {
            x: 42,
            y: 1,
            width: 18,
            height: 10,
        };
        let mut popup = InlineAssistPopup::new_in_layout(
            &editor,
            "line 4",
            InlineAssistPopupState::Applied {
                edited: true,
                comments: 0,
            },
            OverlayLayout {
                viewport,
                anchor: (48, 4),
                avoid_rows: Some((4, 4)),
            },
        );

        assert!(popup.dialog.x >= viewport.x);
        assert!(
            popup
                .dialog
                .x
                .saturating_add(popup.dialog.width)
                .saturating_add(2)
                <= viewport.x + viewport.width
        );

        let resized_viewport = ScreenRect {
            x: 24,
            y: 2,
            width: 14,
            height: 8,
        };
        assert!(popup.update_overlay_layout(OverlayLayout {
            viewport: resized_viewport,
            anchor: (28, 4),
            avoid_rows: Some((4, 4)),
        }));
        assert!(popup.dialog.x >= resized_viewport.x);
        assert!(
            popup
                .dialog
                .x
                .saturating_add(popup.dialog.width)
                .saturating_add(2)
                <= resized_viewport.x + resized_viewport.width
        );
    }
}
