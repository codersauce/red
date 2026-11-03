//! Named Helix commands. Config snippets can use these snake_case names.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    MoveCharLeft,
    MoveCharRight,
    MoveLineUp,
    MoveLineDown,
    MoveNextWordStart,
    MovePrevWordStart,
    MoveNextWordEnd,
    MovePrevWordEnd,
    MoveNextLongWordStart,
    MovePrevLongWordStart,
    MoveNextLongWordEnd,
    MovePrevLongWordEnd,
    FindNextChar,
    FindPrevChar,
    TillNextChar,
    TillPrevChar,
    ExtendLineBelow,
    ExtendToLineBounds,
    ShrinkToLineBounds,
    SelectAll,
    CollapseSelection,
    FlipSelections,
    DeleteSelection,
    DeleteSelectionNoYank,
    ChangeSelection,
    ChangeSelectionNoYank,
    Yank,
    PasteAfter,
    PasteBefore,
    Replace,
    ReplaceWithYanked,
    SwitchCase,
    SwitchToLowercase,
    SwitchToUppercase,
    InsertMode,
    AppendMode,
    InsertAtLineStart,
    InsertAtLineEnd,
    OpenBelow,
    OpenAbove,
    SelectMode,
    NormalMode,
    GotoFileStart,
    GotoLastLine,
    GotoLine,
    GotoLineStart,
    GotoLineEnd,
    GotoFirstNonwhitespace,
    GotoWindowTop,
    GotoWindowCenter,
    GotoWindowBottom,
    GotoDefinition,
    GotoDeclaration,
    GotoTypeDefinition,
    GotoImplementation,
    GotoReference,
    GotoLastAccessedFile,
    GotoNextBuffer,
    GotoPreviousBuffer,
    GotoLastModification,
    MatchBrackets,
    SelectRegex,
    SplitSelection,
    SplitSelectionOnNewline,
    KeepSelections,
    RemoveSelections,
    KeepPrimarySelection,
    RemovePrimarySelection,
    RotateSelectionsForward,
    RotateSelectionsBackward,
    AlignSelections,
    TrimSelections,
    CopySelectionOnNextLine,
    CopySelectionOnPrevLine,
    SelectTextobjectAround,
    SelectTextobjectInner,
    SurroundAdd,
    SurroundReplace,
    SurroundDelete,
    ExpandSelection,
    ShrinkSelection,
    SelectNextSibling,
    SelectPrevSibling,
    SelectReferences,
    SearchNext,
    SearchPrev,
    SearchSelection,
    Indent,
    Unindent,
    JoinSelections,
    FormatSelections,
    ToggleComments,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    JumpBackward,
    JumpForward,
    Undo,
    Redo,
    Earlier,
    Later,
    CommandMode,
    Search,
    Rsearch,
    AlignViewCenter,
    AlignViewTop,
    AlignViewBottom,
    ScrollDown,
    ScrollUp,
    Hover,
    FilePicker,
    CodeAction,
}

impl Command {
    pub fn from_name(name: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
    }

    pub fn label(self) -> String {
        let Some(serde_json::Value::String(name)) = serde_json::to_value(self).ok() else {
            return "Command".to_string();
        };
        name.split('_')
            .map(|word| {
                let mut characters = word.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Commands whose next key is an argument rather than a binding.
    pub const fn needs_character(self) -> bool {
        matches!(
            self,
            Self::FindNextChar
                | Self::FindPrevChar
                | Self::TillNextChar
                | Self::TillPrevChar
                | Self::Replace
                | Self::SelectTextobjectAround
                | Self::SelectTextobjectInner
                | Self::SurroundAdd
                | Self::SurroundDelete
                | Self::SurroundReplace
        )
    }

    pub const fn needs_prompt(self) -> bool {
        matches!(
            self,
            Self::SelectRegex
                | Self::SplitSelection
                | Self::KeepSelections
                | Self::RemoveSelections
        )
    }
}
