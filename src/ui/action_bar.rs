//! Shared, display-width-aware actions for terminal UI surfaces.
//!
//! Actions are described once and can be projected into a dialog, composer,
//! picker, or plugin workspace without cutting a key binding or hiding the
//! number of actions that did not fit.

use serde::{Deserialize, Serialize};

use crate::{
    editor::RenderBuffer,
    theme::{Style, Theme},
    unicode_utils::{display_width, truncate_display_width},
};

/// Determines which actions remain visible when a surface becomes narrow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionPriority {
    /// Submission, cancellation, approval, and other indispensable actions.
    Essential,
    /// Actions normally visible when enough terminal columns are available.
    #[default]
    Primary,
    /// Discoverable conveniences that may move into the overflow list.
    Secondary,
}

/// The interaction mode for which a surface action is currently meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionMode {
    /// Vim insert mode or an editable text input.
    Insert,
    /// Vim normal mode.
    Normal,
    /// Vim visual mode.
    Visual,
    /// A focused, read-only conversation or list.
    Read,
}

impl ActionMode {
    /// Returns the full, user-facing mode indicator.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Normal => "NORMAL",
            Self::Visual => "VISUAL",
            Self::Read => "READ",
        }
    }

    const fn compact_label(self) -> &'static str {
        match self {
            Self::Insert => "I",
            Self::Normal => "N",
            Self::Visual => "V",
            Self::Read => "R",
        }
    }
}

const fn action_enabled_by_default() -> bool {
    true
}

/// One semantic, optionally mode-specific keyboard or mouse action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UiAction {
    /// Stable surface-local identifier used for dispatch and overflow help.
    pub id: String,
    /// Display form of the actual active key binding.
    #[serde(default)]
    pub key: String,
    /// Full user-facing action label.
    pub label: String,
    /// Optional shorter label that never changes the action's meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_label: Option<String>,
    /// Optional shorter representation of the same key binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_key: Option<String>,
    /// Responsive display priority.
    #[serde(default)]
    pub priority: ActionPriority,
    /// Empty means the action is available in every surface mode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<ActionMode>,
    /// Disabled actions are excluded from both the bar and overflow.
    #[serde(default = "action_enabled_by_default")]
    pub enabled: bool,
}

impl UiAction {
    /// Creates an enabled, normally prioritized surface action.
    #[must_use]
    pub fn new(id: impl Into<String>, key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            key: key.into(),
            label: label.into(),
            compact_label: None,
            compact_key: None,
            priority: ActionPriority::Primary,
            modes: Vec::new(),
            enabled: true,
        }
    }

    /// Sets the responsive importance of this action.
    #[must_use]
    pub const fn with_priority(mut self, priority: ActionPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Sets a shorter label used only when the full action does not fit.
    #[must_use]
    pub fn with_compact_label(mut self, label: impl Into<String>) -> Self {
        self.compact_label = Some(label.into());
        self
    }

    /// Sets a shorter, semantically equivalent key-binding display.
    #[must_use]
    pub fn with_compact_key(mut self, key: impl Into<String>) -> Self {
        self.compact_key = Some(key.into());
        self
    }

    /// Restricts this action to the supplied interaction modes.
    #[must_use]
    pub fn with_modes(mut self, modes: impl IntoIterator<Item = ActionMode>) -> Self {
        self.modes.extend(modes);
        self
    }

    /// Enables or disables this action without changing its definition.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn is_available(&self, mode: Option<ActionMode>) -> bool {
        self.enabled
            && (self.modes.is_empty() || mode.is_some_and(|mode| self.modes.contains(&mode)))
    }
}

/// The semantic color role of one rendered action-bar fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionBarRole {
    /// The current Vim or surface interaction mode.
    Mode,
    /// A complete keyboard shortcut.
    Key,
    /// The human-readable action label.
    Label,
    /// Whitespace or punctuation between actions.
    Separator,
    /// A nonessential status message.
    Status,
    /// The count of complete actions available in overflow.
    Overflow,
}

/// One individually styled, grapheme-complete action-bar fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionBarSpan {
    /// Text that fits without splitting a terminal grapheme.
    pub text: String,
    /// The semantic theme role applied when the bar is rendered.
    pub role: ActionBarRole,
}

/// The visible and hidden result of a responsive action-bar layout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionBarLayout {
    /// Ordered fragments that together fit within the requested width.
    pub spans: Vec<ActionBarSpan>,
    /// Complete action definitions omitted from the visible bar.
    pub hidden_actions: Vec<UiAction>,
}

