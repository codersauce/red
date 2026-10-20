//! Branded first-run choices and safe, editor-owned tutorial previews.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseEventKind};

use crate::{
    config::KeyAction,
    editor::{Action, Editor, RenderBuffer},
    plugin::markdown::{render_markdown_lines, RenderedTextLine, TextPanelSpanStyle},
    splash::{self, Role},
    theme::{SelectionForegroundPriority, Style, Theme},
    tutorial::{TutorialController, TutorialTrack},
    unicode_utils::{display_width, truncate_display_width},
};

use super::{
    dialog::{BorderStyle, Dialog, SurfaceRole},
    paint_rich_text, ActionPriority, Component, UiAction,
};

const MAX_WELCOME_WIDTH: usize = 98;
const MAX_WELCOME_HEIGHT: usize = 25;
const MIN_WELCOME_WIDTH: usize = 36;
const MIN_WELCOME_HEIGHT: usize = 12;
const WIDE_CARDS_WIDTH: usize = 72;
const COACH_HEIGHT: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WelcomePage {
    Home,
    ReleaseHighlights,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WelcomeCard {
    GuidedTour,
    ReleaseHighlights,
}

/// Keyboard-first welcome that enters the editor before requesting any setup.
pub struct WelcomePanel {
    dialog: Dialog,
    page: WelcomePage,
    selected: WelcomeCard,
    release_lines: Vec<RenderedTextLine>,
    release_scroll: usize,
    can_resume: bool,
    theme: Theme,
}

impl WelcomePanel {
    /// Compact welcome screens still require a readable body and close action.
    #[must_use]
    pub const fn fits(width: usize, height: usize) -> bool {
        width >= MIN_WELCOME_WIDTH && height >= MIN_WELCOME_HEIGHT
    }

    /// Builds a responsive welcome over the current theme and viewport.
    #[must_use]
    pub fn new(editor: &Editor, can_resume: bool) -> Self {
        let (x, y, width, height) = geometry(editor.vwidth(), editor.vheight());
        let theme = editor.theme.clone();
        let style = theme.ui_style.dialog.clone();
        let dialog = Dialog::new(
            Some(format!("WELCOME TO RED · v{}", env!("CARGO_PKG_VERSION"))),
            x,
            y,
            width,
            height,
            &style,
            BorderStyle::Rounded,
            &theme,
        )
        .with_surface_theme(&theme, SurfaceRole::Dialog);

        let mut panel = Self {
            dialog,
            page: WelcomePage::Home,
            selected: WelcomeCard::GuidedTour,
            release_lines: Vec::new(),
            release_scroll: 0,
            can_resume,
            theme,
        };
        panel.reflow();
        panel
    }

    fn release_body_height(&self) -> usize {
        self.dialog.height.saturating_sub(6)
    }

    fn reflow(&mut self) {
        let markdown = embedded_release_notes(env!("CARGO_PKG_VERSION"));
        self.release_lines = render_markdown_lines(&markdown, self.dialog.width.saturating_sub(6));
        self.release_scroll = self.release_scroll.min(
            self.release_lines
                .len()
                .saturating_sub(self.release_body_height()),
        );
        self.update_actions();
    }

    fn update_actions(&mut self) {
        let actions = match self.page {
            WelcomePage::Home => {
                let mut actions = vec![
                    UiAction::new("select", "Enter", "Choose")
                        .with_priority(ActionPriority::Essential),
                    UiAction::new("dismiss", "Esc", "Start editing")
                        .with_priority(ActionPriority::Essential),
                    UiAction::new("quick", "i", "Quick tour"),
                    UiAction::new("notes", "v", "What’s new"),
                    UiAction::new("config", "c", "Create config")
                        .with_priority(ActionPriority::Secondary),
                ];
                if self.can_resume {
                    actions.push(UiAction::new("resume", "r", "Resume tour"));
                }
                actions
            }
            WelcomePage::ReleaseHighlights => vec![
                UiAction::new("back", "Esc", "Back").with_priority(ActionPriority::Essential),
                UiAction::new("tour", "t", "Guided tour").with_priority(ActionPriority::Essential),
                UiAction::new("scroll", "j/k", "Scroll"),
                UiAction::new("release", "o", "Full release notes"),
            ],
        };
        self.dialog.set_actions(actions);
    }

    fn center(&self, buffer: &mut RenderBuffer, row: usize, text: &str, style: &Style) {
        if row >= self.dialog.height.saturating_sub(1) {
            return;
        }
        let width = self.dialog.width.saturating_sub(4);
        let text = truncate_display_width(text, width);
        let x = self.dialog.x + 1 + self.dialog.width.saturating_sub(display_width(&text)) / 2;
        buffer.set_text(x, self.dialog.y + 1 + row, &text, style);
    }

    fn draw_brand(&self, buffer: &mut RenderBuffer, row: usize) {
        let palette = splash::palette(&self.theme);
        let mark = "red";
        let width = display_width(mark) + 2;
        let x = self.dialog.x + 1 + self.dialog.width.saturating_sub(width) / 2;
        let y = self.dialog.y + 1 + row;
        buffer.set_text(x, y, mark, palette.style(Role::Mark));
        buffer.set_text(
            x + display_width(mark) + 1,
            y,
            "●",
            palette.style(Role::Dot),
        );
    }

    fn draw_card(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        card: WelcomeCard,
    ) {
        if width < 14 || y + 4 >= self.dialog.y + self.dialog.height {
            return;
        }
        let selected = self.selected == card;
        let selected_style = self.theme.selected_style(
            &self.theme.ui_style.dialog,
            &self.theme.ui_style.picker_selected_item,
            SelectionForegroundPriority::Selection,
        );
        let surface = if selected {
            selected_style
        } else {
            self.theme.ui_style.dialog.clone()
        };
        let palette = splash::palette(&self.theme);
        let border = if selected {
            palette.style(Role::Key).with_bg(surface.bg)
        } else {
            self.theme.ui_style.dialog_border.clone()
        };
        let title = match card {
            WelcomeCard::GuidedTour => "Take the guided tour",
            WelcomeCard::ReleaseHighlights => "What’s new",
        };
        let lines: [&str; 2] = match card {
            WelcomeCard::GuidedTour => ["Learn Red by using it.", "Editing · Git · safe agents"],
            WelcomeCard::ReleaseHighlights => [
                "Discover release highlights.",
                "Immediate and offline-ready",
            ],
        };

        buffer.fill_rect(x, y, width, 5, ' ', &surface, &self.theme);
        buffer.set_text(x, y, &format!("╭{}╮", "─".repeat(width - 2)), &border);
        buffer.set_text(x, y + 4, &format!("╰{}╯", "─".repeat(width - 2)), &border);
        for row in 1..4 {
            buffer.set_text(x, y + row, "│", &border);
            buffer.set_text(x + width - 1, y + row, "│", &border);
        }
        let title_style = Style {
            bold: true,
            ..surface.clone()
        };
        buffer.set_text(
            x + 2,
            y + 1,
            &truncate_display_width(title, width.saturating_sub(4)),
            &title_style,
        );
        for (offset, line) in lines.iter().enumerate() {
            let style = if selected {
                surface.clone()
            } else {
                self.theme.ui_style.muted.clone().with_bg(surface.bg)
            };
            buffer.set_text(
                x + 2,
                y + offset + 2,
                &truncate_display_width(line, width.saturating_sub(4)),
                &style,
            );
        }
    }

    fn draw_home(&self, buffer: &mut RenderBuffer) {
        let palette = splash::palette(&self.theme);
        let tall = self.dialog.height >= 17;
        let brand_row = if tall { 1 } else { 0 };
        self.draw_brand(buffer, brand_row);
        let title = Style {
            bold: true,
            ..self.theme.ui_style.dialog_title.clone()
        };
        self.center(
            buffer,
            brand_row + 2,
            "Your editor. Your muscle memory.",
            &title,
        );
        self.center(
            buffer,
            brand_row + 3,
            "An agent that knows your editor.",
            palette.style(Role::Muted),
        );

        let cards_y = self.dialog.y + 1 + brand_row + 5;
        if self.dialog.width >= WIDE_CARDS_WIDTH && self.dialog.height >= 14 {
            let inner_width = self.dialog.width.saturating_sub(6);
            let first_width = inner_width / 2;
            let second_width = inner_width.saturating_sub(first_width + 2);
            let first_x = self.dialog.x + 3;
            self.draw_card(
                buffer,
                first_x,
                cards_y,
                first_width,
                WelcomeCard::GuidedTour,
            );
            self.draw_card(
                buffer,
                first_x + first_width + 2,
                cards_y,
                second_width,
                WelcomeCard::ReleaseHighlights,
            );
            self.center(
                buffer,
                brand_row + 11,
                "Already know Vim? Press i for the 90-second highlights tour.",
                palette.style(Role::Muted),
            );
        } else {
            for (offset, (card, label)) in [
                (
                    WelcomeCard::GuidedTour,
                    "Take the guided tour · about 5 min",
                ),
                (
                    WelcomeCard::ReleaseHighlights,
                    "See what’s new in this release",
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let prefix = if self.selected == card { "› " } else { "  " };
                let style = if self.selected == card {
                    palette.style(Role::Key)
                } else {
                    palette.style(Role::Text)
                };
                self.center(
                    buffer,
                    brand_row + 5 + offset,
                    &format!("{prefix}{label}"),
                    style,
                );
            }
        }
    }

    fn draw_release(&self, buffer: &mut RenderBuffer) {
        let palette = splash::palette(&self.theme);
        let title = Style {
            bold: true,
            ..self.theme.ui_style.dialog_title.clone()
        };
        self.center(buffer, 0, "What’s new in Red", &title);
        self.center(
            buffer,
            1,
            &format!("Release {}", env!("CARGO_PKG_VERSION")),
            palette.style(Role::Muted),
        );

        let rule_width = self.dialog.width.saturating_sub(6);
        buffer.set_text(
            self.dialog.x + 3,
            self.dialog.y + 4,
            &"─".repeat(rule_width),
            palette.style(Role::Rule),
        );
        let x = self.dialog.x + 3;
        let y = self.dialog.y + 5;
        for (offset, line) in self
            .release_lines
            .iter()
            .skip(self.release_scroll)
            .take(self.release_body_height())
            .enumerate()
        {
            paint_rich_text(buffer, x, y + offset, rule_width, line, |span| {
                let base = match span.style {
                    TextPanelSpanStyle::Heading | TextPanelSpanStyle::Strong => Style {
                        bold: true,
                        ..self.theme.ui_style.dialog_title.clone()
                    },
                    TextPanelSpanStyle::Muted | TextPanelSpanStyle::Quote => {
                        self.theme.ui_style.muted.clone()
                    }
                    TextPanelSpanStyle::InlineCode
                    | TextPanelSpanStyle::Code
                    | TextPanelSpanStyle::Link => palette.style(Role::Key).clone(),
                    _ => self.theme.ui_style.dialog.clone(),
                };
                base.with_bg(self.theme.ui_style.dialog.bg)
            });
        }
    }

    fn toggle_selection(&mut self) {
        self.selected = match self.selected {
            WelcomeCard::GuidedTour => WelcomeCard::ReleaseHighlights,
            WelcomeCard::ReleaseHighlights => WelcomeCard::GuidedTour,
        };
    }

    fn open_highlights(&mut self) -> Option<KeyAction> {
        self.page = WelcomePage::ReleaseHighlights;
        self.release_scroll = 0;
        self.update_actions();
        Some(KeyAction::Single(Action::Refresh))
    }
}

impl Component for WelcomePanel {
    fn shortcut_context(&self) -> &str {
        "Welcome to Red"
    }

    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        match self.page {
            WelcomePage::Home => self.draw_home(buffer),
            WelcomePage::ReleaseHighlights => self.draw_release(buffer),
        }
        Ok(())
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        if !Self::fits(viewport_width, viewport_height) {
            return false;
        }
        let (x, y, width, height) = geometry(viewport_width, viewport_height);
        self.dialog.x = x;
        self.dialog.y = y;
        self.dialog.width = width;
        self.dialog.height = height;
        self.reflow();
        true
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else {
            return match event {
                Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::ScrollDown) => {
                    self.release_scroll = self.release_scroll.saturating_add(3).min(
                        self.release_lines
                            .len()
                            .saturating_sub(self.release_body_height()),
                    );
                    Some(KeyAction::Single(Action::Refresh))
                }
                Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::ScrollUp) => {
                    self.release_scroll = self.release_scroll.saturating_sub(3);
                    Some(KeyAction::Single(Action::Refresh))
                }
                _ => None,
            };
        };

        match self.page {
            WelcomePage::Home => match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Char('q'), _) => {
                    Some(KeyAction::Single(Action::DismissWelcome))
                }
                (KeyCode::Enter, _) => match self.selected {
                    WelcomeCard::GuidedTour => Some(KeyAction::Single(Action::StartTutorial(
                        TutorialTrack::Guided,
                    ))),
                    WelcomeCard::ReleaseHighlights => self.open_highlights(),
                },
                (KeyCode::Char('t'), _) => Some(KeyAction::Single(Action::StartTutorial(
                    TutorialTrack::Guided,
                ))),
                (KeyCode::Char('i'), _) => Some(KeyAction::Single(Action::StartTutorial(
                    TutorialTrack::Quick,
                ))),
                (KeyCode::Char('r'), _) if self.can_resume => {
                    Some(KeyAction::Single(Action::ResumeTutorial))
                }
                (KeyCode::Char('c'), _) => Some(KeyAction::Single(Action::CreateStarterConfig)),
                (KeyCode::Char('v'), _) => self.open_highlights(),
                (KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab, _) => {
                    self.toggle_selection();
                    Some(KeyAction::Single(Action::Refresh))
                }
                _ => None,
            },
            WelcomePage::ReleaseHighlights => match (key.code, key.modifiers) {
                (KeyCode::Esc | KeyCode::Char('q'), _) => {
                    self.page = WelcomePage::Home;
                    self.update_actions();
                    Some(KeyAction::Single(Action::Refresh))
                }
                (KeyCode::Char('t'), _) => Some(KeyAction::Single(Action::StartTutorial(
                    TutorialTrack::Guided,
                ))),
                (KeyCode::Char('o'), _) => Some(KeyAction::Single(Action::OpenWhatsNew)),
                (KeyCode::Down | KeyCode::Char('j'), _) => {
                    self.release_scroll = self.release_scroll.saturating_add(1).min(
                        self.release_lines
                            .len()
                            .saturating_sub(self.release_body_height()),
                    );
                    Some(KeyAction::Single(Action::Refresh))
                }
                (KeyCode::Up | KeyCode::Char('k'), _) => {
                    self.release_scroll = self.release_scroll.saturating_sub(1);
                    Some(KeyAction::Single(Action::Refresh))
                }
                (KeyCode::Char('d'), KeyModifiers::CONTROL) | (KeyCode::PageDown, _) => {
                    self.release_scroll = self
                        .release_scroll
                        .saturating_add(self.release_body_height())
                        .min(
                            self.release_lines
                                .len()
                                .saturating_sub(self.release_body_height()),
                        );
                    Some(KeyAction::Single(Action::Refresh))
                }
                (KeyCode::Char('u'), KeyModifiers::CONTROL) | (KeyCode::PageUp, _) => {
                    self.release_scroll = self
                        .release_scroll
                        .saturating_sub(self.release_body_height());
                    Some(KeyAction::Single(Action::Refresh))
                }
                _ => None,
            },
        }
    }
}

