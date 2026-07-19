//! Command and keymap metadata used by Red's discovery surfaces.

use std::collections::HashMap;

use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use serde_json::json;

use crate::{
    command,
    config::{KeyAction, Keys},
    editor::{Action, Mode, SearchDirection},
    plugin::RegisteredPluginCommand,
    ui::PickerItem,
    unicode_utils::{display_width, truncate_display_width},
};

/// Colon commands handled by the built-in command parser.
pub(crate) const BUILTIN_COLON_COMMANDS: &[&str] = &[
    "$",
    "quit",
    "write",
    "buffer-next",
    "buffer-prev",
    "bd",
    "bdelete",
    "buffer-delete",
    "edit",
    "split",
    "sp",
    "vsplit",
    "vs",
    "close",
    "only",
    "noh",
    "nohlsearch",
    "wrap",
    "nowrap",
    "config-diagnostics",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommandPaletteEntry {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) category: String,
    pub(crate) description: String,
    pub(crate) colon: Option<String>,
    pub(crate) aliases: Vec<String>,
    pub(crate) shortcuts: Vec<String>,
    pub(crate) action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KeymapHint {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) is_group: bool,
}

struct BuiltinCommand {
    id: &'static str,
    title: &'static str,
    category: &'static str,
    description: &'static str,
    colon: Option<&'static str>,
    aliases: &'static [&'static str],
    action: Action,
}

/// Builds searchable palette rows using the effective configured keymaps.
pub(crate) fn entries(
    keys: &Keys,
    plugin_commands: &[RegisteredPluginCommand],
) -> Vec<CommandPaletteEntry> {
    let mut entries = builtin_commands()
        .into_iter()
        .map(|command| {
            let shortcuts = shortcuts_for_action(keys, &command.action);
            CommandPaletteEntry {
                id: command.id.to_string(),
                title: command.title.to_string(),
                category: command.category.to_string(),
                description: command.description.to_string(),
                colon: command.colon.map(str::to_string),
                aliases: command.aliases.iter().map(ToString::to_string).collect(),
                shortcuts,
                action: command.action,
            }
        })
        .collect::<Vec<_>>();

    entries.extend(plugin_commands.iter().map(|command| {
        let action = Action::PluginCommand(command.name.clone());
        let shortcuts = shortcuts_for_action(keys, &action);
        let title = command
            .metadata
            .title
            .clone()
            .unwrap_or_else(|| humanize_identifier(&command.name));
        let category = command
            .metadata
            .category
            .as_deref()
            .unwrap_or(command.plugin.as_str());
        let description = command.metadata.description.as_deref().unwrap_or("");
        let colon = (!colon_name_is_builtin(&command.name)).then(|| format!(":{}", command.name));

        CommandPaletteEntry {
            id: format!("plugin.{}.{}", command.plugin, command.name),
            title,
            category: category.to_string(),
            description: description.to_string(),
            colon,
            aliases: command.metadata.aliases.clone(),
            shortcuts,
            action,
        }
    }));

    entries.sort_unstable_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
    entries
}

/// Converts command entries into aligned, structured picker rows.
pub(crate) fn picker_items(entries: &[CommandPaletteEntry]) -> Vec<PickerItem> {
    let category_width = entries
        .iter()
        .map(|entry| display_width(&entry.category))
        .max()
        .unwrap_or_default()
        .clamp(4, 18);
    let shortcut_width = entries
        .iter()
        .map(|entry| display_width(&primary_shortcut(&entry.shortcuts)))
        .max()
        .unwrap_or_default()
        .min(26);

    entries
        .iter()
        .map(|entry| {
            let category = truncate_display_width(&entry.category, category_width);
            let category_padding =
                " ".repeat(category_width.saturating_sub(display_width(&category)));
            let primary_shortcut = primary_shortcut(&entry.shortcuts);
            let shortcut = truncate_display_width(&primary_shortcut, shortcut_width);
            let shortcut_padding =
                " ".repeat(shortcut_width.saturating_sub(display_width(&shortcut)));
            let detail = match entry.colon.as_deref() {
                Some(colon) if shortcut_width > 0 => {
                    format!("{shortcut}{shortcut_padding}  {colon}")
                }
                Some(colon) => colon.to_string(),
                None => shortcut,
            };

            PickerItem {
                id: entry.id.clone(),
                label: entry.title.clone(),
                kind: Some("Command".to_string()),
                annotation: Some(format!("{category}{category_padding}")),
                detail: (!detail.is_empty()).then_some(detail),
                data: json!({
                    "description": entry.description,
                    "aliases": entry.aliases,
                    "shortcuts": entry.shortcuts,
                    "primary_shortcut": primary_shortcut,
                    "colon": entry.colon,
                }),
                matches: Vec::new(),
                detail_matches: Vec::new(),
                preview: None,
            }
        })
        .collect()
}

