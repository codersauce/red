//! Side-effect-free completion for Red's colon command line.
//!
//! The editor owns input and execution. This module selects a completion source,
//! replaces one argument, and retains the original candidate set while cycling.

use std::{fs, ops::Range, path::PathBuf};

use crate::{
    command, command_palette, copilot::CopilotCommand, plugin::RegisteredPluginCommand,
    utils::expand_user_path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompletionDirection {
    Next,
    Previous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCompletionState {
    replacement: Range<usize>,
    candidates: Vec<String>,
    selected: usize,
    needs_leading_space: bool,
}

impl CommandCompletionState {
    fn apply(&mut self, line: &mut String) {
        let candidate = &self.candidates[self.selected];
        let replacement = if self.needs_leading_space {
            format!(" {candidate}")
        } else {
            candidate.clone()
        };
        line.replace_range(self.replacement.clone(), &replacement);
        self.replacement.end = self.replacement.start + replacement.len();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct ArgumentContext<'a> {
    command: &'a str,
    command_end: usize,
    preceding: Vec<&'a str>,
    fragment: &'a str,
    replacement: Range<usize>,
    needs_leading_space: bool,
}

impl<'a> ArgumentContext<'a> {
    fn parse_at(line: &'a str, offset: usize) -> Option<Self> {
        let start = line.find(|ch: char| !ch.is_whitespace())?;
        let end = line[start..]
            .find(char::is_whitespace)
            .map_or(line.len(), |offset| start + offset);
        let command = &line[start..end];
        if end == line.len() {
            return Some(Self {
                command,
                command_end: offset + end,
                preceding: Vec::new(),
                fragment: "",
                replacement: offset + end..offset + end,
                needs_leading_space: true,
            });
        }
        let argument_start = line[end..]
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(end, |(offset, ch)| end + offset + ch.len_utf8());
        Some(Self {
            command,
            command_end: offset + end,
            preceding: line[end..argument_start].split_whitespace().collect(),
            fragment: &line[argument_start..],
            replacement: offset + argument_start..offset + line.len(),
            needs_leading_space: false,
        })
    }
}

fn bufdo_nested_start(line: &str) -> Option<usize> {
    let mut command_start = line.find(|ch: char| !ch.is_whitespace())?;
    let bytes = line.as_bytes();
    if bytes.get(command_start) == Some(&b'%') {
        command_start += 1;
    } else {
        let first_end = command_start
            + bytes[command_start..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
        if first_end > command_start {
            command_start = first_end;
            if bytes.get(command_start) == Some(&b',') {
                command_start += 1;
                command_start += bytes[command_start..]
                    .iter()
                    .take_while(|byte| byte.is_ascii_digit())
                    .count();
            }
        }
    }
    command_start += line[command_start..]
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>();
    let command_end = line[command_start..]
        .find(char::is_whitespace)
        .map_or(line.len(), |offset| command_start + offset);
    if command_end == line.len() {
        return None;
    }
    let parsed = command::parse(
        command_palette::BUILTIN_COLON_COMMANDS,
        &line[command_start..command_end],
    )?;
    if parsed.commands.as_slice() != ["bufdo"] {
        return None;
    }
    Some(
        command_end
            + line[command_end..]
                .chars()
                .take_while(|ch| ch.is_whitespace())
                .map(char::len_utf8)
                .sum::<usize>(),
    )
}

enum Source {
    Choices(Vec<String>),
    Files,
    Syntax,
}

fn choices(values: &[&str]) -> Source {
    Source::Choices(values.iter().map(|value| (*value).to_string()).collect())
}

fn builtin_source(name: &str, argument_index: usize) -> Option<Source> {
    // Use the execution parser's abbreviations, but never complete a command chain.
    let parsed = command::parse(
        command_palette::BUILTIN_COLON_COMMANDS,
        name.trim_end_matches('!'),
    );
    let canonical = match parsed.as_ref().map(|parsed| parsed.commands.as_slice()) {
        Some([command]) => command.as_str(),
        _ => name,
    };
    match canonical {
        "edit"
        | "write"
        | "saveas"
        | "file"
        | "new"
        | "vnew"
        | "split"
        | "sp"
        | "vsplit"
        | "vs"
        | "InlineHistoryExport" => Some(Source::Files),
        _ if argument_index != 0 => None,
        "syntax" | "syn" | "ft" => Some(Source::Syntax),
        "set" => Some(choices(command::SET_OPTIONS)),
        "languages" => Some(choices(command::LANGUAGE_COMMANDS)),
        "Copilot" => Some(Source::Choices(
            CopilotCommand::ALL
                .iter()
                .map(|(_, name)| (*name).to_string())
                .collect(),
        )),
        _ => None,
    }
}

fn source(context: &ArgumentContext<'_>, plugins: &[RegisteredPluginCommand]) -> Option<Source> {
    if command_palette::colon_name_is_builtin(context.command) {
        return builtin_source(context.command, context.preceding.len());
    }
    plugins
        .iter()
        .find(|command| {
            command.name == context.command
                && command.metadata.visible
                && command.metadata.arguments
        })?
        .metadata
        .completions
        .get(context.preceding.len())
        .cloned()
        .map(Source::Choices)
}

/// Completes without invoking commands, plugin callbacks, or external processes.
pub(crate) fn complete(
    state: &mut Option<CommandCompletionState>,
    line: &mut String,
    direction: CompletionDirection,
    plugins: &[RegisteredPluginCommand],
    language_matches: impl FnOnce(&str) -> Vec<String>,
) {
    if let Some(mut current) = state.take() {
        if current.candidates.len() > 1 {
            current.selected = match direction {
                CompletionDirection::Next => (current.selected + 1) % current.candidates.len(),
                CompletionDirection::Previous => current
                    .selected
                    .checked_sub(1)
                    .unwrap_or(current.candidates.len() - 1),
            };
            current.apply(line);
            *state = Some(current);
            return;
        }
    }
    let nested_start = bufdo_nested_start(line);
    if nested_start == Some(line.len()) {
        let candidates = command_palette::colon_completion_names(plugins)
            .into_iter()
            .filter(|name| name != "bufdo")
            .collect::<Vec<_>>();
        let selected = match direction {
            CompletionDirection::Next => 0,
            CompletionDirection::Previous => candidates.len() - 1,
        };
        let mut current = CommandCompletionState {
            replacement: line.len()..line.len(),
            candidates,
            selected,
            needs_leading_space: false,
        };
        current.apply(line);
        *state = Some(current);
        return;
    }
    let (completion_line, offset) =
        nested_start.map_or((&line[..], 0), |start| (&line[start..], start));
    let Some(context) = ArgumentContext::parse_at(completion_line, offset) else {
        return;
    };
    let argument_source = source(&context, plugins);
    let command_name = context.command.trim_end_matches('!');
    let accepts_bare_arguments = command_name.len() == 1
        || command_palette::BUILTIN_COLON_COMMANDS
            .iter()
            .any(|command| command.name() == command_name);
    // Preserve established bare-command argument shortcuts without letting a newly
    // executable prefix, such as :wr, take over command-name completion.
    let (replacement, needs_leading_space, candidates) = if context.needs_leading_space
        && (!matches!(argument_source, Some(Source::Files | Source::Syntax))
            || !accepts_bare_arguments)
    {
        let start = context.replacement.start - context.command.len();
        let names = command_palette::colon_completion_names(plugins)
            .into_iter()
            .filter(|name| {
                name.starts_with(context.command) && (nested_start.is_none() || name != "bufdo")
            })
            .collect();
        (start..line.len(), false, names)
    } else {
        // File completion has historically owned the entire remaining path,
        // including spaces. Keep that behavior separate from positional choices.
        let (replacement, needs_leading_space, fragment) =
            if matches!(argument_source, Some(Source::Files)) {
                match line[context.command_end..].find(|ch: char| !ch.is_whitespace()) {
                    Some(offset) => {
                        let start = context.command_end + offset;
                        (start..line.len(), false, &line[start..])
                    }
                    None => (context.command_end..line.len(), true, ""),
                }
            } else {
                (
                    context.replacement,
                    context.needs_leading_space,
                    context.fragment,
                )
            };
        let candidates = match argument_source {
            Some(Source::Files) => path_candidates(fragment),
            Some(Source::Syntax) => {
                let fragment = fragment.to_ascii_lowercase();
                let mut candidates = ["auto", "off"]
                    .into_iter()
                    .filter(|name| name.starts_with(&fragment))
                    .map(str::to_string)
                    .chain(language_matches(&fragment))
                    .collect::<Vec<_>>();
                candidates.sort_unstable();
                candidates.dedup();
                candidates
            }
            Some(Source::Choices(values)) => {
                let mut candidates = values
                    .into_iter()
                    .filter(|value| value.starts_with(fragment))
                    .collect::<Vec<_>>();
                candidates.sort_unstable();
                candidates.dedup();
                candidates
            }
            None => return,
        };
        (replacement, needs_leading_space, candidates)
    };
    if candidates.is_empty() {
        return;
    }
    let selected = match direction {
        CompletionDirection::Next => 0,
        CompletionDirection::Previous => candidates.len() - 1,
    };
    let mut current = CommandCompletionState {
        replacement,
        candidates,
        selected,
        needs_leading_space,
    };
    current.apply(line);
    *state = Some(current);
}

fn path_candidates(fragment: &str) -> Vec<String> {
    match fragment {
        "." => return vec!["./".into()],
        ".." => return vec!["../".into()],
        "~" if expand_user_path("~").is_ok() => return vec!["~/".into()],
        _ => {}
    }
    let (directory, prefix) = fragment.rfind('/').map_or(("", fragment), |index| {
        (&fragment[..=index], &fragment[index + 1..])
    });
    let path = if directory.is_empty() {
        PathBuf::from(".")
    } else {
        expand_user_path(directory).unwrap_or_else(|_| PathBuf::from(directory))
    };
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    let mut candidates = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) {
                return None;
            }
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let suffix = if is_dir { "/" } else { "" };
            Some((!is_dir, format!("{directory}{name}{suffix}")))
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.into_iter().map(|(_, name)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::CommandMetadata;

    fn tab(
        state: &mut Option<CommandCompletionState>,
        line: &mut String,
        direction: CompletionDirection,
        plugins: &[RegisteredPluginCommand],
    ) {
        complete(state, line, direction, plugins, |fragment| {
            ["rust", "yaml"]
                .into_iter()
                .filter(|name| name.starts_with(fragment))
                .map(str::to_string)
                .collect()
        });
    }

    fn completed(input: &str) -> String {
        let mut line = input.to_string();
        tab(&mut None, &mut line, CompletionDirection::Next, &[]);
        line
    }

    fn plugin(name: &str, choices: &[&[&str]]) -> RegisteredPluginCommand {
        RegisteredPluginCommand {
            name: name.into(),
            plugin: "test".into(),
            metadata: CommandMetadata {
                arguments: true,
                completions: choices
                    .iter()
                    .map(|values| values.iter().map(|value| (*value).to_string()).collect())
                    .collect(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn builtin_arguments_complete_without_execution() {
        for (input, expected) in [
            ("Copilot en", "Copilot enable"),
            ("Copilot ", "Copilot complete"),
            ("Copilot", "Copilot"),
            ("Copilot enable ", "Copilot enable "),
            ("Copilot Enable", "Copilot Enable"),
            ("copilot en", "copilot en"),
            ("languages re", "languages reload"),
            ("set norn", "set nornu"),
            ("se norn", "se nornu"),
            ("set rnu ", "set rnu "),
            ("syntax RU", "syntax rust"),
            ("sy ru", "sy rust"),
            ("syn ru", "syn rust"),
            ("bufdo synt", "bufdo syntax"),
            ("bufdo syntax RU", "bufdo syntax rust"),
            ("bufd! synt", "bufd! syntax"),
            ("2,4bufdo synt", "2,4bufdo syntax"),
            ("ft ", "ft auto"),
            ("syntax rust extra", "syntax rust extra"),
            ("q sr", "q sr"),
            ("wq sr", "wq sr"),
            ("  Copilot   en", "  Copilot   enable"),
        ] {
            assert_eq!(completed(input), expected, "{input:?}");
        }
    }

    #[test]
    fn cycles_original_choices_in_both_directions() {
        let mut state = None;
        let mut line = "Copilot sign".to_string();
        tab(&mut state, &mut line, CompletionDirection::Next, &[]);
        assert_eq!(line, "Copilot signin");
        tab(&mut state, &mut line, CompletionDirection::Next, &[]);
        assert_eq!(line, "Copilot signout");
        tab(&mut state, &mut line, CompletionDirection::Previous, &[]);
        assert_eq!(line, "Copilot signin");
        state = None;
        line = "Copilot sign".into();
        tab(&mut state, &mut line, CompletionDirection::Previous, &[]);
        assert_eq!(line, "Copilot signout");
    }

    #[test]
    fn command_names_keep_their_existing_cycle() {
        let mut state = None;
        let mut line = "wr".to_string();
        tab(&mut state, &mut line, CompletionDirection::Next, &[]);
        assert_eq!(line, "wrap");
        tab(&mut state, &mut line, CompletionDirection::Next, &[]);
        assert_eq!(line, "write");
        tab(&mut state, &mut line, CompletionDirection::Previous, &[]);
        assert_eq!(line, "wrap");
        assert_eq!(completed("Wr"), "Wr");
    }

    #[test]
    fn plugin_positions_replace_only_the_active_unicode_argument() {
        let plugins = [plugin(
            "Service",
            &[&["enable", "disable"], &["équipe", "écran"]],
        )];
        let mut state = None;
        let mut line = "Service enable  é".to_string();
        tab(&mut state, &mut line, CompletionDirection::Next, &plugins);
        assert_eq!(line, "Service enable  écran");
        tab(&mut state, &mut line, CompletionDirection::Next, &plugins);
        assert_eq!(line, "Service enable  équipe");
        state = None;
        line.push(' ');
        let original = line.clone();
        tab(&mut state, &mut line, CompletionDirection::Next, &plugins);
        assert_eq!(line, original);
    }

    #[test]
    fn hidden_legacy_and_shadowed_plugin_commands_have_no_argument_choices() {
        let mut hidden = plugin("Hidden", &[&["value"]]);
        hidden.metadata.visible = false;
        let mut legacy = plugin("Legacy", &[&["value"]]);
        legacy.metadata.arguments = false;
        let plugins = [
            hidden,
            legacy,
            plugin("Copilot", &[&["evil"]]),
            plugin("quit", &[&["value"]]),
        ];
        for input in ["Hidden v", "Legacy v", "Copilot ev", "quit v"] {
            let mut line = input.to_string();
            tab(&mut None, &mut line, CompletionDirection::Next, &plugins);
            assert_eq!(line, input);
        }
    }

    #[test]
    fn file_completion_preserves_aliases_prefixes_and_directory_order() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("zdir")).unwrap();
        fs::write(directory.path().join("afile"), "").unwrap();
        for command in [
            "e", "ed", "edit", "w", "wr", "wri", "write", "sav", "saveas", "fi", "file", "new",
            "vne", "vnew", "sp", "spl", "split", "vs", "vsp", "vsplit", "e!",
        ] {
            let mut state = None;
            let mut line = format!("{command} {}/", directory.path().display());
            tab(&mut state, &mut line, CompletionDirection::Next, &[]);
            assert_eq!(
                line,
                format!("{command} {}/zdir/", directory.path().display())
            );
            tab(&mut state, &mut line, CompletionDirection::Next, &[]);
            assert_eq!(
                line,
                format!("{command} {}/afile", directory.path().display())
            );
        }
        assert_eq!(completed("e ."), "e ./");
        assert_eq!(completed("e .."), "e ../");
    }

    #[test]
    fn file_completion_keeps_paths_containing_spaces() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("a directory")).unwrap();
        fs::write(directory.path().join("a directory/file.rs"), "").unwrap();
        let mut state = None;
        let mut line = format!("e {}/a d", directory.path().display());
        tab(&mut state, &mut line, CompletionDirection::Next, &[]);
        assert_eq!(
            line,
            format!("e {}/a directory/", directory.path().display())
        );
        tab(&mut state, &mut line, CompletionDirection::Next, &[]);
        assert_eq!(
            line,
            format!("e {}/a directory/file.rs", directory.path().display())
        );
    }

    #[test]
    fn context_preserves_whitespace_and_byte_boundaries() {
        let context = ArgumentContext::parse_at("Service  one\u{2003}é", 0).unwrap();
        assert_eq!(context.preceding, ["one"]);
        assert_eq!(context.fragment, "é");
        assert_eq!(&"Service  one\u{2003}é"[context.replacement], "é");
    }
}
