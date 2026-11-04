use super::*;
mod layers;
use felix_core::{Range as SelectionRange, Selections, Structure};
pub(crate) use layers::FelixSyntax;

fn query_source(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "bash" => include_str!("../queries/felix/bash.scm"),
        "c-sharp" | "c_sharp" => include_str!("../queries/felix/c-sharp.scm"),
        "c" => include_str!("../queries/felix/c.scm"),
        "cpp" => concat!(
            include_str!("../queries/felix/c.scm"),
            "\n",
            include_str!("../queries/felix/cpp.scm")
        ),
        "fish" => include_str!("../queries/felix/fish.scm"),
        "go" => include_str!("../queries/felix/go.scm"),
        "java" => include_str!("../queries/felix/java.scm"),
        "javascript" => concat!(
            include_str!("../queries/felix/ecma.scm"),
            "\n",
            include_str!("../queries/felix/javascript.scm")
        ),
        "json" => include_str!("../queries/felix/json.scm"),
        "jsx" => concat!(
            include_str!("../queries/felix/ecma.scm"),
            "\n",
            include_str!("../queries/felix/javascript.scm")
        ),
        "kotlin" => include_str!("../queries/felix/kotlin.scm"),
        "lua" => include_str!("../queries/felix/lua.scm"),
        "php" => include_str!("../queries/felix/php.scm"),
        "python" => include_str!("../queries/felix/python.scm"),
        "rust" => include_str!("../queries/felix/rust.scm"),
        "swift" => include_str!("../queries/felix/swift.scm"),
        "toml" => include_str!("../queries/felix/toml.scm"),
        "tsx" => concat!(
            include_str!("../queries/felix/_typescript.scm"),
            "\n",
            include_str!("../queries/felix/ecma.scm"),
            "\n",
            include_str!("../queries/felix/tsx.scm")
        ),
        "typescript" => concat!(
            include_str!("../queries/felix/_typescript.scm"),
            "\n",
            include_str!("../queries/felix/ecma.scm"),
            "\n",
            include_str!("../queries/felix/typescript.scm")
        ),
        "yaml" => include_str!("../queries/felix/yaml.scm"),
        _ => return None,
    })
}

impl SyntaxTextObjectService {
    pub(crate) fn felix_structure(
        &mut self,
        buffer: &Buffer,
        lang: &str,
        selection: &Selections,
        action: Structure,
        extend: bool,
    ) -> anyhow::Result<Option<Selections>> {
        let Some(grammar) = self.registry.grammar(lang) else {
            if matches!(action, Structure::Motion { .. }) {
                anyhow::bail!("Syntax-tree is not available in current buffer");
            }
            return Ok(None);
        };
        self.ensure_tree(buffer, lang, &grammar)?;
        let Some(document) = self.documents.get(&buffer.id()) else {
            return Ok(None);
        };
        let rope = buffer.contents_snapshot();
        let text = rope.slice(..);
        let root = document.tree.root_node();
        let object_key = match action {
            Structure::Object { key, .. } | Structure::Motion { key, .. } => Some(key),
            _ => None,
        };
        if let Some(key) = object_key {
            let Some(source) = query_source(lang) else {
                return Ok(None);
            };
            let query = match self.felix_queries.entry(lang.into()) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(Query::new(&grammar, source)?)
                }
            };
            let kinds: &[&str] = match action {
                Structure::Object { around: true, .. } => &["around"],
                Structure::Object { .. } => &["inside"],
                _ => &["movement", "around", "inside"],
            };
            let name = match key {
                't' => "class",
                'f' => "function",
                'a' => "parameter",
                'c' => "comment",
                'T' => "test",
                'e' => "entry",
                _ => return Ok(None),
            };
            let Some(index) = kinds
                .iter()
                .find_map(|kind| query.capture_index_for_name(&format!("{name}.{kind}")))
            else {
                return Ok(None);
            };
            let captures = capture_ranges(query, root, document.contents.as_bytes(), text, index)?;
            return Ok(Some(selection.transform(text, |r| {
                capture_range(text, r, action, extend, &captures)
            })));
        }

        let Some(layers) = self.felix_syntax(buffer, lang, selection)? else {
            return Ok(None);
        };
        let mut ranges = Vec::new();
        let mut primary = 0;
        for (i, &r) in selection.ranges().iter().enumerate() {
            if i == selection.primary_index() {
                primary = ranges.len();
            }
            let previous = ranges.len();
            select_nodes(&layers, text, r, action, extend, &mut ranges);
            if ranges.len() == previous {
                ranges.push(r);
            }
        }
        Ok(Selections::new(text, ranges, primary))
    }
}