/// Scores one command row against a tokenized query without searching descriptions.
pub(crate) fn filter_score(item: &PickerItem, query: &str) -> Option<i64> {
    let matcher = SkimMatcherV2::default();
    let category = item.annotation.as_deref().unwrap_or_default().trim_end();
    let colon = item
        .data
        .get("colon")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let aliases = item
        .data
        .get("aliases")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    let shortcuts = item
        .data
        .get("shortcuts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();

    query.split_whitespace().try_fold(0, |total, token| {
        let shortcut_query = token.eq_ignore_ascii_case("space")
            || token.starts_with("Ctrl-")
            || token.starts_with("Alt-")
            || token.starts_with("Shift-");
        let score = if token.starts_with(':') {
            field_score(&matcher, colon, token, 5000)
        } else if shortcut_query {
            shortcuts
                .iter()
                .filter_map(|shortcut| field_score(&matcher, shortcut, token, 4500))
                .max()
        } else {
            field_score(&matcher, &item.label, token, 5000)
                .into_iter()
                .chain(field_score(&matcher, category, token, 4500))
                .chain(field_score(&matcher, colon, token, 4000))
                .chain(
                    aliases
                        .iter()
                        .filter_map(|alias| field_score(&matcher, alias, token, 3500)),
                )
                .chain(
                    shortcuts
                        .iter()
                        .filter_map(|shortcut| field_score(&matcher, shortcut, token, 2500)),
                )
                .max()
        }?;
        Some(total + score)
    })
}

fn field_score(matcher: &SkimMatcherV2, field: &str, token: &str, weight: i64) -> Option<i64> {
    if field.is_empty() {
        return None;
    }
    let field_lower = field.to_ascii_lowercase();
    let token_lower = token.to_ascii_lowercase();
    let contains = field_lower.contains(&token_lower);
    if token.chars().count() <= 3 && token.chars().all(char::is_alphanumeric) && !contains {
        return None;
    }
    let exact_bonus = if field.eq_ignore_ascii_case(token) {
        3000
    } else if field_lower.starts_with(&token_lower) {
        1500
    } else if contains {
        750
    } else {
        0
    };
    matcher
        .fuzzy_match(field, token)
        .map(|score| weight + exact_bonus + score)
}

fn primary_shortcut(shortcuts: &[String]) -> String {
    let Some(first) = shortcuts.first() else {
        return String::new();
    };
    if shortcuts.len() == 1 {
        first.clone()
    } else {
        format!("{first} +{}", shortcuts.len() - 1)
    }
}

/// Returns the immediate continuations for an active keymap prefix.
pub(crate) fn keymap_hints(
    prefix: &[String],
    mappings: &HashMap<String, KeyAction>,
) -> Vec<KeymapHint> {
    let mut hints = mappings
        .iter()
        .filter_map(|(key, action)| {
            let is_group = matches!(action, KeyAction::Nested(_));
            let label = if is_group {
                group_label(prefix, key)
            } else {
                key_action_label(action)?
            };
            Some(KeymapHint {
                key: display_key(key).to_string(),
                label,
                is_group,
            })
        })
        .collect::<Vec<_>>();

    hints.sort_unstable_by(|left, right| {
        left.is_group
            .cmp(&right.is_group)
            .then_with(|| left.key.to_lowercase().cmp(&right.key.to_lowercase()))
            .then_with(|| left.key.cmp(&right.key))
    });
    hints
}

fn builtin_commands() -> Vec<BuiltinCommand> {
    vec![
        builtin(
            "editor.command_palette",
            "All commands",
            "Editor",
            "Search available commands and keymaps",
            Some(":commands"),
            &[":command-palette"],
            Action::CommandPalette,
        ),
        builtin(
            "editor.config_diagnostics",
            "Configuration diagnostics",
            "Editor",
            "Review ignored settings and active fallbacks",
            Some(":config-diagnostics"),
            &[],
            Action::ConfigDiagnostics,
        ),
        builtin(
            "file.save",
            "Save file",
            "File",
            "Write the current buffer",
            Some(":w"),
            &[":write"],
            Action::Save,
        ),
        builtin(
            "file.save_and_quit",
            "Save and quit",
            "File",
            "Write the current buffer and quit",
            Some(":wq"),
            &[],
            Action::Command("wq".to_string()),
        ),
        builtin(
            "file.quit",
            "Quit",
            "File",
            "Quit when all buffers are saved",
            Some(":q"),
            &[":quit"],
            Action::Quit(false),
        ),
        builtin(
            "file.force_quit",
            "Force quit",
            "File",
            "Quit and discard unsaved changes",
            Some(":q!"),
            &[":quit!"],
            Action::Quit(true),
        ),
        builtin(
            "file.reload",
            "Reload file",
            "File",
            "Reload the current file from disk",
            Some(":e!"),
            &[":edit!"],
            Action::ReloadFile(true),
        ),
        builtin(
            "file.picker",
            "Find file",
            "File",
            "Open the file picker",
            None,
            &["file picker", "open file"],
            Action::FilePicker,
        ),
        builtin(
            "buffer.next",
            "Next buffer",
            "Buffer",
            "Switch to the next buffer",
            Some(":bn"),
            &[":buffer-next"],
            Action::NextBuffer,
        ),
        builtin(
            "buffer.previous",
            "Previous buffer",
            "Buffer",
            "Switch to the previous buffer",
            Some(":bp"),
            &[":buffer-prev"],
            Action::PreviousBuffer,
        ),
        builtin(
            "buffer.delete",
            "Delete buffer",
            "Buffer",
            "Close the current buffer",
            Some(":bd"),
            &[":bdelete", ":buffer-delete"],
            Action::DeleteBuffer(false),
        ),
        builtin(
            "edit.undo",
            "Undo",
            "Edit",
            "Undo the last edit",
            None,
            &["revert"],
            Action::Undo,
        ),
        builtin(
            "edit.redo",
            "Redo",
            "Edit",
            "Redo the last undone edit",
            None,
            &[],
            Action::Redo,
        ),
        builtin(
            "edit.repeat",
            "Repeat last change",
            "Edit",
            "Repeat the last semantic edit",
            None,
            &["dot repeat"],
            Action::RepeatLastChange,
        ),
        builtin(
            "edit.join",
            "Join lines",
            "Edit",
            "Join the current and next line",
            Some(":join"),
            &[":j"],
            Action::JoinLines(2),
        ),
        builtin(
            "edit.join_keep_spaces",
            "Join lines without trimming",
            "Edit",
            "Join lines and preserve whitespace",
            Some(":join!"),
            &[":j!"],
            Action::JoinLinesKeepSpaces(2),
        ),
        builtin(
            "search.forward",
            "Search forward",
            "Search",
            "Search toward the end of the buffer",
            None,
            &["find next"],
            Action::EnterSearch(SearchDirection::Forward),
        ),
        builtin(
            "search.backward",
            "Search backward",
            "Search",
            "Search toward the start of the buffer",
            None,
            &["find previous"],
            Action::EnterSearch(SearchDirection::Backward),
        ),
        builtin(
            "search.clear",
            "Clear search highlights",
            "Search",
            "Remove visible search highlights",
            Some(":noh"),
            &[":nohlsearch"],
            Action::ClearSearchHighlight,
        ),
        builtin(
            "view.toggle_wrap",
            "Toggle line wrapping",
            "View",
            "Toggle wrapping for long lines",
            None,
            &["wrap", "nowrap"],
            Action::ToggleWrap,
        ),
        builtin(
            "view.enable_wrap",
            "Enable line wrapping",
            "View",
            "Wrap long lines at the window edge",
            Some(":wrap"),
            &[],
            Action::SetWrap(true),
        ),
        builtin(
            "view.disable_wrap",
            "Disable line wrapping",
            "View",
            "Scroll horizontally instead of wrapping",
            Some(":nowrap"),
            &[],
            Action::SetWrap(false),
        ),
        builtin(
            "window.split_horizontal",
            "Split horizontally",
            "Window",
            "Create a horizontal split",
            Some(":sp"),
            &[":split"],
            Action::SplitHorizontal,
        ),
        builtin(
            "window.split_vertical",
            "Split vertically",
            "Window",
            "Create a vertical split",
            Some(":vs"),
            &[":vsplit"],
            Action::SplitVertical,
        ),
        builtin(
            "window.close",
            "Close window",
            "Window",
            "Close the active split",
            Some(":close"),
            &[],
            Action::CloseWindow,
        ),
        builtin(
            "window.only",
            "Keep only current window",
            "Window",
            "Close every other split",
            Some(":only"),
            &[],
            Action::OnlyWindow,
        ),
        builtin(
            "window.next",
            "Next window",
            "Window",
            "Focus the next split",
            None,
            &[],
            Action::NextWindow,
        ),
        builtin(
            "window.previous",
            "Previous window",
            "Window",
            "Focus the previous split",
            None,
            &[],
            Action::PreviousWindow,
        ),
        builtin(
            "window.left",
            "Focus window left",
            "Window",
            "Move focus to the split on the left",
            None,
            &[],
            Action::MoveWindowLeft,
        ),
        builtin(
            "window.down",
            "Focus window below",
            "Window",
            "Move focus to the split below",
            None,
            &[],
            Action::MoveWindowDown,
        ),
        builtin(
            "window.up",
            "Focus window above",
            "Window",
            "Move focus to the split above",
            None,
            &[],
            Action::MoveWindowUp,
        ),
        builtin(
            "window.right",
            "Focus window right",
            "Window",
            "Move focus to the split on the right",
            None,
            &[],
            Action::MoveWindowRight,
        ),
        builtin(
            "window.balance",
            "Balance windows",
            "Window",
            "Give all splits equal space",
            None,
            &[],
            Action::BalanceWindows,
        ),
        builtin(
            "window.maximize",
            "Maximize window",
            "Window",
            "Maximize the active split",
            None,
            &[],
            Action::MaximizeWindow,
        ),
        builtin(
            "lsp.definition",
            "Go to definition",
            "LSP",
            "Jump to the symbol definition",
            None,
            &["definition"],
            Action::GoToDefinition,
        ),
        builtin(
            "lsp.hover",
            "Show hover documentation",
            "LSP",
            "Show documentation for the symbol under the cursor",
            None,
            &["hover"],
            Action::Hover,
        ),
        builtin(
            "lsp.format",
            "Format document",
            "LSP",
            "Format the current document",
            None,
            &["formatter"],
            Action::FormatDocument,
        ),
        builtin(
            "lsp.code_action",
            "Show code actions",
            "LSP",
            "Show applicable fixes and refactors",
            None,
            &["quick fix", "refactor"],
            Action::CodeAction,
        ),
        builtin(
            "lsp.rename",
            "Rename symbol",
            "LSP",
            "Rename the symbol under the cursor",
            None,
            &["rename"],
            Action::StartRename,
        ),
        builtin(
            "lsp.signature",
            "Show signature help",
            "LSP",
            "Show the active call signature",
            None,
            &["signature"],
            Action::SignatureHelp,
        ),
        builtin(
            "debug.buffer",
            "Dump buffer",
            "Debug",
            "Show the current buffer state",
            Some(":db"),
            &[],
            Action::DumpBuffer,
        ),
        builtin(
            "debug.history",
            "Dump history",
            "Debug",
            "Show editor action history",
            Some(":dh"),
            &[],
            Action::DumpHistory,
        ),
        builtin(
            "debug.diagnostics",
            "Dump diagnostics",
            "Debug",
            "Show language-server diagnostics",
            Some(":di"),
            &[],
            Action::DumpDiagnostics,
        ),
        builtin(
            "debug.capabilities",
            "Dump LSP capabilities",
            "Debug",
            "Show language-server capabilities",
            Some(":dc"),
            &[],
            Action::DumpCapabilities,
        ),
        builtin(
            "debug.timers",
            "Dump plugin timers",
            "Debug",
            "Show plugin timer statistics",
            Some(":dt"),
            &[],
            Action::DumpTimers,
        ),
        builtin(
            "debug.logs",
            "View logs",
            "Debug",
            "Open the editor log",
            None,
            &["log file"],
            Action::ViewLogs,
        ),
        builtin(
            "debug.plugins",
            "List plugins",
            "Debug",
            "Show loaded plugin information",
            None,
            &["plugin list"],
            Action::ListPlugins,
        ),
        builtin(
            "debug.registers",
            "Show registers",
            "Debug",
            "Show register contents",
            Some(":registers"),
            &[],
            Action::PrintRegisters,
        ),
    ]
}

fn builtin(
    id: &'static str,
    title: &'static str,
    category: &'static str,
    description: &'static str,
    colon: Option<&'static str>,
    aliases: &'static [&'static str],
    action: Action,
) -> BuiltinCommand {
    BuiltinCommand {
        id,
        title,
        category,
        description,
        colon,
        aliases,
        action,
    }
}

