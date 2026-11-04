use super::*;
use crate::highlighter::{collect_injections_with_options, MAX_INJECTION_DEPTH};
use std::sync::Arc;

/// A snapshot for one document revision. Nodes in every tree use the original document's bytes.
#[derive(Clone)]
pub(crate) struct FelixSyntax(Arc<Vec<Layer>>);

#[derive(Clone)]
struct Layer {
    language: String,
    tree: Arc<Tree>,
    parent: usize,
    regions: Option<Vec<Region>>,
}

#[derive(Clone)]
struct Region {
    language: String,
    range: tree_sitter::Range,
    /// Furthest end among this and all preceding regions, including overlaps.
    max_end: usize,
    child: Option<usize>,
}

impl FelixSyntax {
    fn new(lang: &str, tree: Tree) -> Self {
        Self(Arc::new(vec![Layer {
            language: lang.to_owned(),
            tree: Arc::new(tree),
            parent: 0,
            regions: None,
        }]))
    }

    pub(super) fn root(&self, layer: usize) -> Node<'_> {
        self.0[layer].tree.root_node()
    }

    pub(super) fn layer(&self, start: usize, end: usize) -> usize {
        let mut layer = 0;
        while let Some(child) = self.child(layer, start, end.saturating_add(1)) {
            layer = child;
        }
        layer
    }

    fn child(&self, layer: usize, start: usize, end: usize) -> Option<usize> {
        let regions = self.0[layer].regions.as_ref()?;
        let region = &regions[region_at(regions, start, end)?];
        region.child
    }

    pub(super) fn parent<'a>(&'a self, layer: &mut usize, node: Node<'a>) -> Option<Node<'a>> {
        if let Some(parent) = node.parent() {
            return Some(parent);
        }
        if *layer == 0 {
            return None;
        }
        *layer = self.0[*layer].parent;
        Some(self.root(*layer))
    }

    pub(super) fn first_child<'a>(&'a self, layer: usize, node: Node<'a>) -> Option<Node<'a>> {
        self.child(layer, node.start_byte(), node.end_byte())
            .map(|child| self.root(child))
            .or_else(|| node.child(0))
    }

    fn prepare(
        &mut self,
        registry: &LanguageRegistry,
        queries: &mut HashMap<String, Query>,
        source: &str,
        start: usize,
        end: usize,
        started: Instant,
    ) -> anyhow::Result<()> {
        let layers = Arc::make_mut(&mut self.0);
        let mut index = 0;
        for _ in 0..MAX_INJECTION_DEPTH {
            let layer = &mut layers[index];
            if layer.regions.is_none() {
                let mut regions = Vec::new();
                if let Some(source_query) = registry.injections(&layer.language) {
                    let query = match queries.entry(layer.language.clone()) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(Query::new(
                                &registry
                                    .grammar(&layer.language)
                                    .context("missing injection grammar")?,
                                source_query,
                            )?)
                        }
                    };
                    let mut expired = false;
                    let mut stop = |_: &QueryCursorState| {
                        expired = started.elapsed() >= STRUCTURAL_QUERY_BUDGET;
                        expired
                    };
                    let captures = collect_injections_with_options(
                        query,
                        layer.tree.root_node(),
                        source,
                        QueryCursorOptions::new().progress_callback(&mut stop),
                        MAX_STRUCTURAL_CAPTURES + 1,
                    );
                    anyhow::ensure!(
                        !expired,
                        "structural injection query exceeded its time budget"
                    );
                    anyhow::ensure!(
                        captures.len() <= MAX_STRUCTURAL_CAPTURES,
                        "too many syntax injections"
                    );
                    for capture in captures {
                        if let Some(id) = registry.id_for_injection_name(&capture.language_name) {
                            regions.push(Region {
                                language: id.to_string(),
                                range: capture.content,
                                max_end: 0,
                                child: None,
                            });
                        }
                    }
                    regions.sort_by_key(|region| (region.range.start_byte, region.range.end_byte));
                    let mut max_end = 0;
                    for region in &mut regions {
                        max_end = max_end.max(region.range.end_byte);
                        region.max_end = max_end;
                    }
                }
                layer.regions = Some(regions);
            }
            let regions = layer.regions.as_mut().unwrap();
            let Some(slot) = region_at(regions, start, end) else {
                break;
            };
            let region = &regions[slot];
            if let Some(child) = region.child {
                index = child;
                continue;
            }
            let Some(grammar) = registry.grammar(&region.language) else {
                break;
            };
            let mut parser = Parser::new();
            parser.set_language(&grammar)?;
            parser.set_included_ranges(&[region.range])?;
            let mut stop = |_: &ParseState| started.elapsed() >= STRUCTURAL_PARSE_BUDGET;
            let tree = parser
                .parse_with_options(
                    &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
                    None,
                    Some(ParseOptions::new().progress_callback(&mut stop)),
                )
                .context("injected syntax parsing exceeded its time budget")?;
            let child = Layer {
                language: region.language.clone(),
                tree: Arc::new(tree),
                parent: index,
                regions: None,
            };
            let next = layers.len();
            layers[index].regions.as_mut().unwrap()[slot].child = Some(next);
            layers.push(child);
            index = next;
        }
        Ok(())
    }
}

