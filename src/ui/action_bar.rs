//! Shared, display-width-aware actions for terminal UI surfaces.
//!
//! Actions are described once and can be projected into a dialog, composer,
//! picker, or plugin workspace without cutting a key binding or hiding the
//! number of actions that did not fit.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::{
    editor::RenderBuffer,
    theme::{SelectionForegroundPriority, Style, SurfacePalette, Theme},
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
    /// Available in keyboard help but never promoted into the compact strip.
    Reference,
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
    /// Optional heading in the complete shortcut reference.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
    /// Additional searchable explanation, omitted from the compact strip.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Optional shorter label that never changes the action's meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_label: Option<String>,
    /// Optional shorter representation of the same key binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_key: Option<String>,
    /// Canonical single key used when a grouped display label is chosen from help.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
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
            group: String::new(),
            description: String::new(),
            compact_label: None,
            compact_key: None,
            trigger: None,
            priority: ActionPriority::Primary,
            modes: Vec::new(),
            enabled: true,
        }
    }

    /// Groups this action in keyboard help.
    #[must_use]
    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }

    /// Sets a searchable, full-length explanation.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
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

    #[must_use]
    pub fn with_trigger(mut self, key: impl Into<String>) -> Self {
        self.trigger = Some(key.into());
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

    /// Resolves the first advertised alternative for modal action-menu dispatch.
    pub(crate) fn event(&self) -> Option<Event> {
        let binding = self.trigger.as_deref().unwrap_or(&self.key).trim();
        let binding = if binding == "/" || binding.ends_with("+/") {
            binding
        } else {
            binding.split('/').next()?
        };
        let normalized = binding
            .replace("Ctrl-", "Ctrl+")
            .replace("Alt-", "Alt+")
            .replace("Shift-", "Shift+")
            .replace("C-", "Ctrl+")
            .replace("A-", "Alt+");
        let mut key = normalized.as_str();
        let mut modifiers = KeyModifiers::NONE;
        for (prefix, modifier) in [
            ("Ctrl+", KeyModifiers::CONTROL),
            ("Alt+", KeyModifiers::ALT),
            ("Shift+", KeyModifiers::SHIFT),
            ("^", KeyModifiers::CONTROL),
        ] {
            if let Some(rest) = key.strip_prefix(prefix) {
                modifiers |= modifier;
                key = rest;
            }
        }
        let code = match key {
            "Enter" | "↵" => KeyCode::Enter,
            "Esc" => KeyCode::Esc,
            "Tab" => KeyCode::Tab,
            "Space" => KeyCode::Char(' '),
            "Backspace" => KeyCode::Backspace,
            "Delete" => KeyCode::Delete,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            "↑" | "↑↓" => KeyCode::Up,
            "↓" => KeyCode::Down,
            "←" => KeyCode::Left,
            "→" => KeyCode::Right,
            value if value.starts_with('F') && value.len() > 1 => {
                KeyCode::F(value[1..].parse().ok()?)
            }
            value if value.chars().count() == 1 => {
                let character = value.chars().next()?;
                KeyCode::Char(
                    if modifiers.contains(KeyModifiers::CONTROL)
                        && !modifiers.contains(KeyModifiers::SHIFT)
                    {
                        character.to_ascii_lowercase()
                    } else {
                        character
                    },
                )
            }
            _ => return None,
        };
        Some(Event::Key(KeyEvent::new(code, modifiers)))
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
    context: Option<&'a str>,
    shortcut_help: bool,
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
            context: None,
            shortcut_help: true,
        }
    }

    /// Displays the current interaction mode and filters mode-specific actions.
    #[must_use]
    pub const fn with_mode(mut self, mode: ActionMode) -> Self {
        self.mode = Some(mode);
        self
    }

    /// Identifies the focused surface without pretending it is a Vim mode.
    #[must_use]
    pub const fn with_context(mut self, context: &'a str) -> Self {
        self.context = Some(context);
        self
    }

    /// Appends status only after the primary actions and overflow are preserved.
    #[must_use]
    pub const fn with_status(mut self, status: Option<&'a str>) -> Self {
        self.status = status;
        self
    }

    /// Suppresses help on the help menu's own navigation strip.
    #[must_use]
    pub const fn without_shortcut_help(mut self) -> Self {
        self.shortcut_help = false;
        self
    }

    /// Calculates complete action fragments for the supplied display-cell width.
    #[must_use]
    pub fn layout(&self, width: usize) -> ActionBarLayout {
        let available = self
            .actions
            .iter()
            .enumerate()
            .filter(|(_, action)| action.is_available(self.mode))
            .collect::<Vec<_>>();
        if !self.shortcut_help || available.is_empty() || width < 2 {
            return self.layout_contents(width);
        }
        let minimum = minimum_essential_width(&available);
        let status_width = self
            .status
            .filter(|status| !status.is_empty())
            .map_or(0, display_width);
        // A complete validation error and the essential escape route take
        // precedence when even the two-cell help key cannot fit beside them.
        if status_width > 0 && minimum.saturating_add(status_width).saturating_add(4) > width {
            return self.layout_contents(width);
        }
        let primary = available
            .iter()
            .filter(|(_, action)| action.priority <= ActionPriority::Primary)
            .collect::<Vec<_>>();
        let primary_width = primary
            .iter()
            .map(|(_, action)| action_width(action, true))
            .sum::<usize>()
            .saturating_add(primary.len().saturating_sub(1) * 3);
        let preferred_minimum =
            if primary_width.saturating_add(status_width).saturating_add(4) <= width {
                primary_width
            } else {
                minimum
            };
        let preserve_status = if minimum.saturating_add(status_width).saturating_add(4) <= width {
            status_width.saturating_add(usize::from(status_width > 0))
        } else {
            0
        };
        let help_width = width
            .saturating_sub(
                preferred_minimum
                    .saturating_add(usize::from(preferred_minimum > 0))
                    .saturating_add(preserve_status),
            )
            .max(2)
            .min(width);
        let mut marker = overflow_marker(0, help_width);
        let mut contents = self.layout_contents(width.saturating_sub(display_width(&marker) + 1));
        for _ in 0..4 {
            let next = overflow_marker(contents.hidden_count(), help_width);
            if next == marker {
                break;
            }
            marker = next;
            contents = self.layout_contents(width.saturating_sub(display_width(&marker) + 1));
        }
        let used = display_width(&contents.text());
        let padding = width.saturating_sub(used + display_width(&marker));
        if padding > 0 {
            contents.spans.push(ActionBarSpan {
                text: " ".repeat(padding),
                role: ActionBarRole::Separator,
            });
        }
        contents.spans.push(ActionBarSpan {
            text: marker,
            role: ActionBarRole::Overflow,
        });
        contents
    }

    fn layout_contents(&self, width: usize) -> ActionBarLayout {
        if width == 0 {
            return ActionBarLayout {
                spans: Vec::new(),
                hidden_actions: self
                    .actions
                    .iter()
                    .filter(|action| action.is_available(self.mode))
                    .cloned()
                    .collect(),
            };
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
        let packed = self.pack(width, status_reserved, &available);

        let mut spans = Vec::new();
        let mut used = 0;

        if let Some(mode) = packed.mode {
            push_span(&mut spans, &mut used, mode, ActionBarRole::Mode);
        }

        for (_, action, compact) in &packed.visible {
            if used > 0 {
                push_span(&mut spans, &mut used, " · ", ActionBarRole::Separator);
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

        let palette = SurfacePalette::new(theme, surface);
        buffer.set_text(x, y, &" ".repeat(width), &palette.surface);
        let layout = self.layout(width);
        let mut column = match alignment {
            ActionBarAlignment::Left => x,
            ActionBarAlignment::Right => {
                x.saturating_add(width.saturating_sub(display_width(&layout.text())))
            }
        };
        for span in &layout.spans {
            if span.role == ActionBarRole::Overflow && !span.text.is_empty() {
                buffer
                    .shortcut_help_regions
                    .push(super::ShortcutHelpRegion {
                        rect: super::ScreenRect {
                            x: column,
                            y,
                            width: display_width(&span.text),
                            height: 1,
                        },
                        context: self
                            .context
                            .or(self.mode.map(ActionMode::label))
                            .unwrap_or("Actions")
                            .to_owned(),
                        actions: self
                            .actions
                            .iter()
                            .filter(|action| action.is_available(self.mode))
                            .cloned()
                            .collect(),
                    });
            }
            let style = match span.role {
                ActionBarRole::Mode => palette.accent.clone(),
                ActionBarRole::Key | ActionBarRole::Overflow => Style {
                    bold: true,
                    ..palette.secondary.clone()
                },
                ActionBarRole::Separator => palette.divider.clone(),
                ActionBarRole::Status | ActionBarRole::Label => palette.muted.clone(),
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
        let mode = self
            .context
            .or(self.mode.map(ActionMode::label))
            .and_then(|full| {
                let compact = self.mode.map(ActionMode::compact_label).unwrap_or(full);
                if display_width(full)
                    .saturating_add(usize::from(action_preferred > 0) * 3)
                    .saturating_add(action_preferred)
                    .saturating_add(reserved)
                    <= width
                {
                    Some(full)
                } else if display_width(compact)
                    .saturating_add(usize::from(action_preferred > 0) * 3)
                    .saturating_add(action_preferred)
                    .saturating_add(reserved)
                    <= width
                {
                    Some(compact)
                } else if display_width(full)
                    .saturating_add(usize::from(action_minimum > 0) * 3)
                    .saturating_add(action_minimum)
                    .saturating_add(reserved)
                    <= width
                {
                    Some(full)
                } else if display_width(compact)
                    .saturating_add(usize::from(action_minimum > 0) * 3)
                    .saturating_add(action_minimum)
                    .saturating_add(reserved)
                    <= width
                {
                    Some(compact)
                } else {
                    None
                }
            });

        let mut used = mode.map_or(0, display_width);
        let mut visible = Vec::new();
        let mut missing_essential = false;

        for (position, (index, action)) in actions.iter().enumerate() {
            if action.priority == ActionPriority::Reference {
                continue;
            }
            if missing_essential && action.priority != ActionPriority::Essential {
                continue;
            }

            let separator_width = usize::from(used > 0) * 3;
            let remaining = width
                .saturating_sub(used)
                .saturating_sub(separator_width)
                .saturating_sub(reserved);
            let full_width = action_width(action, false);
            let compact_width = action_width(action, true);
            let remaining_essentials = minimum_essential_width(&actions[position + 1..]);
            let following_separator = usize::from(remaining_essentials > 0) * 3;
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

        visible.sort_by_key(|(index, _, _)| *index);
        PackedActions { mode, visible }
    }
}

struct PackedActions<'a> {
    mode: Option<&'a str>,
    visible: Vec<(usize, &'a UiAction, bool)>,
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
            width = width.saturating_add(3);
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
    let full = if hidden == 0 {
        "F1 shortcuts".to_owned()
    } else {
        format!("F1 shortcuts +{hidden}")
    };
    let compact = if hidden == 0 {
        "F1".to_owned()
    } else {
        format!("F1 +{hidden}")
    };
    [full, compact, "F1".to_owned()]
        .into_iter()
        .find(|marker| display_width(marker) <= width)
        .unwrap_or_default()
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

/// A small, surface-owned menu for the same actions shown in an action strip.
/// It does not replace the owning dialog or lose its selection/input state.
#[derive(Debug, Default)]
pub(crate) struct ActionMenu {
    selected: Option<usize>,
}

impl ActionMenu {
    pub fn is_open(&self) -> bool {
        self.selected.is_some()
    }

    pub fn open(&mut self) {
        self.selected = Some(0);
    }

    /// Returns the selected action ID. Navigation and cancellation are consumed.
    pub fn handle(&mut self, action: &str, actions: &[UiAction]) -> Option<String> {
        let selected = self.selected.as_mut()?;
        let available = actions
            .iter()
            .filter(|action| action.enabled)
            .collect::<Vec<_>>();
        match action {
            "up" | "k" => *selected = selected.saturating_sub(1),
            "down" | "j" => {
                *selected = selected
                    .saturating_add(1)
                    .min(available.len().saturating_sub(1))
            }
            "escape" | "q" | "?" | "F1" => self.selected = None,
            "activate" => {
                let id = available.get(*selected).map(|action| action.id.clone());
                self.selected = None;
                return id;
            }
            _ => {}
        }
        None
    }

    pub fn render(&self, buffer: &mut RenderBuffer, theme: &Theme, actions: &[UiAction]) {
        let Some(selected) = self.selected else {
            return;
        };
        let actions = actions
            .iter()
            .filter(|action| action.enabled)
            .collect::<Vec<_>>();
        if buffer.width < 8 || buffer.height < 5 {
            return;
        }
        let key_width = actions
            .iter()
            .map(|action| display_width(&action.key))
            .max()
            .unwrap_or(0);
        let width = actions
            .iter()
            .map(|action| key_width + display_width(&action.label) + 6)
            .max()
            .unwrap_or(20)
            .clamp(32, 76)
            .min(buffer.width.saturating_sub(4));
        let height = (actions.len() + 3).min(buffer.height.saturating_sub(2));
        let x = (buffer.width - width) / 2;
        let y = (buffer.height - height) / 2;
        let palette = SurfacePalette::new(theme, &theme.ui_style.popup);
        for row in y..y + height {
            buffer.set_text(x, row, &" ".repeat(width), &palette.surface);
            buffer.set_text(x, row, "│", &palette.divider);
            buffer.set_text(x + width - 1, row, "│", &palette.divider);
        }
        buffer.set_text(
            x,
            y,
            &format!("┌{}┐", "─".repeat(width - 2)),
            &palette.divider,
        );
        buffer.set_text(
            x,
            y + height - 1,
            &format!("└{}┘", "─".repeat(width - 2)),
            &palette.divider,
        );
        let title = format!(
            " Actions {}/{} ",
            selected.saturating_add(1).min(actions.len()),
            actions.len()
        );
        if display_width(&title) <= width.saturating_sub(4) {
            buffer.set_text(x + 2, y, &title, &palette.accent);
        }
        let content_width = width.saturating_sub(4);
        let key_width = key_width.min(content_width);
        let visible = height.saturating_sub(3);
        let first = selected.saturating_sub(visible.saturating_sub(1));
        for (offset, action) in actions.iter().skip(first).take(visible).enumerate() {
            let row = y + 1 + offset;
            let style = if first + offset == selected {
                theme.selected_style(
                    &palette.surface,
                    &theme.list_selection_style(),
                    SelectionForegroundPriority::Selection,
                )
            } else {
                palette.surface.clone()
            };
            buffer.set_text(x + 1, row, &" ".repeat(width - 2), &style);
            let key = if display_width(&action.key) <= key_width {
                action.key.as_str()
            } else {
                action
                    .compact_key
                    .as_deref()
                    .filter(|key| display_width(key) <= key_width)
                    .unwrap_or("")
            };
            let key_style = Style {
                bold: true,
                ..style.clone()
            };
            buffer.set_text(x + 2, row, key, &key_style);
            let label_width = content_width.saturating_sub(key_width + 2);
            if label_width > 0 {
                buffer.set_text(
                    x + 2 + key_width + 2,
                    row,
                    &truncate_display_width(&action.label, label_width),
                    &style,
                );
            }
        }
        let navigation = [
            UiAction::new("select", "Enter", "select")
                .with_compact_key("↵")
                .with_priority(ActionPriority::Essential),
            UiAction::new("return", "Esc", "back").with_priority(ActionPriority::Essential),
        ];
        ActionBar::new(&navigation).without_shortcut_help().render(
            buffer,
            x + 1,
            y + height - 2,
            width - 2,
            theme,
            &palette.surface,
        );
    }
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
    fn overflow_preserves_reserved_validation_status() {
        let actions = vec![
            UiAction::new("send", "Ctrl+Enter", "send")
                .with_compact_key("^↵")
                .with_priority(ActionPriority::Secondary),
            UiAction::new("cancel", "Esc", "normal").with_priority(ActionPriority::Essential),
        ];
        let layout = ActionBar::new(&actions)
            .with_status(Some("Prompt exceeds 128 KiB"))
            .layout(40);
        assert!(layout.text().contains("Esc normal"));
        assert!(
            layout.text().contains("Prompt exceeds 128 KiB"),
            "{}",
            layout.text()
        );
        assert!(display_width(&layout.text()) <= 40);
    }

    #[test]
    fn grouped_hint_dispatches_its_canonical_key() {
        let action = UiAction::new("move", "hjkl/arrows", "move").with_trigger("h");
        assert_eq!(
            action.event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('h'),
                KeyModifiers::NONE
            )))
        );
        assert_eq!(
            UiAction::new("scroll", "^J/^K", "scroll").event(),
            Some(Event::Key(KeyEvent::new(
                KeyCode::Char('j'),
                KeyModifiers::CONTROL
            )))
        );
    }

    #[test]
    fn menu_returns_only_enabled_actions_and_can_cancel() {
        let actions = vec![
            UiAction::new("disabled", "x", "disabled").with_enabled(false),
            UiAction::new("send", "Enter", "send"),
        ];
        let mut menu = ActionMenu::default();
        menu.open();
        assert_eq!(menu.handle("activate", &actions).as_deref(), Some("send"));
        assert!(!menu.is_open());
        menu.open();
        assert_eq!(menu.handle("escape", &actions), None);
        assert!(!menu.is_open());
    }

    #[test]
    fn wide_layout_shows_all_complete_actions_without_overflow() {
        let actions = actions();
        let layout = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .layout(100);

        assert!(layout.text().starts_with("INSERT · Ctrl+Enter Send"));
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
        assert!(layout.text().contains("F1"));
        assert!(layout.hidden_count() >= 1);
    }

    #[test]
    fn compact_key_fits_without_clipping_the_actual_binding() {
        let actions = vec![UiAction::new("send", "Ctrl+Enter", "Send")
            .with_compact_key("C-Enter")
            .with_priority(ActionPriority::Essential)];
        let layout = ActionBar::new(&actions)
            .with_mode(ActionMode::Insert)
            .layout(16);

        assert!(layout.text().contains("C-Enter Send"));
        assert!(display_width(&layout.text()) <= 16);
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
        assert!(row.ends_with("F1"), "{row:?}");
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

#[cfg(test)]
mod shortcut_discovery_tests {
    use super::*;
    #[test]
    fn shortcut_help_is_persistent_counted_and_hit_testable() {
        let actions = vec![
            UiAction::new("send", "Enter", "send"),
            UiAction::new("extra", "Ctrl+x", "Extra action")
                .with_priority(ActionPriority::Reference),
        ];
        let theme = Theme::default();
        for width in 2..100 {
            let mut buffer = RenderBuffer::new(width, 1, &theme.style);
            let layout = ActionBar::new(&actions).with_context("Composer").render(
                &mut buffer,
                0,
                0,
                width,
                &theme,
                &theme.style,
            );
            assert!(display_width(&layout.text()) <= width);
            assert!(layout.text().contains("F1"));
            assert!(layout
                .hidden_actions
                .iter()
                .any(|action| action.id == "extra"));
            let help = buffer.shortcut_help_regions.last().unwrap();
            assert_eq!(help.context, "Composer");
            assert_eq!(help.actions.len(), 2);
            assert_eq!(help.rect.x + help.rect.width, width);
        }
        let plain = [UiAction::new("send", "Enter", "send")];
        assert!(ActionBar::new(&plain)
            .layout(80)
            .text()
            .ends_with("F1 shortcuts"));
    }
}
