//! Compact, lazily decorated row models for large filesystem tree panels.
//!
//! Directory entries remain shared with the Husk plugin state. The flattened model
//! retains only entry coordinates and ancestry, and creates rich [`PanelRow`] values
//! only for the terminal viewport or a selected row.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use husk_runtime::Value;

use crate::theme::ThemeStyleSpec;

use super::{PanelRow, PanelRowKind, PanelSegment};

const FOLDER_STYLE: &[&str] = &[
    "symbolIcon.folderForeground",
    "sideBarTitle.foreground",
    "list.highlightForeground",
    "scope:entity.name.type",
    "editor.foreground",
];
const INDENT_STYLE: &[&str] = &[
    "tree.indentGuidesStroke",
    "editorIndentGuide.background",
    "editorLineNumber.foreground",
    "scope:comment",
];
const FILE_STYLE: &[&str] = &[
    "symbolIcon.fileForeground",
    "sideBar.foreground",
    "editor.foreground",
];
const FILE_NAME_STYLE: &[&str] = &["sideBar.foreground", "editor.foreground"];

#[derive(Debug, Clone)]
struct TreeDirectory {
    entries: Arc<Vec<Value>>,
}

#[derive(Debug, Clone, Copy)]
enum TreeRowSource {
    Root,
    Entry { directory: u32, entry: u32 },
}

#[derive(Debug, Clone, Copy)]
struct TreeRow {
    source: TreeRowSource,
    parent: Option<u32>,
    last: bool,
    expanded: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum GitStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
    Untracked,
    Ignored,
    Staged,
    Conflict,
    #[default]
    Unknown,
}

impl GitStatus {
    fn parse(value: &str) -> Self {
        match value {
            "added" => Self::Added,
            "deleted" => Self::Deleted,
            "modified" => Self::Modified,
            "renamed" => Self::Renamed,
            "untracked" => Self::Untracked,
            "ignored" => Self::Ignored,
            "staged" => Self::Staged,
            "conflict" => Self::Conflict,
            _ => Self::Unknown,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Added => "✚",
            Self::Deleted => "✖",
            Self::Modified => "",
            Self::Renamed => "",
            Self::Untracked => "",
            Self::Ignored => "",
            Self::Staged => "",
            Self::Conflict => "",
            Self::Unknown => "",
        }
    }

