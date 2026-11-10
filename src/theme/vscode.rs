//! Translation from VS Code color-theme JSON into Red's concrete [`Theme`].
//!
//! Import resolves `extends` through the runtime asset system, then workbench colors,
//! token scopes, font styles, and fallback relationships. The resolved document keeps
//! inherited `type` and merged colors. Other unknown source fields are ignored for
//! compatibility; invalid colors or required structural values remain parse errors.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};

use json_comments::StripComments;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::color::{blend_color, ensure_minimum_contrast, parse_rgb, Color};
use crate::config::Config;

/// A theme may follow this many `extends` links, so the root plus four parents load.
const MAX_THEME_EXTENDS_DEPTH: usize = 4;

use super::{
    compose_selection_style, SelectionForegroundPriority, StatuslineStyle, Style, Theme,
    TokenStyle, UiStyle, MINIMUM_SELECTION_TEXT_CONTRAST,
};

static SYNTAX_HIGHLIGHTING_MAP: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();

    m.insert("constant", "constant");
    m.insert("entity.name.type", "type");
    m.insert("support.type", "type");
    m.insert("entity.name.function.constructor", "constructor");
    m.insert("variable.other.enummember", "constructor");
    m.insert("entity.name.function", "function");
    m.insert("meta.function-call", "function");
    m.insert("entity.name.function.member", "function.method");
    m.insert("variable.function", "function.method");
    m.insert("entity.name.function.macro", "function.macro");
    m.insert("support.function.macro", "function.macro");
    m.insert("entity.name.tag", "tag");
    m.insert("entity.other.attribute-name", "attribute");
    m.insert("variable.other.member", "property");
    m.insert("variable.other.property", "property");
    m.insert("variable.parameter", "variable.parameter");
    m.insert("entity.name.label", "label");
    m.insert("comment", "comment");
    m.insert("punctuation.definition.comment", "comment");
    m.insert("punctuation.section.block", "punctuation.bracket");
    m.insert("punctuation.definition.brackets", "punctuation.bracket");
    m.insert("punctuation.separator", "punctuation.delimiter");
    m.insert("punctuation.accessor", "punctuation.delimiter");
    m.insert("keyword", "keyword");
    m.insert("keyword.control", "keyword");
    m.insert("support.type.primitive", "type.builtin");
    m.insert("keyword.type", "type.builtin");
    m.insert("variable.language", "variable.builtin");
    m.insert("support.variable", "variable.builtin");
    m.insert("string.quoted.double", "string");
    m.insert("string.quoted.single", "string");
    m.insert("constant.language", "constant.builtin");
    m.insert("constant.numeric", "constant.builtin");
    m.insert("constant.character", "constant.builtin");
    m.insert("constant.character.escape", "escape");
    m.insert("keyword.operator", "operator");
    m.insert("storage.modifier.attribute", "attribute");
    m.insert("meta.attribute", "attribute");

    m
});

pub fn parse_vscode_theme(file: &str) -> anyhow::Result<Theme> {
    let contents = fs::read_to_string(file)?;
    let value = parse_theme_json(&contents)?;
    if value.get("extends").is_none() {
        return theme_from_value(value);
    }
    let name = Path::new(file).file_name().and_then(|name| name.to_str());
    let config_dir = Config::config_dir();
    let layers = collect_layers(name, value, &config_dir, &mut Vec::new(), 0)?;
    theme_from_layers(&layers)
}

pub fn parse_vscode_theme_contents(contents: &str) -> anyhow::Result<Theme> {
    let value = parse_theme_json(contents)?;
    if value.get("extends").is_none() {
        return theme_from_value(value);
    }
    let config_dir = Config::config_dir();
    let layers = collect_layers(None, value, &config_dir, &mut Vec::new(), 0)?;
    theme_from_layers(&layers)
}

/// Loads a theme by file name, following `extends` from `config_dir`.
pub fn load_resolved_theme(name: &str, config_dir: &Path) -> anyhow::Result<Theme> {
    theme_from_layers(&layers_for_named_theme(name, config_dir)?)
}

/// Returns the theme document after `extends` has been merged away.
pub fn resolved_vscode_theme_json(name: &str, config_dir: &Path) -> anyhow::Result<Value> {
    Ok(merge_layers(&layers_for_named_theme(name, config_dir)?))
}

fn theme_from_vscode(vscode_theme: VsCodeTheme) -> anyhow::Result<Theme> {
    let default_theme = Theme::default();

    let error_style = vscode_theme.style_from("editorError.foreground", "editorError.background");
    let cursor_style = vscode_theme
        .style_from("editorCursor.foreground", "editorCursor.background")
        .or_else(|| {
            vscode_theme.style_from("terminalCursor.foreground", "terminalCursor.background")
        });

    let gutter_style = Style {
        fg: vscode_theme.color_from("editorLineNumber.foreground"),
        bg: vscode_theme.color_from("editorLineNumber.background"),
        ..Default::default()
    };

    let line_highlight_style = vscode_theme
        .color_from("editor.lineHighlightBackground")
        .map(|color| Style {
            bg: Some(color),
            ..Default::default()
        });

    let selection_style =
        vscode_theme.style_from("editor.selectionForeground", "editor.selectionBackground");

    let bracket_match_style = vscode_theme
        .style_from(
            "editorBracketMatch.foreground",
            "editorBracketMatch.background",
        )
        .map(|mut style| {
            style.fg = style
                .fg
                .or_else(|| vscode_theme.color_from("editorBracketMatch.border"));
            style
        })
        .or_else(|| {
            vscode_theme.style_from("editorBracketMatch.border", "editorBracketMatch.background")
        });

    let find_match_style = vscode_theme
        .color_from("editor.findMatchBackground")
        .map(|color| Style {
            bg: Some(color),
            ..Default::default()
        });

    let find_match_highlight_style = vscode_theme
        .color_from("editor.findMatchHighlightBackground")
        .map(|color| Style {
            bg: Some(color),
            ..Default::default()
        });

    let statusline_style = vscode_theme.statusline_style(selection_style.as_ref());

    // VS Code derives its default token rule from the workbench editor colors and
    // ignores scope-less token rules when scoped highlighting rules are assembled.
    // Keep accepting the legacy TextMate defaults only when the corresponding
    // workbench color is absent so older standalone themes remain usable.
    let scope_less_token_color = |component: &str| {
        vscode_theme
            .token_colors
            .iter()
            .filter(|tc| tc.scope.is_none())
            .find_map(|tc| tc.settings.get(component))
    };
    let fg = vscode_theme
        .colors
        .get("editor.foreground")
        .or_else(|| scope_less_token_color("foreground"));
    let bg = vscode_theme
        .colors
        .get("editor.background")
        .or_else(|| scope_less_token_color("background"));

    let editor_style = Style {
        fg: fg
            .and_then(|value| parse_color_value(value).ok())
            .or(default_theme.style.fg),
        bg: bg
            .and_then(|value| parse_color_value(value).ok())
            .or(default_theme.style.bg),
        bold: false,
        italic: false,
        underline: false,
    };
    let ui_style = vscode_theme.ui_style(&editor_style, selection_style.as_ref());

    let token_styles = vscode_theme
        .token_colors
        .into_iter()
        .filter(|token| token.scope.is_some())
        .map(|tc| tc.try_into())
        .collect::<Result<Vec<TokenStyle>, _>>()?;
    let colors = vscode_theme
        .colors
        .iter()
        .filter_map(|(key, value)| {
            parse_color_value(value)
                .ok()
                .map(|color| (key.to_string(), color))
        })
        .collect::<BTreeMap<_, _>>();

    Ok(Theme {
        name: vscode_theme.name.unwrap_or_default(),
        colors,
        style: editor_style,
        ui_style,
        token_styles,
        gutter_style,
        statusline_style,
        line_highlight_style,
        bracket_match_style,
        find_match_style,
        find_match_highlight_style,
        selection_style,
        cursor_style,
        error_style,
    })
}