/// Which real-world workflow is being demonstrated without side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TutorialDemoKind {
    /// An entirely synthetic, read-only Git workspace diff.
    Git,
    /// A local proposal that changes only the unnamed practice buffer.
    Agent,
}

/// Clearly labeled Git or agent demonstration inside the guided tour.
pub struct TutorialDemoPanel {
    dialog: Dialog,
    kind: TutorialDemoKind,
    theme: Theme,
}

impl TutorialDemoPanel {
    /// Creates a safe demonstration without launching Git, Codex, or a shell.
    #[must_use]
    pub fn new(editor: &Editor, kind: TutorialDemoKind) -> Self {
        let viewport_width = editor.vwidth();
        let viewport_height = editor.vheight();
        let width = viewport_width.saturating_sub(6).clamp(1, 76);
        let height = viewport_height.saturating_sub(4).clamp(1, 12);
        let x = viewport_width.saturating_sub(width + 2) / 2;
        let y = viewport_height.saturating_sub(height + 2) / 2;
        let theme = editor.theme.clone();
        let title = match kind {
            TutorialDemoKind::Git => "GIT · SAFE PRACTICE DIFF",
            TutorialDemoKind::Agent => "AGENT · SAFE PRACTICE",
        };
        let mut dialog = Dialog::new(
            Some(title.to_string()),
            x,
            y,
            width,
            height,
            &theme.ui_style.dialog,
            BorderStyle::Rounded,
            &theme,
        )
        .with_surface_theme(&theme, SurfaceRole::Dialog);
        let actions = match kind {
            TutorialDemoKind::Git => vec![UiAction::new("continue", "Esc", "Continue")
                .with_priority(ActionPriority::Essential)],
            TutorialDemoKind::Agent => vec![
                UiAction::new("accept", "a", "Apply practice change")
                    .with_priority(ActionPriority::Essential),
                UiAction::new("reject", "r", "Skip").with_priority(ActionPriority::Essential),
            ],
        };
        dialog.set_actions(actions);
        Self {
            dialog,
            kind,
            theme,
        }
    }
}