fn region_at(regions: &[Region], start: usize, end: usize) -> Option<usize> {
    let right = regions.partition_point(|region| region.range.start_byte <= start);
    let left = regions[..right].partition_point(|region| region.max_end < end);
    regions[left..right]
        .iter()
        .enumerate()
        .filter(|(_, region)| end <= region.range.end_byte)
        .min_by_key(|(_, region)| region.range.end_byte - region.range.start_byte)
        .map(|(index, _)| left + index)
}

impl SyntaxTextObjectService {
    pub(crate) fn felix_syntax(
        &mut self,
        buffer: &Buffer,
        lang: &str,
        selection: &Selections,
    ) -> anyhow::Result<Option<FelixSyntax>> {
        let Some(grammar) = self.registry.grammar(lang) else {
            return Ok(None);
        };
        self.ensure_tree(buffer, lang, &grammar)?;
        let doc = self
            .documents
            .get_mut(&buffer.id())
            .context("missing syntax document")?;
        let layers = doc
            .felix_layers
            .get_or_insert_with(|| FelixSyntax::new(lang, doc.tree.clone()));
        if self.registry.injections(lang).is_some() {
            let text = buffer.contents_snapshot();
            let started = Instant::now();
            for range in selection.ranges() {
                let start = text.char_to_byte(range.start());
                let end = text.char_to_byte(range.end());
                // Preload the range and both cursor endpoints before handing out the immutable snapshot.
                for (from, to) in [(start, end), (start, start), (end, end)] {
                    layers.prepare(
                        &self.registry,
                        &mut self.felix_injection_queries,
                        &doc.contents,
                        from,
                        to,
                        started,
                    )?;
                }
            }
        }
        Ok(Some(layers.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use felix_core::Range as SelectionRange;

    #[test]
    fn injected_parses_are_lazy_and_scoped_to_the_document_revision() {
        let src = "```rust\nfn f() { left(); }\n```\n\n```js\nright();\n```\n";
        let mut doc = Buffer::new(Some("sample.md".into()), src.into());
        let mut service = SyntaxTextObjectService::new(Arc::new(LanguageRegistry::bundled()));
        let mut snapshot = |doc: &Buffer, label: &str| {
            let source = doc.contents_snapshot();
            let byte = doc.contents().find(label).unwrap();
            let range = SelectionRange::at(source.slice(..), source.byte_to_char(byte) + 1);
            let selection = Selections::single(source.slice(..), range);
            service
                .felix_syntax(doc, "markdown", &selection)
                .unwrap()
                .unwrap()
        };
        let first = snapshot(&doc, "left");
        assert_eq!(first.0.len(), 2);
        let first_child_id = first.root(1).id();
        let second = snapshot(&doc, "right");
        assert_eq!(second.0.len(), 3);
        assert_eq!(second.root(1).id(), first_child_id);
        let point = TextPosition::new(0, 0);
        doc.replace_range_raw(TextRange::new(point, point), "α\n");
        let after_edit = snapshot(&doc, "right");
        assert_eq!(after_edit.0.len(), 2);
        assert_eq!(after_edit.0[1].language, "javascript");
        assert_eq!(
            after_edit.root(1).start_byte(),
            doc.contents().find("right").unwrap()
        );
        assert_eq!(first.root(1).id(), first_child_id);
    }
}
