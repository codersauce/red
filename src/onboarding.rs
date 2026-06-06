//! First-run onboarding.
//!
//! When `~/.config/red/config.toml` is missing, [`run`] welcomes the user on
//! the plain terminal and offers to create a starter config plus the default
//! theme. The bundled `default_config.toml` and `themes/mocha.json` are
//! embedded in the binary so a fresh install can bootstrap itself with no
//! external files.

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

/// The starter config written on initialization. Single source of truth: the
/// same file that ships in the repository root.
const DEFAULT_CONFIG: &str = include_str!("../default_config.toml");

/// The theme referenced by [`DEFAULT_CONFIG`]. Without it, the editor exits on
/// startup because the theme file is required.
const DEFAULT_THEME: &str = include_str!("../themes/mocha.json");

/// File name of the bundled theme, matching the `theme = ` line in
/// [`DEFAULT_CONFIG`].
const DEFAULT_THEME_FILE: &str = "mocha.json";

/// Result of the onboarding flow.
pub enum Outcome {
    /// Config + theme were written; the caller should continue to load them.
    Initialized,
    /// The user declined; the caller should exit cleanly without launching.
    Declined,
}

/// Run the first-run onboarding flow. Called only when `config.toml` is
/// absent. On a non-interactive terminal it initializes silently; otherwise it
/// welcomes the user and offers to create the starter files.
pub fn run(config_dir: &Path) -> anyhow::Result<Outcome> {
    // Without an interactive stdin (piped input, CI) we can't prompt, so we
    // initialize silently and let the editor start.
    if !io::stdin().is_terminal() {
        write_default_assets(config_dir)?;
        return Ok(Outcome::Initialized);
    }

    let use_color = color_enabled(
        std::env::var_os("NO_COLOR").is_some(),
        io::stdout().is_terminal(),
    );

    print!("{}", render_welcome(config_dir, use_color));
    io::stdout().flush()?;

    if prompt_yes_no(true)? {
        write_default_assets(config_dir)?;
        println!();
        println!(
            "  {}",
            paint(GREEN, "✓ Created your starter config and theme.", use_color)
        );
        println!("  {}", paint(DIM, "Launching red…", use_color));
        println!();
        Ok(Outcome::Initialized)
    } else {
        println!();
        println!("  No config created.");
        println!(
            "  {}",
            paint(
                DIM,
                "Run `red` again to set it up, or create the file manually.",
                use_color,
            )
        );
        Ok(Outcome::Declined)
    }
}

/// Write the embedded starter config and default theme under `config_dir`,
/// creating the directory (and `themes/`) if needed.
fn write_default_assets(config_dir: &Path) -> anyhow::Result<()> {
    let themes_dir = config_dir.join("themes");
    fs::create_dir_all(&themes_dir)?;
    fs::write(config_dir.join("config.toml"), DEFAULT_CONFIG)?;
    fs::write(themes_dir.join(DEFAULT_THEME_FILE), DEFAULT_THEME)?;
    Ok(())
}

/// Print the prompt caret and read a `[Y/n]` answer from stdin.
fn prompt_yes_no(default_yes: bool) -> io::Result<bool> {
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(parse_yes_no(&line, default_yes))
}

/// Interpret a `[Y/n]` answer. Recognized yes/no tokens win; anything else
/// (including an empty line) falls back to `default_yes`.
fn parse_yes_no(input: &str, default_yes: bool) -> bool {
    match input.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default_yes,
    }
}

/// Build the welcome banner shown before the prompt. Pure so it can be tested;
/// ANSI styling is included only when `use_color` is true.
fn render_welcome(config_dir: &Path, use_color: bool) -> String {
    let config_path = config_dir.join("config.toml");
    let theme_path = config_dir.join("themes").join(DEFAULT_THEME_FILE);
    let bar = paint(DIM, "│", use_color);

    let mut out = String::new();
    out.push('\n');
    out.push_str(&format!(
        "  {bar} {}\n",
        paint(BOLD, "Welcome to red", use_color)
    ));
    out.push_str(&format!("  {bar}\n"));
    out.push_str(&format!("  {bar} No configuration file was found.\n"));
    out.push_str(&format!(
        "  {bar} red can create a starter config and theme for you:\n"
    ));
    out.push_str(&format!("  {bar}\n"));
    out.push_str(&format!(
        "  {bar}   {}  {}\n",
        paint(DIM, "config", use_color),
        paint(CYAN, &config_path.display().to_string(), use_color),
    ));
    out.push_str(&format!(
        "  {bar}   {}   {}\n",
        paint(DIM, "theme", use_color),
        paint(CYAN, &theme_path.display().to_string(), use_color),
    ));
    out.push_str(&format!("  {bar}\n"));
    out.push_str(&format!(
        "  {bar} Create it now? {} ",
        paint(BOLD, "[Y/n]", use_color)
    ));
    out
}