impl Component for TutorialDemoPanel {
    fn draw(&self, buffer: &mut RenderBuffer) -> anyhow::Result<()> {
        self.dialog.draw(buffer)?;
        let palette = splash::palette(&self.theme);
        let lines = match self.kind {
            TutorialDemoKind::Git => [
                "  practice/src/prices.rs                         modified",
                "",
                "  @@ fn total_price(prices: &[u32]) -> u32 @@",
                "  -    prices.iter().sum()",
                "  +    prices.iter().copied().sum()",
                "",
                "  Sample only — your repository is unchanged.",
            ],
            TutorialDemoKind::Agent => [
                "  Agent request: Explain and improve total_price.",
                "",
                "  SAMPLE CHANGE · NOT APPLIED",
                "  -    prices.iter().sum()",
                "  +    prices.iter().copied().sum()",
                "",
                "  This demo changes only the unnamed practice buffer.",
            ],
        };
        let width = self.dialog.width.saturating_sub(4);
        for (offset, line) in lines.iter().enumerate() {
            if offset >= self.dialog.height.saturating_sub(1) {
                break;
            }
            let role = if line.trim_start().starts_with("+") {
                Role::Key
            } else if line.trim_start().starts_with("-") {
                Role::Dot
            } else if line.contains("Sample only") || line.contains("This demo changes") {
                Role::Muted
            } else {
                Role::Text
            };
            buffer.set_text(
                self.dialog.x + 2,
                self.dialog.y + 2 + offset,
                &truncate_display_width(line, width),
                palette.style(role),
            );
        }
        Ok(())
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
        self.dialog.apply_surface_theme(theme, SurfaceRole::Dialog);
    }