struct ThemeLayer {
    fields: Map<String, Value>,
    token_colors_present: bool,
    token_colors: Vec<Value>,
}

fn theme_from_value(value: Value) -> anyhow::Result<Theme> {
    let vscode_theme: VsCodeTheme = serde_json::from_value(value)?;
    theme_from_vscode(vscode_theme)
}

fn theme_from_layers(layers: &[ThemeLayer]) -> anyhow::Result<Theme> {
    let mut document = merge_layers(layers);
    if layers.len() > 1 {
        // Scope lookup keeps the first matching rule. Child layers go first so
        // they win, while each file keeps its own rule order.
        if let Some(Value::Array(tokens)) = document.get_mut("tokenColors") {
            let mut child_first = Vec::with_capacity(tokens.len());
            for layer in layers.iter().rev() {
                if layer.token_colors_present {
                    child_first.extend(layer.token_colors.iter().cloned());
                }
            }
            *tokens = child_first;
        }
    }
    theme_from_value(document)
}

fn layers_for_named_theme(name: &str, config_dir: &Path) -> anyhow::Result<Vec<ThemeLayer>> {
    let Some(asset) = crate::assets::resolve_theme(name, config_dir) else {
        anyhow::bail!("Theme file {name} not found");
    };
    let value = parse_theme_json(&asset.read_to_string()?)?;
    collect_layers(Some(name), value, config_dir, &mut Vec::new(), 0)
}

fn collect_layers(
    name: Option<&str>,
    value: Value,
    config_dir: &Path,
    chain: &mut Vec<String>,
    depth: usize,
) -> anyhow::Result<Vec<ThemeLayer>> {
    if let Some(name) = name {
        if chain.iter().any(|existing| existing == name) {
            chain.push(name.to_string());
            let message = format!("theme inheritance cycle: {}", chain.join(" → "));
            chain.pop();
            anyhow::bail!(message);
        }
        chain.push(name.to_string());
    }
    if depth > MAX_THEME_EXTENDS_DEPTH {
        let message = format!(
            "theme inheritance is deeper than {MAX_THEME_EXTENDS_DEPTH}: {}",
            chain.join(" → ")
        );
        if name.is_some() {
            chain.pop();
        }
        anyhow::bail!(message);
    }

    let result = assemble_layers(value, config_dir, chain, depth);
    if name.is_some() {
        chain.pop();
    }
    result
}

fn assemble_layers(
    value: Value,
    config_dir: &Path,
    chain: &mut Vec<String>,
    depth: usize,
) -> anyhow::Result<Vec<ThemeLayer>> {
    let ThemeParts { extends, layer } = split_theme_document(value)?;
    let mut layers = Vec::new();
    if let Some(parent_name) = extends {
        let parent_name = parent_name.trim();
        if parent_name.is_empty() {
            anyhow::bail!("theme extends must name a theme file");
        }
        let parent_value = read_parent_theme(parent_name, config_dir, chain)?;
        layers = collect_layers(
            Some(parent_name),
            parent_value,
            config_dir,
            chain,
            depth + 1,
        )?;
    }
    layers.push(layer);
    Ok(layers)
}

fn read_parent_theme(name: &str, config_dir: &Path, chain: &[String]) -> anyhow::Result<Value> {
    let Some(asset) = crate::assets::resolve_theme(name, config_dir) else {
        let mut missing = chain.to_vec();
        missing.push(name.to_string());
        anyhow::bail!("theme {name} not found: {}", missing.join(" → "));
    };
    parse_theme_json(&asset.read_to_string()?)
}

struct ThemeParts {
    extends: Option<String>,
    layer: ThemeLayer,
}

fn split_theme_document(value: Value) -> anyhow::Result<ThemeParts> {
    let Value::Object(mut object) = value else {
        anyhow::bail!("theme JSON must be an object");
    };
    let extends = match object.remove("extends") {
        None => None,
        Some(Value::String(name)) => Some(name),
        Some(other) => anyhow::bail!("theme extends must be a string, got {other}"),
    };
    let (token_colors_present, token_colors) = match object.remove("tokenColors") {
        None => (false, Vec::new()),
        Some(Value::Array(tokens)) => (true, tokens),
        Some(other) => anyhow::bail!("theme tokenColors must be an array, got {other}"),
    };
    Ok(ThemeParts {
        extends,
        layer: ThemeLayer {
            fields: object,
            token_colors_present,
            token_colors,
        },
    })
}