/// Decide whether to emit ANSI color: only on a TTY and only when the
/// `NO_COLOR` convention is not set.
fn color_enabled(no_color_set: bool, is_tty: bool) -> bool {
    is_tty && !no_color_set
}

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

/// Wrap `text` in an ANSI `code` when `use_color`, otherwise return it plain.
fn paint(code: &str, text: &str, use_color: bool) -> String {
    if use_color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn unique_temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("red-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn parse_yes_no_empty_uses_default() {
        assert!(parse_yes_no("", true));
        assert!(!parse_yes_no("", false));
    }

    #[test]
    fn parse_yes_no_recognizes_yes_tokens() {
        assert!(parse_yes_no("y", false));
        assert!(parse_yes_no("Y", false));
        assert!(parse_yes_no("yes", false));
        assert!(parse_yes_no("YES", false));
        assert!(parse_yes_no("  yes  ", false));
    }

    #[test]
    fn parse_yes_no_recognizes_no_tokens() {
        assert!(!parse_yes_no("n", true));
        assert!(!parse_yes_no("N", true));
        assert!(!parse_yes_no("no", true));
        assert!(!parse_yes_no("  NO  ", true));
    }

    #[test]
    fn parse_yes_no_unrecognized_falls_back_to_default() {
        assert!(parse_yes_no("maybe", true));
        assert!(!parse_yes_no("maybe", false));
    }

    #[test]
    fn write_default_assets_creates_config_and_theme() {
        let dir = unique_temp_dir("onboarding-write");

        write_default_assets(&dir).unwrap();

        let config = fs::read_to_string(dir.join("config.toml")).unwrap();
        let theme = fs::read_to_string(dir.join("themes").join(DEFAULT_THEME_FILE)).unwrap();
        assert_eq!(config, DEFAULT_CONFIG);
        assert_eq!(theme, DEFAULT_THEME);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_default_assets_creates_missing_parent_dirs() {
        // Parent does not exist yet; write_default_assets must create it.
        let dir = unique_temp_dir("onboarding-nested").join("red");
        assert!(!dir.exists());

        write_default_assets(&dir).unwrap();

        assert!(dir.join("config.toml").exists());
        assert!(dir.join("themes").join(DEFAULT_THEME_FILE).exists());

        fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn write_default_assets_produces_loadable_config() {
        // The written config must parse as a real Config, not just exist.
        let dir = unique_temp_dir("onboarding-loadable");

        write_default_assets(&dir).unwrap();

        let contents = fs::read_to_string(dir.join("config.toml")).unwrap();
        let parsed: Result<crate::config::Config, _> = toml::from_str(&contents);
        assert!(parsed.is_ok(), "starter config should parse: {parsed:?}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn render_welcome_mentions_red_and_paths_and_prompt() {
        let dir = Path::new("/home/example/.config/red");
        let banner = render_welcome(dir, false);

        assert!(banner.to_lowercase().contains("red"));
        assert!(banner.contains("config.toml"));
        assert!(banner.contains(DEFAULT_THEME_FILE));
        assert!(banner.contains("[Y/n]"));
    }

    #[test]
    fn render_welcome_omits_ansi_when_color_disabled() {
        let dir = Path::new("/home/example/.config/red");
        let banner = render_welcome(dir, false);
        assert!(
            !banner.contains('\x1b'),
            "expected no ANSI escapes: {banner:?}"
        );
    }

    #[test]
    fn render_welcome_includes_ansi_when_color_enabled() {
        let dir = Path::new("/home/example/.config/red");
        let banner = render_welcome(dir, true);
        assert!(
            banner.contains('\x1b'),
            "expected ANSI escapes when colored"
        );
    }

    #[test]
    fn color_enabled_only_on_tty_without_no_color() {
        assert!(color_enabled(false, true));
        assert!(!color_enabled(true, true)); // NO_COLOR set disables color
        assert!(!color_enabled(false, false)); // not a tty disables color
        assert!(!color_enabled(true, false));
    }
}
