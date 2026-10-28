//! Source-backed Markdown navigation. Parsing never changes the document, and
//! destination validation completes before a buffer or jump entry is added.

use std::{collections::HashMap, ops::Range, path::Path, sync::Arc};

use anyhow::{anyhow, bail, Context as _};
use pulldown_cmark::{BrokenLink, Event, LinkType, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation as _;

use super::{render_buffer::RenderBuffer, Action, Editor, Mode, Runtime};
use crate::{
    buffer::{Buffer, BufferId},
    config::MarkdownLinkSplitFocus,
    plugin,
    utils::normalized_file_path,
    window::{JumpList, WindowId},
};

/// Bound synchronous parsing and retained indexes, as with table projections.
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct CachedLinks {
    revision: u64,
    index: Arc<LinkIndex>,
}

#[derive(Debug)]
struct Link {
    source: Range<usize>,
    destination: Result<String, String>,
}

#[derive(Debug, Default)]
struct LinkIndex {
    links: Vec<Link>,
    /// Heading identifiers map to UTF-8 source offsets, including Setext headings.
    headings: HashMap<String, usize>,
}

impl LinkIndex {
    fn parse(source: &str) -> Self {
        let mut index = Self::default();
        let mut unresolved = Vec::new();
        let mut broken = |link: BrokenLink<'_>| {
            unresolved.push(Link {
                source: link.span,
                destination: Err(format!("Undefined Markdown reference: {}", link.reference)),
            });
            None
        };
        let parser = Parser::new_with_broken_link_callback(
            source,
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
            Some(&mut broken),
        );
        for (_, definition) in parser.reference_definitions().iter() {
            index.links.push(Link {
                source: definition.span.clone(),
                destination: Ok(definition.dest.to_string()),
            });
        }
        let mut heading = None;
        let mut suffixes = HashMap::new();
        for (event, range) in parser.into_offset_iter() {
            match event {
                Event::Start(Tag::Link {
                    dest_url,
                    link_type,
                    ..
                }) => index.links.push(Link {
                    source: range,
                    destination: Ok(if link_type == LinkType::Email {
                        format!("mailto:{dest_url}")
                    } else {
                        dest_url.to_string()
                    }),
                }),
                Event::Start(Tag::Heading { .. }) => {
                    heading = Some((range.start, String::new()));
                }
                Event::Text(text) | Event::Code(text) => {
                    if let Some((_, title)) = &mut heading {
                        title.push_str(&text);
                    }
                }
                Event::SoftBreak | Event::HardBreak => {
                    if let Some((_, title)) = &mut heading {
                        title.push(' ');
                    }
                }
                Event::End(TagEnd::Heading(_)) => {
                    if let Some((offset, title)) = heading.take() {
                        let base = heading_slug(&title);
                        let suffix = suffixes.entry(base.clone()).or_insert(0);
                        let mut slug = base.clone();
                        while index.headings.contains_key(&slug) {
                            *suffix += 1;
                            slug = format!("{base}-{suffix}");
                        }
                        index.headings.insert(slug, offset);
                    }
                }
                _ => {}
            }
        }
        index.links.extend(unresolved);
        index.links.sort_by_key(|link| link.source.start);
        index
    }

    fn destination_at(&self, byte: usize) -> anyhow::Result<&str> {
        let link = self
            .links
            .iter()
            .find(|link| link.source.contains(&byte))
            .ok_or_else(|| anyhow!("No Markdown link under the cursor"))?;
        link.destination
            .as_deref()
            .map_err(|error| anyhow!("{error}"))
    }
}

