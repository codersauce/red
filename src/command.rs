//! Parsing of built-in colon-command abbreviations, arguments, and force flags.
//!
//! Parsing receives authoritative command specifications from the caller. Exact names,
//! conventional initial-based aliases, explicitly bounded Vim-style prefixes, and the
//! `wq` command chain are resolved before arguments are returned. Ambiguous or unknown
//! names never decompose into unrelated commands, preventing unintended editor changes.

/// Accepted values for the first `:set` argument.
pub(crate) const SET_OPTIONS: &[&str] = &["relativenumber", "rnu", "norelativenumber", "nornu"];

/// Accepted `:languages` operations.
pub(crate) const LANGUAGE_COMMANDS: &[&str] = &["reload"];

/// A built-in command and the shortest prefix permitted to invoke it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    name: &'static str,
    min_prefix_len: usize,
}

impl CommandSpec {
    /// Creates a command with an explicitly declared minimum prefix length.
    ///
    /// Command names are ASCII, so prefix lengths are measured in bytes.
    pub const fn new(name: &'static str, min_prefix_len: usize) -> Self {
        assert!(min_prefix_len > 0);
        assert!(min_prefix_len <= name.len());
        Self {
            name,
            min_prefix_len,
        }
    }

    /// Creates a command that has no implicit prefix abbreviations.
    pub const fn exact(name: &'static str) -> Self {
        Self::new(name, name.len())
    }

    /// Returns the canonical name used for dispatch and completion.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub(crate) fn accepts_prefix(&self, input: &str) -> bool {
        input.len() >= self.min_prefix_len && self.name.starts_with(input)
    }
}

/// Splits an exact plugin command name from its unexpanded argument text.
pub(crate) fn split_invocation(input: &str) -> (&str, &str) {
    input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(name, args)| (name, args.trim_start()))
}

/// Modifier parsed from a built-in colon command.
#[derive(Debug, PartialEq)]
pub enum CommandFlag {
    /// The trailing `!` force modifier.
    Force,
}

/// Resolved built-in command chain, arguments, and modifiers.
#[derive(Debug, Default, PartialEq)]
pub struct ParsedCommand {
    /// Canonical command names in execution order.
    pub commands: Vec<String>,
    /// Space-separated arguments following the command token.
    pub args: Vec<String>,
    /// Parsed command modifiers.
    pub flags: Vec<CommandFlag>,
}

impl ParsedCommand {
    /// Returns whether the command included a trailing force modifier.
    pub fn is_forced(&self) -> bool {
        self.flags.contains(&CommandFlag::Force)
    }

    /// Reassembles the unexpanded argument text following the command name.
    ///
    /// Splitting and joining on literal spaces preserves repeated spaces returned
    /// by command completion and nested command arguments.
    pub(crate) fn argument_text(&self) -> Option<String> {
        let arguments = self.args.join(" ");
        let arguments = arguments.trim_start();
        (!arguments.is_empty()).then(|| arguments.to_string())
    }

    /// Reassembles the single unexpanded path consumed by file commands.
    pub(crate) fn file_argument(&self) -> Option<String> {
        self.argument_text()
    }
}

/// Resolves a command line against the supplied built-in command specifications.
///
/// Returns `None` for unknown or ambiguous command names. Only the intentional `wq`
/// chain is recognized. Arguments are split without shell quoting or expansion.
pub fn parse(commands: &[CommandSpec], input: &str) -> Option<ParsedCommand> {
    let mut parts = input.splitn(2, ' ');
    let (flags, input) = parse_flags(parts.next()?);
    let args = parts
        .next()
        .map(|s| s.split(' ').map(|s| s.to_string()).collect())
        .unwrap_or_default();
    let commands = parse_commands(commands, input);

    if commands.is_empty() {
        return None;
    }

    Some(ParsedCommand {
        commands,
        args,
        flags,
    })
}

