use super::*;

/// How a successful shell invocation acts on each selected fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Pipe,
    PipeTo,
    Insert,
    Append,
    Keep,
}

impl Shell {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pipe => "pipe",
            Self::PipeTo => "pipe-to",
            Self::Insert => "insert-output",
            Self::Append => "append-output",
            Self::Keep => "keep-pipe",
        }
    }
}

impl Session {
    /// Apply all results together. A missing output represents a rejected selection for keep-pipe.
    pub fn shell_result(
        &self,
        selections: &Selections,
        kind: Shell,
        outputs: &[Option<String>],
    ) -> Plan {
        let mut p = self.unchanged(selections);
        if outputs.len() != selections.ranges().len()
            || (kind != Shell::Keep && outputs.iter().any(Option::is_none))
        {
            p.message = Some("Incomplete shell results".into());
            return p;
        }
        if kind == Shell::PipeTo {
            return p;
        }
        if kind == Shell::Keep {
            let mut primary = None;
            let mut kept = Vec::new();
            for (i, (&r, out)) in selections.ranges().iter().zip(outputs).enumerate() {
                if out.is_some() {
                    if i >= selections.primary_index() {
                        primary.get_or_insert(kept.len());
                    }
                    kept.push(r);
                }
            }
            if kept.is_empty() {
                p.message = Some("No selections remaining".into());
            } else {
                p.primary = primary.unwrap_or(kept.len() - 1);
                p.ranges = kept;
            }
            return p;
        }
        let mut added = 0;
        let mut removed = 0;
        for (i, (&r, out)) in selections.ranges().iter().zip(outputs).enumerate() {
            let (start, end) = match kind {
                Shell::Insert => (r.start(), r.start()),
                Shell::Append => (r.end(), r.end()),
                _ => (r.start(), r.end()),
            };
            let output = out.as_deref().unwrap_or_default();
            let size = output.chars().count();
            let at = start - removed + added;
            p.ranges[i] = r.with_bounds(at, at + size);
            p.edits.push(Edit {
                start,
                end,
                insert: output.into(),
            });
            added += size;
            removed += end - start;
        }
        p.history = History::Atomic;
        p
    }
}

impl Plan {
    pub(super) fn shell_key(
        &mut self,
        kind: Shell,
        input: &Prompt,
        key: &str,
        text: RopeSlice<'_>,
        selections: &Selections,
        register: Option<&[String]>,
    ) {
        let Pending::Shell { input: next, .. } = &mut self.next.pending else {
            unreachable!()
        };
        match key {
            "<esc>" | "<c-c>" => self.next.pending = Pending::None,
            "<enter>" if next.keep_directory_completion() => {}
            "<enter>" => {
                let previous = input
                    .history(&[])
                    .last()
                    .map(String::as_str)
                    .unwrap_or_default();
                let command = if input.line.is_empty() {
                    previous
                } else {
                    &input.line
                };
                if !command.is_empty() {
                    self.shell = Some((kind, command.to_owned()));
                    if command != previous {
                        self.search_name = '|';
                        self.search = Some(command.to_owned());
                    }
                }
                self.next.pending = Pending::None;
            }
            "<tab>" | "<backtab>" | "<c-i>" => next.cycle(key == "<backtab>"),
            "<up>" | "<c-p>" | "<down>" | "<c-n>" => {
                next.recall(input.history(&[]), matches!(key, "<up>" | "<c-p>"))
            }
            "<c-s>" => {
                let range = crate::motion::word_object(text, selections.primary(), false, false);
                next.paste(&text.slice(range.start()..range.end()).to_string());
            }
            "<c-r>" => {
                next.selected = None;
                next.await_register = true;
            }
            _ if input.await_register && argument_character(key).is_some() => {
                next.await_register = false;
                if let Some(value) = register.and_then(|r| r.first()) {
                    next.paste(value);
                }
            }
            _ => {
                next.edit(key);
            }
        }
    }
}

impl Session {
    /// Resolve suggestions only while a shell prompt is actually displayed.
    pub fn update_shell_completions(
        &mut self,
        complete: impl FnOnce(&str) -> (usize, Vec<String>),
        registers: impl FnOnce() -> Vec<String>,
    ) {
        if let Pending::Shell { kind, input } = &mut self.pending
            && input.selected.is_none()
        {
            if input.await_register {
                input.set_shell_candidates(0, registers());
            } else if *kind != Shell::Keep {
                let (start, items) = complete(&input.line);
                input.set_shell_candidates(start, items);
            } else {
                input.set_shell_candidates(0, Vec::new());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_pipe_results_preserve_every_selection_and_make_no_edits() {
        let doc = ropey::Rope::from_str("a bb\n");
        let selections = Selections::new(
            doc.slice(..),
            vec![Range { anchor: 1, head: 0 }, Range { anchor: 2, head: 4 }],
            1,
        )
        .unwrap();
        let results = [Some("x".into()), None];
        let session = Session::default();
        for incomplete in [&results[..1], &results[..]] {
            let plan = session.shell_result(&selections, Shell::Pipe, incomplete);
            assert!(plan.edits.is_empty());
            assert_eq!(plan.history, History::None);
            assert_eq!(plan.selections(doc.slice(..)), selections);
            assert_eq!(plan.message.as_deref(), Some("Incomplete shell results"));
        }
        let filtered = session
            .shell_result(&selections, Shell::Keep, &results)
            .selections(doc.slice(..));
        assert_eq!(filtered.ranges(), &selections.ranges()[..1]);
        assert_eq!(filtered.primary_index(), 0);
    }
}