fn shortcuts_for_action(keys: &Keys, target: &Action) -> Vec<String> {
    let tables = [
        ("Normal", &keys.normal),
        ("Insert", &keys.insert),
        ("Visual", &keys.visual),
        ("Visual line", &keys.visual_line),
        ("Visual block", &keys.visual_block),
        ("Command", &keys.command),
    ];
    let mut shortcuts = Vec::new();
    for (mode, mappings) in tables {
        let mut paths = Vec::new();
        collect_shortcuts(mappings, target, &mut Vec::new(), &mut paths);
        paths.sort_unstable_by_key(|path| (path.len(), path.join(" ")));
        shortcuts.extend(paths.into_iter().map(|path| {
            let sequence = path.join(" ");
            if mode == "Normal" {
                sequence
            } else {
                format!("{mode}: {sequence}")
            }
        }));
    }
    shortcuts
}

fn collect_shortcuts(
    mappings: &HashMap<String, KeyAction>,
    target: &Action,
    prefix: &mut Vec<String>,
    output: &mut Vec<Vec<String>>,
) {
    for (key, action) in mappings {
        prefix.push(display_key(key).to_string());
        match action {
            KeyAction::Single(action) if action == target => output.push(prefix.clone()),
            KeyAction::Multiple(actions)
                if actions.len() == 1 && actions.first().is_some_and(|action| action == target) =>
            {
                output.push(prefix.clone());
            }
            KeyAction::Nested(mappings) => collect_shortcuts(mappings, target, prefix, output),
            KeyAction::Repeating(_, action) if key_action_is(action, target) => {
                output.push(prefix.clone());
            }
            KeyAction::None
            | KeyAction::Single(_)
            | KeyAction::Multiple(_)
            | KeyAction::Repeating(_, _) => {}
        }
        prefix.pop();
    }
}