fn parse_flags(input: &str) -> (Vec<CommandFlag>, &str) {
    if let Some(input) = input.strip_suffix("!") {
        (vec![CommandFlag::Force], input)
    } else {
        (vec![], input)
    }
}

fn parse_commands(commands: &[CommandSpec], input: &str) -> Vec<String> {
    if let Some(command) = commands.iter().find(|command| command.name == input) {
        return vec![command.name.to_string()];
    }

    if let Some(command) = commands.iter().find(|command| {
        command
            .name
            .split('-')
            .filter_map(|part| part.chars().next())
            .eq(input.chars())
    }) {
        return vec![command.name.to_string()];
    }

    if input == "wq"
        && commands.iter().any(|command| command.name == "write")
        && commands.iter().any(|command| command.name == "quit")
    {
        return vec!["write".to_string(), "quit".to_string()];
    }

    let mut matches = commands
        .iter()
        .filter(|command| command.accepts_prefix(input));
    let Some(command) = matches.next() else {
        return Vec::new();
    };
    if matches.next().is_some() {
        return Vec::new();
    }

    vec![command.name.to_string()]
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse() {
        let commands = [
            CommandSpec::new("quit", 1),
            CommandSpec::new("write", 1),
            CommandSpec::new("edit", 1),
            CommandSpec::exact("buffer-next"),
            CommandSpec::exact("buffer-previous"),
        ];
        assert_eq!(
            parse(&commands, "quit"),
            Some(ParsedCommand {
                commands: vec!["quit".to_string()],
                flags: vec![],
                ..Default::default()
            })
        );
        assert_eq!(
            parse(&commands, "q"),
            Some(ParsedCommand {
                commands: vec!["quit".to_string()],
                flags: vec![],
                ..Default::default()
            })
        );
        assert_eq!(
            parse(&commands, "q!"),
            Some(ParsedCommand {
                commands: vec!["quit".to_string()],
                flags: vec![CommandFlag::Force],
                ..Default::default()
            })
        );
        assert_eq!(
            parse(&commands, "wq"),
            Some(ParsedCommand {
                commands: vec!["write".to_string(), "quit".to_string()],
                flags: vec![],
                ..Default::default()
            })
        );
        assert_eq!(
            parse(&commands, "wq!"),
            Some(ParsedCommand {
                commands: vec!["write".to_string(), "quit".to_string()],
                flags: vec![CommandFlag::Force],
                ..Default::default()
            })
        );
        assert_eq!(
            parse(&commands, "e src/name.rs"),
            Some(ParsedCommand {
                commands: vec!["edit".to_string()],
                args: vec!["src/name.rs".to_string()],
                flags: vec![]
            })
        );
    }

    #[test]
    fn file_arguments_preserve_spaces_and_literal_bangs() {
        let commands = [
            CommandSpec::new("edit", 1),
            CommandSpec::new("write", 1),
            CommandSpec::new("split", 2),
            CommandSpec::new("vsplit", 2),
        ];
        let path = "dir/name  with spaces.txt!";

        for command in commands.iter().map(CommandSpec::name) {
            let parsed = parse(&commands, &format!("{command} {path}")).unwrap();
            assert_eq!(parsed.file_argument().as_deref(), Some(path));
            assert!(!parsed.is_forced());

            let forced = parse(&commands, &format!("{command}! {path}")).unwrap();
            assert_eq!(forced.file_argument().as_deref(), Some(path));
            assert!(forced.is_forced());
        }

        assert_eq!(parse(&commands, "edit   ").unwrap().file_argument(), None);
    }

    #[test]
    fn test_parse_command() {
        let commands = [
            CommandSpec::new("quit", 1),
            CommandSpec::new("write", 1),
            CommandSpec::new("edit", 1),
            CommandSpec::exact("buffer-next"),
            CommandSpec::exact("buffer-previous"),
        ];
        assert_eq!(parse_commands(&commands, "quit"), vec!["quit"]);
        assert_eq!(parse_commands(&commands, "q"), vec!["quit"]);
        assert_eq!(parse_commands(&commands, "wq"), vec!["write", "quit"]);
        assert_eq!(parse_commands(&commands, "bn"), vec!["buffer-next"]);
        assert_eq!(parse_commands(&commands, "bp"), vec!["buffer-previous"]);
    }

    #[test]
    fn unknown_commands_do_not_partially_resolve_to_builtins() {
        let commands = [
            CommandSpec::new("quit", 1),
            CommandSpec::new("write", 1),
            CommandSpec::new("edit", 1),
            CommandSpec::exact("buffer-next"),
            CommandSpec::exact("buffer-previous"),
        ];

        assert_eq!(parse(&commands, "DefinitelyNotACommand"), None);
        assert_eq!(parse(&commands, "wzq"), None);
    }

    #[test]
    fn unknown_names_never_expand_into_unrelated_command_chains() {
        let commands = [
            CommandSpec::new("quit", 1),
            CommandSpec::new("write", 1),
            CommandSpec::new("edit", 1),
            CommandSpec::exact("noh"),
            CommandSpec::exact("languages"),
            CommandSpec::new("split", 2),
            CommandSpec::exact("bd"),
        ];

        for unknown in ["enew", "new", "vnew", "ls", "ew", "lss"] {
            assert_eq!(parse(&commands, unknown), None, "{unknown}");
        }

        assert_eq!(parse(&commands, "wq!").unwrap().commands, ["write", "quit"]);
    }

    #[test]
    fn vim_prefixes_respect_explicit_minimum_lengths() {
        let commands = [
            CommandSpec::new("edit", 1),
            CommandSpec::new("enew", 3),
            CommandSpec::new("write", 1),
            CommandSpec::new("wall", 2),
            CommandSpec::new("saveas", 3),
            CommandSpec::new("set", 2),
            CommandSpec::new("syntax", 2),
        ];

        for (input, expected) in [
            ("e", "edit"),
            ("ed", "edit"),
            ("edi", "edit"),
            ("ene", "enew"),
            ("enew", "enew"),
            ("w", "write"),
            ("wa", "wall"),
            ("wal", "wall"),
            ("wall", "wall"),
            ("sav", "saveas"),
            ("save", "saveas"),
            ("se", "set"),
            ("sy", "syntax"),
        ] {
            assert_eq!(parse(&commands, input).unwrap().commands, [expected]);
        }

        for input in ["en", "sa", "eneww"] {
            assert_eq!(parse(&commands, input), None, "{input}");
        }

        assert_eq!(
            parse(&commands, "ene!").unwrap(),
            ParsedCommand {
                commands: vec!["enew".to_string()],
                flags: vec![CommandFlag::Force],
                ..Default::default()
            }
        );
    }

    #[test]
    fn ambiguous_prefixes_are_rejected_instead_of_selecting_by_order() {
        let commands = [
            CommandSpec::new("release", 3),
            CommandSpec::new("reload", 3),
        ];

        assert_eq!(parse(&commands, "rel"), None);
        assert_eq!(parse(&commands, "rele").unwrap().commands, ["release"]);
        assert_eq!(parse(&commands, "relo").unwrap().commands, ["reload"]);
    }

    #[test]
    fn exact_names_take_precedence_over_longer_command_prefixes() {
        let commands = [
            CommandSpec::new("buffer", 1),
            CommandSpec::exact("b"),
            CommandSpec::exact("buffers"),
        ];

        assert_eq!(parse(&commands, "b").unwrap().commands, ["b"]);
        assert_eq!(parse(&commands, "buf").unwrap().commands, ["buffer"]);
        assert_eq!(parse(&commands, "buffers").unwrap().commands, ["buffers"]);
    }

    #[test]
    fn test_parse_flags() {
        assert_eq!(parse_flags("q"), (vec![], "q"));
        assert_eq!(parse_flags("q!"), (vec![CommandFlag::Force], "q"));
    }
}
