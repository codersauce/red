//! Full-window learning hub and a nonmodal, space-reserving lesson coach.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    learn::{Lesson, PracticeStep, TRACKS},
    theme::{Style, SurfaceCardColors, SurfaceCardPalette, SurfacePalette, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{ActionBar, ActionBarRole, ActionPriority, Component, UiAction};

const WIDE_HUB: usize = 92;
const LIST_WIDTH: usize = 40;

#[derive(Clone, Copy)]
struct Region {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Region {
    fn right(self) -> usize {
        self.x + self.width
    }
    fn bottom(self) -> usize {
        self.y + self.height
    }
}

struct HubLayout {
    frame: Region,
    body_y: usize,
    body_bottom: usize,
    footer_y: usize,
    wide: bool,
}

impl HubLayout {
    fn new(width: usize, height: usize) -> Self {
        let frame_width = if width >= 96 {
            width.saturating_sub(4).min(128)
        } else {
            width
        };
        let frame_height = height.min(42);
        let frame = Region {
            x: (width - frame_width) / 2,
            y: (height - frame_height) / 2,
            width: frame_width,
            height: frame_height,
        };
        let footer_y = frame.bottom().saturating_sub(2);
        Self {
            frame,
            body_y: frame.y + if frame.height >= 18 { 8 } else { 4 },
            body_bottom: footer_y.saturating_sub(1),
            footer_y,
            wide: frame.width >= WIDE_HUB && frame.height >= 22,
        }
    }

    fn row_height(&self) -> usize {
        let available = self.body_bottom.saturating_sub(self.body_y);
        [4, 3, 2, 1]
            .into_iter()
            .find(|height| available >= TRACKS.len() * height)
            .unwrap_or(1)
    }

    fn visible_tracks(&self, selected: usize) -> (usize, usize) {
        let slots = (self.body_bottom.saturating_sub(self.body_y) / self.row_height()).max(1);
        (selected.saturating_sub(slots.saturating_sub(1)), slots)
    }

    fn tracks(&self) -> Region {
        Region {
            x: self.frame.x + 2,
            y: self.body_y,
            width: if self.wide {
                LIST_WIDTH - 2
            } else {
                self.frame.width.saturating_sub(4)
            },
            height: self.body_bottom.saturating_sub(self.body_y),
        }
    }
}

pub(crate) struct LearnHub {
    selected: usize,
    details: bool,
    completed: [bool; Lesson::AVAILABLE.len()],
    width: usize,
    height: usize,
    theme: Theme,
}

impl LearnHub {
    pub fn new(editor: &Editor, completed: [bool; Lesson::AVAILABLE.len()]) -> Self {
        Self {
            selected: 0,
            details: false,
            completed,
            width: editor.vwidth(),
            height: editor.inline_history_viewport_height().saturating_add(1),
            theme: editor.theme.clone(),
        }
    }

    fn layout(&self) -> HubLayout {
        HubLayout::new(self.width, self.height)
    }
    fn wide(&self) -> bool {
        self.layout().wide
    }

    fn next_lesson(&self) -> Lesson {
        Lesson::AVAILABLE
            .into_iter()
            .find(|lesson| !self.completed[lesson.index()])
            .unwrap_or_default()
    }

    fn actions(&self) -> Vec<UiAction> {
        let open = if !self.wide() && !self.details {
            "View track"
        } else if self.selected == 0 {
            if self.completed.iter().all(|completed| *completed) {
                "Replay lesson"
            } else if self.completed.iter().any(|completed| *completed) {
                "Continue lesson"
            } else {
                "Start lesson"
            }
        } else {
            "Track planned"
        };
        vec![
            UiAction::new("open", "Enter", open).with_priority(ActionPriority::Essential),
            UiAction::new(
                "close",
                "Esc",
                if self.details && !self.wide() {
                    "All tracks"
                } else {
                    "Start editing"
                },
            )
            .with_priority(ActionPriority::Essential),
            UiAction::new("next", "j/k", "Choose track").with_trigger("j"),
        ]
    }

    fn open_selected(&mut self) -> KeyAction {
        if !self.wide() && !self.details {
            self.details = true;
            KeyAction::Single(Action::Refresh)
        } else if TRACKS[self.selected].id == "essentials" {
            KeyAction::Single(Action::StartLearnLesson)
        } else {
            KeyAction::Single(Action::Refresh)
        }
    }

    fn move_selection(&mut self, direction: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(direction)
            .min(TRACKS.len() - 1);
    }

    fn draw_tracks(&self, buffer: &mut RenderBuffer, layout: &HubLayout, palette: &SurfacePalette) {
        let area = layout.tracks();
        let row_height = layout.row_height();
        let (first, slots) = layout.visible_tracks(self.selected);
        let card = SurfaceCardPalette::new(palette, true, SurfaceCardColors::default());
        for (index, track) in TRACKS.iter().enumerate().skip(first).take(slots) {
            let y = area.y + (index - first) * row_height;
            if y >= layout.body_bottom {
                break;
            }
            let active = index == self.selected;
            let framed = row_height >= 4;
            let content = if active { &card.content } else { palette };
            if active {
                let height = if row_height == 3 { 2 } else { row_height };
                draw_card(buffer, Region { y, height, ..area }, &card, &self.theme);
            }
            let title_y = y + usize::from(framed);
            put(
                buffer,
                area.x + 2,
                title_y,
                area.width.saturating_sub(4),
                &format!("{:02}  {}", index + 1, track.title),
                &Style {
                    bold: active,
                    ..content.primary.clone()
                },
            );
            if row_height > 1 {
                put(
                    buffer,
                    area.x + 6,
                    title_y + 1,
                    area.width.saturating_sub(8),
                    &format!("{} · {}", track.category, track.duration),
                    &content.secondary,
                );
            }
        }
    }

    fn draw_details(&self, buffer: &mut RenderBuffer, area: Region, palette: &SurfacePalette) {
        let track = &TRACKS[self.selected];
        let x = area.x;
        let width = area.width.min(70);
        let bottom = area.bottom();
        let mut y = area.y;
        put(
            buffer,
            x,
            y,
            width,
            &format!("{}  ·  {}", track.category, track.duration),
            &palette.accent,
        );
        y += 1;
        put(
            buffer,
            x,
            y,
            width,
            track.title,
            &Style {
                bold: true,
                ..palette.primary.clone()
            },
        );
        y += 2;
        y = wrapped(
            buffer,
            x,
            y,
            width,
            bottom,
            track.description,
            &palette.secondary,
        );
        if y + 4 < bottom {
            y += 1;
            put(buffer, x, y, width, "YOU WILL", &palette.accent);
            y = wrapped(
                buffer,
                x,
                y + 1,
                width,
                bottom,
                track.outcome,
                &palette.primary,
            );
        }
        if y + 4 < bottom {
            y += 1;
            put(buffer, x, y, width, "YOUR PATH", &palette.secondary);
            y += 1;
            for (index, lesson) in track.lessons.iter().enumerate() {
                if y >= bottom.saturating_sub(2) {
                    break;
                }
                let mark = if self.selected == 0 && self.completed.get(index) == Some(&true) {
                    "✓ ".to_string()
                } else {
                    format!("{:02}", index + 1)
                };
                put(
                    buffer,
                    x,
                    y,
                    width,
                    &format!("{mark}  {lesson}"),
                    &palette.primary,
                );
                y += 1;
            }
        }
        if y + 1 < bottom {
            let status = if self.selected == 0 {
                if self.completed.iter().all(|completed| *completed) {
                    "✓ Available lessons complete · Enter to replay".to_string()
                } else if self.completed.iter().any(|completed| *completed) {
                    format!("Enter  Continue: {} →", self.next_lesson().title())
                } else {
                    "Enter  Start the first lesson →".to_string()
                }
            } else {
                "Planned · more lessons are on the way".to_string()
            };
            if y + 3 < bottom {
                horizontal_rule(buffer, x, y + 1, width, &palette.divider);
                y += 2;
            }
            put(
                buffer,
                x,
                y + 1,
                width,
                &status,
                if self.selected == 0 {
                    &palette.accent
                } else {
                    &palette.secondary
                },
            );
        }
    }
}

impl Component for LearnHub {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        let height = buffer.height.saturating_sub(1);
        if buffer.width < 4 || height < 4 {
            return Ok(());
        }
        let layout = HubLayout::new(buffer.width, height);
        let frame = layout.frame;
        let palette = SurfacePalette::new(&self.theme, &self.theme.ui_style.dialog);
        buffer.fill_rect(
            0,
            0,
            buffer.width,
            height,
            ' ',
            &self.theme.style,
            &self.theme,
        );
        buffer.fill_rect(
            frame.x,
            frame.y,
            frame.width,
            frame.height,
            ' ',
            &palette.surface,
            &self.theme,
        );
        draw_frame(buffer, frame, &palette.divider);
        put(
            buffer,
            frame.x + 3,
            frame.y + 1,
            frame.width.saturating_sub(6),
            "red ●   │   Learn",
            &palette.primary,
        );
        if frame.height >= 18 {
            put(
                buffer,
                frame.x + 3,
                frame.y + 3,
                frame.width.saturating_sub(6),
                "What would you like to do?",
                &Style {
                    bold: true,
                    ..palette.primary.clone()
                },
            );
            put(
                buffer,
                frame.x + 3,
                frame.y + 4,
                frame.width.saturating_sub(6),
                "Pick a path. Learn by doing. Come back whenever you like.",
                &palette.secondary,
            );
        }
        horizontal_rule(
            buffer,
            frame.x + 1,
            layout.body_y.saturating_sub(2),
            frame.width.saturating_sub(2),
            &palette.divider,
        );
        if layout.wide {
            self.draw_tracks(buffer, &layout, &palette);
            let divider_x = frame.x + LIST_WIDTH + 1;
            for y in layout.body_y.saturating_sub(1)..layout.body_bottom {
                put(buffer, divider_x, y, 1, "│", &palette.divider);
            }
            self.draw_details(
                buffer,
                Region {
                    x: divider_x + 3,
                    y: layout.body_y,
                    width: frame.right().saturating_sub(divider_x + 6),
                    height: layout.body_bottom.saturating_sub(layout.body_y),
                },
                &palette,
            );
        } else if self.details {
            self.draw_details(
                buffer,
                Region {
                    x: frame.x + 3,
                    y: layout.body_y,
                    width: frame.width.saturating_sub(6),
                    height: layout.body_bottom.saturating_sub(layout.body_y),
                },
                &palette,
            );
        } else {
            self.draw_tracks(buffer, &layout, &palette);
        }
        horizontal_rule(
            buffer,
            frame.x + 1,
            layout.footer_y.saturating_sub(1),
            frame.width.saturating_sub(2),
            &palette.divider,
        );
        let actions = self.actions();
        let mut x = frame.x + 3;
        // Keep the shared footer's clickable help region, then apply Learn's
        // higher-contrast text roles to the same layout.
        let footer = ActionBar::new(&actions)
            .with_context(self.shortcut_context())
            .render(
                buffer,
                x,
                layout.footer_y,
                frame.width.saturating_sub(6),
                &self.theme,
                &palette.surface,
            );
        for span in footer.spans {
            let style = match span.role {
                ActionBarRole::Key | ActionBarRole::Overflow | ActionBarRole::Mode => {
                    &palette.accent
                }
                ActionBarRole::Separator => &palette.divider,
                ActionBarRole::Label | ActionBarRole::Status => &palette.secondary,
            };
            put(
                buffer,
                x,
                layout.footer_y,
                frame.right().saturating_sub(x + 1),
                &span.text,
                style,
            );
            x += display_width(&span.text);
        }
        Ok(())
    }

    fn uses_full_editor_viewport(&self) -> bool {
        true
    }
    fn shortcut_context(&self) -> &str {
        "Learn Red"
    }
    fn surface_actions(&self) -> Vec<UiAction> {
        self.actions()
    }
    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }
    fn resize(&mut self, width: usize, height: usize) -> bool {
        self.width = width;
        self.height = height.saturating_add(1);
        true
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        match event {
            Event::Key(key)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        if self.details && !self.wide() {
                            self.details = false;
                            Some(KeyAction::Single(Action::Refresh))
                        } else {
                            Some(KeyAction::Single(Action::CloseDialog))
                        }
                    }
                    KeyCode::Enter => Some(self.open_selected()),
                    KeyCode::Down | KeyCode::Char('j') => {
                        self.move_selection(1);
                        Some(KeyAction::Single(Action::Refresh))
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.move_selection(-1);
                        Some(KeyAction::Single(Action::Refresh))
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        self.details = true;
                        Some(KeyAction::Single(Action::Refresh))
                    }
                    KeyCode::Left | KeyCode::BackTab => {
                        self.details = false;
                        Some(KeyAction::Single(Action::Refresh))
                    }
                    KeyCode::Char(c @ '1'..='6') => {
                        self.selected = c as usize - '1' as usize;
                        Some(KeyAction::Single(Action::Refresh))
                    }
                    _ => None,
                }
            }
            Event::Mouse(mouse)
                if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                ) =>
            {
                self.move_selection(if mouse.kind == MouseEventKind::ScrollDown {
                    1
                } else {
                    -1
                });
                Some(KeyAction::Single(Action::Refresh))
            }
            Event::Mouse(mouse)
                if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && (!self.details || self.wide()) =>
            {
                let x = usize::from(mouse.column);
                let y = usize::from(mouse.row);
                let layout = self.layout();
                let area = layout.tracks();
                if x >= area.x && x < area.right() && y >= area.y && y < area.bottom() {
                    let (first, slots) = layout.visible_tracks(self.selected);
                    let row = (y - area.y) / layout.row_height();
                    let index = first + row;
                    if row < slots && index < TRACKS.len() {
                        self.selected = index;
                        return Some(KeyAction::Single(Action::Refresh));
                    }
                }
                None
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CoachLayout {
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl CoachLayout {
    pub const fn new(width: usize, height: usize) -> Self {
        if width >= 104 && height >= 18 {
            Self {
                top: 2,
                right: 40,
                bottom: 0,
            }
        } else if height >= 14 {
            Self {
                top: 1,
                right: 0,
                bottom: 7,
            }
        } else if height >= 8 {
            Self {
                top: 1,
                right: 0,
                bottom: 3,
            }
        } else {
            Self {
                top: 0,
                right: 0,
                bottom: 0,
            }
        }
    }
}

pub(crate) fn draw_learn_coach(
    buffer: &mut RenderBuffer,
    theme: &Theme,
    lesson: Lesson,
    step: PracticeStep,
    shortcut: Option<&str>,
) {
    let layout = CoachLayout::new(buffer.width, buffer.height);
    let palette = SurfacePalette::new(theme, &theme.ui_style.dialog);
    if layout.top > 0 {
        buffer.fill_rect(0, 0, buffer.width, layout.top, ' ', &palette.surface, theme);
        put(
            buffer,
            2,
            0,
            buffer.width.saturating_sub(4),
            "red ●   Learn / Essentials   ·   Practice buffer",
            &palette.primary,
        );
        if layout.top > 1 {
            horizontal_rule(buffer, 0, 1, buffer.width, &palette.divider);
        }
    }
    let area = if layout.right > 0 {
        Region {
            x: buffer.width - layout.right,
            y: layout.top,
            width: layout.right,
            height: buffer.height.saturating_sub(layout.top + 2),
        }
    } else if layout.bottom > 0 {
        Region {
            x: 0,
            y: buffer.height.saturating_sub(layout.bottom + 2),
            width: buffer.width,
            height: layout.bottom,
        }
    } else {
        return;
    };
    buffer.fill_rect(
        area.x,
        area.y,
        area.width,
        area.height,
        ' ',
        &palette.surface,
        theme,
    );
    let left = area.x + 2;
    let text_width = area.width.saturating_sub(4);
    let instruction = step.instruction(lesson, shortcut);
    let exit = if step == PracticeStep::Complete {
        lesson.next().map_or_else(
            || ":tutorial next  →  All tracks".to_string(),
            |next| format!(":tutorial next  →  Lesson {}", next.index() + 1),
        )
    } else {
        ":tutorial quit  ·  :tutorial restart".to_string()
    };
    if area.height <= 3 {
        horizontal_rule(buffer, area.x, area.y, area.width, &palette.divider);
        put(
            buffer,
            left,
            area.y + 1,
            text_width,
            &instruction,
            &palette.primary,
        );
        put(
            buffer,
            left,
            area.y + 2,
            text_width,
            &exit,
            &palette.secondary,
        );
        return;
    }
    draw_frame(buffer, area, &palette.divider);
    let bottom = area.bottom().saturating_sub(1);
    let footer_y = bottom.saturating_sub(1);
    if layout.right == 0 {
        put(
            buffer,
            left,
            area.y + 1,
            text_width,
            &format!("ESSENTIALS  ·  {}", lesson.title()),
            &palette.accent,
        );
        wrapped(
            buffer,
            left,
            area.y + 2,
            text_width,
            footer_y,
            &instruction,
            &palette.primary,
        );
        put(
            buffer,
            left,
            footer_y,
            text_width,
            &exit,
            &palette.secondary,
        );
        return;
    }
    put(
        buffer,
        left,
        area.y + 1,
        text_width,
        &format!(
            "{:02} / {:02}  ·  ESSENTIALS",
            lesson.index() + 1,
            TRACKS[0].lessons.len()
        ),
        &palette.accent,
    );
    put(
        buffer,
        left,
        area.y + 2,
        text_width,
        lesson.title(),
        &Style {
            bold: true,
            ..palette.primary.clone()
        },
    );
    horizontal_rule(buffer, left, area.y + 4, text_width, &palette.divider);
    let next = wrapped(
        buffer,
        left,
        area.y + 6,
        text_width,
        footer_y.saturating_sub(1),
        &instruction,
        &palette.primary,
    );
    if next + 7 < footer_y {
        put(
            buffer,
            left,
            next + 2,
            text_width,
            &format!("CHECKPOINT  ·  {}/4", step.completed_steps()),
            &palette.secondary,
        );
        for (index, text) in lesson.checkpoints().iter().enumerate() {
            let completed = index < step.completed_steps();
            let active = index == step.completed_steps();
            let mark = if completed {
                "✓"
            } else if active {
                "›"
            } else {
                "○"
            };
            put(
                buffer,
                left,
                next + 3 + index,
                text_width,
                &format!("{mark} {text}"),
                if active {
                    &palette.accent
                } else if completed {
                    &palette.primary
                } else {
                    &palette.secondary
                },
            );
        }
    }
    horizontal_rule(
        buffer,
        left,
        footer_y.saturating_sub(1),
        text_width,
        &palette.divider,
    );
    put(buffer, left, footer_y, text_width, &exit, &palette.accent);
}

fn horizontal_rule(buffer: &mut RenderBuffer, x: usize, y: usize, width: usize, style: &Style) {
    put(buffer, x, y, width, &"─".repeat(width), style);
}

fn draw_frame(buffer: &mut RenderBuffer, area: Region, style: &Style) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let last_x = area.right() - 1;
    let last_y = area.bottom() - 1;
    horizontal_rule(buffer, area.x + 1, area.y, area.width - 2, style);
    horizontal_rule(buffer, area.x + 1, last_y, area.width - 2, style);
    for y in area.y + 1..last_y {
        put(buffer, area.x, y, 1, "│", style);
        put(buffer, last_x, y, 1, "│", style);
    }
    for (x, y, glyph) in [
        (area.x, area.y, "╭"),
        (last_x, area.y, "╮"),
        (area.x, last_y, "╰"),
        (last_x, last_y, "╯"),
    ] {
        put(buffer, x, y, 1, glyph, style);
    }
}

/// Uses the same caps and continuous rails as selected agent-scrollback prompts.
fn draw_card(buffer: &mut RenderBuffer, area: Region, card: &SurfaceCardPalette, theme: &Theme) {
    if area.width < 2 || area.height == 0 {
        return;
    }
    buffer.fill_rect(
        area.x + 1,
        area.y,
        area.width - 2,
        area.height,
        ' ',
        &card.content.surface,
        theme,
    );
    for y in area.y..area.bottom() {
        let cap = if area.height >= 4 && y == area.y {
            Some(("▄", "╷"))
        } else if area.height >= 4 && y + 1 == area.bottom() {
            Some(("▀", "╵"))
        } else {
            None
        };
        let rail = if let Some((cap, rail)) = cap {
            put(
                buffer,
                area.x + 1,
                y,
                area.width - 2,
                &cap.repeat(area.width - 2),
                &card.cap,
            );
            rail
        } else {
            "│"
        };
        put(buffer, area.x, y, 1, rail, &card.edge);
        put(buffer, area.right() - 1, y, 1, rail, &card.edge);
    }
}

fn put(buffer: &mut RenderBuffer, x: usize, y: usize, width: usize, text: &str, style: &Style) {
    if y < buffer.height && x < buffer.width {
        buffer.set_text(
            x,
            y,
            &truncate_display_width(text, width.min(buffer.width - x)),
            style,
        );
    }
}

fn wrapped(
    buffer: &mut RenderBuffer,
    x: usize,
    mut y: usize,
    width: usize,
    bottom: usize,
    text: &str,
    style: &Style,
) -> usize {
    if width == 0 {
        return y;
    }
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && display_width(&line) + 1 + display_width(word) > width {
            if y >= bottom {
                return y;
            }
            put(buffer, x, y, width, &line, style);
            y += 1;
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() && y < bottom {
        put(buffer, x, y, width, &line, style);
        y += 1;
    }
    y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::contrast_ratio;
    use crossterm::event::{KeyEvent, MouseEvent};

    fn text_rows(buffer: &RenderBuffer) -> Vec<String> {
        buffer
            .cells
            .chunks(buffer.width)
            .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
            .collect()
    }

    fn assert_readable_text(buffer: &RenderBuffer, context: &str) {
        for cell in &buffer.cells {
            if cell.text.chars().any(char::is_alphanumeric) {
                assert!(
                    contrast_ratio(cell.style.fg.unwrap(), cell.style.bg.unwrap()) >= 4.5,
                    "unreadable {:?} in {context}: {:?}",
                    cell.text,
                    cell.style
                );
            }
        }
    }

    fn panel(width: usize, height: usize) -> LearnHub {
        LearnHub {
            selected: 0,
            details: false,
            completed: [false; Lesson::AVAILABLE.len()],
            width,
            height: height - 1,
            theme: Theme::default(),
        }
    }

    #[test]
    fn narrow_hub_opens_details_before_starting() {
        let mut hub = panel(80, 24);
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            hub.handle_event(&enter),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(
            hub.handle_event(&enter),
            Some(KeyAction::Single(Action::StartLearnLesson))
        );
    }

    #[test]
    fn planned_tracks_cannot_start_a_fake_lesson() {
        let mut hub = panel(120, 32);
        hub.selected = 1;
        assert_eq!(hub.open_selected(), KeyAction::Single(Action::Refresh));
    }

    #[test]
    fn learn_footer_registers_contextual_shortcut_help() {
        let mut hub = panel(120, 32);
        let mut buffer = RenderBuffer::new(120, 32, &hub.theme.style);
        hub.draw(&mut buffer).unwrap();
        let help = buffer.shortcut_help_regions.last().unwrap();
        assert_eq!(help.context, "Learn Red");
        assert_eq!(help.rect.y, hub.layout().footer_y);
        assert!(help.actions.iter().any(|action| action.id == "open"));
        assert_eq!(
            hub.activate_surface_action("open"),
            Some(KeyAction::Single(Action::StartLearnLesson))
        );
    }

    #[test]
    fn hub_and_coach_render_at_small_and_large_sizes() {
        for (width, height) in [(24, 8), (60, 18), (80, 24), (120, 32)] {
            let mut buffer = RenderBuffer::new(width, height, &Style::default());
            panel(width, height).draw(&mut buffer).unwrap();
            draw_learn_coach(
                &mut buffer,
                &Theme::default(),
                Lesson::FindYourFooting,
                PracticeStep::Insert,
                Some("i"),
            );
        }
    }

    #[test]
    fn learn_surfaces_keep_text_readable_and_backgrounds_consistent() {
        for path in [
            "themes/kanso.json",
            "themes/tokyonight-storm.json",
            "themes/night-owl-light.json",
            "themes/community-material-theme-lighter-high-contrast.json",
        ] {
            let theme = crate::theme::parse_vscode_theme(path).unwrap();
            let palette = SurfacePalette::new(&theme, &theme.ui_style.dialog);
            let card = SurfaceCardPalette::new(&palette, true, SurfaceCardColors::default());
            for (width, height) in [(32, 12), (80, 24), (120, 32), (160, 44)] {
                let mut hub = panel(width, height);
                hub.theme = theme.clone();
                let mut buffer = RenderBuffer::new(width, height, &theme.style);
                hub.draw(&mut buffer).unwrap();
                assert_readable_text(&buffer, path);
                assert!(
                    buffer.cells.iter().all(|cell| [
                        theme.style.bg,
                        palette.surface.bg,
                        card.content.surface.bg
                    ]
                    .contains(&cell.style.bg)),
                    "stray hub background in {path}"
                );
                let frame = hub.layout().frame;
                assert_eq!(buffer.cells[frame.y * width + frame.x].text, "╭");
                assert_eq!(
                    buffer.cells[(frame.bottom() - 1) * width + frame.right() - 1].text,
                    "╯"
                );

                let mut buffer = RenderBuffer::new(width, height, &theme.style);
                draw_learn_coach(
                    &mut buffer,
                    &theme,
                    Lesson::FindYourFooting,
                    PracticeStep::Complete,
                    None,
                );
                assert_readable_text(&buffer, path);
                assert!(
                    buffer
                        .cells
                        .iter()
                        .all(|cell| [theme.style.bg, palette.surface.bg].contains(&cell.style.bg)),
                    "stray coach background in {path}"
                );
            }
        }
    }

    #[test]
    fn roomy_hub_reuses_prompt_caps_and_keeps_action_near_outline() {
        let hub = panel(160, 44);
        let mut buffer = RenderBuffer::new(160, 44, &hub.theme.style);
        hub.draw(&mut buffer).unwrap();
        let layout = hub.layout();
        let area = layout.tracks();
        assert_eq!(buffer.cells[area.y * buffer.width + area.x].text, "╷");
        assert_eq!(buffer.cells[area.y * buffer.width + area.x + 1].text, "▄");
        assert_eq!(buffer.cells[(area.y + 3) * buffer.width + area.x].text, "╵");
        let rows = text_rows(&buffer);
        let action_y = rows
            .iter()
            .position(|row| row.contains("Start the first lesson"))
            .unwrap();
        assert!(action_y < layout.footer_y - 2);
        assert!(rows.iter().any(|row| row.contains("Make Red yours")));
    }

    #[test]
    fn clicking_a_scrolled_track_uses_the_visible_index() {
        let mut hub = panel(32, 12);
        hub.selected = 5;
        let layout = hub.layout();
        let area = layout.tracks();
        let (first, _) = layout.visible_tracks(hub.selected);
        assert!(first > 0);
        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x as u16,
            row: area.y as u16,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            hub.handle_event(&click),
            Some(KeyAction::Single(Action::Refresh))
        );
        assert_eq!(hub.selected, first);
    }
}