fn capture_ranges(
    query: &Query,
    root: Node<'_>,
    source: &[u8],
    text: ropey::RopeSlice<'_>,
    index: u32,
) -> anyhow::Result<Vec<(usize, usize)>> {
    let mut cursor = QueryCursor::new();
    let started = Instant::now();
    let mut expired = false;
    let mut stop = |_: &QueryCursorState| {
        expired = started.elapsed() > STRUCTURAL_QUERY_BUDGET;
        expired
    };
    let mut ranges = Vec::new();
    {
        let mut matches = cursor.matches_with_options(
            query,
            root,
            source,
            QueryCursorOptions::new().progress_callback(&mut stop),
        );
        while let Some(m) = matches.next() {
            let mut captures = m.captures.iter().filter(|c| c.index == index);
            if let Some(first) = captures.next() {
                let last = captures.next_back().unwrap_or(first);
                ranges.push((
                    text.byte_to_char(first.node.start_byte()),
                    text.byte_to_char(last.node.end_byte()),
                ));
                anyhow::ensure!(
                    ranges.len() <= MAX_STRUCTURAL_CAPTURES,
                    "Too many syntax objects"
                );
            }
        }
    }
    anyhow::ensure!(!expired, "Syntax query exceeded its time budget");
    Ok(ranges)
}

fn directed(start: usize, end: usize, backward: bool) -> SelectionRange {
    if backward {
        SelectionRange {
            anchor: end,
            head: start,
        }
    } else {
        SelectionRange {
            anchor: start,
            head: end,
        }
    }
}

// Tree-house walks through the first child at the start byte and moves back up
// if that child fails to contain the full range. This matters on white space.
fn at_range<'a>(root: Node<'a>, start: usize, end: usize) -> Node<'a> {
    let mut cursor = root.walk();
    while cursor.goto_first_child_for_byte(start).is_some() {
        let node = cursor.node();
        if start < node.start_byte() || end > node.end_byte() {
            cursor.goto_parent();
            break;
        }
    }
    cursor.node()
}