fn key_action_is(action: &KeyAction, target: &Action) -> bool {
    matches!(action, KeyAction::Single(action) if action == target)
        || matches!(
            action,
            KeyAction::Multiple(actions)
                if actions.len() == 1 && actions.first().is_some_and(|action| action == target)
        )
}

fn key_action_label(action: &KeyAction) -> Option<String> {
    match action {
        KeyAction::None => None,
        KeyAction::Single(action) => Some(action_label(action)),
        KeyAction::Multiple(actions)
            if matches!(
                actions.as_slice(),
                [
                    Action::MoveToTop,
                    Action::EnterMode(Mode::VisualLine),
                    Action::MoveToBottom
                ]
            ) =>
        {
            Some("Select all".to_string())
        }
        KeyAction::Multiple(actions) => Some(
            actions
                .iter()
                .map(action_label)
                .collect::<Vec<_>>()
                .join(" then "),
        ),
        KeyAction::Nested(_) => Some("More keymaps".to_string()),
        KeyAction::Repeating(_, action) => key_action_label(action),
    }
}

fn action_label(action: &Action) -> String {
    match action {
        Action::CommandPalette => "All commands".to_string(),
        Action::ConfigDiagnostics => "Configuration diagnostics".to_string(),
        Action::PluginCommand(name) => humanize_identifier(name),
        Action::Save => "Save file".to_string(),
        Action::Quit(_) => "Quit".to_string(),
        Action::Undo => "Undo".to_string(),
        Action::Redo => "Redo".to_string(),
        Action::RepeatLastChange => "Repeat last change".to_string(),
        Action::NextBuffer => "Next buffer".to_string(),
        Action::PreviousBuffer => "Previous buffer".to_string(),
        Action::FilePicker => "Find file".to_string(),
        Action::GoToDefinition => "Go to definition".to_string(),
        Action::FormatDocument => "Format document".to_string(),
        Action::CodeAction => "Show code actions".to_string(),
        Action::StartRename => "Rename symbol".to_string(),
        Action::Hover => "Show hover documentation".to_string(),
        Action::SignatureHelp => "Show signature help".to_string(),
        Action::ClearSearchHighlight => "Clear search highlights".to_string(),
        Action::ToggleWrap => "Toggle line wrapping".to_string(),
        Action::SplitHorizontal => "Split horizontally".to_string(),
        Action::SplitVertical => "Split vertically".to_string(),
        Action::CloseWindow => "Close window".to_string(),
        Action::OnlyWindow => "Keep only current window".to_string(),
        Action::BalanceWindows => "Balance windows".to_string(),
        Action::MaximizeWindow => "Maximize window".to_string(),
        Action::NextWindow => "Next window".to_string(),
        Action::PreviousWindow => "Previous window".to_string(),
        Action::MoveWindowLeft => "Focus window left".to_string(),
        Action::MoveWindowDown => "Focus window below".to_string(),
        Action::MoveWindowUp => "Focus window above".to_string(),
        Action::MoveWindowRight => "Focus window right".to_string(),
        Action::ViewLogs => "View logs".to_string(),
        Action::ListPlugins => "List plugins".to_string(),
        Action::DumpBuffer => "Dump buffer".to_string(),
        Action::DumpDiagnostics => "Dump diagnostics".to_string(),
        Action::DumpCapabilities => "Dump LSP capabilities".to_string(),
        Action::DumpHistory => "Dump history".to_string(),
        _ => humanize_identifier(&action_variant_name(action)),
    }
}