    fn style(self) -> &'static [&'static str] {
        match self {
            Self::Added => &[
                "gitDecoration.addedResourceForeground",
                "gitDecoration.untrackedResourceForeground",
                "editorGutter.addedBackground",
                "scope:string",
            ],
            Self::Deleted => &[
                "gitDecoration.deletedResourceForeground",
                "gitDecoration.stageDeletedResourceForeground",
                "editorGutter.deletedBackground",
                "editorError.foreground",
                "scope:markup.deleted",
                "scope:invalid.illegal",
                "scope:keyword",
            ],
            Self::Modified => &[
                "gitDecoration.modifiedResourceForeground",
                "gitDecoration.stageModifiedResourceForeground",
                "editorGutter.modifiedBackground",
                "scope:constant.numeric",
            ],
            Self::Renamed => &[
                "gitDecoration.renamedResourceForeground",
                "gitDecoration.modifiedResourceForeground",
                "editorGutter.modifiedBackground",
                "scope:constant.numeric",
            ],
            Self::Untracked => &[
                "gitDecoration.untrackedResourceForeground",
                "gitDecoration.addedResourceForeground",
                "editorGutter.addedBackground",
                "scope:string",
            ],
            Self::Ignored => &[
                "gitDecoration.ignoredResourceForeground",
                "editorLineNumber.foreground",
                "scope:comment",
            ],
            Self::Staged => &[
                "gitDecoration.stageModifiedResourceForeground",
                "gitDecoration.addedResourceForeground",
                "gitDecoration.untrackedResourceForeground",
                "editorGutter.addedBackground",
                "scope:string",
            ],
            Self::Conflict => &[
                "gitDecoration.conflictingResourceForeground",
                "editorError.foreground",
                "scope:invalid.illegal",
                "scope:markup.deleted",
                "scope:keyword",
            ],
            Self::Unknown => &[],
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ClipboardAction {
    Copy,
    Move,
}

/// A complete flattened filesystem tree with viewport-only row decoration.
#[derive(Debug, Clone)]
pub struct TreePanelModel {
    cwd: String,
    status_root: String,
    directories: Vec<TreeDirectory>,
    rows: Vec<TreeRow>,
    statuses: HashMap<String, GitStatus>,
    selected: HashSet<String>,
    clipboard: HashMap<String, ClipboardAction>,
}

impl TreePanelModel {
    /// Builds a complete index while borrowing shared directory-entry arrays from Husk.
    pub(crate) fn from_husk_values(args: &[Value]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            args.len() == 7,
            "a tree panel model requires seven arguments"
        );
        let cwd = args[0]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("tree panel cwd must be a string"))?
            .to_string();
        let children = value_array(&args[1], "children")?;
        let expanded = value_array(&args[2], "expanded")?
            .iter()
            .filter_map(Value::as_str)
            .collect::<HashSet<_>>();
        let selected = value_array(&args[3], "selected")?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<HashSet<_>>();
        let clipboard = value_array(&args[4], "clipboard")?
            .iter()
            .filter_map(|entry| {
                let path = value_field_string(entry, "path")?;
                let action = match value_field_string(entry, "action")? {
                    "copy" => ClipboardAction::Copy,
                    "move" => ClipboardAction::Move,
                    _ => return None,
                };
                Some((path.to_string(), action))
            })
            .collect::<HashMap<_, _>>();
        let status_root = normalize_path(args[5].as_str().unwrap_or_default());
        let statuses = value_array(&args[6], "statuses")?
            .iter()
            .filter_map(|entry| {
                Some((
                    value_field_string(entry, "path")?.to_string(),
                    GitStatus::parse(value_field_string(entry, "status")?),
                ))
            })
            .collect::<HashMap<_, _>>();

        let mut directories = Vec::with_capacity(children.len());
        let mut directory_indices = HashMap::with_capacity(children.len());
        for child in children {
            let Some(path) = value_field_string(child, "path") else {
                continue;
            };
            let Some(entries) = value_field_array(child, "entries") else {
                continue;
            };
            directory_indices.insert(path.to_string(), directories.len());
            directories.push(TreeDirectory {
                entries: Arc::clone(entries),
            });
        }

        let root_expanded = expanded.contains(".");
        let mut model = Self {
            cwd,
            status_root,
            directories,
            rows: vec![TreeRow {
                source: TreeRowSource::Root,
                parent: None,
                last: true,
                expanded: root_expanded,
            }],
            statuses,
            selected,
            clipboard,
        };
        if root_expanded {
            if let Some(&root) = directory_indices.get(".") {
                model.append_directory(root, 0, &directory_indices, &expanded)?;
            }
        }
        Ok(model)
    }

    fn append_directory(
        &mut self,
        directory: usize,
        parent: usize,
        directory_indices: &HashMap<String, usize>,
        expanded: &HashSet<&str>,
    ) -> anyhow::Result<()> {
        let entries = Arc::clone(&self.directories[directory].entries);
        let last = entries.iter().rposition(|entry| {
            matches!(
                value_field_string(entry, "kind"),
                Some("file" | "directory")
            )
        });
        for (index, entry) in entries.iter().enumerate() {
            let Some(kind @ ("file" | "directory")) = value_field_string(entry, "kind") else {
                continue;
            };
            let Some(path) = value_field_string(entry, "path") else {
                continue;
            };
            let open = kind == "directory" && expanded.contains(path);
            let row_index = self.rows.len();
            self.rows.push(TreeRow {
                source: TreeRowSource::Entry {
                    directory: u32::try_from(directory)
                        .map_err(|_| anyhow::anyhow!("tree directory index exceeds u32"))?,
                    entry: u32::try_from(index)
                        .map_err(|_| anyhow::anyhow!("tree directory entry index exceeds u32"))?,
                },
                parent: Some(
                    u32::try_from(parent)
                        .map_err(|_| anyhow::anyhow!("tree parent index exceeds u32"))?,
                ),
                last: Some(index) == last,
                expanded: open,
            });
            if open {
                if let Some(&child) = directory_indices.get(path) {
                    self.append_directory(child, row_index, directory_indices, expanded)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn position(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|row| match row.source {
            TreeRowSource::Root => id == ".",
            TreeRowSource::Entry { directory, entry } => {
                self.directories[directory as usize]
                    .entries
                    .get(entry as usize)
                    .and_then(|value| value_field_string(value, "path"))
                    == Some(id)
            }
        })
    }

    pub(crate) fn row(&self, index: usize) -> Option<PanelRow> {
        let row = self.rows.get(index)?;
        match row.source {
            TreeRowSource::Root => Some(self.root_row(row.expanded)),
            TreeRowSource::Entry { directory, entry } => {
                let entry = self
                    .directories
                    .get(directory as usize)?
                    .entries
                    .get(entry as usize)?;
                self.entry_row(index, row, entry)
            }
        }
    }

    fn root_row(&self, expanded: bool) -> PanelRow {
        let name = self.cwd.rsplit('/').next().unwrap_or(&self.cwd);
        let status = self.status_for_path(".");
        PanelRow {
            id: ".".to_string(),
            path: Some(".".to_string()),
            expanded: Some(expanded),
            kind: PanelRowKind::Directory,
            segments: vec![
                segment(" ", &[], false),
                segment(" ", FOLDER_STYLE, false),
                segment(
                    name,
                    &[
                        "sideBarTitle.foreground",
                        "symbolIcon.folderForeground",
                        "list.highlightForeground",
                        "scope:entity.name.type",
                        "editor.foreground",
                    ],
                    true,
                ),
            ],
            right_segments: visible_status_segments(status, true, expanded),
        }
    }

    fn entry_row(&self, index: usize, row: &TreeRow, entry: &Value) -> Option<PanelRow> {
        let name = value_field_string(entry, "name")?;
        let path = value_field_string(entry, "path")?;
        let directory = value_field_string(entry, "kind")? == "directory";
        let status = self.status_for_path(path);
        let mut segments = vec![segment(
            self.selection_marker(path),
            &["list.highlightForeground"],
            true,
        )];
        self.append_branch_segments(index, &mut segments);
        segments.push(segment(
            format!("{} ", entry_icon(name, directory, row.expanded)),
            entry_icon_style(name, status, directory),
            false,
        ));
        segments.push(segment(name, entry_name_style(status, directory), false));
        Some(PanelRow {
            id: path.to_string(),
            path: Some(path.to_string()),
            expanded: Some(row.expanded),
            kind: if directory {
                PanelRowKind::Directory
            } else {
                PanelRowKind::File
            },
            segments,
            right_segments: visible_status_segments(status, directory, row.expanded),
        })
    }

    fn append_branch_segments(&self, index: usize, segments: &mut Vec<PanelSegment>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let mut ancestors = Vec::new();
        let mut parent = row.parent;
        while let Some(parent_index) = parent {
            let Some(ancestor) = self.rows.get(parent_index as usize) else {
                break;
            };
            if matches!(ancestor.source, TreeRowSource::Root) {
                break;
            }
            ancestors.push(ancestor.last);
            parent = ancestor.parent;
        }
        ancestors.reverse();

        if ancestors.is_empty() {
            segments.push(segment("  ", INDENT_STYLE, false));
            return;
        }
        for (depth, last) in ancestors.into_iter().enumerate() {
            segments.push(segment(
                if depth > 0 && !last { "│ " } else { "  " },
                INDENT_STYLE,
                false,
            ));
        }
        segments.push(segment(
            if row.last { "└ " } else { "├ " },
            INDENT_STYLE,
            false,
        ));
    }

    fn selection_marker(&self, path: &str) -> &'static str {
        if self.selected.contains(path) {
            return "✓ ";
        }
        match self.clipboard.get(path) {
            Some(ClipboardAction::Move) => "✂ ",
            Some(ClipboardAction::Copy) => "⧉ ",
            None => "  ",
        }
    }

    fn status_for_path(&self, path: &str) -> GitStatus {
        if self.status_root.is_empty() {
            return GitStatus::Unknown;
        }
        let relative = path.strip_prefix("./").unwrap_or(path);
        let absolute = if relative.is_empty() || relative == "." {
            self.status_root.clone()
        } else if self.status_root == "/" {
            format!("/{relative}")
        } else {
            format!("{}/{relative}", self.status_root)
        };
        self.statuses.get(&absolute).copied().unwrap_or_default()
    }
}

fn value_array<'a>(value: &'a Value, name: &str) -> anyhow::Result<&'a [Value]> {
    match value {
        Value::Array(values) => Ok(values.as_slice()),
        _ => anyhow::bail!("tree panel {name} must be an array"),
    }
}