impl ActionBarLayout {
    /// Returns the complete visible action-bar text.
    #[must_use]
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }

    /// Returns the number of hidden but available surface actions.
    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.hidden_actions.len()
    }
}

/// Lays out a shared action bar without silently clipping keyboard shortcuts.
#[derive(Debug, Clone, Copy)]
pub struct ActionBar<'a> {
    actions: &'a [UiAction],
    mode: Option<ActionMode>,
    status: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionBarAlignment {
    Left,
    Right,
}

impl<'a> ActionBar<'a> {
    /// Creates an action bar for a caller-owned action registry.
    #[must_use]
    pub const fn new(actions: &'a [UiAction]) -> Self {
        Self {
            actions,
            mode: None,
            status: None,
        }
    }

    /// Displays the current interaction mode and filters mode-specific actions.
    #[must_use]
    pub const fn with_mode(mut self, mode: ActionMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Appends status only after the primary actions and overflow are preserved.
    #[must_use]
    pub const fn with_status(mut self, status: Option<&'a str>) -> Self {
        self.status = status;
        self
    }

    /// Calculates complete action fragments for the supplied display-cell width.
    #[must_use]
    pub fn layout(&self, width: usize) -> ActionBarLayout {
        if width == 0 {
            return ActionBarLayout::default();
        }

        let mut available = self
            .actions
            .iter()
            .enumerate()
            .filter(|(_, action)| action.is_available(self.mode))
            .collect::<Vec<_>>();
        available.sort_by_key(|(index, action)| (action.priority, *index));

        let essential_width = minimum_essential_width(&available);
        let status_reserved = self
            .status
            .filter(|status| !status.is_empty())
            .map(|status| {
                let status_width = display_width(status);
                let separator = if available.is_empty() {
                    0
                } else if essential_width
                    .saturating_add(status_width)
                    .saturating_add(3)
                    <= width
                {
                    3
                } else {
                    1
                };
                status_width
                    .saturating_add(separator)
                    .min(width.saturating_sub(essential_width))
            })
            .unwrap_or_default();
        let mut reserved = status_reserved;
        let mut packed = self.pack(width, reserved, &available);
        for _ in 0..3 {
            let hidden = available.len().saturating_sub(packed.visible.len());
            if hidden == 0 {
                break;
            }
            let marker = overflow_marker(hidden, width);
            let next_reserved = status_reserved
                .saturating_add(display_width(&marker))
                .saturating_add(usize::from(packed.used > 0))
                .min(width.saturating_sub(essential_width));
            if next_reserved == reserved {
                break;
            }
            reserved = next_reserved;
            packed = self.pack(width, reserved, &available);
        }

        let mut spans = Vec::new();
        let mut used = 0;

        if let Some(mode) = packed.mode {
            push_span(&mut spans, &mut used, mode, ActionBarRole::Mode);
        }

        for (_, action, compact) in &packed.visible {
            if used > 0 {
                push_span(&mut spans, &mut used, "  ", ActionBarRole::Separator);
            }
            let key = if *compact {
                action.compact_key.as_deref().unwrap_or(&action.key)
            } else {
                &action.key
            };
            let label = if *compact {
                action.compact_label.as_deref().unwrap_or(&action.label)
            } else {
                &action.label
            };
            if !key.is_empty() {
                push_span(&mut spans, &mut used, key, ActionBarRole::Key);
            }
            if !key.is_empty() && !label.is_empty() {
                push_span(&mut spans, &mut used, " ", ActionBarRole::Separator);
            }
            if !label.is_empty() {
                push_span(&mut spans, &mut used, label, ActionBarRole::Label);
            }
        }

        let hidden_actions = available
            .iter()
            .filter(|(index, _)| {
                !packed
                    .visible
                    .iter()
                    .any(|(visible_index, _, _)| visible_index == index)
            })
            .map(|(_, action)| (*action).clone())
            .collect::<Vec<_>>();

        if !hidden_actions.is_empty() {
            let separator_width = usize::from(used > 0);
            let marker = overflow_marker(
                hidden_actions.len(),
                width.saturating_sub(used).saturating_sub(separator_width),
            );
            if !marker.is_empty() {
                if used > 0 {
                    push_span(&mut spans, &mut used, " ", ActionBarRole::Separator);
                }
                push_span(&mut spans, &mut used, &marker, ActionBarRole::Overflow);
            }
        }

        if let Some(status) = self.status.filter(|status| !status.is_empty()) {
            let separator = if used == 0 {
                ""
            } else if display_width(status).saturating_add(3) <= width.saturating_sub(used) {
                " · "
            } else {
                " "
            };
            let separator_width = display_width(separator);
            if width.saturating_sub(used) > separator_width {
                let visible_status = truncate_display_width(
                    status,
                    width.saturating_sub(used).saturating_sub(separator_width),
                );
                if !visible_status.is_empty() {
                    push_span(&mut spans, &mut used, separator, ActionBarRole::Separator);
                    push_span(
                        &mut spans,
                        &mut used,
                        &visible_status,
                        ActionBarRole::Status,
                    );
                }
            }
        }

        ActionBarLayout {
            spans,
            hidden_actions,
        }
    }

    /// Paints a one-row bar while retaining the surrounding surface background.
    pub fn render(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        theme: &Theme,
        surface: &Style,
    ) -> ActionBarLayout {
        self.render_aligned(
            buffer,
            x,
            y,
            width,
            theme,
            surface,
            ActionBarAlignment::Left,
        )
    }

    /// Paints actions against the right edge without changing their priority or background.
    pub fn render_right_aligned(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        theme: &Theme,
        surface: &Style,
    ) -> ActionBarLayout {
        self.render_aligned(
            buffer,
            x,
            y,
            width,
            theme,
            surface,
            ActionBarAlignment::Right,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_aligned(
        &self,
        buffer: &mut RenderBuffer,
        x: usize,
        y: usize,
        width: usize,
        theme: &Theme,
        surface: &Style,
        alignment: ActionBarAlignment,
    ) -> ActionBarLayout {
        let width = width.min(buffer.width.saturating_sub(x));
        if width == 0 || y >= buffer.height {
            return ActionBarLayout::default();
        }

        buffer.set_text(x, y, &" ".repeat(width), surface);
        let layout = self.layout(width);
        let mut column = match alignment {
            ActionBarAlignment::Left => x,
            ActionBarAlignment::Right => {
                x.saturating_add(width.saturating_sub(display_width(&layout.text())))
            }
        };
        for span in &layout.spans {
            let style = match span.role {
                ActionBarRole::Mode => theme.ui_style.popup_title.with_bg(surface.bg),
                ActionBarRole::Key | ActionBarRole::Overflow => {
                    theme.ui_style.picker_prompt.with_bg(surface.bg)
                }
                ActionBarRole::Separator | ActionBarRole::Status => {
                    theme.ui_style.muted.with_bg(surface.bg)
                }
                ActionBarRole::Label => surface.clone(),
            };
            buffer.set_text(column, y, &span.text, &style);
            column = column.saturating_add(display_width(&span.text));
        }
        layout
    }

    fn pack<'b>(
        &self,
        width: usize,
        reserved: usize,
        actions: &'b [(usize, &'a UiAction)],
    ) -> PackedActions<'a> {
        let essential_minimum = minimum_essential_width(actions);
        let action_minimum = if essential_minimum == 0 {
            actions
                .first()
                .map(|(_, action)| action_width(action, true))
                .unwrap_or_default()
        } else {
            essential_minimum
        };
        let action_preferred = if essential_minimum == 0 {
            actions
                .first()
                .map(|(_, action)| action_width(action, false))
                .unwrap_or_default()
        } else {
            essential_width(actions, false)
        };
        let mode = self.mode.and_then(|mode| {
            let full = mode.label();
            if display_width(full)
                .saturating_add(usize::from(action_preferred > 0) * 2)
                .saturating_add(action_preferred)
                .saturating_add(reserved)
                <= width
            {
                Some(full)
            } else if display_width(mode.compact_label())
                .saturating_add(usize::from(action_preferred > 0) * 2)
                .saturating_add(action_preferred)
                .saturating_add(reserved)
                <= width
            {
                Some(mode.compact_label())
            } else if display_width(full)
                .saturating_add(usize::from(action_minimum > 0) * 2)
                .saturating_add(action_minimum)
                .saturating_add(reserved)
                <= width
            {
                Some(full)
            } else if display_width(mode.compact_label())
                .saturating_add(usize::from(action_minimum > 0) * 2)
                .saturating_add(action_minimum)
                .saturating_add(reserved)
                <= width
            {
                Some(mode.compact_label())
            } else {
                None
            }
        });