fn select_nodes(
    layers: &FelixSyntax,
    text: ropey::RopeSlice<'_>,
    r: SelectionRange,
    action: Structure,
    extend: bool,
    out: &mut Vec<SelectionRange>,
) {
    let start = text.char_to_byte(r.start());
    let end = text.char_to_byte(r.end());
    let backward = r.head < r.anchor;
    let mut layer = layers.layer(start, end);
    let root = layers.root(layer);
    if matches!(action, Structure::ParentStart | Structure::ParentEnd) {
        let Some(mut node) = root.named_descendant_for_byte_range(start, end) else {
            return;
        };
        let pos = if action == Structure::ParentStart {
            let cursor = text.char_to_byte(r.cursor(text));
            while node.start_byte() == cursor {
                let Some(parent) = node.parent() else { break };
                node = parent;
            }
            text.byte_to_char(node.start_byte())
        } else {
            text.byte_to_char(node.end_byte())
        };
        out.push(if extend {
            SelectionRange {
                anchor: r.anchor,
                head: if pos >= r.anchor {
                    pos.saturating_add(1)
                } else {
                    pos
                },
            }
        } else {
            directed(pos, pos.saturating_add(1), backward)
        });
        return;
    }
    let mut node = at_range(root, start, end);
    match action {
        Structure::Expand => {
            while node.start_byte() == start && node.end_byte() == end {
                let Some(parent) = layers.parent(&mut layer, node) else {
                    break;
                };
                node = parent;
            }
        }
        Structure::Shrink => node = layers.first_child(layer, node).unwrap_or(node),
        Structure::Previous | Structure::Next => loop {
            let sibling = if action == Structure::Next {
                node.next_sibling()
            } else {
                node.prev_sibling()
            };
            if let Some(s) = sibling {
                node = s;
                break;
            }
            let Some(parent) = layers.parent(&mut layer, node) else {
                break;
            };
            node = parent;
        },
        Structure::Siblings | Structure::Children => {
            if action == Structure::Siblings {
                loop {
                    let Some(parent) = layers.parent(&mut layer, node) else {
                        return;
                    };
                    node = parent;
                    if node.child_count() > 1 {
                        break;
                    }
                }
            }
            for child in std::iter::successors(layers.first_child(layer, node), Node::next_sibling)
                .filter(Node::is_named)
            {
                out.push(directed(
                    text.byte_to_char(child.start_byte()),
                    text.byte_to_char(child.end_byte()),
                    backward,
                ));
            }
            return;
        }
        _ => return,
    }
    out.push(directed(
        text.byte_to_char(node.start_byte()),
        text.byte_to_char(node.end_byte()),
        match action {
            Structure::Next => false,
            Structure::Previous => true,
            _ => backward,
        },
    ));
}

fn capture_range(
    text: ropey::RopeSlice<'_>,
    original: SelectionRange,
    action: Structure,
    extend: bool,
    objects: &[(usize, usize)],
) -> SelectionRange {
    let len = text.len_chars();
    let mut range = original;
    if let Structure::Motion { forward, count, .. } = action {
        for _ in 0..count {
            let cursor = range.cursor(text);
            let candidate = if forward {
                objects
                    .iter()
                    .filter(|p| p.0 > cursor)
                    .min_by_key(|p| (p.0, std::cmp::Reverse(p.1)))
            } else {
                objects
                    .iter()
                    .filter(|p| p.1 < cursor)
                    .max_by_key(|p| (p.1, std::cmp::Reverse(p.0)))
            };
            let Some(&(start, end)) = candidate else {
                break;
            };
            if end >= len {
                break;
            }
            let next = directed(start, end, false);
            if next == range {
                break;
            }
            range = next;
        }
        if extend {
            return SelectionRange {
                anchor: original.anchor,
                head: if range.head < original.anchor {
                    range.anchor
                } else {
                    range.head
                },
            };
        }
        return directed(range.start(), range.end(), !forward);
    }
    let cursor = original.cursor(text);
    objects
        .iter()
        .copied()
        .filter(|&(a, b)| a <= cursor && cursor < b)
        .min_by_key(|&(a, b)| b - a)
        .filter(|&(_, b)| b < len)
        .map_or(original, |(a, b)| directed(a, b, false))
}

impl felix_core::SyntaxPairs for FelixSyntax {
    fn closest(
        &self,
        text: ropey::RopeSlice<'_>,
        range: SelectionRange,
        count: usize,
    ) -> Option<(usize, usize)> {
        let mut at = range.end();
        let mut remaining = count;
        while at < text.len_chars() {
            let target = self.find(text, at, true)?;
            let other = self.find(text, target, false)?;
            let (open, close) = (target.min(other), target.max(other));
            if open <= range.start() && close >= range.end().saturating_sub(1) {
                remaining -= 1;
                if remaining == 0 {
                    return Some((open, close));
                }
            }
            let next = felix_core::boundary(text, close, true);
            if next <= at {
                break;
            }
            at = next;
        }
        None
    }

    fn matching(&self, text: ropey::RopeSlice<'_>, position: usize) -> Option<usize> {
        self.find(text, position, true)
    }
}

