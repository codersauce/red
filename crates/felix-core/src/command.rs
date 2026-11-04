//! Document commands produce edits in original document coordinates. The host
//! commits them in reverse order, then resolves the new selection against its Rope.
mod insert;
mod lines;
use crate::search::Query;
use crate::{Range, Selections, boundary};
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
        input: String,
        original: Selections,
    },
}

/// Document settings and register contents supplied by the current host.
pub struct Context<'a> {
    pub search: Option<&'a str>,
    pub register: &'a [String],
    pub tab_width: usize,
    pub indent: &'a str,
    pub page_height: usize,
}

impl Default for Context<'_> {
    fn default() -> Self {
        Self {
            search: None,
            register: &[],
            tab_width: 4,
            indent: "    ",
            page_height: 24,
        }
    }
}

/// A session is advanced by the host only after accepting its plan.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub mode: Mode,
    pending: Pending,
    count: Option<usize>,
    goals: Vec<usize>,
    restore_append: bool,
    last_find: Option<(char, char, usize, bool)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostAction {
    Undo,
    Redo,
    NextBuffer,
    PreviousBuffer,
    CommandLine,
    References,
    RepeatInsert(usize),
    SyntaxParentEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum History {
    #[default]
    None,
    Begin,
    Commit,
    Checkpoint,
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
            return;
        }
        let linewise = values.iter().any(|v| v.ends_with('\n'));
        let newline = document_newline(text);
        let mut added = 0;
        let mut ranges = Vec::with_capacity(selections.ranges().len());
        for (i, r) in selections.ranges().iter().enumerate() {
            let value = values[i.min(values.len() - 1)]
                .replace("\r\n", "\n")
                .replace('\n', newline);
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
            let Some(bytes) = value
                .len()
                .checked_mul(count)
                .filter(|&n| n <= isize::MAX as usize)
            else {
                self.edits.clear();
                self.message = Some("Paste exceeds maximum document size".into());
                return;
            };
            let mut insert = String::new();
            if insert.try_reserve_exact(bytes).is_err() {
                self.edits.clear();
                self.message = Some("Insufficient memory for paste".into());
                return;
            }
            if !value.is_empty() {
                for _ in 0..count {
                    insert.push_str(&value);
                }
            }
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
        if self.next.mode != Mode::Insert {
            self.history = History::Atomic;
            self.next.mode = Mode::Normal;
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
        let mut removed = 0;
        let mut added = 0;
        for (i, r) in selection.ranges().iter().enumerate() {
            let insert = transform(i, text.slice(r.start()..r.end()));
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
            message: None,
            ranges: selections.ranges.clone(),
            primary: selections.primary,
        }
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
    pub fn count(&self) -> Option<usize> {
        self.count
    }

    pub fn awaiting_input(&self) -> bool {
        !matches!(self.pending, Pending::None)
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
        let search = context.search;
        let register = context.register;
        let count = self.count.unwrap_or(1);
        let extend = self.mode == Mode::Select;
        if !matches!(key, "j" | "k" | "<down>" | "<up>" | "g")
            && !key.bytes().all(|ch| ch.is_ascii_digit())
        {
            p.next.goals.clear();
        }
        match &self.pending {
            Pending::Regex {
                original,
                input,
                kind,
            } => {
                let mut query = input.clone();
                match key {
                    "<esc>" => {
                        p.set(original.clone());
                        p.next.pending = Pending::None;
                        return p;
                    }
                    "<enter>" => {
                        if Query::compile(input).is_some() {
                            p.search = Some(input.clone());
                        }
                        p.next.pending = Pending::None;
                        return p;
                    }
                    "<backspace>" => {
                        query.pop();
                    }
                    _ if key.chars().count() == 1 => query.push_str(key),
                    _ => return p,
                }
                let preview = Query::compile(&query)
                    .and_then(|regex| kind.apply(text, original, &regex, extend));
                p.set(preview.unwrap_or_else(|| original.clone()));
                p.message = Some(format!("{}: {query}", kind.label()));
                p.next.pending = Pending::Regex {
                    kind: *kind,
                    original: original.clone(),
                    input: query,
                };
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
                        let Some(c) = text.get_char(pos) else {
                            return r;
                        };
                        let pair = crate::motion::pair_object(text, r, c, true, 1);
                        if pair == r || pair.head == 0 {
                            return r;
                        }
                        let target = if pair.anchor == pos {
                            pair.head - 1
                        } else {
                            pair.anchor
                        };
                        r.move_to(text, target, extend)
                    }));
                }
                return p;
            }
            Pending::Object { around, count } => {
                p.next.pending = Pending::None;
                if key.chars().count() == 1 {
                    p.set(selections.transform(text, |r| match key {
                        "w" | "W" => crate::motion::word_object(text, r, key == "W", *around),
                        _ => crate::motion::pair_object(
                            text,
                            r,
                            key.chars().next().unwrap(),
                            *around,
                            *count,
                        ),
                    }));
                }
                return p;
            }
            Pending::Replace => {
                p.next.pending = Pending::None;
                let replacement = match key {
                    "<enter>" => document_newline(text),
                    "<tab>" => "\t",
                    ch if ch.chars().count() == 1 => ch,
                    _ => return p,
                };
                p.replace_fragments(text, selections, |_, slice| {
                    use unicode_segmentation::UnicodeSegmentation;
                    replacement.repeat(std::borrow::Cow::<str>::from(slice).graphemes(true).count())
                });
                return p;
            }
            Pending::Find { command, count } => {
                p.next.pending = Pending::None;
                let ch = match key {
                    "<tab>" => '\t',
                    "<enter>" => '\n',
                    ch if ch.chars().count() == 1 => ch.chars().next().unwrap(),
                    _ => return p,
                };
                p.next.last_find = Some((*command, ch, *count, extend));
                p.set(selections.transform(text, |r| {
                    crate::motion::find(text, r, *command, ch, *count, extend)
                }));
                return p;
            }
            Pending::Goto => {
                p.next.count = None;
                p.next.pending = Pending::None;
                match key {
                    "n" => p.host = Some(HostAction::NextBuffer),
                    "p" => p.host = Some(HostAction::PreviousBuffer),
                    "g" | "e" | "h" | "l" | "s" => p.set(selections.transform(text, |r| {
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
        if let [digit @ b'0'..=b'9'] = key.as_bytes()
            && (*digit != b'0' || self.count.is_some())
        {
            let next = self.count.unwrap_or(0) * 10 + (digit - b'0') as usize;
            if next <= 100_000_000 {
                p.next.count = Some(next);
            }
            return p;
        }
        if key != "g" && key != "m" {
            p.next.count = None;
        }
        match key {
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
                    input: String::new(),
                };
                p.message = Some(format!("{}: ", kind.label()));
            }
            "r" => p.next.pending = Pending::Replace,
            "f" | "F" | "t" | "T" => {
                p.next.pending = Pending::Find {
                    command: key.chars().next().unwrap(),
                    count,
                };
            }
            "<a-.>" => {
                if let Some((cmd, ch, count, extend)) = self.last_find {
                    p.set(selections.transform(text, |r| {
                        crate::motion::find(text, r, cmd, ch, count, extend)
                    }));
                }
            }
            "R" if !register.is_empty() => p.replace_fragments(text, selections, |i, slice| {
                if slice.len_chars() == 0 {
                    String::new()
                } else {
                    register[i.min(register.len() - 1)].clone()
                }
            }),
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
            "x" | "X" | "<a-x>" => p.set(selections.transform(text, |r| {
                let (first, last) = r.lines(text);
                let start = text.line_to_char(first);
                let end = text.line_to_char(last + 1);
                match key {
                    "x" => {
                        let extra = usize::from(start == r.start() && end == r.end());
                        let line = last
                            .saturating_add(count)
                            .saturating_add(extra)
                            .min(text.len_lines());
                        Range {
                            anchor: start,
                            head: text.line_to_char(line),
                        }
                    }
                    "<a-x>" if first == last => r,
                    "<a-x>" => r.with_bounds(
                        if r.start() == start {
                            start
                        } else {
                            text.line_to_char(first + 1)
                        },
                        if r.end() == end {
                            end
                        } else {
                            text.line_to_char(last)
                        },
                    ),
                    _ => r.with_bounds(start, end),
                }
            })),
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
            "u" => p.host = Some(HostAction::Undo),
            "U" => p.host = Some(HostAction::Redo),
            ":" => p.host = Some(HostAction::CommandLine),
            ">" | "<" | "J" | "<a-J>" => p.edit_lines(text, selections, key, count, context),
            "o" | "O" => p.open_lines(text, selections, key == "o", count),
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
            "j" | "k" | "<up>" | "<down>" | "<pageup>" | "<pagedown>" | "<c-f>" | "<c-b>"
            | "<c-u>" | "<c-d>" => {
                let rows = match key {
                    "<pageup>" | "<pagedown>" | "<c-f>" | "<c-b>" => {
                        context.page_height.saturating_sub(2).max(1)
                    }
                    "<c-u>" | "<c-d>" => (context.page_height / 2).max(1),
                    _ => 1,
                };
                p.vertical(
                    text,
                    selections,
                    matches!(key, "j" | "<down>" | "<pagedown>" | "<c-f>" | "<c-d>"),
                    count.saturating_mul(rows),
                    context.tab_width,
                    extend,
                );
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
                crate::motion::goto(text, r, "g", self.count, extend)
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
                    p.replace(selections.ranges(), "");
                    let change = key == "c" || key == "<a-c>";
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