        let mut used = mode.map_or(0, display_width);
        let mut visible = Vec::new();
        let mut missing_essential = false;

        for (position, (index, action)) in actions.iter().enumerate() {
            if missing_essential && action.priority != ActionPriority::Essential {
                continue;
            }

            let separator_width = usize::from(used > 0) * 2;
            let remaining = width
                .saturating_sub(used)
                .saturating_sub(separator_width)
                .saturating_sub(reserved);
            let full_width = action_width(action, false);
            let compact_width = action_width(action, true);
            let remaining_essentials = minimum_essential_width(&actions[position + 1..]);
            let following_separator = usize::from(remaining_essentials > 0) * 2;
            let preserve_essentials = remaining_essentials.saturating_add(following_separator);
            let compact = if full_width.saturating_add(preserve_essentials) <= remaining {
                false
            } else if compact_width.saturating_add(preserve_essentials) <= remaining
                || (action.priority == ActionPriority::Essential && compact_width <= remaining)
            {
                true
            } else {
                if action.priority == ActionPriority::Essential {
                    missing_essential = true;
                }
                continue;
            };

            used = used
                .saturating_add(separator_width)
                .saturating_add(if compact { compact_width } else { full_width });
            visible.push((*index, *action, compact));
        }

        PackedActions {
            mode,
            visible,
            used,
        }
    }
}

