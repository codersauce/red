//! Authoritative lookup surface for existing file, symbol, and completion icons.

use crate::{color::Color, config::PickerIconStyle, lsp::types::CompletionItemKind};

use super::picker::{picker_file_icon, picker_file_icon_color, picker_kind_icon};

/// A terminal icon and its optional semantic foreground.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IconSpec {
    pub(crate) glyph: &'static str,
    pub(crate) color: Option<Color>,
}

/// Reuses the established filename-first inventory and configured icon modes.
pub(crate) struct IconCatalog;

impl IconCatalog {
    /// Resolves a file without changing existing extension or filename precedence.
    #[must_use]
    pub(crate) fn file(path: &str, style: PickerIconStyle) -> IconSpec {
        IconSpec {
            glyph: picker_file_icon(path, style),
            color: picker_file_icon_color(path),
        }
    }

    /// Resolves a structured picker symbol in its configured visual mode.
    #[must_use]
    pub(crate) fn symbol(kind: &str, style: PickerIconStyle) -> IconSpec {
        IconSpec {
            glyph: picker_kind_icon(kind, style),
            color: None,
        }
    }

    /// Resolves LSP completion kinds to single-column glyphs for aligned menu rows.
    #[must_use]
    pub(crate) fn completion(kind: &CompletionItemKind) -> IconSpec {
        let glyph = match kind {
            CompletionItemKind::Text => "≡",
            CompletionItemKind::Method => "ƒ",
            CompletionItemKind::Function => "λ",
            CompletionItemKind::Constructor => "◇",
            CompletionItemKind::Field => "◆",
            CompletionItemKind::Variable => "𝑥",
            CompletionItemKind::Class => "○",
            CompletionItemKind::Interface => "◌",
            CompletionItemKind::Module => "□",
            CompletionItemKind::Property => "◇",
            CompletionItemKind::Unit => "∅",
            CompletionItemKind::Value => "=",
            CompletionItemKind::Enum => "ℰ",
            CompletionItemKind::Keyword => "κ",
            CompletionItemKind::Snippet => "✂",
            CompletionItemKind::Color => "◉",
            CompletionItemKind::File => "▤",
            CompletionItemKind::Reference => "→",
            CompletionItemKind::Folder => "▸",
            CompletionItemKind::EnumMember => "ℯ",
            CompletionItemKind::Constant => "π",
            CompletionItemKind::Struct => "▦",
            CompletionItemKind::Event => "↯",
            CompletionItemKind::Operator => "±",
            CompletionItemKind::TypeParameter => "𝑇",
        };
        IconSpec { glyph, color: None }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        config::PickerIconStyle, lsp::types::CompletionItemKind, unicode_utils::display_width,
    };

    use super::IconCatalog;

    #[test]
    fn filename_icons_keep_precedence_over_the_extension_fallback() {
        let readme = IconCatalog::file("docs/README.md", PickerIconStyle::NerdFont);

        assert_eq!(readme.glyph, "󰂺");
        assert!(readme.color.is_some());
    }

    #[test]
    fn icon_modes_preserve_hidden_and_ascii_symbols() {
        assert_eq!(
            IconCatalog::symbol("Function", PickerIconStyle::Ascii).glyph,
            "fn"
        );
        assert_eq!(
            IconCatalog::symbol("Function", PickerIconStyle::None).glyph,
            ""
        );
    }

    #[test]
    fn lsp_completion_uses_a_compact_file_icon() {
        assert_eq!(
            IconCatalog::completion(&CompletionItemKind::File).glyph,
            "▤"
        );
    }

    #[test]
    fn lsp_text_completion_uses_an_icon_that_fits_the_icon_column() {
        assert_eq!(
            IconCatalog::completion(&CompletionItemKind::Text).glyph,
            "≡"
        );
    }

    #[test]
    fn every_lsp_completion_icon_occupies_one_terminal_column() {
        let kinds = [
            CompletionItemKind::Text,
            CompletionItemKind::Method,
            CompletionItemKind::Function,
            CompletionItemKind::Constructor,
            CompletionItemKind::Field,
            CompletionItemKind::Variable,
            CompletionItemKind::Class,
            CompletionItemKind::Interface,
            CompletionItemKind::Module,
            CompletionItemKind::Property,
            CompletionItemKind::Unit,
            CompletionItemKind::Value,
            CompletionItemKind::Enum,
            CompletionItemKind::Keyword,
            CompletionItemKind::Snippet,
            CompletionItemKind::Color,
            CompletionItemKind::File,
            CompletionItemKind::Reference,
            CompletionItemKind::Folder,
            CompletionItemKind::EnumMember,
            CompletionItemKind::Constant,
            CompletionItemKind::Struct,
            CompletionItemKind::Event,
            CompletionItemKind::Operator,
            CompletionItemKind::TypeParameter,
        ];

        for kind in kinds {
            let glyph = IconCatalog::completion(&kind).glyph;
            assert_eq!(display_width(glyph), 1, "{kind:?} uses {glyph:?}");
        }
    }
}