    fn resize(&mut self, viewport_width: usize, viewport_height: usize) -> bool {
        if viewport_width < 24 || viewport_height < 8 {
            return false;
        }
        self.dialog.width = viewport_width.saturating_sub(6).clamp(1, 76);
        self.dialog.height = viewport_height.saturating_sub(4).clamp(1, 12);
        self.dialog.x = viewport_width.saturating_sub(self.dialog.width + 2) / 2;
        self.dialog.y = viewport_height.saturating_sub(self.dialog.height + 2) / 2;
        true
    }

    fn handle_event(&mut self, event: &Event) -> Option<KeyAction> {
        let Event::Key(key) = event else {
            return None;
        };
        match (self.kind, key.code) {
            (TutorialDemoKind::Git, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) => {
                Some(KeyAction::Single(Action::DismissTutorialDemo))
            }
            (TutorialDemoKind::Agent, KeyCode::Char('a') | KeyCode::Enter) => {
                Some(KeyAction::Single(Action::AcceptTutorialProposal))
            }
            (TutorialDemoKind::Agent, KeyCode::Esc | KeyCode::Char('r') | KeyCode::Char('q')) => {
                Some(KeyAction::Single(Action::RejectTutorialProposal))
            }
            _ => None,
        }
    }
}

/// Paints a nonmodal coach so pickers and real editor actions keep working.
pub(crate) fn draw_tutorial_coach(
    buffer: &mut RenderBuffer,
    theme: &Theme,
    tutorial: &TutorialController,
    shortcut: Option<&str>,
) {
    let Some(lesson) = tutorial.lesson() else {
        return;
    };
    if buffer.width < 28 || buffer.height < COACH_HEIGHT + 3 {
        return;
    }
    let width = buffer.width.saturating_sub(4).min(96);
    let x = buffer.width.saturating_sub(width) / 2;
    let y = buffer.height.saturating_sub(COACH_HEIGHT + 2);
    let palette = splash::palette(theme);
    let surface = &theme.ui_style.dialog;
    let border = &theme.ui_style.dialog_border;
    buffer.fill_rect(x, y, width, COACH_HEIGHT, ' ', surface, theme);
    buffer.set_text(x, y, &format!("╭{}╮", "─".repeat(width - 2)), border);
    buffer.set_text(
        x,
        y + COACH_HEIGHT - 1,
        &format!("╰{}╯", "─".repeat(width - 2)),
        border,
    );
    for row in 1..COACH_HEIGHT - 1 {
        buffer.set_text(x, y + row, "│", border);
        buffer.set_text(x + width - 1, y + row, "│", border);
    }

    let progress = tutorial.progress();
    let heading = format!(
        " {}   ·   {}/{} ",
        lesson.title(),
        progress.lesson_index + 1,
        progress.track.lessons().len()
    );
    buffer.set_text(
        x + 2,
        y,
        &truncate_display_width(&heading, width.saturating_sub(4)),
        &theme.ui_style.dialog_title,
    );
    let instruction = lesson.instruction(progress.phase, shortcut);
    buffer.set_text(
        x + 3,
        y + 2,
        &truncate_display_width(&instruction, width.saturating_sub(6)),
        palette.style(Role::Key),
    );
    buffer.set_text(
        x + 3,
        y + 3,
        &truncate_display_width(lesson.explanation(), width.saturating_sub(6)),
        &theme.ui_style.muted,
    );
    buffer.set_text(
        x + 3,
        y + 4,
        &truncate_display_width(":tutorial next  ·  :tutorial quit", width.saturating_sub(6)),
        &theme.ui_style.muted,
    );
}

