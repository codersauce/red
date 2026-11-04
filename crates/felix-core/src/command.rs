//! Document commands produce edits in original document coordinates. The host
//! commits them in reverse order, then resolves the new selection against its Rope.
mod insert;
mod lines;
use crate::search::{Prompt, Query};
use crate::{Range, Selections, boundary};
pub use lines::open_line_site;
use ropey::RopeSlice;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Select,
    Insert,
}

#[derive(Debug, Clone, Default)]
enum Pending {
    #[default]
    None,
    Goto,
    Replace,
    Match,
    Object {
        around: bool,
        count: usize,
    },
    Find {
        command: char,
        count: usize,
    },
    Regex {
        kind: Query,
        input: Prompt,
        original: Selections,
        viewport_start: usize,
    },
}

// Helix character callbacks receive the character even when Ctrl or Alt is held.
fn argument_character(key: &str) -> Option<char> {
    let value = key
        .strip_prefix("<c-")
        .or_else(|| key.strip_prefix("<a-"))
        .and_then(|modified| modified.strip_suffix('>'))
        .unwrap_or(key);
    let mut chars = value.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

#[derive(Debug, Clone, Copy)]
enum ArgumentMotion {
    Find {
        command: char,
        ch: char,
        count: usize,
        extend: bool,
    },
    Object {
        ch: char,
        count: usize,
        around: bool,
    },
}

impl ArgumentMotion {
    fn apply(self, text: RopeSlice<'_>, selections: &Selections) -> Selections {
        selections.transform(text, |range| match self {
            Self::Find {
                command,
                ch,
                count,
                extend,
            } => crate::motion::find(text, range, command, ch, count, extend),
            Self::Object { ch, count, around } => match ch {
                'p' => crate::motion::paragraph(text, range, count, around),
                'w' | 'W' => crate::motion::word_object(text, range, ch == 'W', around),
                _ => crate::motion::pair_object(text, range, ch, around, count),
            },
        })
    }
}

/// Map a character offset to a destination on another display row, retaining
/// the preferred screen column across consecutive vertical movements.
pub type VisualMotion<'a> = dyn Fn(usize, isize, Option<usize>) -> (usize, usize) + 'a;

/// Document settings and register contents supplied by the current host.
pub struct Context<'a> {
    /// Search register entries, oldest first.
    pub search_history: &'a [String],
    pub register: &'a [String],
    pub tab_width: usize,
    pub indent: &'a str,
    /// Language indentation in display columns, sorted by original break position.
    pub newline_indents: &'a [(usize, usize)],
    pub comment_tokens: &'a [&'a str],
    pub page_height: usize,
    pub viewport_start: usize,
    pub scrolloff: usize,
    pub visual_motion: Option<&'a VisualMotion<'a>>,
}

impl Default for Context<'_> {
    fn default() -> Self {
        Self {
            search_history: &[],
            register: &[],
            tab_width: 4,
            indent: "    ",
            newline_indents: &[],
            comment_tokens: &[],
            page_height: 22,
            viewport_start: 0,
            scrolloff: 5,
            visual_motion: None,
        }
    }
}

impl Context<'_> {
    fn indentation_at(&self, pos: usize, fallback_columns: usize) -> String {
        let columns = self
            .newline_indents
            .binary_search_by_key(&pos, |&(pos, _)| pos)
            .map_or(fallback_columns, |i| self.newline_indents[i].1);
        let width = if self.indent == "\t" {
            self.tab_width
        } else {
            self.indent.len()
        };
        self.indent.repeat(columns / width.max(1))
    }

    fn comment_at(&self, text: RopeSlice<'_>, pos: usize) -> Option<&str> {
        self.comment_tokens
            .iter()
            .copied()
            .filter(|token| {
                !token.is_empty()
                    && text
                        .chars_at(pos)
                        .take(token.chars().count())
                        .eq(token.chars())
            })
            .max_by_key(|token| token.len())
    }

    fn row_start(&self, text: RopeSlice<'_>, pos: usize, delta: isize) -> usize {
        if let Some(resolve) = self.visual_motion {
            return resolve(pos, delta, Some(0)).0;
        }
        let start = crate::motion::logical_start(text, pos);
        let (target, crossed) =
            crate::motion::shift_lines(text, start, delta > 0, delta.unsigned_abs());
        if delta > 0 && crossed < delta.unsigned_abs() {
            text.len_chars()
        } else {
            target
        }
    }

    fn search_viewport(&self, text: RopeSlice<'_>, cursor: usize) -> usize {
        let last_row = self.page_height.max(1) - 1;
        let target = self.row_start(text, cursor, 0);
        let bottom = self.row_start(text, self.viewport_start, last_row as isize);
        let top = if (self.viewport_start..=bottom).contains(&target) {
            self.viewport_start
        } else {
            self.row_start(text, cursor, -((last_row / 2) as isize))
        };
        self.reveal(text, top, cursor)
    }

    fn reveal(&self, text: RopeSlice<'_>, top: usize, cursor: usize) -> usize {
        let height = self.page_height.max(1);
        let upper = self.scrolloff.min((height - 1) / 2) as isize;
        let lower = (height - 1 - self.scrolloff.min(height / 2)) as isize;
        top.clamp(
            self.row_start(text, cursor, -lower),
            self.row_start(text, cursor, -upper),
        )
    }
}