fn action_variant_name(action: &Action) -> String {
    match serde_json::to_value(action).ok() {
        Some(serde_json::Value::String(name)) => name,
        Some(serde_json::Value::Object(value)) => value
            .into_iter()
            .next()
            .map(|(name, _)| name)
            .unwrap_or_else(|| "Action".to_string()),
        _ => "Action".to_string(),
    }
}

fn group_label(prefix: &[String], key: &str) -> String {
    let prefix = prefix.join(" ");
    match (prefix.as_str(), display_key(key)) {
        ("Space", "h") => "Git hunks".to_string(),
        ("Space", "c") => "Commit message".to_string(),
        ("Space", "d") => "Debug".to_string(),
        (_, "Ctrl-w") => "Windows".to_string(),
        (_, "g") => "Go to".to_string(),
        (_, "z") => "View".to_string(),
        (_, "[") => "Previous".to_string(),
        (_, "]") => "Next".to_string(),
        _ => "More keymaps".to_string(),
    }
}

fn display_key(key: &str) -> &str {
    if key == " " {
        "Space"
    } else {
        key
    }
}

fn colon_name_is_builtin(name: &str) -> bool {
    matches!(
        name,
        "commands"
            | "command-palette"
            | "db"
            | "dh"
            | "di"
            | "dc"
            | "dt"
            | "registers"
            | "undotree"
            | "j"
            | "join"
    ) || command::parse(BUILTIN_COLON_COMMANDS, name).is_some()
}