fn merge_layers(layers: &[ThemeLayer]) -> Value {
    let mut merged = Map::new();
    let mut tokens = Vec::new();
    let mut saw_tokens = false;
    for layer in layers {
        overlay_theme_fields(&mut merged, &layer.fields);
        if layer.token_colors_present {
            saw_tokens = true;
            tokens.extend(layer.token_colors.iter().cloned());
        }
    }
    if saw_tokens {
        merged.insert("tokenColors".to_string(), Value::Array(tokens));
    }
    Value::Object(merged)
}

fn overlay_theme_fields(base: &mut Map<String, Value>, child: &Map<String, Value>) {
    for (key, value) in child {
        if key == "colors" {
            if let Some(child_colors) = value.as_object() {
                if let Some(Value::Object(parent_colors)) = base.get_mut(key) {
                    for (color_key, color_value) in child_colors {
                        parent_colors.insert(color_key.clone(), color_value.clone());
                    }
                    continue;
                }
            }
        }
        base.insert(key.clone(), value.clone());
    }
}

fn parse_theme_json(contents: &str) -> anyhow::Result<Value> {
    serde_json::from_str(contents)
        .or_else(|_| serde_json::from_reader(StripComments::new(contents.as_bytes())))
        .map_err(|error| anyhow::anyhow!("invalid theme JSON: {error}"))
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTheme {
    name: Option<String>,
    colors: Map<String, Value>,
    token_colors: Vec<VsCodeTokenColor>,
}

impl VsCodeTheme {
    fn color_from(&self, key: &str) -> Option<Color> {
        self.colors
            .get(key)
            .and_then(|value| parse_color_value(value).ok())
    }

    fn style_from(&self, fg_key: &str, bg_key: &str) -> Option<Style> {
        let fg = self.color_from(fg_key);
        let bg = self.color_from(bg_key);

        if fg.is_none() && bg.is_none() {
            return None;
        }

        Some(Style {
            fg,
            bg,
            bold: false,
            italic: false,
            underline: false,
        })
    }

    fn statusline_style(&self, selection_style: Option<&Style>) -> StatuslineStyle {
        let fallback_outer_bg = Color::Rgb {
            r: 184,
            g: 144,
            b: 243,
        };
        let fallback_inner_fg = Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        };
        let fallback_inner_bg = Color::Rgb {
            r: 67,
            g: 70,
            b: 89,
        };

        let inner_fg = self
            .color_from("statusBar.foreground")
            .unwrap_or(fallback_inner_fg);
        let inner_bg = self
            .color_from("statusBar.background")
            .filter(|color| !is_transparent(*color))
            .unwrap_or(fallback_inner_bg);

        let (outer_bg, outer_fg, selection_derived) = self
            .statusline_accent_from(
                "statusBarItem.prominentBackground",
                "statusBarItem.prominentForeground",
            )
            .map(|(bg, fg)| (bg, fg, false))
            .or_else(|| {
                self.statusline_accent_from(
                    "statusBarItem.remoteBackground",
                    "statusBarItem.remoteForeground",
                )
                .map(|(bg, fg)| (bg, fg, false))
            })
            .or_else(|| {
                selection_style
                    .and_then(|style| style.bg)
                    .filter(|color| !is_transparent(*color))
                    .map(|bg| {
                        (
                            bg,
                            self.color_from("statusBar.foreground").unwrap_or(inner_fg),
                            true,
                        )
                    })
            })
            .unwrap_or((fallback_outer_bg, Color::Rgb { r: 0, g: 0, b: 0 }, false));
        let (outer_bg, outer_fg) = if selection_derived {
            let outer_bg = blend_color(outer_bg, inner_bg);
            let outer_fg =
                ensure_minimum_contrast(outer_fg, outer_bg, MINIMUM_SELECTION_TEXT_CONTRAST);
            (outer_bg, outer_fg)
        } else {
            (outer_bg, outer_fg)
        };

        StatuslineStyle {
            outer_style: Style {
                fg: Some(outer_fg),
                bg: Some(outer_bg),
                bold: true,
                ..Default::default()
            },
            outer_chars: [' ', '', '', ' '],
            inner_style: Style {
                fg: Some(inner_fg),
                bg: Some(inner_bg),
                ..Default::default()
            },
        }
    }

    fn statusline_accent_from(&self, bg_key: &str, fg_key: &str) -> Option<(Color, Color)> {
        let bg = self
            .color_from(bg_key)
            .filter(|color| !is_transparent(*color))?;
        let fg = self
            .color_from(fg_key)
            .or_else(|| self.color_from("statusBar.foreground"))
            .unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });
        Some((bg, fg))
    }

    fn ui_style(&self, editor_style: &Style, selection_style: Option<&Style>) -> UiStyle {
        let editor_fg = editor_style.fg.unwrap_or(Color::Rgb {
            r: 255,
            g: 255,
            b: 255,
        });
        let editor_bg = editor_style.bg.unwrap_or(Color::Rgb { r: 0, g: 0, b: 0 });

        let popup_bg = self
            .color_from("quickInput.background")
            .or_else(|| self.color_from("editorWidget.background"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or_else(|| adjust_color(editor_bg, 8));
        let popup_fg = self
            .color_from("quickInput.foreground")
            .or_else(|| self.color_from("editorWidget.foreground"))
            .unwrap_or(editor_fg);
        let border_fg = self
            .color_from("quickInputTitle.background")
            .or_else(|| self.color_from("focusBorder"))
            .or_else(|| self.color_from("input.border"))
            .or_else(|| self.color_from("editorWidget.border"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or_else(|| adjust_color(popup_bg, 18));
        let selected_bg = self
            .color_from("quickInputList.focusBackground")
            .or_else(|| self.color_from("list.activeSelectionBackground"))
            .or_else(|| selection_style.and_then(|style| style.bg))
            .filter(|color| !is_transparent(*color))
            .unwrap_or_else(|| adjust_color(popup_bg, 16));
        let selected_fg = self
            .color_from("quickInputList.focusForeground")
            .or_else(|| self.color_from("list.activeSelectionForeground"))
            .unwrap_or_else(|| readable_foreground(selected_bg, popup_fg));
        let selected_style = compose_selection_style(
            editor_style,
            &Style {
                fg: Some(popup_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            &Style {
                fg: Some(selected_fg),
                bg: Some(selected_bg),
                ..Default::default()
            },
            SelectionForegroundPriority::Selection,
        );
        let prompt_bg = self
            .color_from("input.background")
            .filter(|color| !is_transparent(*color))
            .unwrap_or(popup_bg);
        let prompt_fg = self.color_from("input.foreground").unwrap_or(popup_fg);
        let muted_fg = self
            .color_from("input.placeholderForeground")
            .or_else(|| self.color_from("descriptionForeground"))
            .or_else(|| self.color_from("editorLineNumber.foreground"))
            .unwrap_or_else(|| adjust_color(popup_fg, -30));
        let deprecated_fg = self
            .color_from("list.warningForeground")
            .or_else(|| self.color_from("editorWarning.foreground"))
            .or_else(|| self.color_from("editorError.foreground"))
            .unwrap_or_else(|| adjust_color(popup_fg, -45));
        let dialog_bg = self
            .color_from("editorHoverWidget.background")
            .or_else(|| self.color_from("editorWidget.background"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or(popup_bg);
        let dialog_fg = self
            .color_from("editorHoverWidget.foreground")
            .or_else(|| self.color_from("editorWidget.foreground"))
            .unwrap_or(popup_fg);
        let dialog_border_fg = self
            .color_from("editorHoverWidget.border")
            .or_else(|| self.color_from("editorWidget.border"))
            .filter(|color| !is_transparent(*color))
            .unwrap_or(border_fg);

        UiStyle {
            popup: Style {
                fg: Some(popup_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            popup_border: Style {
                fg: Some(border_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            popup_title: Style {
                fg: Some(popup_fg),
                bg: Some(popup_bg),
                bold: true,
                ..Default::default()
            },
            dialog: Style {
                fg: Some(dialog_fg),
                bg: Some(dialog_bg),
                ..Default::default()
            },
            dialog_border: Style {
                fg: Some(dialog_border_fg),
                bg: Some(dialog_bg),
                ..Default::default()
            },
            dialog_title: Style {
                fg: Some(dialog_fg),
                bg: Some(dialog_bg),
                bold: true,
                ..Default::default()
            },
            picker_item: Style {
                fg: Some(popup_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            picker_selected_item: Style {
                fg: selected_style.fg,
                bg: selected_style.bg,
                ..Default::default()
            },
            picker_prompt: Style {
                fg: Some(prompt_fg),
                bg: Some(prompt_bg),
                ..Default::default()
            },
            muted: Style {
                fg: Some(muted_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
            deprecated: Style {
                fg: Some(deprecated_fg),
                bg: Some(popup_bg),
                ..Default::default()
            },
        }
    }
}

fn is_transparent(color: Color) -> bool {
    matches!(color, Color::Rgba { a: 0, .. })
}

fn adjust_color(color: Color, percentage: i32) -> Color {
    let Color::Rgb { r, g, b } = color else {
        return color;
    };

    let adjust = |component: u8| -> u8 {
        let delta = (255.0 * (percentage as f32 / 100.0)) as i32;
        (component as i32 + delta).clamp(0, 255) as u8
    };

    Color::Rgb {
        r: adjust(r),
        g: adjust(g),
        b: adjust(b),
    }
}

fn readable_foreground(background: Color, fallback: Color) -> Color {
    ensure_minimum_contrast(fallback, background, MINIMUM_SELECTION_TEXT_CONTRAST)
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct VsCodeTokenColor {
    name: Option<String>,
    scope: Option<VsCodeScope>,
    settings: Map<String, Value>,
}

impl TryFrom<VsCodeTokenColor> for TokenStyle {
    type Error = anyhow::Error;

    fn try_from(tc: VsCodeTokenColor) -> Result<Self, Self::Error> {
        let mut style = Style::default();

        if let Some(fg) = tc.settings.get("foreground") {
            style.fg = Some(parse_color_value(fg)?);
        }

        if let Some(bg) = tc.settings.get("background") {
            style.bg = Some(parse_color_value(bg)?);
        }

        if let Some(font_style) = tc.settings.get("fontStyle").and_then(Value::as_str) {
            style.bold = font_style.contains("bold");
            style.italic = font_style.contains("italic");
            style.underline = font_style.contains("underline");
        }

        let Some(scope) = tc.scope else {
            return Err(anyhow::anyhow!("TokenColor has no scope"));
        };

        Ok(Self {
            name: tc.name,
            scope: scope.into(),
            style,
        })
    }
}

fn parse_color_value(value: &Value) -> anyhow::Result<Color> {
    let Some(color) = value.as_str() else {
        anyhow::bail!("theme color must be a string, got {value}");
    };
    parse_rgb(color)
}

fn translate_scope(vscode_scope: String) -> String {
    if vscode_scope
        .split_whitespace()
        .any(is_textmate_property_scope)
    {
        return "property".to_string();
    }

    SYNTAX_HIGHLIGHTING_MAP
        .get(&vscode_scope.as_str())
        .map(|s| s.to_string())
        .unwrap_or(vscode_scope)
}

fn is_textmate_property_scope(scope: &str) -> bool {
    scope == "property"
        || scope == "entity.name.tag.yaml"
        || scope == "meta.property-name"
        || scope_is_or_has_suffix(scope, "support.type.property-name")
        || scope_is_or_has_suffix(scope, "punctuation.support.type.property-name")
        || scope_is_or_has_suffix(scope, "variable.other.member")
        || scope_is_or_has_suffix(scope, "variable.other.property")
}

fn scope_is_or_has_suffix(scope: &str, base: &str) -> bool {
    scope == base
        || scope
            .strip_prefix(base)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum VsCodeScope {
    Single(String),
    Multiple(Vec<String>),
}

impl From<VsCodeScope> for Vec<String> {
    fn from(scope: VsCodeScope) -> Self {
        match scope {
            VsCodeScope::Single(s) => vec![translate_scope(s)],
            VsCodeScope::Multiple(v) => v.into_iter().map(translate_scope).collect(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::color::{blend_color, contrast_ratio};
    use crate::theme::{
        MINIMUM_CURSOR_STATE_CONTRAST, MINIMUM_CURSOR_TEXT_CONTRAST,
        MINIMUM_SELECTION_STATE_CONTRAST,
    };

    #[test]
    fn test_parse_vscode() {
        let theme = parse_vscode_theme("./src/fixtures/frappe.json").unwrap();
        println!("{:#?}", theme);
    }

    #[test]
    fn test_statusline_uses_vscode_statusbar_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.statusline_style.inner_style.fg,
            Some(Color::Rgb {
                r: 205,
                g: 214,
                b: 244,
            })
        );
        assert_eq!(
            theme.statusline_style.inner_style.bg,
            Some(Color::Rgb {
                r: 17,
                g: 17,
                b: 27,
            })
        );
        assert_eq!(
            theme.statusline_style.outer_style.bg,
            Some(Color::Rgb {
                r: 137,
                g: 180,
                b: 250,
            })
        );
        assert_eq!(
            theme.statusline_style.outer_style.fg,
            Some(Color::Rgb {
                r: 17,
                g: 17,
                b: 27,
            })
        );
    }

    #[test]
    fn test_ui_style_uses_vscode_quick_input_colors() {
        let theme = parse_vscode_theme("./src/fixtures/nord.json").unwrap();

        assert_eq!(
            theme.ui_style.popup.bg,
            Some(Color::Rgb {
                r: 46,
                g: 52,
                b: 64,
            })
        );
        assert_eq!(
            theme.ui_style.popup_border.fg,
            Some(Color::Rgb {
                r: 59,
                g: 66,
                b: 82,
            })
        );
        assert_eq!(
            theme.ui_style.picker_selected_item.bg,
            Some(Color::Rgb {
                r: 136,
                g: 192,
                b: 208,
            })
        );
        assert_eq!(
            theme.ui_style.picker_selected_item.fg,
            Some(Color::Rgb {
                r: 46,
                g: 52,
                b: 64,
            })
        );
    }

    #[test]
    fn test_ui_style_uses_vscode_hover_widget_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.ui_style.dialog.bg,
            Some(Color::Rgb {
                r: 24,
                g: 24,
                b: 37,
            })
        );
        assert_eq!(
            theme.ui_style.dialog.fg,
            Some(Color::Rgb {
                r: 205,
                g: 214,
                b: 244,
            })
        );
        assert_eq!(
            theme.ui_style.dialog_border.fg,
            Some(Color::Rgb {
                r: 88,
                g: 91,
                b: 112,
            })
        );
        assert_eq!(theme.ui_style.dialog_title.bg, theme.ui_style.dialog.bg);
    }

    #[test]
    fn test_search_styles_use_vscode_find_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.find_match_style.and_then(|style| style.bg),
            Some(Color::Rgb {
                r: 94,
                g: 63,
                b: 83,
            })
        );
        assert_eq!(
            theme.find_match_highlight_style.and_then(|style| style.bg),
            Some(Color::Rgb {
                r: 62,
                g: 87,
                b: 103,
            })
        );
    }

    #[test]
    fn test_bracket_match_style_uses_vscode_bracket_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();
        let style = theme.bracket_match_style.unwrap();

        assert_eq!(
            style.bg,
            Some(Color::Rgba {
                r: 147,
                g: 153,
                b: 178,
                a: 26,
            })
        );
        assert_eq!(
            style.fg,
            Some(Color::Rgb {
                r: 147,
                g: 153,
                b: 178,
            })
        );
    }

    #[test]
    fn test_ui_style_uses_vscode_input_and_list_fallbacks() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.ui_style.popup.bg,
            Some(Color::Rgb {
                r: 24,
                g: 24,
                b: 37,
            })
        );
        assert_eq!(
            theme.ui_style.picker_prompt.bg,
            Some(Color::Rgb {
                r: 49,
                g: 50,
                b: 68,
            })
        );
        let selected_bg = theme.ui_style.picker_selected_item.bg.unwrap();
        let selected_fg = theme.ui_style.picker_selected_item.fg.unwrap();
        let popup_bg = theme.ui_style.popup.bg.unwrap();
        assert!(contrast_ratio(selected_bg, popup_bg) >= MINIMUM_SELECTION_STATE_CONTRAST);
        assert!(contrast_ratio(selected_fg, selected_bg) >= MINIMUM_SELECTION_TEXT_CONTRAST);
    }

    #[test]
    fn test_cursor_uses_vscode_editor_cursor_colors() {
        let theme = parse_vscode_theme("./src/fixtures/latte.json").unwrap();

        assert_eq!(
            theme.cursor_style,
            Some(Style {
                fg: Some(Color::Rgb {
                    r: 220,
                    g: 138,
                    b: 120,
                }),
                bg: Some(Color::Rgb {
                    r: 239,
                    g: 241,
                    b: 245,
                }),
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_editor_selection_foreground_is_preserved_for_high_contrast_themes() {
        let theme = parse_vscode_theme("themes/github-dark-high-contrast.json").unwrap();

        assert_eq!(
            theme.selection_style.and_then(|style| style.fg),
            Some(Color::Rgb {
                r: 10,
                g: 12,
                b: 16,
            })
        );
    }

    #[test]
    fn test_exposes_raw_vscode_workbench_colors() {
        let theme = parse_vscode_theme("./src/fixtures/mocha.json").unwrap();

        assert_eq!(
            theme.colors.get("gitDecoration.modifiedResourceForeground"),
            Some(&Color::Rgb {
                r: 249,
                g: 226,
                b: 175,
            })
        );
        assert_eq!(
            theme.colors.get("symbolIcon.folderForeground"),
            Some(&Color::Rgb {
                r: 203,
                g: 166,
                b: 247,
            })
        );
    }

    #[test]
    fn test_statusline_falls_back_without_vscode_statusbar_colors() {
        let theme = parse_vscode_theme("src/fixtures/token-color-with-no-scope.json").unwrap();

        assert_eq!(
            theme.statusline_style.inner_style.fg,
            Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            })
        );
        assert_eq!(
            theme.statusline_style.inner_style.bg,
            Some(Color::Rgb {
                r: 67,
                g: 70,
                b: 89,
            })
        );
        assert_eq!(
            theme.statusline_style.outer_style.bg,
            Some(Color::Rgb {
                r: 184,
                g: 144,
                b: 243,
            })
        );
    }

    #[test]
    fn test_token_color_with_no_scope() {
        let theme = parse_vscode_theme("src/fixtures/token-color-with-no-scope.json").unwrap();

        assert_eq!(theme.style.fg, Some(parse_rgb("#d8dee9ff").unwrap()));
        assert_eq!(theme.style.bg, Some(parse_rgb("#2e3440ff").unwrap()));
    }

    #[test]
    fn test_editor_colors_override_scope_less_token_defaults() {
        let theme = parse_vscode_theme_contents(
            r##"
            {
                "name": "conflicting defaults",
                "colors": {
                    "editor.foreground": "#112233",
                    "editor.background": "#f4f5f6"
                },
                "tokenColors": [
                    {
                        "settings": {
                            "foreground": "#ffffff",
                            "background": "#010203"
                        }
                    }
                ]
            }
            "##,
        )
        .unwrap();

        assert_eq!(theme.style.fg, Some(parse_rgb("#112233").unwrap()));
        assert_eq!(theme.style.bg, Some(parse_rgb("#f4f5f6").unwrap()));
    }

    #[test]
    fn test_bundled_conflicting_defaults_use_editor_colors() {
        for (file, expected_fg, expected_bg) in [
            ("themes/night-owl-light.json", "#403f53", "#FBFBFB"),
            (
                "themes/night-owl-light-no-italics.json",
                "#403f53",
                "#FBFBFB",
            ),
            ("themes/andromeda-bordered.json", "#D5CED9", "#262A33"),
            (
                "themes/andromeda-italic-bordered.json",
                "#D5CED9",
                "#262A33",
            ),
            ("themes/ayu-dark-bordered.json", "#bfbdb6", "#10141c"),
            ("themes/ayu-light-bordered.json", "#5c6166", "#fcfcfc"),
            ("themes/ayu-mirage-bordered.json", "#cccac2", "#242936"),
            ("themes/community-material-theme.json", "#EEFFFF", "#263238"),
            ("themes/winter-is-coming-light.json", "#236ebf", "#FFFFFF"),
        ] {
            let theme = parse_vscode_theme(file).unwrap();
            assert_eq!(
                theme.style.fg,
                Some(parse_rgb(expected_fg).unwrap()),
                "wrong editor foreground for {file}"
            );
            assert_eq!(
                theme.style.bg,
                Some(parse_rgb(expected_bg).unwrap()),
                "wrong editor background for {file}"
            );
        }
    }

    #[test]
    fn test_fluoromachine_syntax_tokens_do_not_paint_background_tiles() {
        let theme = parse_vscode_theme("themes/fluoromachine.json").unwrap();

        for scope in [
            "constant",
            "constant.numeric",
            "constant.language",
            "entity.name.function",
            "entity.name.function.member",
            "support.type",
            "keyword",
            "keyword.control",
            "keyword.operator",
            "storage.type",
            "entity.other.attribute-name",
        ] {
            assert_eq!(
                theme.get_style(scope).and_then(|style| style.bg),
                None,
                "unexpected syntax background for {scope}"
            );
        }
    }

    #[test]
    fn test_theme_with_comments() {
        parse_vscode_theme("src/fixtures/nord.json").unwrap();
    }

    #[test]
    fn direct_json_theme_loading_preserves_comment_markers_and_commented_files() {
        for (contents, expected_name) in [
            (
                r##"{
                    "name": "https://example.test/theme/*literal*/",
                    "colors": { "editor.background": "#102030" },
                    "tokenColors": []
                }"##,
                "https://example.test/theme/*literal*/",
            ),
            (
                r##"{
                    // VS Code permits line comments.
                    "name": "commented",
                    "colors": {
                        /* Workbench colors also allow block comments. */
                        "editor.background": "#102030"
                    },
                    "tokenColors": []
                }"##,
                "commented",
            ),
        ] {
            let theme = parse_vscode_theme_contents(contents).unwrap();
            assert_eq!(theme.name, expected_name);
            assert_eq!(
                theme.style.bg,
                Some(Color::Rgb {
                    r: 16,
                    g: 32,
                    b: 48,
                })
            );
        }
    }

    #[test]
    fn test_theme_parsing_accepts_short_hex_and_missing_foreground() {
        let theme = parse_vscode_theme_contents(
            r##"
            {
                "name": "partial",
                "colors": {
                    "editor.background": "#123",
                    "editorLineNumber.foreground": "transparent",
                    "editor.findMatchBackground": 7
                },
                "tokenColors": [
                    {
                        "scope": "keyword",
                        "settings": {
                            "foreground": "#fff",
                            "fontStyle": 4
                        }
                    }
                ]
            }
            "##,
        )
        .unwrap();

        assert_eq!(
            theme.style.bg,
            Some(Color::Rgb {
                r: 17,
                g: 34,
                b: 51,
            })
        );
        assert_eq!(
            theme.style.fg,
            Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            })
        );
        assert_eq!(
            theme.gutter_style.fg,
            Some(Color::Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            })
        );
        assert!(theme.find_match_style.is_none());
        assert_eq!(
            theme.token_styles[0].style.fg,
            Some(Color::Rgb {
                r: 255,
                g: 255,
                b: 255,
            })
        );
    }

    #[test]
    fn test_jsx_scopes_map_to_tree_sitter_captures() {
        let theme = parse_vscode_theme("themes/andromeda-bordered.json").unwrap();

        assert!(theme.get_style("tag").is_some());
        assert!(theme.get_style("attribute").is_some());
    }

    #[test]
    fn test_textmate_property_scopes_map_to_tree_sitter_property_capture() {
        for scope in [
            "support.type.property-name.yaml",
            "punctuation.support.type.property-name.json",
            "source.yaml entity.name.tag.yaml",
            "source.json meta.structure.dictionary.json support.type.property-name.json",
            "variable.other.member",
            "variable.other.property.ts",
        ] {
            assert_eq!(translate_scope(scope.to_string()), "property", "{scope}");
        }

        assert_eq!(translate_scope("entity.name.tag".to_string()), "tag");
    }

    #[test]
    fn test_bundled_themes_parse() {
        let themes_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("themes");
        let mut theme_files = std::fs::read_dir(&themes_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        theme_files.sort();

        assert!(!theme_files.is_empty());

        for theme_file in theme_files {
            let theme_file = theme_file.to_string_lossy();
            match std::panic::catch_unwind(|| parse_vscode_theme(&theme_file)) {
                Ok(Ok(theme)) => {
                    let popup_bg = theme.ui_style.popup.bg.unwrap();
                    let selected_bg = theme.ui_style.picker_selected_item.bg.unwrap();
                    let selected_fg = theme.ui_style.picker_selected_item.fg.unwrap();
                    assert!(
                        contrast_ratio(selected_bg, popup_bg) >= MINIMUM_SELECTION_STATE_CONTRAST,
                        "selection background contrast failed for {theme_file}"
                    );
                    assert!(
                        contrast_ratio(selected_fg, selected_bg) >= MINIMUM_SELECTION_TEXT_CONTRAST,
                        "selection text contrast failed for {theme_file}"
                    );

                    let editor_selection = theme.selected_style(
                        &theme.style,
                        &theme.editor_selection_style(),
                        SelectionForegroundPriority::Selection,
                    );
                    assert!(
                        contrast_ratio(editor_selection.bg.unwrap(), theme.style.bg.unwrap())
                            >= MINIMUM_SELECTION_STATE_CONTRAST,
                        "editor selection background contrast failed for {theme_file}"
                    );
                    assert!(
                        contrast_ratio(editor_selection.fg.unwrap(), editor_selection.bg.unwrap())
                            >= MINIMUM_SELECTION_TEXT_CONTRAST,
                        "editor selection text contrast failed for {theme_file}"
                    );

                    let cursor = theme.synthetic_cursor_style(&theme.style);
                    assert!(
                        contrast_ratio(cursor.bg.unwrap(), theme.style.bg.unwrap())
                            >= MINIMUM_CURSOR_STATE_CONTRAST,
                        "cursor background contrast failed for {theme_file}"
                    );
                    assert!(
                        contrast_ratio(cursor.fg.unwrap(), cursor.bg.unwrap())
                            >= MINIMUM_CURSOR_TEXT_CONTRAST,
                        "cursor text contrast failed for {theme_file}"
                    );

                    let editor_bg = theme.style.bg.unwrap();
                    let terminal_cursor = theme.terminal_cursor_color(&theme.style);
                    assert!(
                        contrast_ratio(terminal_cursor, editor_bg) >= MINIMUM_CURSOR_STATE_CONTRAST,
                        "terminal cursor contrast failed for {theme_file}"
                    );
                    let picker_bg = blend_color(
                        theme.ui_style.picker_prompt.bg.unwrap_or(editor_bg),
                        editor_bg,
                    );
                    let picker_cursor = theme.terminal_cursor_color(&theme.ui_style.picker_prompt);
                    assert!(
                        contrast_ratio(picker_cursor, picker_bg) >= MINIMUM_CURSOR_STATE_CONTRAST,
                        "picker cursor contrast failed for {theme_file}"
                    );
                }
                Ok(Err(error)) => panic!("failed to parse {theme_file}: {error}"),
                Err(error) => {
                    let message = error
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| error.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("parser panicked");
                    panic!("failed to parse {theme_file}: {message}");
                }
            }
        }
    }

    fn write_theme(dir: &std::path::Path, name: &str, contents: &str) {
        let themes = dir.join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        std::fs::write(themes.join(name), contents).unwrap();
    }

    fn rgb(hex: &str) -> Color {
        crate::color::parse_rgb(hex).unwrap()
    }

    #[test]
    fn extends_bundled_parent_merges_colors_tokens_and_type() {
        let dir = tempfile::tempdir().unwrap();
        write_theme(
            dir.path(),
            "child.json",
            r##"{
                "name": "child",
                "extends": "red.json",
                "colors": { "editor.background": "#FFFFFF" },
                "tokenColors": [{
                    "scope": "comment",
                    "settings": { "foreground": "#ABCDEF" }
                }]
            }"##,
        );
        write_theme(
            dir.path(),
            "typed.json",
            r##"{
                "name": "typed",
                "extends": "red.json",
                "type": "light",
                "colors": { "editor.background": "#FFFFFF" },
                "tokenColors": []
            }"##,
        );

        let parent = load_resolved_theme("red.json", dir.path()).unwrap();
        let child = load_resolved_theme("child.json", dir.path()).unwrap();
        let resolved = resolved_vscode_theme_json("child.json", dir.path()).unwrap();
        let parent_json = resolved_vscode_theme_json("red.json", dir.path()).unwrap();

        assert_eq!(child.style.fg, parent.style.fg);
        assert_eq!(child.style.bg, Some(rgb("#FFFFFF")));
        assert_ne!(child.style.bg, parent.style.bg);
        assert_eq!(
            child.get_style("comment").and_then(|style| style.fg),
            Some(rgb("#ABCDEF"))
        );
        assert_eq!(
            child.get_style("string").and_then(|style| style.fg),
            parent.get_style("string").and_then(|style| style.fg)
        );
        assert!(resolved.get("extends").is_none());
        assert_eq!(resolved["type"], parent_json["type"]);
        assert_eq!(resolved["colors"]["editor.background"], "#FFFFFF");
        assert_eq!(
            resolved["colors"]["editor.foreground"],
            parent_json["colors"]["editor.foreground"]
        );
        let tokens = resolved["tokenColors"].as_array().unwrap();
        assert_eq!(tokens.last().unwrap()["settings"]["foreground"], "#ABCDEF");
        assert_ne!(tokens.first().unwrap()["settings"]["foreground"], "#ABCDEF");

        let typed = resolved_vscode_theme_json("typed.json", dir.path()).unwrap();
        assert_eq!(typed["type"], "light");
    }

    #[test]
    fn extends_user_parent_overrides_a_bundled_name_and_keeps_rule_order() {
        let dir = tempfile::tempdir().unwrap();
        write_theme(
            dir.path(),
            "red.json",
            r##"{
                "name": "user-red",
                "type": "light",
                "colors": {
                    "editor.background": "#EEEEEE",
                    "editor.foreground": "#ABCABC",
                    "editor.lineHighlightBackground": "#DDDDDD"
                },
                "tokenColors": [
                    {
                        "scope": "comment",
                        "settings": { "foreground": "#111111", "fontStyle": "italic" }
                    },
                    {
                        "scope": "comment",
                        "settings": { "foreground": "#999999" }
                    },
                    {
                        "scope": "string",
                        "settings": { "foreground": "#222222" }
                    }
                ]
            }"##,
        );
        write_theme(
            dir.path(),
            "child.json",
            r##"{
                // A short override of the user copy, not the bundled theme.
                "name": "user-child",
                "extends": "red.json",
                "colors": { "editor.background": "#FFFFFF" },
                "tokenColors": [{
                    "scope": "comment",
                    "settings": { "foreground": "#333333" }
                }]
            }"##,
        );
        write_theme(
            dir.path(),
            "grand.json",
            r##"{
                "name": "grand",
                "type": "dark",
                "colors": {
                    "editor.background": "#101010",
                    "editor.foreground": "#EFEFEF"
                },
                "tokenColors": [
                    { "scope": "comment", "settings": { "foreground": "#AAAAAA" } },
                    { "scope": "keyword", "settings": { "foreground": "#BBBBBB" } }
                ]
            }"##,
        );
        write_theme(
            dir.path(),
            "mid.json",
            r##"{
                "name": "mid",
                "extends": "grand.json",
                "tokenColors": [
                    { "scope": "comment", "settings": { "foreground": "#CCCCCC" } }
                ]
            }"##,
        );
        write_theme(
            dir.path(),
            "nested.json",
            r##"{ "name": "nested-child", "extends": "mid.json" }"##,
        );
        write_theme(
            dir.path(),
            "order.json",
            r##"{
                "name": "order",
                "extends": "red.json",
                "colors": { "editor.background": "#FFFFFF" },
                "tokenColors": []
            }"##,
        );

        let child = load_resolved_theme("child.json", dir.path()).unwrap();
        assert_eq!(child.name, "user-child");
        assert_eq!(child.style.bg, Some(rgb("#FFFFFF")));
        assert_eq!(child.style.fg, Some(rgb("#ABCABC")));
        assert_eq!(
            child.colors.get("editor.lineHighlightBackground").copied(),
            Some(rgb("#DDDDDD"))
        );
        assert_eq!(
            child.get_style("comment").and_then(|style| style.fg),
            Some(rgb("#333333"))
        );
        assert_eq!(
            child.get_style("string").and_then(|style| style.fg),
            Some(rgb("#222222"))
        );

        let resolved = resolved_vscode_theme_json("child.json", dir.path()).unwrap();
        assert_eq!(resolved["type"], "light");
        assert!(resolved.get("extends").is_none());
        let tokens = resolved["tokenColors"].as_array().unwrap();
        assert_eq!(tokens[0]["settings"]["foreground"], "#111111");
        assert_eq!(tokens[1]["settings"]["foreground"], "#999999");
        assert_eq!(tokens[2]["settings"]["foreground"], "#222222");
        assert_eq!(tokens[3]["settings"]["foreground"], "#333333");

        let inherited = load_resolved_theme("order.json", dir.path()).unwrap();
        assert_eq!(
            inherited.get_style("comment").and_then(|style| style.fg),
            Some(rgb("#111111"))
        );

        let nested = load_resolved_theme("nested.json", dir.path()).unwrap();
        let nested_json = resolved_vscode_theme_json("nested.json", dir.path()).unwrap();
        assert_eq!(nested_json["type"], "dark");
        assert_eq!(nested.style.bg, Some(rgb("#101010")));
        assert_eq!(
            nested.get_style("comment").and_then(|style| style.fg),
            Some(rgb("#CCCCCC"))
        );
        assert_eq!(
            nested.get_style("keyword").and_then(|style| style.fg),
            Some(rgb("#BBBBBB"))
        );
    }

    #[test]
    fn extends_reports_cycles_missing_parents_and_depth() {
        let dir = tempfile::tempdir().unwrap();
        write_theme(
            dir.path(),
            "a.json",
            r##"{
                "name": "a",
                "extends": "b.json",
                "colors": { "editor.background": "#000000" },
                "tokenColors": []
            }"##,
        );
        write_theme(
            dir.path(),
            "b.json",
            r##"{
                "name": "b",
                "extends": "a.json",
                "colors": { "editor.background": "#000000" },
                "tokenColors": []
            }"##,
        );
        let cycle = load_resolved_theme("a.json", dir.path())
            .unwrap_err()
            .to_string();
        assert!(cycle.contains("a.json → b.json → a.json"), "{cycle}");

        write_theme(
            dir.path(),
            "missing-child.json",
            r##"{
                "name": "missing",
                "extends": "nope.json",
                "colors": { "editor.background": "#000000" },
                "tokenColors": []
            }"##,
        );
        let missing = load_resolved_theme("missing-child.json", dir.path())
            .unwrap_err()
            .to_string();
        assert!(
            missing.contains("theme nope.json not found: missing-child.json → nope.json"),
            "{missing}"
        );

        for index in 0..=4 {
            let parent = (index < 4).then(|| format!("d{}.json", index + 1));
            write_theme(
                dir.path(),
                &format!("d{index}.json"),
                &chain_theme(&format!("d{index}"), parent.as_deref()),
            );
        }
        assert!(load_resolved_theme("d0.json", dir.path()).is_ok());

        for index in 0..=5 {
            let parent = (index < 5).then(|| format!("e{}.json", index + 1));
            write_theme(
                dir.path(),
                &format!("e{index}.json"),
                &chain_theme(&format!("e{index}"), parent.as_deref()),
            );
        }
        let deep = load_resolved_theme("e0.json", dir.path())
            .unwrap_err()
            .to_string();
        assert!(
            deep.contains(
                "theme inheritance is deeper than 4: e0.json → e1.json → e2.json → e3.json → e4.json → e5.json"
            ),
            "{deep}"
        );
    }

    fn chain_theme(name: &str, parent: Option<&str>) -> String {
        let extends = parent
            .map(|parent| format!("\"extends\": \"{parent}\",\n  "))
            .unwrap_or_default();
        format!(
            "{{\n  \"name\": \"{name}\",\n  {extends}\"type\": \"dark\",\n  \"colors\": {{ \"editor.background\": \"#010101\", \"editor.foreground\": \"#EEEEEE\" }},\n  \"tokenColors\": []\n}}\n"
        )
    }
}