struct PackedActions<'a> {
    mode: Option<&'static str>,
    visible: Vec<(usize, &'a UiAction, bool)>,
    used: usize,
}

fn minimum_essential_width(actions: &[(usize, &UiAction)]) -> usize {
    essential_width(actions, true)
}

fn essential_width(actions: &[(usize, &UiAction)], compact: bool) -> usize {
    let mut width = 0usize;
    let mut count = 0usize;
    for (_, action) in actions {
        if action.priority != ActionPriority::Essential {
            continue;
        }
        if count > 0 {
            width = width.saturating_add(2);
        }
        width = width.saturating_add(action_width(action, compact));
        count = count.saturating_add(1);
    }
    width
}

fn action_width(action: &UiAction, compact: bool) -> usize {
    let key = if compact {
        action.compact_key.as_deref().unwrap_or(&action.key)
    } else {
        &action.key
    };
    let label = if compact {
        action.compact_label.as_deref().unwrap_or(&action.label)
    } else {
        &action.label
    };
    display_width(key)
        .saturating_add(usize::from(!key.is_empty() && !label.is_empty()))
        .saturating_add(display_width(label))
}

fn overflow_marker(hidden: usize, width: usize) -> String {
    if hidden == 0 || width == 0 {
        return String::new();
    }
    let marker = format!("… +{hidden}");
    if display_width(&marker) <= width {
        marker
    } else if display_width("…") <= width {
        "…".to_string()
    } else {
        String::new()
    }
}

