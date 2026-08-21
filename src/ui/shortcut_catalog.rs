//! Shared reference bindings for built-in non-editor surfaces.

use super::{ActionPriority, ShortcutEntry, UiAction};

pub(crate) fn surface_reference_actions(mode: &str) -> Vec<UiAction> {
    let mut definitions: Vec<(&str, &str, &str)> = Vec::new();
    if mode.starts_with("SCROLLBACK") {
        definitions.extend([
            (
                "Navigation",
                "0 / ^ / $",
                "Line start / first nonblank / line end",
            ),
            ("Navigation", "w / W / b / B / e / E", "Move by word"),
            ("Navigation", "{ / }", "Previous / next paragraph"),
            ("Navigation", "Ctrl+b / Ctrl+f", "Previous / next page"),
            ("Navigation", "Ctrl+u / Ctrl+d", "Scroll half a page"),
            (
                "Navigation",
                "H / M / L",
                "Top / middle / bottom of viewport",
            ),
            (
                "Navigation",
                "f<char> / F<char> / t<char> / T<char>",
                "Find or move until a character",
            ),
            ("Navigation", "; / ,", "Repeat character motion / reverse"),
            ("Navigation", "g / G", "First / latest output"),
            ("Focus", "i / a / Esc", "Return to the composer"),
        ]);
        if mode == "SCROLLBACK NORMAL" {
            definitions.push(("Search", "n / N", "Next / previous search result"));
        }
    } else if mode == "NAV" {
        definitions.extend([
            ("Navigation", "PageUp / PageDown", "Previous / next page"),
            ("Navigation", "Ctrl+b / Ctrl+f", "Previous / next page"),
            ("Focus", "Esc", "Return focus to the editor"),
            ("Focus", "Tab / Shift+Tab", "Switch composer and transcript"),
            ("Conversation", "N", "Start a new conversation"),
            ("Conversation", "x", "Clear the conversation"),
        ]);
    } else if mode == "NORMAL" || mode == "VISUAL" {
        definitions.extend([
            (
                "Navigation",
                "h / j / k / l",
                "Move left / down / up / right",
            ),
            ("Navigation", "w / W / b / B / e / E", "Move by word"),
            ("Navigation", "{ / }", "Previous / next paragraph"),
            ("Navigation", "( / )", "Previous / next sentence"),
            (
                "Navigation",
                "0 / ^ / $",
                "Line start / first nonblank / line end",
            ),
            ("Navigation", "gg / G", "First / last line"),
            (
                "Navigation",
                "f<char> / F<char> / t<char> / T<char>",
                "Find or move until a character",
            ),
            (
                "Navigation",
                "; / , / %",
                "Repeat character motion / reverse / match delimiter",
            ),
            (
                "Editing",
                "i / I / a / A / o / O",
                "Insert before / at start / after / at end / on a new line",
            ),
            (
                "Editing",
                "d / c / y + motion",
                "Delete / change / copy a text range",
            ),
            ("Editing", "dd / cc / yy", "Delete / change / copy lines"),
            ("Editing", "D / C / Y", "Delete / change / copy to line end"),
            (
                "Editing",
                "x / X / s / S",
                "Delete or replace characters / lines",
            ),
            ("Editing", "p / P", "Paste after / before"),
            ("Editing", "u / U / Ctrl+r", "Undo / redo"),
            (
                "Editing",
                "r<char> / ~ / J / .",
                "Replace / change case / join / repeat edit",
            ),
            (
                "Selection",
                "v / V / Ctrl+v",
                "Character / line / block selection",
            ),
            (
                "Search",
                "/ / ? / n / N",
                "Search forward / backward / repeat",
            ),
            (
                "Macros",
                "q<register> / @<register>",
                "Record / replay a macro",
            ),
            ("Editing", "1–9", "Repeat the next motion or edit"),
        ]);
    } else if mode == "INSERT" {
        definitions.extend([
            (
                "Editing",
                "Backspace / Delete",
                "Delete previous / next character",
            ),
            ("Editing", "Ctrl+w / Alt+Backspace", "Delete previous word"),
            ("Navigation", "← / → / ↑ / ↓", "Move the text cursor"),
            ("Navigation", "Home / End", "Start / end of line"),
            (
                "History",
                "Ctrl+p / Ctrl+n",
                "Previous / next submitted prompt",
            ),
        ]);
    }
    definitions
        .into_iter()
        .map(|(group, key, label)| {
            UiAction::new(format!("reference:{key}"), key, label)
                .with_group(group)
                .with_priority(ActionPriority::Reference)
        })
        .collect()
}

