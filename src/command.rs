//! Parsing of built-in colon-command abbreviations, arguments, and force flags.
//!
//! Parsing receives the authoritative command-name list from the caller. Exact names,
//! conventional initial-based abbreviations, and command chains are resolved before
//! arguments are returned; unknown input fails as a whole so a partially recognized
//! chain cannot execute unintended commands.

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
}

/// Resolves a command line against the supplied canonical command names.
///
/// Returns `None` when any command in a chain is unknown. Arguments are split on spaces
/// without shell quoting or expansion.
pub fn parse(commands: &[&str], input: &str) -> Option<ParsedCommand> {
    let (flags, input) = parse_flags(input);
    let mut parts = input.splitn(2, ' ');
    let input = parts.next()?;
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

fn parse_commands(commands: &[&str], input: &str) -> Vec<String> {
    for command in commands {
        if &input == command {
            return vec![command.to_string()];
        }
    }

    if let Some(command) = commands.iter().find(|command| {
        command
            .split('-')
            .filter_map(|part| part.chars().next())
            .eq(input.chars())
    }) {
        return vec![(*command).to_string()];
    }

    let mut result = Vec::new();
    for c in input.chars() {
        let Some(command) = commands.iter().find(|cmd| cmd.starts_with(c)) else {
            return Vec::new();
        };
        result.push(command.to_string());
    }

    result
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parse() {
        let commands = ["quit", "write", "edit", "buffer-next", "buffer-previous"];
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
    fn test_parse_command() {
        let commands = ["quit", "write", "edit", "buffer-next", "buffer-previous"];
        assert_eq!(parse_commands(&commands, "quit"), vec!["quit"]);
        assert_eq!(parse_commands(&commands, "q"), vec!["quit"]);
        assert_eq!(parse_commands(&commands, "wq"), vec!["write", "quit"]);
        assert_eq!(parse_commands(&commands, "bn"), vec!["buffer-next"]);
        assert_eq!(parse_commands(&commands, "bp"), vec!["buffer-previous"]);
    }

    #[test]
    fn unknown_commands_do_not_partially_resolve_to_builtins() {
        let commands = ["quit", "write", "edit", "buffer-next", "buffer-previous"];

        assert_eq!(parse(&commands, "DefinitelyNotACommand"), None);
        assert_eq!(parse(&commands, "wzq"), None);
    }

    #[test]
    fn test_parse_flags() {
        assert_eq!(parse_flags("q"), (vec![], "q"));
        assert_eq!(parse_flags("q!"), (vec![CommandFlag::Force], "q"));
    }
}