/// A session is advanced by the host only after accepting its plan.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub mode: Mode,
    pending: Pending,
    count: Option<usize>,
    // A logical source column and a screen column differ on a wrapped line.
    goals: Vec<usize>,
    visual_goals: Vec<usize>,
    restore_append: bool,
    last_motion: Option<ArgumentMotion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAction {
    Undo(usize),
    Redo(usize),
    SwitchBuffer { forward: bool, count: usize },
    CommandLine,
    References,
    RepeatInsert(usize),
    SyntaxParentEnd,
    Jump { forward: bool, count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum History {
    #[default]
    None,
    Begin,
    Commit,
    Checkpoint,
    /// Commit surrounding typing separately on both sides of a paste.
    InsertPaste,
    Atomic,
}

#[derive(Debug)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub insert: String,
}

#[derive(Debug)]
pub struct Plan {
    pub next: Session,
    pub edits: Vec<Edit>,
    pub history: History,
    pub register: Option<Vec<String>>,
    pub search: Option<String>,
    pub host: Option<HostAction>,
    pub save_jump: Option<Selections>,
    /// New top display row, as a document character offset.
    pub viewport_start: Option<usize>,
    pub message: Option<String>,
    ranges: Vec<Range>,
    primary: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Paste {
    Before,
    After,
    Cursor,
}

/// Pasting and replacing use the destination document's line endings.
fn register_text(value: &str, newline: &str, count: usize) -> Result<String, &'static str> {
    static ENDINGS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\r\n|\r|\n").unwrap());
    let normalized = ENDINGS.replace_all(value, newline);
    if count == 1 || normalized.is_empty() {
        return Ok(normalized.into_owned());
    }
    let bytes = normalized
        .len()
        .checked_mul(count)
        .ok_or("Register text is too large")?;
    let mut result = String::new();
    result
        .try_reserve_exact(bytes)
        .map_err(|_| "Insufficient memory for register text")?;
    for _ in 0..count {
        result.push_str(&normalized);
    }
    Ok(result)
}

impl Plan {
    fn paste(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        values: &[String],
        placement: Paste,
        count: usize,
    ) {
        if values.is_empty() {
            if self.next.mode == Mode::Select {
                self.next.mode = Mode::Normal;
            }
            return;
        }
        let linewise = values.iter().any(|v| v.ends_with('\n'));
        let newline = document_newline(text);
        let mut added = 0;
        let mut ranges = Vec::with_capacity(selections.ranges().len());
        for (i, r) in selections.ranges().iter().enumerate() {
            let value = &values[i.min(values.len() - 1)];
            let pos = match (placement, linewise) {
                (Paste::Before, true) => text.line_to_char(text.char_to_line(r.start())),
                (Paste::After, true) => text.line_to_char(
                    (text.char_to_line(if r.start() < r.end() {
                        r.end() - 1
                    } else {
                        r.end()
                    }) + 1)
                        .min(text.len_lines()),
                ),
                (Paste::Before, false) => r.start(),
                (Paste::After, false) => r.end(),
                (Paste::Cursor, _) => r.cursor(text),
            };
            let insert = match register_text(value, newline, count) {
                Ok(insert) => insert,
                Err(reason) => {
                    self.edits.clear();
                    self.message = Some(reason.into());
                    return;
                }
            };
            let width = insert.chars().count();
            let from = pos + added;
            let mapped = if self.next.mode == Mode::Normal {
                r.with_bounds(from, from + width)
            } else {
                map_insertion(*r, pos, width, added)
            };
            ranges.push(mapped);
            self.edits.push(Edit {
                start: pos,
                end: pos,
                insert,
            });
            added += width;
        }
        self.ranges = ranges;
        if self.next.mode == Mode::Insert {
            self.history = History::InsertPaste;
        } else {
            self.history = History::Atomic;
            self.next.mode = Mode::Normal;
        }
    }

    fn replace_from_register(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        values: &[String],
        count: usize,
    ) {
        self.next.mode = Mode::Normal;
        if values.is_empty() {
            return;
        }
        let newline = document_newline(text);
        let mut index = 0;
        let replacements: Result<Vec<_>, _> = selections
            .ranges()
            .iter()
            .map(|range| {
                if range.start() == range.end() {
                    return Ok(String::new());
                }
                let fragment = &values[index];
                index = (index + 1).min(values.len() - 1);
                register_text(fragment, newline, count)
            })
            .collect();
        match replacements {
            Ok(values) => self.replace_selections(selections, values),
            Err(reason) => self.message = Some(reason.into()),
        }
    }

    pub fn selections(&self, committed_text: RopeSlice<'_>) -> Selections {
        Selections::new(committed_text, self.ranges.clone(), self.primary)
            .expect("command preserves a selection")
    }

    fn replace_fragments(
        &mut self,
        text: RopeSlice<'_>,
        selection: &Selections,
        mut transform: impl FnMut(usize, RopeSlice<'_>) -> String,
    ) {
        let replacements = selection
            .ranges()
            .iter()
            .enumerate()
            .map(|(i, r)| transform(i, text.slice(r.start()..r.end())));
        self.replace_selections(selection, replacements);
    }

    fn replace_selections(
        &mut self,
        selection: &Selections,
        replacements: impl IntoIterator<Item = String>,
    ) {
        let mut removed = 0;
        let mut added = 0;
        for (i, (r, insert)) in selection.ranges().iter().zip(replacements).enumerate() {
            let start = r.start() - removed + added;
            let width = insert.chars().count();
            self.ranges[i] = r.with_bounds(start, start + width);
            self.edits.push(Edit {
                start: r.start(),
                end: r.end(),
                insert,
            });
            removed += r.end() - r.start();
            added += width;
        }
        self.history = History::Atomic;
        self.next.mode = Mode::Normal;
    }

    fn vertical(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        down: bool,
        count: usize,
        tab_width: usize,
        extend: bool,
    ) {
        let mut goals = Vec::new();
        for (i, &r) in selections.ranges().iter().enumerate() {
            let (range, goal) = crate::motion::vertical(
                text,
                r,
                down,
                count,
                tab_width,
                self.next.goals.get(i).copied(),
                extend,
            );
            self.ranges[i] = range;
            goals.push(goal);
        }
        self.next.goals = goals;
    }

    fn visual_vertical(
        &mut self,
        text: RopeSlice<'_>,
        selections: &Selections,
        delta: isize,
        ctx: &Context<'_>,
    ) {
        let extend = self.next.mode == Mode::Select;
        let Some(resolve) = ctx.visual_motion else {
            self.vertical(
                text,
                selections,
                delta >= 0,
                delta.unsigned_abs(),
                ctx.tab_width,
                extend,
            );
            return;
        };
        let mut goals = Vec::with_capacity(self.ranges.len());
        for (index, &range) in selections.ranges().iter().enumerate() {
            let (pos, goal) = resolve(
                range.cursor(text),
                delta,
                self.next.visual_goals.get(index).copied(),
            );
            if !extend || text.line(text.char_to_line(pos)).len_chars() != 0 {
                self.ranges[index] = range.move_to(text, pos, extend);
            }
            goals.push(goal);
        }
        self.next.visual_goals = goals;
        self.next.goals.clear();
    }

    fn page(&mut self, text: RopeSlice<'_>, selections: &Selections, key: &str, ctx: &Context<'_>) {
        let height = ctx.page_height.max(1);
        let half = matches!(key, "<c-d>" | "<c-u>");
        let down = matches!(key, "<c-f>" | "<pagedown>" | "<c-d>");
        let distance = if half { height / 2 } else { height } as isize;
        let delta = if down { distance } else { -distance };
        let top = ctx.row_start(text, ctx.viewport_start, delta);
        if half {
            self.visual_vertical(text, selections, delta, ctx);
        } else {
            let margin = ctx.scrolloff.min((height - 1) / 2);
            let edge = if down { margin } else { height - 1 - margin };
            let head = ctx.row_start(text, top, edge as isize);
            let primary = selections.primary();
            let cursor = primary.cursor(text);
            if (down && head > cursor) || (!down && head < cursor) {
                self.ranges[self.primary] = Range {
                    anchor: if self.next.mode == Mode::Select {
                        primary.anchor
                    } else {
                        head
                    },
                    head,
                };
            }
        }
        let cursor = self.selections(text).primary().cursor(text);
        self.viewport_start = Some(ctx.reveal(text, top, cursor));
    }

    fn set(&mut self, selections: Selections) {
        self.ranges = selections.ranges;
        self.primary = selections.primary;
    }

    fn replace(&mut self, ranges: &[Range], insert: &str) {
        let mut removed = 0;
        let mut added = 0;
        let width = insert.chars().count();
        self.ranges.clear();
        for range in ranges {
            let start = range.start();
            let end = range.end();
            self.edits.push(Edit {
                start,
                end,
                insert: insert.into(),
            });
            let position = start - removed + added + width;
            self.ranges.push(Range {
                anchor: position,
                head: position,
            });
            removed += end - start;
            added += width;
        }
    }
}

fn document_newline(text: RopeSlice<'_>) -> &'static str {
    let line = text.line(0);
    if line.len_chars() >= 2
        && line.char(line.len_chars() - 2) == '\r'
        && line.char(line.len_chars() - 1) == '\n'
    {
        "\r\n"
    } else {
        "\n"
    }
}

fn map_insertion(range: Range, pos: usize, width: usize, prior: usize) -> Range {
    let map = |offset, after| {
        offset
            + prior
            + if offset > pos || offset == pos && after {
                width
            } else {
                0
            }
    };
    Range {
        anchor: map(range.anchor, range.anchor <= range.head),
        head: map(range.head, range.head <= range.anchor),
    }
}

impl Session {
    fn unchanged(&self, selections: &Selections) -> Plan {
        Plan {
            next: self.clone(),
            edits: Vec::new(),
            history: History::None,
            register: None,
            search: None,
            host: None,
            save_jump: None,
            viewport_start: None,
            message: None,
            ranges: selections.ranges.clone(),
            primary: selections.primary,
        }
    }

    pub fn replay_completion(
        &self,
        text: RopeSlice<'_>,
        selections: &Selections,
        change: &crate::CompletionChange,
    ) -> Plan {
        let mut plan = self.unchanged(selections);
        let cursor = selections.primary().cursor(text);
        if let Some(range) = change.destination(cursor, text.len_chars()) {
            let size = change.text.chars().count();
            for selection in &mut plan.ranges {
                *selection = selection.map_edit(range.start, range.end, size);
            }
            plan.edits.push(Edit {
                start: range.start,
                end: range.end,
                insert: change.text.clone(),
            });
        }
        plan
    }

    pub fn bracketed_paste(
        &self,
        text: RopeSlice<'_>,
        selections: &Selections,
        value: &str,
    ) -> Plan {
        if matches!(self.pending, Pending::Regex { .. }) {
            let mut plan = self.unchanged(selections);
            if let Pending::Regex { input, .. } = &mut plan.next.pending {
                input.paste(value);
            }
            return plan;
        }
        let placement = if self.mode == Mode::Normal {
            Paste::Before
        } else {
            Paste::Cursor
        };
        self.paste(text, selections, &[value.to_owned()], placement)
    }

    pub fn paste(
        &self,
        text: RopeSlice<'_>,
        selections: &Selections,
        values: &[String],
        placement: Paste,
    ) -> Plan {
        let mut plan = self.unchanged(selections);
        plan.paste(text, selections, values, placement, self.count.unwrap_or(1));
        plan.next.cancel_pending();
        plan
    }
    /// Consume a decimal count digit while a document or host keymap awaits a command.
    /// A leading zero is available as a binding. Excess digits leave the count intact.
    pub fn push_count_digit(&mut self, ch: char) -> bool {
        if !ch.is_ascii_digit() || (ch == '0' && self.count.is_none()) {
            return false;
        }
        let next = self.count.unwrap_or(0) * 10 + usize::from(ch as u8 - b'0');
        if next <= 100_000_000 {
            self.count = Some(next);
        }
        true
    }

    pub fn count(&self) -> Option<usize> {
        self.count
    }

    /// The active prompt label, text and UTF-8 byte offset of its caret.
    pub fn prompt(&self) -> Option<(&'static str, &str, usize)> {
        match &self.pending {
            Pending::Regex { kind, input, .. } => Some((kind.label(), &input.line, input.cursor)),
            _ => None,
        }
    }

    /// Visible history completions and the highlighted index in the active prompt.
    pub fn prompt_completions(&self) -> Option<(&[String], Option<usize>)> {
        match &self.pending {
            Pending::Regex { input, .. } => Some((input.candidates(), input.selected)),
            _ => None,
        }
    }

    pub fn awaiting_input(&self) -> bool {
        !matches!(self.pending, Pending::None)
    }

    /// Give the document's input back to the host. A live search preview restores
    /// its pre-prompt selection and viewport just as Escape would.
    pub fn leave_document_input(&mut self) -> Option<(Selections, usize)> {
        self.count = None;
        match std::mem::take(&mut self.pending) {
            Pending::Regex {
                original,
                viewport_start,
                ..
            } => Some((original, viewport_start)),
            _ => None,
        }
    }

    pub fn cancel_pending(&mut self) {
        self.pending = Pending::None;
        self.count = None;
    }

    /// Text keys are literal characters. Special keys use names such as
    /// `<esc>`, `<enter>`, and `<a-;>` to avoid collisions with typed text.
    pub fn plan(
        &self,
        text: RopeSlice<'_>,
        selections: &Selections,
        key: &str,
        context: &Context<'_>,
    ) -> Plan {
        let mut p = self.unchanged(selections);
        let search = context.search_history.last().map(String::as_str);
        let register = context.register;
        let count = self.count.unwrap_or(1);
        let extend = self.mode == Mode::Select;
        if !matches!(key, "j" | "k" | "<down>" | "<up>" | "<c-d>" | "<c-u>" | "g")
            && !key.bytes().all(|ch| ch.is_ascii_digit())
        {
            p.next.goals.clear();
            p.next.visual_goals.clear();
        }
        // Prefix maps still accept counts. Prompts and commands waiting for
        // a character (such as f and r) consume a digit as their argument.
        if self.mode != Mode::Insert
            && matches!(self.pending, Pending::None | Pending::Goto | Pending::Match)
            && let [digit] = key.as_bytes()
            && p.next.push_count_digit(char::from(*digit))
        {
            return p;
        }
        match &self.pending {
            Pending::Regex {
                original,
                input,
                kind,
                viewport_start,
            } => {
                let mut prompt = input.clone();
                let submit = key == "<enter>";
                match key {
                    "<esc>" | "<c-c>" => {
                        p.set(original.clone());
                        p.viewport_start = Some(*viewport_start);
                        p.next.pending = Pending::None;
                        return p;
                    }
                    "<tab>" | "<backtab>" => prompt.cycle(key == "<backtab>"),
                    "<enter>" if prompt.keep_directory_completion() => {
                        if let Pending::Regex { input, .. } = &mut p.next.pending {
                            *input = prompt;
                        }
                        return p;
                    }
                    "<enter>" => {
                        p.next.pending = Pending::None;
                        if prompt.line.is_empty() {
                            prompt.line = search.unwrap_or_default().to_owned();
                        } else if search != Some(prompt.line.as_str()) {
                            p.search = Some(prompt.line.clone());
                        }
                    }
                    "<up>" | "<c-p>" | "<down>" | "<c-n>" => {
                        p.set(original.clone());
                        p.viewport_start = Some(*viewport_start);
                        prompt.recall(context.search_history, matches!(key, "<up>" | "<c-p>"));
                        if context.search_history.is_empty() {
                            return p;
                        }
                    }
                    "<c-s>" => {
                        let word =
                            crate::motion::word_object(text, selections.primary(), false, false);
                        if word.start() == word.end() {
                            return p;
                        }
                        prompt.paste(&text.slice(word.start()..word.end()).to_string());
                    }
                    _ => {
                        if !prompt.edit(key) {
                            if let Pending::Regex { input, .. } = &mut p.next.pending {
                                *input = prompt;
                            }
                            return p;
                        }
                    }
                }
                let query = &prompt.line;
                if !query.is_empty() {
                    if let Some(regex) = Query::compile(query) {
                        let preview = kind.apply(text, original, &regex, extend);
                        if let Some(ref found) = preview
                            && matches!(kind, Query::Forward | Query::Backward)
                        {
                            p.viewport_start =
                                Some(context.search_viewport(text, found.primary().cursor(text)));
                        }
                        if preview.is_none() {
                            p.message = match kind {
                                Query::Select => Some("nothing selected".into()),
                                Query::Keep | Query::Remove => {
                                    Some("no selections remaining".into())
                                }
                                _ => None,
                            };
                        }
                        p.set(preview.unwrap_or_else(|| original.clone()));
                        if submit {
                            p.save_jump = Some(original.clone());
                        }
                    } else {
                        p.set(original.clone());
                        p.viewport_start = Some(*viewport_start);
                        if submit {
                            p.message = Some(format!("Invalid regex: {query}"));
                        }
                    }
                }
                if !submit {
                    p.next.pending = Pending::Regex {
                        kind: *kind,
                        original: original.clone(),
                        viewport_start: *viewport_start,
                        input: prompt,
                    };
                }
                return p;
            }
            Pending::Match => {
                p.next.pending = Pending::None;
                p.next.count = None;
                if key == "i" || key == "a" {
                    p.next.pending = Pending::Object {
                        around: key == "a",
                        count,
                    };
                }
                if key == "m" {
                    p.set(selections.transform(text, |r| {
                        let pos = r.cursor(text);
                        crate::motion::match_bracket(text, pos)
                            .map_or(r, |target| r.move_to(text, target, extend))
                    }));
                }
                return p;
            }
            Pending::Object { around, count } => {
                p.next.pending = Pending::None;
                if let Some(ch) = argument_character(key) {
                    let motion = ArgumentMotion::Object {
                        ch,
                        count: *count,
                        around: *around,
                    };
                    p.set(motion.apply(text, selections));
                    p.next.last_motion = Some(motion);
                }
                return p;
            }
            Pending::Replace => {
                p.next.pending = Pending::None;
                let mut encoded = [0; 4];
                let replacement = match key {
                    "<enter>" => document_newline(text),
                    "<tab>" => "\t",
                    _ => match argument_character(key) {
                        Some(ch) => ch.encode_utf8(&mut encoded),
                        None => return p,
                    },
                };
                p.replace_fragments(text, selections, |_, slice| {
                    use unicode_segmentation::UnicodeSegmentation;
                    replacement.repeat(std::borrow::Cow::<str>::from(slice).graphemes(true).count())
                });
                return p;
            }
            Pending::Find { command, count } => {
                p.next.pending = Pending::None;
                if key == "<enter>" {
                    p.set(selections.transform(text, |range| {
                        crate::motion::find_line_ending(text, range, *command, *count, extend)
                    }));
                    return p;
                }
                let ch = match key {
                    "<tab>" => '\t',
                    _ => match argument_character(key) {
                        Some(ch) => ch,
                        None => return p,
                    },
                };
                let motion = ArgumentMotion::Find {
                    command: *command,
                    ch,
                    count: *count,
                    extend,
                };
                p.set(motion.apply(text, selections));
                p.next.last_motion = Some(motion);
                return p;
            }
            Pending::Goto => {
                p.next.count = None;
                p.next.pending = Pending::None;
                p.save_jump = matches!(key, "g" | "e").then(|| selections.clone());
                match key {
                    "n" | "p" => {
                        p.host = Some(HostAction::SwitchBuffer {
                            forward: key == "n",
                            count,
                        })
                    }
                    "g" | "e" | "h" | "l" | "s" | "|" => p.set(selections.transform(text, |r| {
                        crate::motion::goto(text, r, key, self.count, extend)
                    })),
                    "j" | "k" => p.vertical(
                        text,
                        selections,
                        key == "j",
                        count,
                        context.tab_width,
                        extend,
                    ),
                    "r" => p.host = Some(HostAction::References),
                    _ => {}
                }
                return p;
            }
            Pending::None => {}
        }
        if self.mode == Mode::Insert {
            p.insert_key(text, selections, key, context);
            return p;
        }
        if key != "g" && key != "m" {
            p.next.count = None;
        }
        p.save_jump = (key == "G" && self.count.is_some()).then(|| selections.clone());
        match key {
            "<c-s>" => p.save_jump = Some(selections.clone()),
            "<c-o>" | "<c-i>" | "<tab>" => {
                p.host = Some(HostAction::Jump {
                    forward: key != "<c-o>",
                    count,
                })
            }
            "." => p.host = Some(HostAction::RepeatInsert(count)),
            "g" => p.next.pending = Pending::Goto,
            "m" => p.next.pending = Pending::Match,
            "/" | "?" | "s" | "S" | "K" | "<a-K>" => {
                let kind = match key {
                    "/" => Query::Forward,
                    "?" => Query::Backward,
                    "S" => Query::Split,
                    "K" => Query::Keep,
                    "<a-K>" => Query::Remove,
                    _ => Query::Select,
                };
                p.next.pending = Pending::Regex {
                    kind,
                    original: selections.clone(),
                    viewport_start: context.viewport_start,
                    input: Prompt::new(kind, context.search_history),
                };
            }
            "r" => p.next.pending = Pending::Replace,
            "f" | "F" | "t" | "T" => {
                p.next.pending = Pending::Find {
                    command: key.chars().next().unwrap(),
                    count,
                };
            }
            "<a-.>" => {
                if let Some(motion) = self.last_motion {
                    let mut current = selections.clone();
                    for _ in 0..count {
                        let next = motion.apply(text, &current);
                        if next == current {
                            break;
                        }
                        current = next;
                    }
                    p.set(current);
                }
            }
            "R" => p.replace_from_register(text, selections, register, count),
            "~" | "`" | "<a-`>" => p.replace_fragments(text, selections, |_, slice| {
                let mut out = String::new();
                for c in slice.chars() {
                    if key == "<a-`>" || key == "~" && c.is_lowercase() {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                }
                out
            }),
            "x" | "X" | "<a-x>" => p.set(selections.transform(text, |range| {
                crate::motion::select_lines(text, range, key, count)
            })),
            "C" | "<a-C>" => p.set(crate::motion::copies(
                text,
                selections,
                key == "C",
                count,
                context.tab_width,
            )),
            "<a-_>" => {
                let mut merged: Vec<Range> = Vec::new();
                let mut primary = 0;
                for (i, &r) in selections.ranges().iter().enumerate() {
                    if let Some(prev) = merged.last_mut().filter(|prev| prev.end() == r.start()) {
                        let lo = prev.start();
                        let hi = r.end();
                        *prev = if prev.head < prev.anchor && r.head < r.anchor {
                            Range {
                                anchor: hi,
                                head: lo,
                            }
                        } else {
                            Range {
                                anchor: lo,
                                head: hi,
                            }
                        };
                    } else {
                        merged.push(r);
                    }
                    if i == selections.primary_index() {
                        primary = merged.len() - 1;
                    }
                }
                p.set(Selections::new(text, merged, primary).unwrap());
            }
            "<a-s>" => {
                let regex = Query::compile(r"\r?\n").unwrap();
                if let Some(split) = Query::Split.apply(text, selections, &regex, false) {
                    p.set(split);
                }
            }
            "<a-->" => {
                let first = selections.ranges()[0];
                let last = *selections.ranges().last().unwrap();
                let bounds = Range {
                    anchor: first.start(),
                    head: last.end(),
                };
                let range = if first.head < first.anchor && last.head < last.anchor {
                    Range {
                        anchor: bounds.head,
                        head: bounds.anchor,
                    }
                } else {
                    bounds
                };
                p.set(Selections::single(text, range));
            }
            "n" | "N" => {
                let kind = if key == "n" {
                    Query::Forward
                } else {
                    Query::Backward
                };
                if let Some(regex) = search.and_then(Query::compile) {
                    let mut state = selections.clone();
                    for _ in 0..count {
                        let Some(next) = kind.apply(text, &state, &regex, extend) else {
                            break;
                        };
                        if next == state {
                            break;
                        }
                        state = next;
                    }
                    p.viewport_start =
                        Some(context.search_viewport(text, state.primary().cursor(text)));
                    p.set(state);
                }
            }
            "p" | "P" => p.paste(
                text,
                selections,
                register,
                if key == "p" {
                    Paste::After
                } else {
                    Paste::Before
                },
                count,
            ),
            "*" | "<a-*>" => {
                let word = |i| {
                    text.get_char(i)
                        .is_some_and(|c| c == '_' || c.is_alphanumeric())
                };
                let mut alternatives = Vec::new();
                for r in selections.ranges() {
                    let mut pattern = regex::escape(&text.slice(r.start()..r.end()).to_string());
                    if key == "*" {
                        if word(r.start()) && (r.start() == 0 || !word(r.start() - 1)) {
                            pattern.insert_str(0, r"\b");
                        }
                        if r.end() > 0
                            && r.end() < text.len_chars()
                            && word(r.end() - 1)
                            && !word(r.end())
                        {
                            pattern.push_str(r"\b");
                        }
                    }
                    if !alternatives.contains(&pattern) {
                        alternatives.push(pattern);
                    }
                }
                p.search = Some(alternatives.join("|"));
            }
            "_" => {
                let mut trimmed = Vec::new();
                let mut primary = None;
                for (i, &r) in selections.ranges().iter().enumerate() {
                    let slice = text.slice(r.start()..r.end());
                    if let Some(first) = slice.chars().position(|c| !c.is_whitespace()) {
                        let mut rev = slice.chars_at(slice.len_chars());
                        rev.reverse();
                        let trailing = rev.take_while(|c| c.is_whitespace()).count();
                        if i == selections.primary_index() {
                            primary = Some(trimmed.len());
                        }
                        trimmed.push(r.with_bounds(r.start() + first, r.end() - trailing));
                    }
                }
                p.set(if trimmed.is_empty() {
                    Selections::single(text, Range::at(text, selections.primary().cursor(text)))
                } else {
                    let i = primary.unwrap_or(trimmed.len() - 1);
                    Selections::new(text, trimmed, i).unwrap()
                });
            }
            "%" => p.set(Selections::single(
                text,
                Range {
                    anchor: 0,
                    head: text.len_chars(),
                },
            )),
            "<a-;>" => p.set(selections.transform(text, |r| Range {
                anchor: r.head,
                head: r.anchor,
            })),
            ";" => p.set(selections.transform(text, |r| Range::at(text, r.cursor(text)))),
            "," => p.set(Selections::single(text, selections.primary())),
            "<a-:>" => p.set(selections.transform(text, |r| Range {
                anchor: r.start(),
                head: r.end(),
            })),
            "v" => {
                p.next.mode = if self.mode == Mode::Select {
                    Mode::Normal
                } else {
                    p.set(selections.transform(text, |r| {
                        if r.anchor == r.head && r.head == text.len_chars() {
                            Range {
                                anchor: boundary(text, r.head, false),
                                head: r.head,
                            }
                        } else {
                            r
                        }
                    }));
                    Mode::Select
                }
            }
            "<esc>" => p.next.mode = Mode::Normal,
            "u" => p.host = Some(HostAction::Undo(count)),
            "U" => p.host = Some(HostAction::Redo(count)),
            ":" => p.host = Some(HostAction::CommandLine),
            ">" | "<" | "J" | "<a-J>" => p.edit_lines(text, selections, key, count, context),
            "o" | "O" => p.open_lines(text, selections, key == "o", count, context),
            "i" | "a" | "I" | "A" => p.enter_insert(text, selections, key),
            "h" | "l" | "<left>" | "<right>" => {
                let forward = key == "l" || key == "<right>";
                p.set(selections.transform(text, |r| {
                    let mut target = r.cursor(text);
                    for _ in 0..count {
                        let moved = boundary(text, target, forward);
                        if moved == target {
                            break;
                        }
                        target = moved;
                    }
                    r.move_to(text, target, extend)
                }));
            }
            "j" | "k" | "<up>" | "<down>" => {
                let distance = count.min(isize::MAX as usize) as isize;
                let delta = if matches!(key, "j" | "<down>") {
                    distance
                } else {
                    -distance
                };
                p.visual_vertical(text, selections, delta, context);
            }
            "<pageup>" | "<pagedown>" | "<c-f>" | "<c-b>" | "<c-u>" | "<c-d>" => {
                p.page(text, selections, key, context);
            }
            "w" | "W" | "e" | "E" | "b" | "B" => p.set(
                selections.transform(text, |r| crate::motion::word(text, r, key, count, extend)),
            ),
            "<home>" | "<end>" => p.set(selections.transform(text, |r| {
                crate::motion::goto(
                    text,
                    r,
                    if key == "<home>" { "h" } else { "l" },
                    None,
                    extend,
                )
            })),
            "G" if self.count.is_some() => p.set(selections.transform(text, |r| {
                crate::motion::goto(text, r, "g", self.count, false)
            })),
            "(" | ")" => {
                let n = p.ranges.len();
                let shift = if key == ")" {
                    count % n
                } else {
                    n.saturating_sub(count) % n
                };
                p.primary = (p.primary + shift) % n;
            }
            "<a-,>" if p.ranges.len() > 1 => {
                p.ranges.remove(p.primary);
                p.primary = p.primary.min(p.ranges.len() - 1);
            }
            "<a-,>" => p.message = Some("no selections remaining".into()),
            "y" | "d" | "c" | "<a-c>" | "<a-d>" => {
                if !key.starts_with("<a-") {
                    p.register = Some(
                        selections
                            .ranges
                            .iter()
                            .map(|r| text.slice(r.start()..r.end()).to_string())
                            .collect(),
                    );
                }
                if key == "y" {
                    p.next.mode = Mode::Normal;
                }
                if key != "y" {
                    let change = key == "c" || key == "<a-c>";
                    let whole_lines = change
                        && selections.ranges().iter().all(|r| {
                            let (first, last) = r.lines(text);
                            text.slice(r.start()..r.end()).len_lines() > 1
                                && r.start() == text.line_to_char(first)
                                && r.end() == text.line_to_char(last + 1)
                        });
                    p.replace(
                        selections.ranges(),
                        if whole_lines {
                            document_newline(text)
                        } else {
                            ""
                        },
                    );
                    if whole_lines {
                        let width = document_newline(text).chars().count();
                        for r in &mut p.ranges {
                            r.anchor -= width;
                            r.head -= width;
                        }
                    }
                    p.history = if change {
                        History::Begin
                    } else {
                        History::Atomic
                    };
                    p.next.mode = if change { Mode::Insert } else { Mode::Normal };
                }
            }
            _ => p.message = Some(format!("Unimplemented Helix key: {key}")),
        }
        p
    }
}

impl Session {
    pub fn pending_keys(&self) -> &[&'static str] {
        match self.pending {
            Pending::Goto => &["g"],
            Pending::Match => &["m"],
            _ => &[],
        }
    }
}