fn geometry(viewport_width: usize, viewport_height: usize) -> (usize, usize, usize, usize) {
    let width = viewport_width.saturating_sub(6).clamp(1, MAX_WELCOME_WIDTH);
    let height = viewport_height
        .saturating_sub(4)
        .clamp(1, MAX_WELCOME_HEIGHT);
    (
        viewport_width.saturating_sub(width + 2) / 2,
        viewport_height.saturating_sub(height + 2) / 2,
        width,
        height,
    )
}

fn embedded_release_notes(version: &str) -> String {
    let changelog = include_str!("../../CHANGELOG.md");
    let heading = format!("## [{version}]");
    let mut lines = changelog.lines();
    for line in lines.by_ref() {
        if line.starts_with(&heading) {
            return lines
                .take_while(|line| !line.starts_with("## ["))
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    format!(
        "## Included with Red {version}\n\n- Familiar modal editing\n- Fast file and project search\n- Reviewable agent proposals\n- Built-in Git and language tools"
    )
}

#[cfg(test)]
mod tests {
    use crate::{buffer::Buffer, config::Config, editor::Editor, lsp::LspManager};

    use super::*;

    fn editor(width: usize, height: usize) -> Editor {
        let config = Config::default();
        let lsp = Box::new(LspManager::new(config.lsp.clone()));
        Editor::with_size(
            lsp,
            width,
            height,
            config,
            Theme::default(),
            vec![Buffer::new(None, String::new())],
        )
        .unwrap()
    }

    #[test]
    fn welcome_renders_brand_choices_and_editor_aware_agent_promise() {
        let editor = editor(/*width*/ 100, /*height*/ 28);
        let panel = WelcomePanel::new(&editor, /*can_resume*/ false);
        let mut buffer = RenderBuffer::new(100, 28, &Style::default());

        panel.draw(&mut buffer).unwrap();
        let rendered = buffer.dump(false).replace('·', " ");
        assert!(rendered.contains("WELCOME TO RED"));
        assert!(rendered.contains("guided tour"));
        assert!(rendered.contains("What’s new"));
        assert!(rendered.contains("agent that knows your editor"));
    }

    #[test]
    fn embedded_notes_are_for_the_installed_version_only() {
        let notes = embedded_release_notes(env!("CARGO_PKG_VERSION"));

        assert!(notes.contains("### Features") || notes.contains("Included with Red"));
        assert!(!notes.contains("## [0.4.0]"));
    }

    #[test]
    fn agent_preview_clearly_labels_its_side_effect_free_contract() {
        let editor = editor(/*width*/ 90, /*height*/ 20);
        let panel = TutorialDemoPanel::new(&editor, TutorialDemoKind::Agent);
        let mut buffer = RenderBuffer::new(90, 20, &Style::default());

        panel.draw(&mut buffer).unwrap();
        let rendered = buffer.dump(false).replace('·', " ");
        assert!(rendered.contains("SAFE PRACTICE"));
        assert!(rendered.contains("NOT APPLIED"));
        assert!(rendered.contains("unnamed practice buffer"));
        assert!(!rendered.contains("REVIEW REQUIRED"));
    }

    #[test]
    fn compact_welcome_never_draws_outside_its_viewport() {
        let editor = editor(/*width*/ 40, /*height*/ 13);
        let panel = WelcomePanel::new(&editor, /*can_resume*/ true);
        let mut buffer = RenderBuffer::new(40, 13, &Style::default());

        panel.draw(&mut buffer).unwrap();
        assert_eq!(buffer.cells.len(), 40 * 13);
    }
}
