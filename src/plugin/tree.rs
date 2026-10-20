//! Rust-owned tree layout shared by custom panels and filesystem trees.
//!
//! Directory entries remain shared with the Husk plugin state. The flattened model
//! retains only entry coordinates and ancestry, and creates rich [`PanelRow`] values
//! only for the terminal viewport or a selected row.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use husk_runtime::Value;
use serde::{Deserialize, Serialize};

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
    entries: TreeEntries,
    notice: String,
}

#[derive(Debug, Clone)]
enum TreeEntries {
    Filesystem(Arc<Vec<Value>>),
    Rows(Arc<Vec<PanelRow>>),
}

impl TreeEntries {
    fn len(&self) -> usize {
        match self {
            Self::Filesystem(entries) => entries.len(),
            Self::Rows(rows) => rows.len(),
        }
    }

    fn entry(&self, index: usize) -> Option<(&str, bool)> {
        match self {
            Self::Filesystem(entries) => {
                let entry = entries.get(index)?;
                let kind = value_field_string(entry, "kind")?;
                matches!(kind, "file" | "directory")
                    .then_some((value_field_string(entry, "path")?, kind == "directory"))
            }
            Self::Rows(rows) => rows
                .get(index)
                .map(|row| (row.id.as_str(), row.kind == PanelRowKind::Directory)),
        }
    }
}

/// An ordered, caller-decorated tree. IDs are opaque and need not be file paths.
/// Rust supplies ancestry guides, expansion layout, scrolling, and selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreePanelSpec {
    pub root: PanelRow,
    pub children: Vec<TreePanelChildren>,
    #[serde(default)]
    pub expanded: Vec<String>,
}

/// Authoritative children of one node, in display order. An empty list is loaded;
/// an omitted list displays a loading notice when its parent is expanded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreePanelChildren {
    pub parent: String,
    pub rows: Vec<PanelRow>,
}

#[derive(Debug, Deserialize)]
struct PathMatch {
    path: String,
    ranges: Vec<[usize; 2]>,
}

#[derive(Debug, Clone, Copy)]
enum TreeRowSource {
    Root,
    Entry { directory: u32, entry: u32 },
    Notice { directory: Option<u32> },
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

/// A complete tree index with viewport-only row decoration. Both custom nodes
/// and the filesystem adapter use the same layout and panel interaction path.
#[derive(Debug, Clone)]
pub struct TreePanelModel {
    root: Option<PanelRow>,
    cwd: String,
    status_root: String,
    directories: Vec<TreeDirectory>,
    rows: Vec<TreeRow>,
    last_position: Cell<Option<usize>>,
    statuses: HashMap<String, GitStatus>,
    selected: HashSet<String>,
    clipboard: HashMap<String, ClipboardAction>,
    matches: HashMap<String, Vec<[usize; 2]>>,
}

impl TreePanelModel {
    /// Builds a reusable tree without filesystem or Neo-tree conventions.
    pub fn new(spec: TreePanelSpec) -> anyhow::Result<Self> {
        let mut ids = HashMap::from([(spec.root.id.as_str(), &spec.root.kind)]);
        for children in &spec.children {
            for row in &children.rows {
                anyhow::ensure!(
                    ids.insert(&row.id, &row.kind).is_none(),
                    "duplicate tree node id `{}`",
                    row.id
                );
            }
        }
        anyhow::ensure!(
            ids.keys().all(|id| !id.starts_with("notice:")),
            "tree node ids starting with `notice:` are reserved for loading rows"
        );
        for children in &spec.children {
            anyhow::ensure!(
                ids.get(children.parent.as_str()) == Some(&&PanelRowKind::Directory),
                "tree parent `{}` must name a directory node",
                children.parent
            );
        }
        let expanded = spec
            .expanded
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let root_expanded =
            spec.root.kind == PanelRowKind::Directory && expanded.contains(spec.root.id.as_str());
        let mut directory_indices = HashMap::new();
        let mut directories = Vec::new();
        for children in spec.children {
            anyhow::ensure!(
                directory_indices
                    .insert(children.parent, directories.len())
                    .is_none(),
                "duplicate tree child listing"
            );
            directories.push(TreeDirectory {
                entries: TreeEntries::Rows(Arc::new(children.rows)),
                notice: String::new(),
            });
        }
        let mut model = Self {
            root: Some(spec.root),
            cwd: String::new(),
            status_root: String::new(),
            directories,
            rows: vec![TreeRow {
                source: TreeRowSource::Root,
                parent: None,
                last: true,
                expanded: root_expanded,
            }],
            last_position: Cell::new(None),
            statuses: HashMap::new(),
            selected: HashSet::new(),
            clipboard: HashMap::new(),
            matches: HashMap::new(),
        };
        model.index_children(&directory_indices, &expanded)?;
        Ok(model)
    }