impl FelixSyntax {
    fn find(&self, text: ropey::RopeSlice<'_>, position: usize, fuzzy: bool) -> Option<usize> {
        let byte = text.char_to_byte(position);
        let root = self.root(self.layer(byte, byte));
        let mut node = root.descendant_for_byte_range(byte, byte)?;
        loop {
            if node.is_named() && node.child_count() >= 2 {
                let (first, last) = (node.child(0)?, node.child(node.child_count() - 1)?);
                if let (Some((start, open)), Some((end, close))) =
                    (delimiter(text, first), delimiter(text, last))
                {
                    if matches_pair(open, close) {
                        if position == end {
                            return Some(start);
                        }
                        if fuzzy || start == position {
                            return Some(end);
                        }
                    }
                }
            }
            if let Some((_, ch)) = delimiter(text, node) {
                if let Some(target) = sibling_pair(text, node, ch, false)
                    .or_else(|| sibling_pair(text, node, ch, true))
                {
                    return Some(target);
                }
            }
            if fuzzy {
                for sibling in
                    std::iter::successors(node.next_sibling(), Node::next_sibling).take(16)
                {
                    if let Some((end, ch)) = delimiter(text, sibling) {
                        if sibling_pair(text, sibling, ch, false).is_some() {
                            return Some(end);
                        }
                    }
                }
            } else if node.is_named() {
                break;
            }
            let Some(parent) = node.parent() else { break };
            node = parent;
        }
        let leaf = root.named_descendant_for_byte_range(byte, byte.checked_add(1)?)?;
        if leaf.child_count() > 0 {
            return None;
        }
        let start = text.byte_to_char(leaf.start_byte());
        let slice = text.byte_slice(leaf.byte_range());
        felix_core::matching_plain_bracket(slice, position.checked_sub(start)?).map(|at| start + at)
    }
}

fn matches_pair(open: char, close: char) -> bool {
    matches!(
        (open, close),
        ('(', ')')
            | ('[', ']')
            | ('{', '}')
            | ('<', '>')
            | ('‘', '’')
            | ('“', '”')
            | ('«', '»')
            | ('「', '」')
            | ('（', '）')
            | ('"', '"')
            | ('\'', '\'')
            | ('`', '`')
    )
}

fn delimiter(text: ropey::RopeSlice<'_>, node: Node<'_>) -> Option<(usize, char)> {
    let byte = node.start_byte();
    let pos = text.byte_to_char(byte);
    let ch = text.get_char(pos)?;
    (ch.len_utf8() == node.end_byte() - byte).then_some((pos, ch))
}

fn sibling_pair(
    text: ropey::RopeSlice<'_>,
    node: Node<'_>,
    ch: char,
    forward: bool,
) -> Option<usize> {
    let step = if forward {
        Node::next_sibling
    } else {
        Node::prev_sibling
    };
    let mut depth = 0usize;
    for other in std::iter::successors(step(&node), step).take(16) {
        let Some((pos, c)) = delimiter(text, other) else {
            continue;
        };
        if if forward {
            matches_pair(ch, c)
        } else {
            matches_pair(c, ch)
        } {
            if depth == 0 {
                return Some(pos);
            }
            depth -= 1;
        } else if ch == c {
            depth += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_felix_query_compiles_with_its_bundled_grammar() {
        let registry = LanguageRegistry::bundled();
        let mut failures = Vec::new();
        for name in [
            "bash",
            "c-sharp",
            "c",
            "cpp",
            "fish",
            "go",
            "java",
            "javascript",
            "json",
            "jsx",
            "kotlin",
            "lua",
            "php",
            "python",
            "rust",
            "swift",
            "toml",
            "tsx",
            "typescript",
            "yaml",
        ] {
            if let Some(grammar) = registry.grammar(name) {
                if let Err(error) = Query::new(&grammar, query_source(name).unwrap()) {
                    failures.push(format!("{name}: {error}"));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
