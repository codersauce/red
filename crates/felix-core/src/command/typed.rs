//! Helix typed commands that edit the selected fragments.
use super::*;

impl Session {
    /// Helix's `:sort`: reorder the selected fragments by their text.
    pub fn sort_selections(
        &self,
        text: RopeSlice<'_>,
        selection: &Selections,
        reverse: bool,
        insensitive: bool,
    ) -> Plan {
        let mut p = self.unchanged(selection);
        p.next.cancel_pending();
        if selection.ranges().len() == 1 {
            p.message =
                Some("Sorting requires multiple selections. Hint: split selection first".into());
            return p;
        }
        let mut fragments: Vec<String> = selection
            .ranges()
            .iter()
            .map(|r| text.slice(r.start()..r.end()).to_string())
            .collect();
        fragments.sort_by(|a, b| {
            let (a, b) = if reverse { (b, a) } else { (a, b) };
            if insensitive {
                a.to_lowercase().cmp(&b.to_lowercase())
            } else {
                a.cmp(b)
            }
        });
        p.replace_selections(selection, fragments);
        // Typed commands leave the mode unchanged, including Select.
        p.next.mode = self.mode;
        p
    }

    /// Replace each selection with the matching fragment as one undo step, for
    /// typed commands such as `:reflow` whose text the host computes.
    pub fn replace_each_selection(&self, selection: &Selections, fragments: Vec<String>) -> Plan {
        let mut p = self.unchanged(selection);
        p.next.cancel_pending();
        if fragments.len() == selection.ranges().len() {
            p.replace_selections(selection, fragments);
            p.next.mode = self.mode;
        }
        p
    }
}