    /// Builds a complete index while borrowing shared directory-entry arrays from Husk.
    pub fn from_husk_values(args: &[Value]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            matches!(args.len(), 7 | 8),
            "a filesystem tree requires seven arguments and optional match ranges"
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
                entries: TreeEntries::Filesystem(Arc::clone(entries)),
                notice: value_field_string(child, "notice")
                    .unwrap_or_default()
                    .to_string(),
            });
        }

        let root_expanded = expanded.contains(".");
        let mut model = Self {
            root: None,
            cwd,
            status_root,
            directories,
            rows: vec![TreeRow {
                source: TreeRowSource::Root,
                parent: None,
                last: true,
                expanded: root_expanded,
            }],
            last_position: Cell::new(None),
            statuses,
            selected,
            clipboard,
            matches: args
                .get(7)
                .map(|value| serde_json::from_value::<Vec<PathMatch>>(value.to_json()))
                .transpose()?
                .unwrap_or_default()
                .into_iter()
                .map(|entry| (entry.path, entry.ranges))
                .collect(),
        };
        model.index_children(&directory_indices, &expanded)?;
        Ok(model)
    }

    fn root_id(&self) -> &str {
        self.root.as_ref().map_or(".", |row| row.id.as_str())
    }

    fn index_children(
        &mut self,
        directory_indices: &HashMap<String, usize>,
        expanded: &HashSet<&str>,
    ) -> anyhow::Result<()> {
        if self.rows[0].expanded {
            if let Some(&root) = directory_indices.get(self.root_id()) {
                self.append_directory(root, 0, directory_indices, expanded)?;
            } else {
                self.append_notice(0, None)?;
            }
        }
        Ok(())
    }

    fn append_directory(
        &mut self,
        directory: usize,
        parent: usize,
        directory_indices: &HashMap<String, usize>,
        expanded: &HashSet<&str>,
    ) -> anyhow::Result<()> {
        // Use a heap stack so a deep, valid tree cannot overflow the native stack.
        let mut visited = HashSet::from([directory]);
        let mut stack = vec![(directory, parent, 0)];
        while let Some((directory, parent, next)) = stack.last_mut() {
            let entries = &self.directories[*directory].entries;
            if *next == entries.len() {
                let (directory, parent, _) = stack.pop().unwrap();
                if !self.directories[directory].notice.is_empty() {
                    self.append_notice(parent, Some(directory))?;
                }
                continue;
            }
            let index = *next;
            *next += 1;
            let Some((path, is_directory)) = entries.entry(index) else {
                continue;
            };
            let open = is_directory && expanded.contains(path);
            let child = open.then(|| directory_indices.get(path).copied()).flatten();
            let row_index = self.rows.len();
            self.rows.push(TreeRow {
                source: TreeRowSource::Entry {
                    directory: u32::try_from(*directory)
                        .map_err(|_| anyhow::anyhow!("tree directory index exceeds u32"))?,
                    entry: u32::try_from(index)
                        .map_err(|_| anyhow::anyhow!("tree directory entry index exceeds u32"))?,
                },
                parent: Some(
                    u32::try_from(*parent)
                        .map_err(|_| anyhow::anyhow!("tree parent index exceeds u32"))?,
                ),
                last: (index + 1..entries.len()).all(|index| entries.entry(index).is_none()),
                expanded: open,
            });
            if open {
                if let Some(child) = child {
                    anyhow::ensure!(
                        visited.insert(child),
                        "tree contains a cycle or repeated branch"
                    );
                    stack.push((child, row_index, 0));
                } else {
                    self.append_notice(row_index, None)?;
                }
            }
        }
        Ok(())
    }

    fn append_notice(&mut self, parent: usize, directory: Option<usize>) -> anyhow::Result<()> {
        self.rows.push(TreeRow {
            source: TreeRowSource::Notice {
                directory: directory.map(u32::try_from).transpose()?,
            },
            parent: Some(u32::try_from(parent)?),
            last: true,
            expanded: false,
        });
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn position(&self, id: &str) -> Option<usize> {
        if let Some(index) = self.last_position.get() {
            if self
                .rows
                .get(index)
                .is_some_and(|row| self.row_id_matches(row, id))
            {
                return Some(index);
            }
        }

        let position = self
            .rows
            .iter()
            .position(|row| self.row_id_matches(row, id));
        self.last_position.set(position);
        position
    }

    fn row_id_matches(&self, row: &TreeRow, id: &str) -> bool {
        match row.source {
            TreeRowSource::Root => id == self.root_id(),
            TreeRowSource::Notice { .. } => false,
            TreeRowSource::Entry { directory, entry } => {
                self.directories[directory as usize]
                    .entries
                    .entry(entry as usize)
                    .map(|(path, _)| path)
                    == Some(id)
            }
        }
    }

    pub fn row(&self, index: usize) -> Option<PanelRow> {
        let row = self.rows.get(index)?;
        match row.source {
            TreeRowSource::Root => Some(self.root_row(row.expanded)),
            TreeRowSource::Notice { directory } => {
                let parent = &self.rows[row.parent? as usize];
                let (parent_id, parent_path) = match parent.source {
                    TreeRowSource::Root => (
                        self.root_id(),
                        self.root
                            .as_ref()
                            .map_or(Some("."), |root| root.path.as_deref()),
                    ),
                    TreeRowSource::Entry { directory, entry } => {
                        let entries = &self.directories[directory as usize].entries;
                        let (id, _) = entries.entry(entry as usize)?;
                        let path = match entries {
                            TreeEntries::Filesystem(_) => Some(id),
                            TreeEntries::Rows(rows) => rows[entry as usize].path.as_deref(),
                        };
                        (id, path)
                    }
                    TreeRowSource::Notice { .. } => return None,
                };
                let text = directory.map_or("Loading…", |directory| {
                    self.directories[directory as usize].notice.as_str()
                });
                let mut segments = Vec::new();
                self.append_branch_segments(index, &mut segments);
                segments.push(segment(text, INDENT_STYLE, false));
                Some(PanelRow {
                    id: format!("notice:{parent_id}"),
                    path: parent_path.map(str::to_string),
                    kind: PanelRowKind::Directory,
                    expanded: Some(false),
                    segments,
                    right_segments: Vec::new(),
                })
            }
            TreeRowSource::Entry { directory, entry } => {
                match &self.directories.get(directory as usize)?.entries {
                    TreeEntries::Filesystem(entries) => {
                        self.entry_row(index, row, entries.get(entry as usize)?)
                    }
                    TreeEntries::Rows(rows) => {
                        let mut result = rows.get(entry as usize)?.clone();
                        let mut segments = Vec::new();
                        self.append_branch_segments(index, &mut segments);
                        segments.append(&mut result.segments);
                        result.segments = segments;
                        result.expanded = Some(row.expanded);
                        Some(result)
                    }
                }
            }
        }
    }

    fn root_row(&self, expanded: bool) -> PanelRow {
        if let Some(root) = &self.root {
            let mut root = root.clone();
            root.expanded = Some(expanded);
            return root;
        }
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
        append_name_segments(
            name,
            self.matches.get(path).map(Vec::as_slice),
            entry_name_style(status, directory),
            &mut segments,
        );
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

fn append_name_segments(
    name: &str,
    matches: Option<&[[usize; 2]]>,
    style: &[&str],
    segments: &mut Vec<PanelSegment>,
) {
    let Some(ranges) = matches.filter(|ranges| !ranges.is_empty()) else {
        segments.push(segment(name, style, false));
        return;
    };
    // Search ranges use Unicode scalar offsets, while Rust string slices use bytes.
    let offsets = name
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(name.len()))
        .collect::<Vec<_>>();
    let mut cursor = 0;
    for &[start, end] in ranges {
        let (Some(&start), Some(&end)) = (offsets.get(start), offsets.get(end)) else {
            continue;
        };
        if start < cursor || end <= start {
            continue;
        }
        if start > cursor {
            segments.push(segment(&name[cursor..start], style, false));
        }
        segments.push(segment(
            &name[start..end],
            &[
                "list.highlightForeground",
                "editor.findMatchHighlightForeground",
                "editor.foreground",
            ],
            true,
        ));
        cursor = end;
    }
    if cursor < name.len() {
        segments.push(segment(&name[cursor..], style, false));
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

    #[test]
    fn custom_trees_preserve_order_decoration_and_opaque_ids() {
        let spec: TreePanelSpec = serde_json::from_value(serde_json::json!({
            "root": { "id": "outline", "kind": "directory", "segments": [{"text":"Symbols"}] },
            "children": [
                { "parent":"outline", "rows":[
                    {"id":"type:Zoo", "kind":"directory", "segments":[{"text":"Zoo"}]},
                    {"id":"fn:alpha", "kind":"file", "segments":[{"text":"alpha"}]}
                ]},
                { "parent":"type:Zoo", "rows":[
                    {"id":"method:new", "kind":"file", "segments":[{"text":"new", "semantic":{"foreground":["scope:entity.name.function"]}}], "right_segments":[{"text":"public"}]}
                ]}
            ],
            "expanded":["outline", "type:Zoo"]
        })).unwrap();
        let tree = TreePanelModel::new(spec.clone()).unwrap();
        assert_eq!(
            (0..tree.len())
                .map(|index| tree.row(index).unwrap().id)
                .collect::<Vec<_>>(),
            ["outline", "type:Zoo", "method:new", "fn:alpha"]
        );
        let method = tree.row(2).unwrap();
        assert_eq!(method.path, None);
        assert_eq!(method.segments[1].text, "└ ");
        assert_eq!(
            method.segments[2].semantic.as_ref().unwrap().foreground,
            ["scope:entity.name.function"]
        );
        assert_eq!(method.right_segments[0].text, "public");
        let mut collapsed = spec.clone();
        collapsed.expanded.pop();
        assert_eq!(TreePanelModel::new(collapsed).unwrap().len(), 3);

        let mut duplicate = spec.clone();
        duplicate.children[0].rows[0].id = "outline".into();
        assert!(TreePanelModel::new(duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
        let mut reserved = spec.clone();
        reserved.children[0].rows[1].id = "notice:type:Zoo".into();
        assert!(TreePanelModel::new(reserved).is_err());
        let mut invalid_parent = spec;
        invalid_parent.children[0].parent = "fn:alpha".into();
        assert!(TreePanelModel::new(invalid_parent).is_err());
    }

    #[test]
    fn search_highlights_preserve_unicode_and_filename_styles() {
        let mut segments = Vec::new();
        append_name_segments(
            "é界picker.rs",
            Some(&[[99, 100], [4, 3], [2, 6], [3, 4]]),
            FILE_NAME_STYLE,
            &mut segments,
        );
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            ["é界", "pick", "er.rs"]
        );
        assert_eq!(
            segments[0].semantic.as_ref().unwrap().foreground,
            FILE_NAME_STYLE
        );
        assert_eq!(
            segments[1].semantic.as_ref().unwrap().foreground[0],
            "list.highlightForeground"
        );
        assert_eq!(segments[1].semantic.as_ref().unwrap().bold, Some(true));
        assert_eq!(
            segments[2].semantic.as_ref().unwrap().foreground,
            FILE_NAME_STYLE
        );
    }

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
    fn expanded_directories_show_loading_and_read_errors_without_hiding_cached_entries() {
        let loading = model(
            serde_json::json!([{
                "path":".", "entries":[{"path":"./src","name":"src","kind":"directory"}]
            }]),
            serde_json::json!([".", "./src"]),
        );
        assert_eq!(
            loading.row(2).unwrap().segments.last().unwrap().text,
            "Loading…"
        );
        let failed = model(
            serde_json::json!([{
                "path":".", "notice":"Cannot read · R retry",
                "entries":[{"path":"./cached.rs","name":"cached.rs","kind":"file"}]
            }]),
            serde_json::json!(["."]),
        );
        assert_eq!(failed.row(1).unwrap().id, "./cached.rs");
        assert_eq!(
            failed.row(2).unwrap().segments.last().unwrap().text,
            "Cannot read · R retry"
        );
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

    #[test]
    fn repeated_positions_reuse_only_the_matching_tree_row() {
        let model = model(
            serde_json::json!([{
                "path": ".",
                "entries": [
                    { "name": "first.rs", "path": "./first.rs", "kind": "file" },
                    { "name": "second.rs", "path": "./second.rs", "kind": "file" }
                ]
            }]),
            serde_json::json!(["."]),
        );

        assert_eq!(model.last_position.get(), None);
        assert_eq!(model.position("./second.rs"), Some(2));
        assert_eq!(model.last_position.get(), Some(2));
        assert_eq!(model.position("./second.rs"), Some(2));
        assert_eq!(model.position("./first.rs"), Some(1));
        assert_eq!(model.last_position.get(), Some(1));
        assert_eq!(model.position("./missing.rs"), None);
        assert_eq!(model.last_position.get(), None);
        assert_eq!(model.position("."), Some(0));
    }
}