fn humanize_identifier(identifier: &str) -> String {
    let characters = identifier.chars().collect::<Vec<_>>();
    let mut words = Vec::new();
    let mut start = 0;
    for index in 1..characters.len() {
        let previous = characters[index - 1];
        let current = characters[index];
        let next = characters.get(index + 1).copied();
        let boundary = current.is_ascii_uppercase()
            && (previous.is_ascii_lowercase()
                || next.is_some_and(|character| character.is_ascii_lowercase()));
        if boundary {
            words.push(characters[start..index].iter().collect::<String>());
            start = index;
        }
    }
    words.push(characters[start..].iter().collect::<String>());

    words
        .into_iter()
        .enumerate()
        .map(|(index, word)| match word.as_str() {
            "Lsp" | "LSP" => "LSP".to_string(),
            _ if index == 0 => word,
            _ => word.to_lowercase(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use husk::CommandMetadata;

    use super::*;

    fn default_keys() -> Keys {
        let config: crate::config::Config =
            toml::from_str(include_str!("../default_config.toml")).unwrap();
        config.keys
    }

    #[test]
    fn palette_lists_builtin_shortcuts_and_colon_aliases() {
        let entries = entries(&default_keys(), &[]);

        let format = entries
            .iter()
            .find(|entry| entry.id == "lsp.format")
            .unwrap();
        assert_eq!(format.category, "LSP");
        assert_eq!(format.title, "Format document");
        assert!(format
            .shortcuts
            .iter()
            .any(|shortcut| shortcut == "Space f"));

        let save = entries
            .iter()
            .find(|entry| entry.id == "file.save")
            .unwrap();
        assert_eq!(save.colon.as_deref(), Some(":w"));
        assert!(save.aliases.iter().any(|alias| alias == ":write"));
    }

    #[test]
    fn palette_uses_effective_user_keymap() {
        let mut keys = default_keys();
        let Some(KeyAction::Nested(leader)) = keys.normal.get_mut(" ") else {
            panic!("expected leader keymap");
        };
        leader.remove("f");
        keys.normal.insert(
            "Ctrl-s".to_string(),
            KeyAction::Single(Action::FormatDocument),
        );

        let entries = entries(&keys, &[]);
        let format = entries
            .iter()
            .find(|entry| entry.id == "lsp.format")
            .unwrap();

        assert!(format.shortcuts.iter().any(|shortcut| shortcut == "Ctrl-s"));
        assert!(!format
            .shortcuts
            .iter()
            .any(|shortcut| shortcut == "Space f"));
    }

    #[test]
    fn palette_lists_plugin_metadata_shortcut_and_exact_colon_command() {
        let plugin = RegisteredPluginCommand {
            name: "ProjectSearch".to_string(),
            plugin: "project_search".to_string(),
            metadata: CommandMetadata {
                title: Some("Search project".to_string()),
                category: Some("Search".to_string()),
                description: Some("Find text across the workspace".to_string()),
                aliases: vec!["ripgrep".to_string()],
            },
        };

        let entries = entries(&default_keys(), &[plugin]);
        let project_search = entries
            .iter()
            .find(|entry| entry.id == "plugin.project_search.ProjectSearch")
            .unwrap();

        assert_eq!(project_search.category, "Search");
        assert_eq!(project_search.title, "Search project");
        assert!(project_search
            .shortcuts
            .iter()
            .any(|shortcut| shortcut == "Space g"));
        assert_eq!(project_search.colon.as_deref(), Some(":ProjectSearch"));
        assert!(project_search
            .aliases
            .iter()
            .any(|alias| alias == "ripgrep"));
    }

    #[test]
    fn palette_does_not_advertise_shadowed_plugin_colon_command() {
        let plugin = RegisteredPluginCommand {
            name: "wrap".to_string(),
            plugin: "custom".to_string(),
            metadata: CommandMetadata {
                title: Some("Custom wrap".to_string()),
                ..CommandMetadata::default()
            },
        };

        let entries = entries(&default_keys(), &[plugin]);
        let custom = entries
            .iter()
            .find(|entry| entry.id == "plugin.custom.wrap")
            .unwrap();

        assert_eq!(custom.colon, None);
    }

    #[test]
    fn palette_items_keep_category_shortcut_colon_and_description_separate() {
        let entries = entries(&default_keys(), &[]);
        let items = picker_items(&entries);
        let format = items.iter().find(|item| item.id == "lsp.format").unwrap();
        let save = items.iter().find(|item| item.id == "file.save").unwrap();

        assert_eq!(format.kind.as_deref(), Some("Command"));
        assert_eq!(format.label, "Format document");
        assert_eq!(format.annotation.as_deref().map(str::trim), Some("LSP"));
        assert!(format
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("Space f")));
        assert_eq!(
            format.data["description"].as_str(),
            Some("Format the current document")
        );
        assert_eq!(save.data["colon"].as_str(), Some(":w"));
        assert_eq!(format.data["primary_shortcut"].as_str(), Some("Space f"));
        assert!(save
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(":w")));
    }

    #[test]
    fn palette_filter_matches_command_fields_without_matching_description_noise() {
        let plugins = [
            RegisteredPluginCommand {
                name: "GitDashboard".to_string(),
                plugin: "git".to_string(),
                metadata: CommandMetadata {
                    title: Some("Open Git dashboard".to_string()),
                    category: Some("Git".to_string()),
                    description: Some("Inspect and manage workspace changes".to_string()),
                    aliases: vec!["source control".to_string()],
                },
            },
            RegisteredPluginCommand {
                name: "Unrelated".to_string(),
                plugin: "other".to_string(),
                metadata: CommandMetadata {
                    title: Some("Go to definition".to_string()),
                    category: Some("Other".to_string()),
                    description: Some("Get information together".to_string()),
                    aliases: vec![],
                },
            },
        ];
        let items = picker_items(&entries(&default_keys(), &plugins));
        let git = items
            .iter()
            .find(|item| item.id == "plugin.git.GitDashboard")
            .unwrap();
        let unrelated = items
            .iter()
            .find(|item| item.id == "plugin.other.Unrelated")
            .unwrap();

        assert!(filter_score(git, "git").is_some());
        assert_eq!(filter_score(unrelated, "git"), None);
        assert!(filter_score(git, ":GitDash").is_some());
        assert!(filter_score(git, "Space G").is_some());
        assert!(filter_score(git, "source control").is_some());
        assert!(filter_score(git, "git dashboard").is_some());
    }

    #[test]
    fn keymap_hints_describe_leaf_commands_and_nested_groups() {
        let keys = default_keys();
        let Some(KeyAction::Nested(leader)) = keys.normal.get(" ") else {
            panic!("expected leader keymap");
        };

        let hints = keymap_hints(&["Space".to_string()], leader);
        assert!(hints
            .iter()
            .any(|hint| hint.key == "f" && hint.label == "Format document" && !hint.is_group));
        assert!(hints
            .iter()
            .any(|hint| hint.key == "h" && hint.label == "Git hunks" && hint.is_group));
        assert!(hints
            .iter()
            .any(|hint| hint.key == "a" && hint.label == "Select all"));
    }

    #[test]
    fn humanizes_camel_case_plugin_names() {
        assert_eq!(humanize_identifier("ProjectSearch"), "Project search");
        assert_eq!(
            humanize_identifier("LspWorkspaceSymbols"),
            "LSP workspace symbols"
        );
        assert_eq!(humanize_identifier("GitHunkStage"), "Git hunk stage");
    }
}