/// Use rendered heading text: lowercase, omit punctuation, replace spaces with
/// hyphens, and retain Unicode letters/marks. Duplicate identifiers get -1, -2…
fn heading_slug(title: &str) -> String {
    title
        .chars()
        .flat_map(char::to_lowercase)
        .filter_map(|character| {
            if character.is_whitespace() {
                Some('-')
            } else if character == '-'
                || character == '_'
                || character.is_alphanumeric()
                || unicode_ident::is_xid_continue(character)
            {
                Some(character)
            } else {
                None
            }
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Position {
    Start,
    Heading(String),
    /// One-based source line and UTF-8 byte column, matching panel file links.
    Source {
        line: usize,
        column: usize,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum Target {
    External(String),
    Local { path: String, position: Position },
}

impl Target {
    fn parse(destination: &str) -> anyhow::Result<Self> {
        let lowercase = destination.to_ascii_lowercase();
        if lowercase.starts_with("https://")
            || lowercase.starts_with("http://")
            || lowercase.starts_with("mailto:")
        {
            return Ok(Self::External(destination.to_string()));
        }

        // Split syntax before decoding: %23 and %3A can belong to filenames.
        let (path, position) =
            if let Some((path, line, column)) = plugin::parse_source_location(destination) {
                (path, Position::Source { line, column })
            } else if let Some((path, fragment)) = destination.split_once('#') {
                let fragment = decode_component(fragment)?;
                let position = if fragment.is_empty() {
                    Position::Source { line: 1, column: 1 }
                } else if let Some((_, line, column)) =
                    plugin::parse_source_location(&format!("_#{fragment}"))
                {
                    Position::Source { line, column }
                } else {
                    Position::Heading(fragment)
                };
                (path.to_string(), position)
            } else {
                (destination.to_string(), Position::Start)
            };
        if let Some((scheme, _)) = path.split_once(':') {
            let windows_drive =
                cfg!(windows) && scheme.len() == 1 && scheme.as_bytes()[0].is_ascii_alphabetic();
            if !windows_drive
                && !scheme.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            {
                bail!("Unsupported link scheme: {scheme}");
            }
        }
        Ok(Self::Local {
            path: decode_component(&path)?,
            position,
        })
    }
}

fn decode_component(value: &str) -> anyhow::Result<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|c| char::from(c).to_digit(16));
            let low = bytes.next().and_then(|c| char::from(c).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                bail!("Invalid percent escape in Markdown link");
            };
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    let decoded = String::from_utf8(decoded).context("Markdown link is not valid UTF-8")?;
    if decoded.contains('\0') {
        bail!("Markdown link contains a null character");
    }
    Ok(decoded)
}

struct PreparedLink {
    destination: PreparedDestination,
    new_buffer: Option<Buffer>,
}

enum PreparedDestination {
    External(String),
    Local {
        buffer_id: BufferId,
        /// Grapheme column and zero-based line; absent for a bare file link.
        position: Option<(usize, usize)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkOpening {
    Default,
    Split,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LinkDestination {
    source_buffer: BufferId,
    window: WindowId,
}

impl Editor {
    /// Drop routes when their source document or either pane no longer exists.
    pub(super) fn reconcile_markdown_link_destinations(&mut self) {
        self.markdown_link_destinations
            .retain(|source, destination| {
                self.window_manager.window(destination.window).is_some()
                    && self.window_manager.window(*source).is_some_and(|window| {
                        self.buffer_manager
                            .get(window.buffer_index)
                            .is_some_and(|buffer| buffer.id() == destination.source_buffer)
                    })
            });
    }

    pub(super) fn markdown_link_destination(&self) -> Option<WindowId> {
        let source = self.window_manager.active_window()?;
        let destination = self.markdown_link_destinations.get(&source.id)?;
        (self.current_buffer().id() == destination.source_buffer
            && self.window_manager.window(destination.window).is_some())
        .then_some(destination.window)
    }

    fn markdown_link_index(&self, buffer: &Buffer) -> anyhow::Result<Arc<LinkIndex>> {
        if buffer.byte_len() > MAX_DOCUMENT_BYTES {
            bail!("Markdown link navigation supports documents up to 2 MiB");
        }
        let mut cache = self.markdown_link_cache.borrow_mut();
        if cache.len() >= 8 && !cache.contains_key(&buffer.id()) {
            cache.clear();
        }
        let cached = cache.entry(buffer.id()).or_insert_with(|| CachedLinks {
            revision: buffer.revision(),
            index: Arc::new(LinkIndex::parse(&buffer.contents())),
        });
        if cached.revision != buffer.revision() {
            cached.revision = buffer.revision();
            cached.index = Arc::new(LinkIndex::parse(&buffer.contents()));
        }
        Ok(Arc::clone(&cached.index))
    }

    fn markdown_link_position(
        &self,
        buffer: &Buffer,
        position: &Position,
    ) -> anyhow::Result<(usize, usize)> {
        match position {
            Position::Start => Ok((0, 0)),
            Position::Source { line, column } => {
                let line = (line - 1).min(buffer.len().saturating_sub(1));
                let column = buffer.get(line).map_or(0, |text| {
                    let text = text.trim_end_matches(['\r', '\n']);
                    if *column > text.len() {
                        text.len()
                    } else {
                        text.grapheme_indices(true)
                            .map(|(byte, _)| byte)
                            .take_while(|byte| *byte < *column)
                            .last()
                            .unwrap_or(0)
                    }
                });
                Ok((line, column))
            }
            Position::Heading(heading) => {
                let index = self.markdown_link_index(buffer)?;
                let byte = index
                    .headings
                    .get(heading)
                    .ok_or_else(|| anyhow!("Markdown heading not found: #{heading}"))?;
                let position = buffer
                    .byte_idx_to_position(*byte)
                    .ok_or_else(|| anyhow!("Markdown heading position is no longer valid"))?;
                Ok((position.line, 0))
            }
        }
    }

    fn prepare_markdown_link(&self) -> anyhow::Result<PreparedLink> {
        if self.mode != Mode::Normal {
            bail!("Follow link is available in Normal mode");
        }
        if self
            .highlight_language_id_for_buffer_index(self.buffer_manager.active_index())
            .as_deref()
            != Some("markdown")
        {
            bail!("Follow link is available in Markdown buffers");
        }
        let document = self.current_buffer();
        let byte = document.position_to_byte_idx(self.cursor_text_position());
        let index = self.markdown_link_index(document)?;
        let (path, position) = match Target::parse(index.destination_at(byte)?)? {
            Target::Local { path, position } => (path, position),
            Target::External(url) => {
                return Ok(PreparedLink {
                    destination: PreparedDestination::External(url),
                    new_buffer: None,
                });
            }
        };

        if path.is_empty() {
            let (line, byte_column) = self.markdown_link_position(document, &position)?;
            let column = document.get(line).map_or(0, |text| {
                crate::unicode_utils::byte_to_grapheme(&text, byte_column)
            });
            return Ok(PreparedLink {
                destination: PreparedDestination::Local {
                    buffer_id: document.id(),
                    position: Some((column, line)),
                },
                new_buffer: None,
            });
        }
        let path = if Path::new(&path).is_absolute() || path.starts_with("~/") {
            normalized_file_path(&path)?
        } else {
            let document_path = document
                .file
                .as_deref()
                .ok_or_else(|| anyhow!("Save this document to follow relative links"))?;
            let document_path = normalized_file_path(document_path)?;
            let parent = document_path
                .parent()
                .ok_or_else(|| anyhow!("Markdown document has no parent directory"))?;
            normalized_file_path(&parent.join(path).to_string_lossy())?
        };
        let path_string = path.to_string_lossy().into_owned();
        let mut new_buffer = None;
        let destination = if let Some(index) = self.file_buffer_index(&path) {
            &self.buffer_manager[index]
        } else {
            let metadata = std::fs::metadata(&path)
                .with_context(|| format!("Cannot open linked file: {}", path.display()))?;
            if !metadata.is_file() {
                bail!("Linked path is not a file: {}", path.display());
            }
            // Never use load_or_create: a missing link must not create an empty buffer.
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("Cannot read linked file: {}", path.display()))?;
            new_buffer.insert(Buffer::new(Some(path_string.clone()), contents))
        };
        let position = if position == Position::Start {
            None
        } else {
            let (line, column) = self.markdown_link_position(destination, &position)?;
            let column = destination.get(line).map_or(0, |text| {
                crate::unicode_utils::byte_to_grapheme(&text, column)
            });
            Some((column, line))
        };
        Ok(PreparedLink {
            destination: PreparedDestination::Local {
                buffer_id: destination.id(),
                position,
            },
            new_buffer,
        })
    }

    pub(super) async fn follow_markdown_link(
        &mut self,
        opening: LinkOpening,
        screen: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        let prepared = self.prepare_markdown_link()?;
        let (buffer_id, position) = match prepared.destination {
            PreparedDestination::Local {
                buffer_id,
                position,
            } => (buffer_id, position),
            PreparedDestination::External(url) => {
                self.execute_with_tracking(&Action::OpenExternalUrl(url), screen, runtime, false)
                    .await?;
                return Ok(());
            }
        };
        let source = self
            .window_manager
            .active_stable_window_id()
            .ok_or_else(|| anyhow!("No source pane for Markdown link"))?;
        let source_buffer = self.current_buffer().id();
        let destination = self.markdown_link_destination();
        let opened = prepared.new_buffer.map(|buffer| {
            let file = buffer.file.clone();
            let index = self.buffer_manager.len();
            self.buffer_manager.push_buffer(buffer);
            if let Some(file) = &file {
                self.rebind_inline_history_file(file);
            }
            (file, index)
        });

        let routed = destination.is_some() || opening == LinkOpening::Split;
        let previous = if let Some(destination) = destination {
            let index = self
                .window_manager
                .window_index(destination)
                .expect("validated Markdown destination must exist");
            self.set_active_window(index);
            Some(self.current_jump_entry())
        } else if routed {
            let index = self.buffer_manager.active_index();
            if !self.update_window_layout(|windows| windows.split_vertical(index)) {
                if opened.is_some() {
                    self.buffer_manager.pop_buffer();
                }
                bail!("Cannot create a link destination split");
            }
            *self.active_jump_list_mut() = JumpList::default();
            None
        } else {
            None
        };

        let result = self
            .open_prepared_local_link(buffer_id, position, screen, runtime)
            .await;
        if result.is_ok() && routed {
            if let Some(previous) = previous {
                self.save_to_history(previous);
            }
            let window = self
                .window_manager
                .active_stable_window_id()
                .expect("opening a link retains the destination pane");
            self.markdown_link_destinations.insert(
                source,
                LinkDestination {
                    source_buffer,
                    window,
                },
            );
        }
        if routed && self.config.markdown_link_split_focus == MarkdownLinkSplitFocus::Source {
            if let Some(index) = self.window_manager.window_index(source) {
                self.set_active_window(index);
            }
        }
        result?;
        if let Some((Some(file), index)) = opened {
            self.plugin_registry
                .notify(
                    runtime,
                    "file:opened",
                    serde_json::json!({ "file": file, "buffer_index": index }),
                )
                .await?;
        }
        self.render(screen)?;
        Ok(())
    }

    async fn open_prepared_local_link(
        &mut self,
        buffer_id: BufferId,
        position: Option<(usize, usize)>,
        screen: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        // Select the prepared buffer even for same-document anchors in another pane.
        if self.current_buffer().id() != buffer_id {
            self.execute_with_tracking(
                &Action::OpenBufferById(buffer_id.as_u64()),
                screen,
                runtime,
                false,
            )
            .await?;
        }
        if let Some((column, line)) = position {
            self.execute_with_tracking(&Action::MoveTo(column, line + 1), screen, runtime, false)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config, test_utils::EditorTestExt as _, theme::Theme,
        unicode_utils::char_to_grapheme,
    };
    use crossterm::event::{Event as TerminalEvent, KeyCode, KeyEvent, KeyModifiers};

    fn editor(buffers: Vec<Buffer>) -> Editor {
        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = Editor::with_size(lsp, 80, 25, config, Theme::default(), buffers).unwrap();
        editor.test_disable_terminal_output();
        editor
    }

    fn document(path: &Path, source: &str) -> Buffer {
        Buffer::new(
            Some(path.to_string_lossy().into_owned()),
            source.to_string(),
        )
    }

    fn cursor_at(editor: &mut Editor, needle: &str) {
        let byte = editor.current_buffer().contents().find(needle).unwrap();
        let position = editor.current_buffer().byte_idx_to_position(byte).unwrap();
        let line = editor.current_buffer().get(position.line).unwrap();
        editor.cx = char_to_grapheme(&line, position.character);
        editor.vtop = 0;
        editor.cy = position.line;
        editor.refresh_cursor_goal();
        editor.sync_to_window();
    }

    async fn key(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) {
        editor
            .test_execute_event(TerminalEvent::Key(KeyEvent::new(code, modifiers)))
            .await
            .unwrap();
    }

    async fn follow(editor: &mut Editor) {
        key(editor, KeyCode::Char('g'), KeyModifiers::NONE).await;
        key(editor, KeyCode::Char('x'), KeyModifiers::NONE).await;
    }

    async fn follow_in_split(editor: &mut Editor) {
        key(editor, KeyCode::Char('w'), KeyModifiers::CONTROL).await;
        key(editor, KeyCode::Char('x'), KeyModifiers::NONE).await;
    }

    fn activate(editor: &mut Editor, window: WindowId) {
        editor.set_active_window(editor.window_manager.window_index(window).unwrap());
    }

    #[tokio::test]
    async fn split_reuse_preserves_source_and_records_only_target_history_in_both_focus_modes() {
        for focus in [
            MarkdownLinkSplitFocus::Source,
            MarkdownLinkSplitFocus::Target,
        ] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(directory.path().join("target.rs"), "first\nsecond\nthird\n").unwrap();
            let mut editor = editor(vec![document(
                &directory.path().join("report.md"),
                "[first](target.rs:2)\n[second](target.rs:3)\n",
            )]);
            editor.config.markdown_link_split_focus = focus;
            let source = editor.window_manager.active_stable_window_id().unwrap();
            cursor_at(&mut editor, "first");
            let origin = editor.current_jump_entry();
            editor.push_history_entry(origin.clone());
            follow_in_split(&mut editor).await;
            let target = editor.markdown_link_destinations[&source].window;
            assert_eq!(editor.window_manager.window_count(), 2);
            assert_eq!(
                editor.window_manager.active_stable_window_id(),
                Some(if focus == MarkdownLinkSplitFocus::Source {
                    source
                } else {
                    target
                })
            );
            activate(&mut editor, target);
            assert_eq!(editor.buffer_line(), 1);
            assert!(editor.active_jump_list().entries.is_empty());
            activate(&mut editor, source);
            assert_eq!(editor.current_jump_entry(), origin);
            cursor_at(&mut editor, "second");
            let origin = editor.current_jump_entry();
            follow(&mut editor).await;
            assert_eq!(
                editor.window_manager.active_stable_window_id(),
                Some(if focus == MarkdownLinkSplitFocus::Source {
                    source
                } else {
                    target
                })
            );
            activate(&mut editor, target);
            assert_eq!(editor.buffer_line(), 2);
            assert_eq!(editor.active_jump_list().entries.len(), 1);
            key(&mut editor, KeyCode::Char('o'), KeyModifiers::CONTROL).await;
            assert_eq!(editor.buffer_line(), 1);
            key(&mut editor, KeyCode::Char('i'), KeyModifiers::CONTROL).await;
            assert_eq!(editor.buffer_line(), 2);
            activate(&mut editor, source);
            assert_eq!(editor.current_jump_entry(), origin);
            assert_eq!(editor.active_jump_list().entries.len(), 1);
            follow_in_split(&mut editor).await;
            assert_eq!(editor.window_manager.window_count(), 2);
        }
    }

    #[tokio::test]
    async fn split_handles_same_document_anchors_and_table_links_without_changing_source() {
        let text = "Name | Value\n--- | ---\n[**Read**](#destination) | é\n\n# Destination\n";
        let mut editor = editor(vec![document(Path::new("/tmp/red-split-table.md"), text)]);
        cursor_at(&mut editor, "Read");
        let origin = editor.current_jump_entry();
        follow_in_split(&mut editor).await;
        let target = editor.markdown_link_destination().unwrap();
        assert_eq!(editor.current_jump_entry(), origin);
        assert_eq!(editor.current_buffer().contents(), text);
        assert!(!editor.current_buffer().is_dirty());
        activate(&mut editor, target);
        assert_eq!(editor.current_buffer().id(), origin.buffer_id);
        assert_eq!(editor.buffer_line(), 4);
    }

    #[tokio::test]
    async fn failed_split_links_do_not_change_layout_focus_or_history() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("target.md"), "# Existing\n").unwrap();
        let mut editor = editor(vec![document(
            &directory.path().join("report.md"),
            "[missing](absent.md)\n[heading](target.md#absent)\n[valid](target.md)\n",
        )]);
        for needle in ["missing", "heading"] {
            cursor_at(&mut editor, needle);
            let origin = editor.current_jump_entry();
            follow_in_split(&mut editor).await;
            assert_eq!(editor.current_jump_entry(), origin);
            assert_eq!(editor.window_manager.window_count(), 1);
            assert_eq!(editor.buffer_manager.len(), 1);
            assert!(editor.markdown_link_destinations.is_empty());
            assert!(editor.active_jump_list().entries.is_empty());
        }
        cursor_at(&mut editor, "valid");
        follow_in_split(&mut editor).await;
        let target = editor.markdown_link_destination().unwrap();
        cursor_at(&mut editor, "missing");
        follow(&mut editor).await;
        assert_eq!(editor.markdown_link_destination(), Some(target));
        assert_eq!(editor.window_manager.window_count(), 2);
        assert!(editor
            .window_manager
            .window(target)
            .unwrap()
            .jump_list
            .entries
            .is_empty());
    }

    #[tokio::test]
    async fn destination_edits_and_manual_buffer_changes_keep_route_and_unsaved_contents() {
        let directory = tempfile::tempdir().unwrap();
        let mut editor = editor(vec![
            document(
                &directory.path().join("report.md"),
                "[one](one.rs)\n[two](two.rs)\n",
            ),
            document(&directory.path().join("one.rs"), "one\n"),
            document(&directory.path().join("two.rs"), "two\n"),
        ]);
        let source = editor.window_manager.active_stable_window_id().unwrap();
        cursor_at(&mut editor, "one");
        follow_in_split(&mut editor).await;
        let target = editor.markdown_link_destination().unwrap();
        activate(&mut editor, target);
        editor
            .test_execute_action(Action::InsertCharAtCursorPos('X'))
            .await
            .unwrap();
        let edited_id = editor.current_buffer().id();
        assert!(editor.current_buffer().is_dirty());
        activate(&mut editor, source);
        cursor_at(&mut editor, "two");
        follow(&mut editor).await;
        activate(&mut editor, target);
        key(&mut editor, KeyCode::Char('o'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.current_buffer().id(), edited_id);
        assert_eq!(editor.current_buffer().contents(), "Xone\n");
        editor
            .test_execute_action(Action::OpenBufferById(
                editor.buffer_manager[2].id().as_u64(),
            ))
            .await
            .unwrap();
        activate(&mut editor, source);
        assert_eq!(editor.markdown_link_destination(), Some(target));
        cursor_at(&mut editor, "one");
        follow(&mut editor).await;
        activate(&mut editor, target);
        assert_eq!(editor.current_buffer().contents(), "Xone\n");
    }

    #[tokio::test]
    async fn routes_survive_reordering_but_clear_on_source_change_explicit_clear_and_closure() {
        let mut editor = editor(vec![
            document(Path::new("/tmp/report.md"), "[jump](#heading)\n# Heading\n"),
            document(Path::new("/tmp/other.md"), "other\n"),
        ]);
        let source = editor.window_manager.active_stable_window_id().unwrap();
        let source_buffer = editor.current_buffer().id();
        cursor_at(&mut editor, "jump");
        follow_in_split(&mut editor).await;
        let target = editor.markdown_link_destination().unwrap();
        editor
            .test_execute_action(Action::MoveWindowToRight)
            .await
            .unwrap();
        assert_eq!(editor.markdown_link_destination(), Some(target));
        follow(&mut editor).await;
        editor
            .test_execute_action(Action::ClearLinkDestination)
            .await
            .unwrap();
        assert!(editor.markdown_link_destination().is_none());
        assert_eq!(editor.window_manager.window_count(), 2);
        follow_in_split(&mut editor).await;
        let target = editor.markdown_link_destination().unwrap();
        editor
            .test_execute_action(Action::OpenBufferById(
                editor.buffer_manager[1].id().as_u64(),
            ))
            .await
            .unwrap();
        editor
            .test_execute_action(Action::OpenBufferById(source_buffer.as_u64()))
            .await
            .unwrap();
        assert!(editor.markdown_link_destination().is_none());
        cursor_at(&mut editor, "jump");
        follow_in_split(&mut editor).await;
        let current_target = editor.markdown_link_destination().unwrap();
        assert_ne!(current_target, target);
        activate(&mut editor, current_target);
        editor
            .test_execute_action(Action::CloseWindow)
            .await
            .unwrap();
        activate(&mut editor, source);
        assert!(editor.markdown_link_destination().is_none());
        let count = editor.window_manager.window_count();
        follow(&mut editor).await;
        assert_eq!(editor.window_manager.window_count(), count);
        assert_eq!(editor.buffer_line(), 1);
    }

    #[tokio::test]
    async fn two_panes_of_one_document_keep_independent_destinations() {
        let mut editor = editor(vec![document(
            Path::new("/tmp/report.md"),
            "[one](#one)\n[two](#two)\n# One\n# Two\n",
        )]);
        let first = editor.window_manager.active_stable_window_id().unwrap();
        editor
            .test_execute_action(Action::SplitHorizontal)
            .await
            .unwrap();
        let second = editor.window_manager.active_stable_window_id().unwrap();
        cursor_at(&mut editor, "two");
        follow_in_split(&mut editor).await;
        let second_target = editor.markdown_link_destination().unwrap();
        activate(&mut editor, first);
        cursor_at(&mut editor, "one");
        follow_in_split(&mut editor).await;
        let first_target = editor.markdown_link_destination().unwrap();
        assert_ne!(first_target, second_target);
        assert_eq!(editor.window_manager.window_count(), 4);
        assert_eq!(editor.markdown_link_destinations.len(), 2);
        activate(&mut editor, first_target);
        assert_eq!(editor.buffer_line(), 2);
        activate(&mut editor, second_target);
        assert_eq!(editor.buffer_line(), 3);
        activate(&mut editor, second);
        assert_eq!(editor.markdown_link_destination(), Some(second_target));
    }

    #[tokio::test]
    async fn nested_routes_take_one_hop_and_source_replacement_releases_outgoing_route() {
        let directory = tempfile::tempdir().unwrap();
        let mut editor = editor(vec![
            document(
                &directory.path().join("a.md"),
                "[read](b.md)\n[replace](c.md)\n",
            ),
            document(&directory.path().join("b.md"), "[next](c.md)\n"),
            document(&directory.path().join("c.md"), "# End\n"),
        ]);
        let a = editor.window_manager.active_stable_window_id().unwrap();
        cursor_at(&mut editor, "read");
        follow_in_split(&mut editor).await;
        let b = editor.markdown_link_destination().unwrap();
        activate(&mut editor, b);
        cursor_at(&mut editor, "next");
        follow_in_split(&mut editor).await;
        let c = editor.markdown_link_destination().unwrap();
        activate(&mut editor, a);
        follow(&mut editor).await;
        assert_eq!(editor.window_manager.window_count(), 3);
        assert_eq!(editor.window_manager.window(b).unwrap().buffer_index, 1);
        assert_eq!(editor.markdown_link_destinations[&b].window, c);
        cursor_at(&mut editor, "replace");
        follow(&mut editor).await;
        assert_eq!(editor.window_manager.window(b).unwrap().buffer_index, 2);
        assert!(!editor.markdown_link_destinations.contains_key(&b));
        assert_eq!(editor.markdown_link_destination(), Some(b));
    }

    #[tokio::test]
    async fn restored_layout_keeps_panes_but_forgets_link_routes() {
        let mut editor = editor(vec![document(
            Path::new("/tmp/report.md"),
            "[jump](#heading)\n# Heading\n",
        )]);
        cursor_at(&mut editor, "jump");
        follow_in_split(&mut editor).await;
        let snapshot = editor.window_manager.snapshot();
        let buffers = HashMap::from([(0, 0)]);
        editor.window_manager =
            crate::window::WindowManager::from_snapshot(&snapshot, (80, 25), &buffers).unwrap();
        editor.sync_with_window();
        assert_eq!(editor.window_manager.window_count(), 2);
        assert!(editor.markdown_link_destinations.is_empty());
        follow(&mut editor).await;
        assert_eq!(editor.buffer_line(), 1);
        assert_eq!(editor.window_manager.window_count(), 2);
    }

    #[tokio::test]
    async fn buffer_reindexing_preserves_routes_and_source_closure_removes_them() {
        let mut editor = editor(vec![
            document(Path::new("/tmp/unused.txt"), "unused\n"),
            document(Path::new("/tmp/report.md"), "[jump](#heading)\n# Heading\n"),
        ]);
        let unused = editor.window_manager.active_stable_window_id().unwrap();
        editor
            .test_execute_action(Action::SplitHorizontal)
            .await
            .unwrap();
        editor
            .test_execute_action(Action::OpenBufferById(
                editor.buffer_manager[1].id().as_u64(),
            ))
            .await
            .unwrap();
        let source = editor.window_manager.active_stable_window_id().unwrap();
        cursor_at(&mut editor, "jump");
        follow_in_split(&mut editor).await;
        let target = editor.markdown_link_destination().unwrap();
        activate(&mut editor, unused);
        editor
            .test_execute_action(Action::DeleteBuffer(false))
            .await
            .unwrap();
        activate(&mut editor, source);
        assert_eq!(editor.markdown_link_destination(), Some(target));
        follow(&mut editor).await;
        editor
            .test_execute_action(Action::CloseWindow)
            .await
            .unwrap();
        assert!(editor.markdown_link_destinations.is_empty());
        assert!(editor.window_manager.window(target).is_some());
    }

    #[test]
    fn indexes_full_source_ranges_and_resolved_reference_destinations() {
        let source = "é [**label**](https://example.test/a \"title\")\n\
                      [named][ID] [id][] [ID]\n\
                      <https://example.test> <user@example.test>\n\n\
                      [id]: guide.md#heading\n";
        let index = LinkIndex::parse(source);
        for needle in ["[**", "label", "https://example.test/a", "title\")"] {
            assert_eq!(
                index.destination_at(source.find(needle).unwrap()).unwrap(),
                "https://example.test/a"
            );
        }
        for needle in ["named", "[id][]", "[ID]", "guide.md#heading"] {
            assert_eq!(
                index.destination_at(source.find(needle).unwrap()).unwrap(),
                "guide.md#heading"
            );
        }
        assert_eq!(
            index.destination_at(source.find("user@").unwrap()).unwrap(),
            "mailto:user@example.test"
        );
        assert!(index.destination_at(0).is_err());
        assert!(index.destination_at(source.len()).is_err());
    }

    #[test]
    fn ignores_literal_links_and_reports_undefined_references() {
        let source = "`[code](code.md)`\n\n```md\n[fenced](fenced.md)\n```\n\n\
                      \\[escaped](escaped.md)\n\n[missing][undefined]\n";
        let index = LinkIndex::parse(source);
        for needle in ["code.md", "fenced.md", "escaped.md"] {
            assert!(index.destination_at(source.find(needle).unwrap()).is_err());
        }
        assert!(index
            .destination_at(source.find("missing").unwrap())
            .unwrap_err()
            .to_string()
            .contains("Undefined Markdown reference"));
    }

    #[test]
    fn headings_use_rendered_text_unicode_and_unique_duplicate_slugs() {
        let source = "# **Hello** `code` &amp; World!\n\n\
                      # Hello code & World!\n\n\
                      # hello-code--world-1\n\n\
                      # Hello code & World!\n\n\
                      Café e\u{301} 中文\n---\n\n\
                      ```md\n# Hidden\n```\n";
        let index = LinkIndex::parse(source);
        assert_eq!(index.headings["hello-code--world"], 0);
        assert_eq!(
            index.headings["hello-code--world-1"],
            source.find("# Hello").unwrap()
        );
        assert!(index.headings.contains_key("hello-code--world-1-1"));
        assert!(index.headings.contains_key("hello-code--world-2"));
        assert!(index.headings.contains_key("café-e\u{301}-中文"));
        assert!(!index.headings.contains_key("hidden"));
    }

    #[test]
    fn target_syntax_is_split_before_percent_decoding() {
        assert_eq!(
            Target::parse("docs/caf%C3%A9%20%23notes.md#hello%20world").unwrap(),
            Target::Local {
                path: "docs/café #notes.md".into(),
                position: Position::Heading("hello world".into()),
            }
        );
        for destination in ["src/main.rs:42:3", "src/main.rs#L42C3-L45C8"] {
            assert_eq!(
                Target::parse(destination).unwrap(),
                Target::Local {
                    path: "src/main.rs".into(),
                    position: Position::Source {
                        line: 42,
                        column: 3
                    },
                }
            );
        }
        assert_eq!(
            Target::parse("#L42").unwrap(),
            Target::Local {
                path: String::new(),
                position: Position::Source {
                    line: 42,
                    column: 1
                },
            }
        );
        assert_eq!(
            Target::parse("a%3A42+b.md").unwrap(),
            Target::Local {
                path: "a:42+b.md".into(),
                position: Position::Start
            }
        );
        for destination in [
            "javascript:alert(1)",
            "data:text/html,hello",
            "red://annotation/id",
        ] {
            assert!(Target::parse(destination)
                .unwrap_err()
                .to_string()
                .contains("Unsupported"));
        }
        for destination in ["file%", "file%GG", "file%FF", "file%00"] {
            assert!(Target::parse(destination).is_err());
        }
    }

    #[tokio::test]
    async fn gx_follows_heading_and_control_o_i_restore_exact_source_cursor() {
        let source = "é [jump](#hello-world)\n\nBefore\n\n# Hello **world**\nAfter\n";
        let mut editor = editor(vec![document(Path::new("/tmp/red-follow.md"), source)]);
        cursor_at(&mut editor, "jump");
        let origin = editor.current_jump_entry();
        let revision = editor.current_buffer().revision();
        follow(&mut editor).await;
        assert_eq!(editor.buffer_line(), 4);
        assert_eq!(editor.active_jump_list().entries.len(), 1);
        key(&mut editor, KeyCode::Char('o'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.current_jump_entry(), origin);
        key(&mut editor, KeyCode::Char('i'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.buffer_line(), 4);
        assert_eq!(editor.current_buffer().contents(), source);
        assert_eq!(editor.current_buffer().revision(), revision);
        assert!(!editor.current_buffer().is_dirty());
    }

    #[tokio::test]
    async fn returned_wrapped_link_survives_screen_line_down_and_up() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.rs");
        std::fs::write(&target, "first\nsecond\nthird\n").unwrap();
        let source = format!("{}[read](target.rs:2)\n", "word ".repeat(28));
        let mut editor = editor(vec![document(&directory.path().join("index.md"), &source)]);
        cursor_at(&mut editor, "read");
        let origin = editor.current_jump_entry();
        follow(&mut editor).await;
        assert_eq!(editor.current_buffer().file.as_deref(), target.to_str());
        assert_eq!(editor.buffer_line(), 1);
        key(&mut editor, KeyCode::Char('o'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.current_jump_entry(), origin);

        for character in "gjgk".chars() {
            key(&mut editor, KeyCode::Char(character), KeyModifiers::NONE).await;
        }
        assert_eq!(editor.current_jump_entry(), origin);
        follow(&mut editor).await;
        assert_eq!(editor.current_buffer().file.as_deref(), target.to_str());
        assert_eq!(editor.buffer_line(), 1);
    }

    #[tokio::test]
    async fn follows_document_relative_encoded_file_and_heading() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("café #notes.md");
        std::fs::write(&target, "# First\n\n# Target\nend\n").unwrap();
        let source = "[next](caf%C3%A9%20%23notes.md#target)\n";
        let mut editor = editor(vec![document(&directory.path().join("index.md"), source)]);
        let origin = editor.current_buffer().id();
        cursor_at(&mut editor, "next");
        follow(&mut editor).await;
        assert_eq!(editor.current_buffer().file.as_deref(), target.to_str());
        assert_eq!(editor.buffer_line(), 2);
        assert_eq!(editor.buffer_manager.len(), 2);
        assert_eq!(editor.active_jump_list().entries.len(), 1);
        key(&mut editor, KeyCode::Char('o'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.current_buffer().id(), origin);
        key(&mut editor, KeyCode::Char('i'), KeyModifiers::CONTROL).await;
        assert_eq!(editor.buffer_line(), 2);
        assert_eq!(editor.current_buffer().file.as_deref(), target.to_str());
    }

    #[tokio::test]
    async fn reuses_unsaved_target_and_finds_its_unsaved_heading() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("guide.md");
        let mut editor = editor(vec![
            document(
                &directory.path().join("index.md"),
                "[read](guide.md#unsaved)\n",
            ),
            document(&target, "# Old\n"),
        ]);
        editor.buffer_manager[1].insert_line(1, "# Unsaved".into());
        let target_id = editor.buffer_manager[1].id();
        cursor_at(&mut editor, "read");
        follow(&mut editor).await;
        assert_eq!(editor.current_buffer().id(), target_id);
        assert_eq!(editor.buffer_line(), 1);
        assert_eq!(editor.buffer_manager.len(), 2);
        assert!(editor.current_buffer().is_dirty());
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn failures_leave_current_buffer_cursor_and_history_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("guide.md"), "# Existing\n").unwrap();
        let source = "[missing](missing.md)\n[anchor](guide.md#absent)\n\
                      [directory](.)\n[unsafe](javascript:alert)\n[reference][unknown]\n";
        let mut editor = editor(vec![document(&directory.path().join("index.md"), source)]);
        for (needle, message) in [
            ("missing", "Cannot open linked file"),
            ("anchor", "heading not found"),
            ("directory", "not a file"),
            ("unsafe", "Unsupported link scheme"),
            ("reference", "Undefined Markdown reference"),
        ] {
            cursor_at(&mut editor, needle);
            let origin = editor.current_jump_entry();
            follow(&mut editor).await;
            assert!(editor.last_error.as_deref().unwrap().contains(message));
            assert_eq!(editor.current_jump_entry(), origin);
            assert_eq!(editor.buffer_manager.len(), 1);
            assert!(editor.active_jump_list().entries.is_empty());
        }
        assert!(!directory.path().join("missing.md").exists());
    }

    #[tokio::test]
    async fn follows_table_reference_using_source_coordinates() {
        let source = "Name | Value\n--- | ---\n[**Read**][guide] | text\n\n\
                      [guide]: #destination\n\n# Destination\n";
        let mut editor = editor(vec![document(Path::new("/tmp/red-follow.md"), source)]);
        cursor_at(&mut editor, "Read");
        let mut screen = RenderBuffer::new(80, 25, &crate::theme::Style::default());
        editor.render(&mut screen).unwrap();
        let window = editor.active_window_with_editor_view().unwrap();
        assert!(editor
            .layout_for_window(&window)
            .rows
            .iter()
            .any(|row| row.line == 2 && row.projection.is_some()));
        follow(&mut editor).await;
        assert_eq!(editor.buffer_line(), 6);
        assert_eq!(editor.current_buffer().contents(), source);
    }

    #[tokio::test]
    async fn source_locations_clamp_to_unicode_grapheme_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("code.rs"), "first\n😀e\u{301} end\n").unwrap();
        let mut editor = editor(vec![document(
            &directory.path().join("index.md"),
            "[emoji](code.rs:2:3) [combining](code.rs#L2C7) [end](code.rs:999:999)\n",
        )]);
        for (needle, column) in [("emoji", 0), ("combining", 1), ("end", 5)] {
            cursor_at(&mut editor, needle);
            follow(&mut editor).await;
            assert_eq!(editor.cx, column);
            assert_eq!(editor.buffer_line(), 1);
            key(&mut editor, KeyCode::Char('o'), KeyModifiers::CONTROL).await;
        }
    }

    #[test]
    fn external_links_prepare_existing_opener_actions_without_a_local_jump() {
        let source = "[web](HTTPS://example.test/a#b) <user@example.test>\n";
        let mut editor = editor(vec![document(Path::new("/tmp/red-follow.md"), source)]);
        for (needle, url) in [
            ("web", "HTTPS://example.test/a#b"),
            ("user@", "mailto:user@example.test"),
        ] {
            cursor_at(&mut editor, needle);
            let prepared = editor.prepare_markdown_link().unwrap();
            assert!(
                matches!(prepared.destination, PreparedDestination::External(actual) if actual == url)
            );
            assert!(prepared.new_buffer.is_none());
            assert!(editor.active_jump_list().entries.is_empty());
        }
    }

    #[test]
    fn cached_index_refreshes_after_unsaved_edits_and_has_a_size_bound() {
        let mut editor = editor(vec![document(
            Path::new("/tmp/red-follow.md"),
            "[old](#old)\n# Old\n",
        )]);
        let before = editor.markdown_link_index(editor.current_buffer()).unwrap();
        editor
            .current_buffer_mut()
            .insert_line(0, "[new](#new)".into());
        editor.current_buffer_mut().insert_line(3, "# New".into());
        let after = editor.markdown_link_index(editor.current_buffer()).unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(after.destination_at(2).unwrap(), "#new");
        assert!(after.headings.contains_key("new"));
        assert!(Arc::ptr_eq(
            &after,
            &editor.markdown_link_index(editor.current_buffer()).unwrap()
        ));
        let huge = Buffer::new(None, "x".repeat(MAX_DOCUMENT_BYTES + 1));
        assert!(editor.markdown_link_index(&huge).is_err());
    }
}