fn value_field_string<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    match value {
        Value::Object(fields) | Value::Struct { fields, .. } => fields.get(name)?.as_str(),
        Value::Json(value) => value.get(name)?.as_str(),
        _ => None,
    }
}

fn value_field_array<'a>(value: &'a Value, name: &str) -> Option<&'a Arc<Vec<Value>>> {
    match value {
        Value::Object(fields) | Value::Struct { fields, .. } => match fields.get(name)? {
            Value::Array(values) => Some(values),
            _ => None,
        },
        _ => None,
    }
}

fn normalize_path(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
}

fn entry_icon(name: &str, directory: bool, expanded: bool) -> &'static str {
    if directory {
        return if expanded { "" } else { "" };
    }
    match extension(name).as_str() {
        "js" | "mjs" | "cjs" => "",
        "ts" => "",
        "tsx" | "jsx" => "",
        "json" => "",
        "toml" => "",
        "rs" => "",
        "lua" => "",
        "md" | "markdown" => "",
        "lock" => "",
        "sh" | "zsh" | "fish" => "",
        _ => "󰈙",
    }
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map_or(name, |(_, suffix)| suffix)
        .to_ascii_lowercase()
}

fn entry_icon_style(name: &str, status: GitStatus, directory: bool) -> &'static [&'static str] {
    if status == GitStatus::Ignored {
        return status.style();
    }
    if directory {
        return FOLDER_STYLE;
    }
    match extension(name).as_str() {
        "rs" | "toml" => &[
            "terminal.ansiBrightYellow",
            "terminal.ansiYellow",
            "scope:constant.numeric",
            "sideBar.foreground",
            "editor.foreground",
        ],
        "js" | "mjs" | "cjs" | "json" => &[
            "terminal.ansiYellow",
            "terminal.ansiBrightYellow",
            "scope:constant.numeric",
            "sideBar.foreground",
            "editor.foreground",
        ],
        "ts" | "lua" => &[
            "terminal.ansiBlue",
            "terminal.ansiCyan",
            "scope:entity.name.function",
            "sideBar.foreground",
            "editor.foreground",
        ],
        "tsx" | "jsx" => &[
            "terminal.ansiCyan",
            "terminal.ansiBlue",
            "scope:entity.name.tag",
            "scope:entity.name.function",
            "sideBar.foreground",
            "editor.foreground",
        ],
        "sh" | "zsh" | "fish" => &[
            "terminal.ansiGreen",
            "scope:string",
            "sideBar.foreground",
            "editor.foreground",
        ],
        "md" | "markdown" => &[
            "scope:markup.heading",
            "terminal.ansiBlue",
            "scope:entity.name.function",
            "sideBar.foreground",
            "editor.foreground",
        ],
        "lock" => &[
            "descriptionForeground",
            "editorLineNumber.foreground",
            "scope:comment",
            "sideBar.foreground",
            "editor.foreground",
        ],
        _ => FILE_STYLE,
    }
}