pub(crate) fn prompt_reference_actions(mode: crate::editor::Mode) -> Vec<UiAction> {
    use crate::editor::Mode;
    let mode = match mode {
        Mode::Normal => "NORMAL",
        Mode::Visual | Mode::VisualLine | Mode::VisualBlock => "VISUAL",
        Mode::Search => "SEARCH",
        _ => "INSERT",
    };
    let mut actions = surface_reference_actions(mode);
    actions.extend(reference_actions(&[
        ("Editing", "Ctrl+z / Ctrl+r", "Undo / redo"),
        ("Navigation", "Ctrl+a / Ctrl+e", "Start / end of prompt"),
        (
            "History",
            "Ctrl+p / Ctrl+n",
            "Previous / next submitted prompt",
        ),
        (
            "Composer",
            "Alt+Enter / Shift+Enter / Ctrl+j",
            "Insert a new line",
        ),
    ]));
    actions
}

pub(crate) fn reference_actions(definitions: &[(&str, &str, &str)]) -> Vec<UiAction> {
    definitions
        .iter()
        .map(|(group, key, label)| {
            UiAction::new(format!("reference:{key}"), *key, *label)
                .with_group(*group)
                .with_priority(ActionPriority::Reference)
        })
        .collect()
}

pub(crate) fn picker_reference_actions() -> Vec<UiAction> {
    reference_actions(&[
        (
            "Navigation",
            "↑ / ↓ / Ctrl+k / Ctrl+j",
            "Previous / next result",
        ),
        (
            "Navigation",
            "PageUp / PageDown / Ctrl+u / Ctrl+d",
            "Previous / next result page",
        ),
        (
            "Preview",
            "Ctrl+b / Ctrl+f",
            "Previous / next preview or result page",
        ),
        ("History", "Ctrl+h / Ctrl+l", "Previous / next picker query"),
        (
            "Search",
            "Type / Backspace",
            "Filter results / remove a character",
        ),
        (
            "Search",
            "Alt+Backspace / Ctrl+Backspace",
            "Delete the previous query word",
        ),
        ("Picker", "Ctrl+c", "Cancel the picker"),
    ])
}

pub(crate) fn completion_reference_actions() -> Vec<UiAction> {
    reference_actions(&[
        (
            "Navigation",
            "↑ / ↓ / Ctrl+p / Ctrl+n",
            "Previous / next completion",
        ),
        (
            "Navigation",
            "PageUp / PageDown",
            "Previous / next completion page",
        ),
        ("Completion", "Tab / Enter", "Accept selected completion"),
        ("Completion", "Ctrl+e", "Close completions"),
        (
            "Completion",
            "Esc",
            "Close completions and enter Normal mode",
        ),
        (
            "Search",
            "Type / Backspace",
            "Edit text and filter completions",
        ),
    ])
}

pub(crate) fn common_shortcut_entries() -> Vec<ShortcutEntry> {
    let mut entries = ShortcutEntry::from_actions("Picker", &picker_reference_actions())
        .into_iter()
        .map(|entry| entry.in_context("Picker"))
        .collect::<Vec<_>>();
    entries.extend(
        ShortcutEntry::from_actions("Completions", &completion_reference_actions())
            .into_iter()
            .map(|entry| entry.in_context("Completions")),
    );
    for mode in [
        crate::editor::Mode::Insert,
        crate::editor::Mode::Normal,
        crate::editor::Mode::Visual,
    ] {
        let context = format!("Composer · {mode:?}");
        entries.extend(
            ShortcutEntry::from_actions(&context, &prompt_reference_actions(mode))
                .into_iter()
                .map(|entry| entry.in_context(&context)),
        );
    }
    entries
}