fn push_span(spans: &mut Vec<ActionBarSpan>, used: &mut usize, text: &str, role: ActionBarRole) {
    if text.is_empty() {
        return;
    }
    *used = used.saturating_add(display_width(text));
    spans.push(ActionBarSpan {
        text: text.to_string(),
        role,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions() -> Vec<UiAction> {
        vec![
            UiAction::new("send", "Ctrl+Enter", "Send")
                .with_compact_key("C-Enter")
                .with_priority(ActionPriority::Essential),
            UiAction::new("cancel", "Esc", "Close").with_priority(ActionPriority::Essential),
            UiAction::new("newline", "Enter", "New line"),
            UiAction::new("history", "Ctrl+P/N", "History")
                .with_priority(ActionPriority::Secondary),
        ]
    }

    #[test]
    fn wide_layout_shows_all_complete_actions_without_overflow() {
        let actions = actions();
        let layout = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .layout(100);

        assert!(layout.text().starts_with("INSERT  Ctrl+Enter Send"));
        assert!(layout.text().contains("Esc Close"));
        assert!(layout.text().contains("Enter New line"));
        assert!(layout.text().contains("Ctrl+P/N History"));
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn narrow_layout_prioritizes_complete_essential_actions_and_overflow() {
        let actions = actions();
        let layout = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .layout(34);

        assert!(display_width(&layout.text()) <= 34);
        assert!(layout.text().contains("Enter Send"));
        assert!(layout.text().contains("Esc Close"));
        assert!(layout.text().contains('…'));
        assert!(layout.hidden_count() >= 1);
    }

    #[test]
    fn compact_key_fits_without_clipping_the_actual_binding() {
        let actions = vec![UiAction::new("send", "Ctrl+Enter", "Send")
            .with_compact_key("C-Enter")
            .with_priority(ActionPriority::Essential)];
        let layout = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .layout(14);

        assert!(layout.text().contains("C-Enter Send"));
        assert!(display_width(&layout.text()) <= 14);
    }

    #[test]
    fn hidden_actions_are_complete_and_stably_ordered() {
        let actions = actions();
        let layout = ActionBar::new(&actions).layout(21);

        assert!(!layout.hidden_actions.is_empty());
        assert!(layout
            .hidden_actions
            .iter()
            .all(|action| !action.id.is_empty() && !action.label.is_empty()));
        assert_eq!(layout.hidden_actions.last().unwrap().id, "history");
    }

    #[test]
    fn disabled_actions_are_neither_advertised_nor_counted() {
        let actions = vec![
            UiAction::new("send", "Enter", "Send"),
            UiAction::new("steer", "s", "Steer").with_enabled(false),
        ];
        let layout = ActionBar::new(&actions).layout(80);

        assert!(layout.text().contains("Enter Send"));
        assert!(!layout.text().contains("Steer"));
        assert_eq!(layout.hidden_count(), 0);
    }

    #[test]
    fn mode_specific_actions_follow_the_active_surface_mode() {
        let actions = vec![
            UiAction::new("edit", "i", "Edit").with_modes([ActionMode::Read]),
            UiAction::new("send", "Enter", "Send").with_modes([ActionMode::Insert]),
        ];

        let reading = ActionBar::new(&actions)
            .with_mode(ActionMode::Read)
            .layout(40);
        let editing = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .layout(40);

        assert!(reading.text().contains("i Edit"));
        assert!(!reading.text().contains("Enter Send"));
        assert!(editing.text().contains("Enter Send"));
        assert!(!editing.text().contains("i Edit"));
    }

    #[test]
    fn status_cannot_displace_submission_or_cancel_actions() {
        let actions = actions();
        let layout = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .with_status(Some("an exceptionally long background agent status"))
            .layout(34);

        assert!(layout.text().contains("Enter Send"));
        assert!(layout.text().contains("Esc Close"));
        assert!(display_width(&layout.text()) <= 34);
    }

    #[test]
    fn unicode_actions_are_measured_in_terminal_cells() {
        let actions = vec![
            UiAction::new("read", "漢", "Read 👨‍👩‍👧").with_priority(ActionPriority::Essential),
            UiAction::new("extra", "e", "Additional action"),
        ];

        for width in 0..30 {
            let layout = ActionBar::new(&actions).layout(width);
            assert!(display_width(&layout.text()) <= width, "width {width}");
            if layout.text().contains('漢') {
                assert!(layout.text().contains("Read 👨‍👩‍👧"));
            }
        }
    }

    #[test]
    fn zero_and_tiny_width_never_overflow() {
        let actions = actions();
        for width in 0..8 {
            let layout = ActionBar::new(&actions)
                .with_mode(ActionMode::Insert)
                .layout(width);
            assert!(display_width(&layout.text()) <= width, "width {width}");
        }
    }

    #[test]
    fn rendering_keeps_the_caller_surface_background() {
        let actions = actions();
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(32, 2, &theme.style);

        let layout = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .render(&mut buffer, 0, 1, 32, &theme, &theme.style);

        assert!(!layout.text().is_empty());
        assert!(buffer.cells[32..64]
            .iter()
            .all(|cell| cell.style.bg == theme.style.bg));
    }

    #[test]
    fn right_aligned_rendering_preserves_the_surface_and_complete_actions() {
        let actions = vec![
            UiAction::new("select", "Enter", "Select").with_priority(ActionPriority::Essential),
            UiAction::new("close", "Esc", "Close").with_priority(ActionPriority::Essential),
        ];
        let theme = Theme::default();
        let mut buffer = RenderBuffer::new(40, 2, &theme.style);

        let layout = ActionBar::new(&actions).render_right_aligned(
            &mut buffer,
            2,
            1,
            34,
            &theme,
            &theme.style,
        );

        let row = buffer.cells[42..76]
            .iter()
            .map(|cell| cell.c)
            .collect::<String>();
        assert!(row.ends_with(&layout.text()), "{row:?}");
        assert!(row.starts_with(' '), "{row:?}");
        assert!(buffer.cells[42..76]
            .iter()
            .all(|cell| cell.style.bg == theme.style.bg));
    }

    #[test]
    fn right_aligned_unicode_actions_remain_inside_the_available_columns() {
        let actions = vec![
            UiAction::new("select", "↵", "選ぶ").with_priority(ActionPriority::Essential),
            UiAction::new("extra", "?", "More actions"),
        ];
        let theme = Theme::default();

        for width in 1..24 {
            let mut buffer = RenderBuffer::new(26, 2, &theme.style);
            let layout = ActionBar::new(&actions).render_right_aligned(
                &mut buffer,
                1,
                1,
                width,
                &theme,
                &theme.style,
            );

            assert!(display_width(&layout.text()) <= width, "width {width}");
            assert_eq!(buffer.cells[26].c, ' ', "width {width}");
            assert_eq!(buffer.cells[27 + width].c, ' ', "width {width}");
        }
    }
}