fn entry_name_style(status: GitStatus, directory: bool) -> &'static [&'static str] {
    let style = status.style();
    if !style.is_empty() {
        style
    } else if directory {
        FOLDER_STYLE
    } else {
        FILE_NAME_STYLE
    }
}

fn visible_status_segments(
    status: GitStatus,
    directory: bool,
    expanded: bool,
) -> Vec<PanelSegment> {
    if status == GitStatus::Unknown || directory && expanded {
        return Vec::new();
    }
    vec![segment(
        status.symbol(),
        status.style(),
        status == GitStatus::Conflict,
    )]
}

fn segment(text: impl Into<String>, foreground: &[&str], bold: bool) -> PanelSegment {
    PanelSegment {
        text: text.into(),
        style: None,
        semantic: Some(ThemeStyleSpec {
            foreground: foreground
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            bold: Some(bold),
            ..ThemeStyleSpec::default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(children: serde_json::Value, expanded: serde_json::Value) -> TreePanelModel {
        TreePanelModel::from_husk_values(&[
            Value::String("/repo".to_string()),
            Value::from_json(children),
            Value::from_json(expanded),
            Value::from_json(serde_json::json!([])),
            Value::from_json(serde_json::json!([])),
            Value::String(String::new()),
            Value::from_json(serde_json::json!([])),
        ])
        .unwrap()
    }

    #[test]
    fn indexes_every_entry_without_allocating_decorated_rows() {
        let entries = (0..8_192)
            .map(|index| {
                serde_json::json!({
                    "name": format!("file-{index:04}.rs"),
                    "path": format!("./file-{index:04}.rs"),
                    "kind": "file",
                })
            })
            .collect::<Vec<_>>();
        let model = model(
            serde_json::json!([{ "path": ".", "entries": entries }]),
            serde_json::json!(["."]),
        );

        assert_eq!(model.len(), 8_193);
        assert_eq!(model.row(8_192).unwrap().id, "./file-8191.rs");
        assert!(std::mem::size_of::<TreeRow>() <= 24);
    }

    #[test]
    fn preserves_nested_tree_guides() {
        let model = model(
            serde_json::json!([
                {
                    "path": ".",
                    "entries": [{ "name": "src", "path": "./src", "kind": "directory" }]
                },
                {
                    "path": "./src",
                    "entries": [{ "name": "main.rs", "path": "./src/main.rs", "kind": "file" }]
                }
            ]),
            serde_json::json!([".", "./src"]),
        );

        let directory = model.row(1).unwrap();
        let file = model.row(2).unwrap();
        assert_eq!(directory.segments[1].text, "  ");
        assert_eq!(file.segments[1].text, "  ");
        assert_eq!(file.segments[2].text, "└ ");
        assert_eq!(file.segments[3].text, " ");
    }
}
