//! Inline images for standalone Markdown image lines.
//!
//! A line whose only content is `![alt](target)` becomes an image block in
//! Normal mode. Insert mode, Visual mode, and a cursor on that line show the
//! raw source, matching Markdown concealment. The block reserves vertical rows
//! from the image's aspect ratio, capped by `markdown_image_max_height`.
//!
//! Protocol support is detected here and encoded by hand. `ratatui-image`
//! paints through ratatui widgets, and `viuer` writes straight to stdout with
//! its own cursor and detection. Neither can share Red's diffed cell grid,
//! placement ids, or the conceal/reveal layout, so the encoders live next to
//! the renderer instead of pulling in a second terminal stack.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::Cursor,
    path::{Path, PathBuf},
    sync::{mpsc, Arc},
    time::{Duration, SystemTime},
};

use base64::Engine as _;
use image::ImageEncoder as _;

use super::display_layout::{image_line_segments, LineSegment};
use crate::{
    buffer::BufferId,
    color::Color,
    config::MarkdownImagesMode,
    unicode_utils::{display_width, truncate_display_width},
};

const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGE_EDGE: u32 = 8_192;
pub(super) const KITTY_GRAPHICS_QUERY: &str = "\u{1b}_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\u{1b}\\";
pub(super) const DA1_QUERY: &str = "\u{1b}[c";
const PROBE_TIMEOUT: Duration = Duration::from_millis(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GraphicsProtocol {
    Kitty,
    Iterm2,
    Sixel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImagePresentation {
    Graphics(GraphicsProtocol),
    Halfblocks,
    Placeholder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalHints {
    pub term: String,
    pub term_program: String,
    pub lc_terminal: String,
    pub colorterm: String,
    pub kitty: bool,
    pub wezterm: bool,
    pub ghostty: bool,
    pub konsole: bool,
    pub iterm: bool,
    pub vscode: bool,
    pub sixel_term: bool,
    pub tmux: bool,
    pub truecolor: bool,
}

impl TerminalHints {
    pub(super) fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let term = var("TERM").unwrap_or_default();
        let term_program = var("TERM_PROGRAM").unwrap_or_default();
        let lc_terminal = var("LC_TERMINAL").unwrap_or_default();
        let colorterm = var("COLORTERM").unwrap_or_default();
        let kitty = var("KITTY_WINDOW_ID").is_some()
            || term == "xterm-kitty"
            || equals_ignore_ascii(&term_program, "kitty");
        let wezterm =
            var("WEZTERM_EXECUTABLE").is_some() || equals_ignore_ascii(&term_program, "wezterm");
        let ghostty =
            var("GHOSTTY_RESOURCES_DIR").is_some() || equals_ignore_ascii(&term_program, "ghostty");
        let konsole = var("KONSOLE_VERSION").is_some();
        let iterm = equals_ignore_ascii(&term_program, "iterm.app")
            || equals_ignore_ascii(&term_program, "iterm2")
            || equals_ignore_ascii(&lc_terminal, "iterm2");
        let vscode = equals_ignore_ascii(&term_program, "vscode");
        let sixel_term = term == "foot"
            || term.starts_with("foot-")
            || term == "mlterm"
            || term.starts_with("mlterm")
            || var("WT_SESSION").is_some()
            || var("WT_PROFILE_ID").is_some();
        let tmux = var("TMUX").is_some();
        let known_truecolor =
            kitty || wezterm || ghostty || konsole || iterm || vscode || sixel_term;
        let truecolor = known_truecolor
            || equals_ignore_ascii(&colorterm, "truecolor")
            || equals_ignore_ascii(&colorterm, "24bit")
            || term.contains("truecolor");
        Self {
            term,
            term_program,
            lc_terminal,
            colorterm,
            kitty,
            wezterm,
            ghostty,
            konsole,
            iterm,
            vscode,
            sixel_term,
            tmux,
            truecolor,
        }
    }

    fn prefer_kitty(&self) -> bool {
        self.kitty || self.wezterm || self.ghostty || self.konsole
    }

    fn prefer_iterm(&self) -> bool {
        self.iterm || self.vscode
    }

    fn prefer_sixel(&self) -> bool {
        self.sixel_term
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Probe {
    pub kitty: bool,
    pub sixel: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ImageRef {
    pub alt: String,
    pub target: String,
    source: ImageSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ImageSource {
    Local(PathBuf),
    Remote(String),
}

#[derive(Debug, Clone)]
pub(super) struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone)]
struct Paint {
    alt: String,
    target: String,
    note: Option<String>,
    image: Option<Arc<DecodedImage>>,
}

#[derive(Clone)]
struct LocalSlot {
    modified: Option<SystemTime>,
    len: u64,
    result: Result<Arc<DecodedImage>, String>,
}

enum RemoteSlot {
    Loading,
    Ready(Arc<DecodedImage>),
    Failed(String),
}

struct ParsedBuffer {
    revision: u64,
    lines: BTreeMap<usize, ImageRef>,
}

#[derive(Clone, Copy)]
struct Placement {
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    signature: u64,
    protocol: GraphicsProtocol,
}

struct PendingDraw {
    id: u32,
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    signature: u64,
    protocol: GraphicsProtocol,
    payload: String,
}

struct StagedImage {
    id: u32,
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    signature: u64,
    protocol: GraphicsProtocol,
    payload: String,
    /// Cell fingerprint taken immediately after the image was painted, before overlays.
    cells: u64,
}

/// Rendered image rows, plus image lines that must stay raw source.
pub(super) struct ImageLayout {
    pub rows: BTreeMap<usize, Vec<LineSegment>>,
    pub raw_lines: BTreeSet<usize>,
}

impl ImageLayout {
    pub(super) fn rendered(&self, line: usize) -> Option<&[LineSegment]> {
        self.rows.get(&line).map(Vec::as_slice)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.raw_lines.is_empty()
    }
}

pub(super) struct ImageRuntime {
    epoch: u64,
    hints: TerminalHints,
    probe: Probe,
    parsed: HashMap<BufferId, ParsedBuffer>,
    local: HashMap<PathBuf, LocalSlot>,
    remote: HashMap<String, RemoteSlot>,
    inflight: HashSet<String>,
    tx: mpsc::Sender<(String, Result<Vec<u8>, String>)>,
    rx: mpsc::Receiver<(String, Result<Vec<u8>, String>)>,
    paints: HashMap<(BufferId, usize), Paint>,
    staged: Vec<StagedImage>,
    pending: Vec<PendingDraw>,
    placed: HashMap<u32, Placement>,
    visible_ids: Vec<u32>,
    ids: HashMap<(BufferId, usize), u32>,
    next_id: u32,
    frame_open: bool,
}

impl ImageRuntime {
    pub(super) fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            epoch: 0,
            hints: TerminalHints::from_env(|key| std::env::var(key).ok()),
            probe: Probe::default(),
            parsed: HashMap::new(),
            local: HashMap::new(),
            remote: HashMap::new(),
            inflight: HashSet::new(),
            tx,
            rx,
            paints: HashMap::new(),
            staged: Vec::new(),
            pending: Vec::new(),
            placed: HashMap::new(),
            visible_ids: Vec::new(),
            ids: HashMap::new(),
            next_id: 0,
            frame_open: false,
        }
    }

    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(super) fn note_probe(&mut self, probe: Probe) {
        if probe != self.probe {
            self.probe = probe;
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    pub(super) fn presentation(&self, mode: MarkdownImagesMode) -> ImagePresentation {
        choose_presentation(mode, &self.hints, self.probe)
    }

    /// Returns true when a remote image finished and the screen should redraw.
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((url, result)) = self.rx.try_recv() {
            self.inflight.remove(&url);
            self.remote.insert(url, remote_slot(result));
            changed = true;
        }
        if changed {
            self.epoch = self.epoch.wrapping_add(1);
        }
        changed
    }

    pub(super) fn sync_document(
        &mut self,
        id: BufferId,
        revision: u64,
        contents: &str,
        base: &Path,
    ) {
        if contents.len() > MAX_DOCUMENT_BYTES {
            if self.parsed.remove(&id).is_some() {
                self.epoch = self.epoch.wrapping_add(1);
            }
            return;
        }
        let stale = self
            .parsed
            .get(&id)
            .is_none_or(|parsed| parsed.revision != revision);
        if stale {
            let lines = parse_document(contents, base);
            self.parsed.insert(id, ParsedBuffer { revision, lines });
            self.epoch = self.epoch.wrapping_add(1);
        }
        let Some(parsed) = self.parsed.get(&id).map(|parsed| parsed.lines.clone()) else {
            return;
        };
        let locals: Vec<PathBuf> = parsed
            .values()
            .filter_map(|image| match &image.source {
                ImageSource::Local(path) => Some(path.clone()),
                ImageSource::Remote(_) => None,
            })
            .collect();
        let remotes: Vec<String> = parsed
            .values()
            .filter_map(|image| match &image.source {
                ImageSource::Remote(url) => Some(url.clone()),
                ImageSource::Local(_) => None,
            })
            .collect();
        for path in locals {
            self.ensure_local(&path);
        }
        for url in remotes {
            self.ensure_remote(url);
        }
    }

    pub(super) fn layout_for(
        &mut self,
        id: BufferId,
        width: usize,
        max_rows: usize,
        reveal: Option<&std::ops::Range<usize>>,
        mode: MarkdownImagesMode,
    ) -> ImageLayout {
        let Some(lines) = self.parsed.get(&id).map(|parsed| parsed.lines.clone()) else {
            return ImageLayout {
                rows: BTreeMap::new(),
                raw_lines: BTreeSet::new(),
            };
        };
        self.paints.retain(|(buffer, _), _| *buffer != id);
        let presentation = self.presentation(mode);
        let mut rows = BTreeMap::new();
        let mut raw_lines = BTreeSet::new();
        for (line, image) in lines {
            let revealed = reveal.is_some_and(|range| range.contains(&line));
            if revealed {
                raw_lines.insert(line);
            }
            let (pixels, note) = self.pixels_for(&image.source);
            let paint = Paint {
                alt: image.alt.clone(),
                target: image.target.clone(),
                note: note.clone(),
                image: pixels.clone(),
            };
            self.paints.insert((id, line), paint);
            if revealed {
                continue;
            }
            let (cols, image_rows, placeholder) = match (&pixels, presentation) {
                (Some(image), ImagePresentation::Halfblocks | ImagePresentation::Graphics(_)) => {
                    let (cols, image_rows) =
                        image_cell_grid(image.width, image.height, width, max_rows);
                    if cols == 0 || image_rows == 0 {
                        (1, 1, true)
                    } else {
                        (cols, image_rows, false)
                    }
                }
                _ => (1, 1, true),
            };
            rows.insert(
                line,
                image_line_segments(line, image_rows, cols, placeholder),
            );
        }
        ImageLayout { rows, raw_lines }
    }

    pub(super) fn begin_frame(&mut self) {
        self.staged.clear();
        self.pending.clear();
        self.frame_open = true;
    }

    fn stage_graphics(&mut self, image: StagedImage) {
        self.staged.push(image);
    }

    /// Queues graphics that no overlay has covered. Returns whether that set changed.
    pub(super) fn commit_visible(&mut self, buffer: &crate::editor::RenderBuffer) -> bool {
        if !self.frame_open {
            return false;
        }
        let staged = std::mem::take(&mut self.staged);
        let mut visible = Vec::new();
        for image in staged {
            let rect = crate::ui::ScreenRect {
                x: usize::from(image.x),
                y: usize::from(image.y),
                width: usize::from(image.cols),
                height: usize::from(image.rows),
            };
            let covered = hash_region(buffer, rect) != image.cells
                || image_hidden_by_overlays(rect, &buffer.overlay_rects);
            if covered {
                continue;
            }
            visible.push(image.id);
            self.pending.push(PendingDraw {
                id: image.id,
                x: image.x,
                y: image.y,
                cols: image.cols,
                rows: image.rows,
                signature: image.signature,
                protocol: image.protocol,
                payload: image.payload,
            });
        }
        visible.sort_unstable();
        let changed = visible != self.visible_ids;
        self.visible_ids = visible;
        changed
    }

    pub(super) fn graphics_payload(&mut self, force: bool) -> String {
        if !self.frame_open {
            return String::new();
        }
        self.frame_open = false;
        let pending = std::mem::take(&mut self.pending);
        if force {
            self.placed.clear();
        }
        let mut out = String::new();
        if force
            && pending
                .iter()
                .any(|draw| draw.protocol == GraphicsProtocol::Kitty)
        {
            out.push_str(&wrap_protocol(self.hints.tmux, &kitty_delete_all()));
        }
        let mut next = HashMap::new();
        for draw in &pending {
            let unchanged = self.placed.get(&draw.id).is_some_and(|placed| {
                placed.x == draw.x
                    && placed.y == draw.y
                    && placed.cols == draw.cols
                    && placed.rows == draw.rows
                    && placed.signature == draw.signature
                    && placed.protocol == draw.protocol
            });
            if unchanged && !force {
                next.insert(draw.id, self.placed[&draw.id]);
                continue;
            }
            if draw.protocol == GraphicsProtocol::Kitty && self.placed.contains_key(&draw.id) {
                out.push_str(&wrap_protocol(self.hints.tmux, &kitty_delete(draw.id)));
            }
            out.push_str(&format!("\u{1b}[{};{}H", draw.y + 1, draw.x + 1));
            out.push_str(&wrap_protocol(self.hints.tmux, &draw.payload));
            next.insert(
                draw.id,
                Placement {
                    x: draw.x,
                    y: draw.y,
                    cols: draw.cols,
                    rows: draw.rows,
                    signature: draw.signature,
                    protocol: draw.protocol,
                },
            );
        }
        for (id, placed) in &self.placed {
            if next.contains_key(id) || placed.protocol != GraphicsProtocol::Kitty {
                continue;
            }
            out.push_str(&wrap_protocol(self.hints.tmux, &kitty_delete(*id)));
        }
        self.placed = next;
        out
    }

    pub(super) fn shutdown_sequence(&self) -> String {
        if self
            .placed
            .values()
            .any(|placed| placed.protocol == GraphicsProtocol::Kitty)
            || self.hints.prefer_kitty()
            || self.probe.kitty
        {
            wrap_protocol(self.hints.tmux, &kitty_delete_all())
        } else {
            String::new()
        }
    }

    fn ensure_local(&mut self, path: &Path) {
        let meta = std::fs::metadata(path);
        let (modified, len) = match &meta {
            Ok(meta) => (meta.modified().ok(), meta.len()),
            Err(_) => (None, 0),
        };
        if let Some(slot) = self.local.get(path) {
            if slot.modified == modified && slot.len == len && meta.is_ok() {
                return;
            }
            if meta.is_err() && slot.result.is_err() && slot.len == 0 {
                return;
            }
        }
        let result = match meta {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err("not found".to_string())
            }
            Err(_) => Err("unreadable".to_string()),
            Ok(meta) if !meta.is_file() => Err("not a file".to_string()),
            Ok(meta) if meta.len() > MAX_IMAGE_BYTES as u64 => Err("too large".to_string()),
            Ok(_) => std::fs::read(path)
                .map_err(|_| "unreadable".to_string())
                .and_then(|bytes| decode_image_bytes(&bytes).map(Arc::new)),
        };
        self.local.insert(
            path.to_path_buf(),
            LocalSlot {
                modified,
                len,
                result,
            },
        );
        self.epoch = self.epoch.wrapping_add(1);
    }

    fn ensure_remote(&mut self, url: String) {
        if self.remote.contains_key(&url) || self.inflight.contains(&url) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.remote
                .insert(url, RemoteSlot::Failed("download unavailable".to_string()));
            return;
        };
        self.inflight.insert(url.clone());
        self.remote.insert(url.clone(), RemoteSlot::Loading);
        let tx = self.tx.clone();
        handle.spawn(async move {
            let result = fetch_remote(&url).await;
            let _ = tx.send((url, result));
        });
    }

    fn pixels_for(&self, source: &ImageSource) -> (Option<Arc<DecodedImage>>, Option<String>) {
        match source {
            ImageSource::Local(path) => match self.local.get(path) {
                Some(LocalSlot {
                    result: Ok(image), ..
                }) => (Some(image.clone()), None),
                Some(LocalSlot {
                    result: Err(note), ..
                }) => (None, Some(note.clone())),
                None => (None, Some("unreadable".to_string())),
            },
            ImageSource::Remote(url) => match self.remote.get(url) {
                Some(RemoteSlot::Ready(image)) => (Some(image.clone()), None),
                Some(RemoteSlot::Failed(note)) => (None, Some(note.clone())),
                Some(RemoteSlot::Loading) | None => (None, Some("loading".to_string())),
            },
        }
    }

    fn id_for(&mut self, buffer: BufferId, line: usize) -> u32 {
        if let Some(id) = self.ids.get(&(buffer, line)) {
            return *id;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.ids.insert((buffer, line), self.next_id);
        self.next_id
    }
}

fn remote_slot(result: Result<Vec<u8>, String>) -> RemoteSlot {
    match result.and_then(|bytes| decode_image_bytes(&bytes).map(Arc::new)) {
        Ok(image) => RemoteSlot::Ready(image),
        Err(note) => RemoteSlot::Failed(note),
    }
}

async fn fetch_remote(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "download unavailable".to_string())?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "download failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!("http {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "download failed".to_string())?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("too large".to_string());
    }
    Ok(bytes.to_vec())
}

pub(super) fn choose_presentation(
    mode: MarkdownImagesMode,
    hints: &TerminalHints,
    probe: Probe,
) -> ImagePresentation {
    match mode {
        MarkdownImagesMode::Off => ImagePresentation::Placeholder,
        MarkdownImagesMode::Halfblocks => ImagePresentation::Halfblocks,
        MarkdownImagesMode::Kitty => ImagePresentation::Graphics(GraphicsProtocol::Kitty),
        MarkdownImagesMode::Iterm2 => ImagePresentation::Graphics(GraphicsProtocol::Iterm2),
        MarkdownImagesMode::Sixel => ImagePresentation::Graphics(GraphicsProtocol::Sixel),
        MarkdownImagesMode::Auto => {
            if let Some(protocol) = detected_graphics(hints, probe) {
                ImagePresentation::Graphics(protocol)
            } else if hints.truecolor || probe.kitty {
                ImagePresentation::Halfblocks
            } else {
                ImagePresentation::Placeholder
            }
        }
    }
}

/// Kitty, then iTerm2, then Sixel. Inside tmux, environment hints are ignored
/// unless a wrapped query was answered, because passthrough may be disabled.
pub(super) fn detected_graphics(hints: &TerminalHints, probe: Probe) -> Option<GraphicsProtocol> {
    if probe.kitty {
        return Some(GraphicsProtocol::Kitty);
    }
    let env_ok = !hints.tmux || probe.sixel;
    if env_ok && hints.prefer_kitty() {
        return Some(GraphicsProtocol::Kitty);
    }
    if env_ok && hints.prefer_iterm() {
        return Some(GraphicsProtocol::Iterm2);
    }
    if probe.sixel || (env_ok && hints.prefer_sixel()) {
        return Some(GraphicsProtocol::Sixel);
    }
    None
}

pub(super) fn parse_probe_response(bytes: &[u8]) -> Probe {
    let text = String::from_utf8_lossy(bytes);
    Probe {
        kitty: text.contains("i=31;OK") || text.contains("_Gi=31;OK"),
        sixel: da1_has_sixel(bytes),
    }
}

pub(super) fn da1_has_sixel(bytes: &[u8]) -> bool {
    let mut index = 0;
    while index + 2 < bytes.len() {
        let start = if bytes[index] == 0x1b && bytes[index + 1] == b'[' {
            index + 2
        } else {
            index += 1;
            continue;
        };
        if bytes.get(start) != Some(&b'?') {
            index += 1;
            continue;
        }
        let mut cursor = start + 1;
        let mut number = None;
        let mut saw_four = false;
        let mut valid = true;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'0'..=b'9' => {
                    let digit = u16::from(bytes[cursor] - b'0');
                    number = Some(
                        number
                            .unwrap_or(0u16)
                            .saturating_mul(10)
                            .saturating_add(digit),
                    );
                }
                b';' => {
                    saw_four |= number == Some(4);
                    number = None;
                }
                b'c' => {
                    saw_four |= number == Some(4);
                    return saw_four || da1_has_sixel(&bytes[cursor + 1..]);
                }
                _ => {
                    valid = false;
                    break;
                }
            }
            cursor += 1;
        }
        if !valid {
            index += 1;
            continue;
        }
        break;
    }
    false
}

pub(super) fn tmux_passthrough(sequence: &str) -> String {
    let mut out = String::from("\u{1b}Ptmux;\u{1b}");
    for character in sequence.chars() {
        if character == '\u{1b}' {
            out.push('\u{1b}');
        }
        out.push(character);
    }
    out.push_str("\u{1b}\\");
    out
}

pub(super) fn probe_query(tmux: bool) -> String {
    if tmux {
        format!(
            "{}{}",
            tmux_passthrough(KITTY_GRAPHICS_QUERY),
            tmux_passthrough(DA1_QUERY)
        )
    } else {
        format!("{KITTY_GRAPHICS_QUERY}{DA1_QUERY}")
    }
}

fn wrap_protocol(tmux: bool, sequence: &str) -> String {
    if tmux {
        tmux_passthrough(sequence)
    } else {
        sequence.to_string()
    }
}

pub(super) fn probe_terminal() -> Probe {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Probe::default();
    }
    let query = probe_query(std::env::var_os("TMUX").is_some());
    let mut output = std::io::stdout();
    if std::io::Write::write_all(&mut output, query.as_bytes()).is_err()
        || std::io::Write::flush(&mut output).is_err()
    {
        return Probe::default();
    }
    parse_probe_response(&read_probe_response(PROBE_TIMEOUT))
}

#[cfg(unix)]
fn read_probe_response(timeout: Duration) -> Vec<u8> {
    let mut buf = Vec::new();
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        let mut pollfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = i32::try_from(remain.as_millis()).unwrap_or(i32::MAX).max(1);
        let ready = unsafe { libc::poll(&mut pollfd, 1, millis) };
        if ready <= 0 {
            break;
        }
        let mut tmp = [0u8; 2048];
        let read = unsafe { libc::read(libc::STDIN_FILENO, tmp.as_mut_ptr().cast(), tmp.len()) };
        if read <= 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..read as usize]);
        if buf.len() > 8_192 {
            break;
        }
    }
    buf
}

#[cfg(not(unix))]
fn read_probe_response(_timeout: Duration) -> Vec<u8> {
    Vec::new()
}

pub(super) fn parse_image_line(line: &str) -> Option<ImageRef> {
    let line = line.trim();
    let rest = line.strip_prefix("![")?;
    let (alt, rest) = split_bracket(rest)?;
    let rest = rest.strip_prefix("](")?;
    let (target, rest) = parse_destination(rest)?;
    if target.is_empty() || !rest.trim().is_empty() {
        return None;
    }
    let source = classify_target(&target)?;
    Some(ImageRef {
        alt,
        target,
        source,
    })
}

fn split_bracket(input: &str) -> Option<(String, &str)> {
    let end = input.find(']')?;
    Some((input[..end].to_string(), &input[end..]))
}

fn parse_destination(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    let (target, rest) = if let Some(rest) = input.strip_prefix('<') {
        let end = rest.find('>')?;
        (percent_decode(&rest[..end]), &rest[end + 1..])
    } else {
        let end = input.find([' ', '\t', ')'])?;
        (input[..end].to_string(), &input[end..])
    };
    let rest = rest.trim_start();
    let rest = if let Some(quoted) = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\'')) {
        let quote = rest.as_bytes()[0] as char;
        let end = quoted.find(quote)?;
        quoted[end + 1..].trim_start()
    } else {
        rest
    };
    let rest = rest.strip_prefix(')')?;
    Some((target, rest))
}

fn classify_target(target: &str) -> Option<ImageSource> {
    let lower = target.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Some(ImageSource::Remote(target.to_string()))
    } else if let Some(path) = target.strip_prefix("file://") {
        Some(ImageSource::Local(PathBuf::from(percent_decode(path))))
    } else if lower.starts_with("data:") {
        None
    } else {
        Some(ImageSource::Local(PathBuf::from(target)))
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push((hi << 4) | lo);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_document(contents: &str, base: &Path) -> BTreeMap<usize, ImageRef> {
    let mut images = BTreeMap::new();
    let mut fenced = false;
    for (index, line) in contents.split_inclusive('\n').enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let Some(mut image) = parse_image_line(line) else {
            continue;
        };
        if let ImageSource::Local(path) = &image.source {
            if !path.is_absolute() {
                image.source = ImageSource::Local(base.join(path));
            }
        }
        images.insert(index, image);
    }
    images
}

/// Cell grid that preserves aspect ratio.
///
/// Terminal cells are about twice as tall as they are wide, and each
/// half-block row carries two pixels. `rows` is capped by `max_rows`.
pub(super) fn image_cell_grid(
    pixel_w: u32,
    pixel_h: u32,
    max_cols: usize,
    max_rows: usize,
) -> (u16, u16) {
    if pixel_w == 0 || pixel_h == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let max_cols = max_cols.min(usize::from(u16::MAX));
    let max_rows = max_rows.min(64);
    let mut cols = max_cols.min(pixel_w as usize).max(1);
    let mut rows = ((u64::from(pixel_h) * cols as u64) / (u64::from(pixel_w) * 2)).max(1) as usize;
    if rows > max_rows {
        rows = max_rows;
        cols = ((rows as u64 * 2 * u64::from(pixel_w)) / u64::from(pixel_h)).max(1) as usize;
        cols = cols.min(max_cols).max(1);
    }
    (cols as u16, rows.min(max_rows).max(1) as u16)
}

pub(super) fn placeholder_text(
    alt: &str,
    target: &str,
    note: Option<&str>,
    width: usize,
) -> String {
    let text = if let Some(note) = note.filter(|note| !note.is_empty()) {
        format!("[image: {alt} ({target}) - {note}]")
    } else {
        format!("[image: {alt} ({target})]")
    };
    if width == 0 {
        return String::new();
    }
    if display_width(&text) <= width {
        text
    } else if width == 1 {
        truncate_display_width(&text, width)
    } else {
        format!("{}…", truncate_display_width(&text, width - 1))
    }
}

pub(super) fn halfblock_colors(
    image: &DecodedImage,
    cols: u32,
    rows: u32,
    col: u32,
    row: u32,
    background: [u8; 3],
) -> (Color, Color) {
    let top = sample(
        image,
        cols,
        rows.saturating_mul(2),
        col,
        row.saturating_mul(2),
        background,
    );
    let bottom = sample(
        image,
        cols,
        rows.saturating_mul(2),
        col,
        row.saturating_mul(2).saturating_add(1),
        background,
    );
    (
        Color::Rgb {
            r: top[0],
            g: top[1],
            b: top[2],
        },
        Color::Rgb {
            r: bottom[0],
            g: bottom[1],
            b: bottom[2],
        },
    )
}

fn sample(
    image: &DecodedImage,
    dest_w: u32,
    dest_h: u32,
    x: u32,
    y: u32,
    background: [u8; 3],
) -> [u8; 3] {
    if image.width == 0 || image.height == 0 || dest_w == 0 || dest_h == 0 {
        return background;
    }
    let sx = (u64::from(x) * u64::from(image.width) / u64::from(dest_w))
        .min(u64::from(image.width - 1)) as u32;
    let sy = (u64::from(y) * u64::from(image.height) / u64::from(dest_h))
        .min(u64::from(image.height - 1)) as u32;
    let index = ((sy * image.width + sx) * 4) as usize;
    let pixel = image.rgba.get(index..index + 4).unwrap_or(&[0, 0, 0, 0]);
    composite(pixel, background)
}

fn composite(pixel: &[u8], background: [u8; 3]) -> [u8; 3] {
    let alpha = u16::from(pixel[3]);
    if alpha == 255 {
        return [pixel[0], pixel[1], pixel[2]];
    }
    if alpha == 0 {
        return background;
    }
    let inverse = 255 - alpha;
    [
        ((u16::from(pixel[0]) * alpha + u16::from(background[0]) * inverse) / 255) as u8,
        ((u16::from(pixel[1]) * alpha + u16::from(background[1]) * inverse) / 255) as u8,
        ((u16::from(pixel[2]) * alpha + u16::from(background[2]) * inverse) / 255) as u8,
    ]
}

pub(super) fn decode_image_bytes(bytes: &[u8]) -> Result<DecodedImage, String> {
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("too large".to_string());
    }
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "unsupported image".to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_EDGE);
    limits.max_image_height = Some(MAX_IMAGE_EDGE);
    limits.max_alloc = Some(32 * 1024 * 1024);
    let mut reader = reader;
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| "unsupported image".to_string())?;
    let rgba = decoded.to_rgba8();
    if rgba.width() == 0 || rgba.height() == 0 {
        return Err("unsupported image".to_string());
    }
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// True when `image` shares any cell with a floating surface.
pub(super) fn image_hidden_by_overlays(
    image: crate::ui::ScreenRect,
    overlays: &[crate::ui::ScreenRect],
) -> bool {
    overlays
        .iter()
        .any(|overlay| rects_overlap(image, *overlay))
}

fn rects_overlap(left: crate::ui::ScreenRect, right: crate::ui::ScreenRect) -> bool {
    left.width > 0
        && left.height > 0
        && right.width > 0
        && right.height > 0
        && left.x < right.x.saturating_add(right.width)
        && right.x < left.x.saturating_add(left.width)
        && left.y < right.y.saturating_add(right.height)
        && right.y < left.y.saturating_add(left.height)
}

fn hash_region(buffer: &crate::editor::RenderBuffer, rect: crate::ui::ScreenRect) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            let cell = buffer
                .cells
                .get(y.saturating_mul(buffer.width).saturating_add(x));
            match cell {
                Some(cell) => {
                    cell.text.hash(&mut hasher);
                    hash_color(cell.style.fg, &mut hasher);
                    hash_color(cell.style.bg, &mut hasher);
                }
                None => 0u8.hash(&mut hasher),
            }
        }
    }
    hasher.finish()
}

fn hash_color(color: Option<Color>, hasher: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match color {
        Some(Color::Rgb { r, g, b } | Color::Rgba { r, g, b, .. }) => (r, g, b).hash(hasher),
        None => 0u8.hash(hasher),
    }
}

pub(super) fn kitty_transmit(id: u32, image: &DecodedImage, cols: u16, rows: u16) -> String {
    let (width, height, rgba) = scale_rgba(
        image,
        u32::from(cols).saturating_mul(4).max(1),
        u32::from(rows).saturating_mul(8).max(1),
    );
    let payload = base64::engine::general_purpose::STANDARD.encode(&rgba);
    chunk_kitty(
        &format!("a=T,f=32,s={width},v={height},i={id},c={cols},r={rows},q=2,t=d,z=-1"),
        &payload,
    )
}

fn chunk_kitty(control: &str, payload: &str) -> String {
    const CHUNK: usize = 4096;
    if payload.len() <= CHUNK {
        return format!("\u{1b}_G{control};{payload}\u{1b}\\");
    }
    let mut out = String::new();
    let mut offset = 0;
    let mut first = true;
    while offset < payload.len() {
        let end = (offset + CHUNK).min(payload.len());
        let more = if end == payload.len() { 0 } else { 1 };
        if first {
            out.push_str(&format!("\u{1b}_G{control},m={more};"));
            first = false;
        } else {
            out.push_str(&format!("\u{1b}_Gm={more};"));
        }
        out.push_str(&payload[offset..end]);
        out.push_str("\u{1b}\\");
        offset = end;
    }
    out
}

pub(super) fn kitty_delete(id: u32) -> String {
    format!("\u{1b}_Ga=d,d=i,i={id},q=2\u{1b}\\")
}

pub(super) fn kitty_delete_all() -> String {
    "\u{1b}_Ga=d,d=A,q=2\u{1b}\\".to_string()
}

pub(super) fn iterm_transmit(image: &DecodedImage, cols: u16, rows: u16) -> String {
    let (width, height, rgba) = scale_rgba(
        image,
        u32::from(cols).saturating_mul(4).max(1),
        u32::from(rows).saturating_mul(8).max(1),
    );
    let mut png = Vec::new();
    let encoded = image::codecs::png::PngEncoder::new(&mut png).write_image(
        &rgba,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    );
    if encoded.is_err() {
        return String::new();
    }
    let payload = base64::engine::general_purpose::STANDARD.encode(&png);
    format!(
        "\u{1b}]1337;File=inline=1;width={cols};height={rows};preserveAspectRatio=1:{payload}\u{7}"
    )
}

pub(super) fn sixel_transmit(image: &DecodedImage, cols: u16, rows: u16) -> String {
    let (width, height, rgba) = scale_rgba(
        image,
        u32::from(cols).saturating_mul(2).max(1),
        u32::from(rows).saturating_mul(6).max(1),
    );
    encode_sixel(&rgba, width as usize, height as usize)
}

fn scale_rgba(image: &DecodedImage, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
    let max_w = max_w.clamp(1, 800);
    let max_h = max_h.clamp(1, 800);
    let width = image.width.min(max_w).max(1);
    let height = image.height.min(max_h).max(1);
    // Fit inside the cap while keeping the source aspect ratio.
    let (width, height) = if image.width as u64 * u64::from(height)
        > image.height as u64 * u64::from(width)
    {
        let height = ((u64::from(width) * u64::from(image.height)) / u64::from(image.width).max(1))
            .clamp(1, u64::from(max_h)) as u32;
        (width, height)
    } else {
        let width = ((u64::from(height) * u64::from(image.width)) / u64::from(image.height).max(1))
            .clamp(1, u64::from(max_w)) as u32;
        (width, height)
    };
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let sx = (u64::from(x) * u64::from(image.width) / u64::from(width))
                .min(u64::from(image.width - 1)) as u32;
            let sy = (u64::from(y) * u64::from(image.height) / u64::from(height))
                .min(u64::from(image.height - 1)) as u32;
            let from = ((sy * image.width + sx) * 4) as usize;
            let to = ((y * width + x) * 4) as usize;
            if let Some(pixel) = image.rgba.get(from..from + 4) {
                rgba[to..to + 4].copy_from_slice(pixel);
            }
        }
    }
    (width, height, rgba)
}

fn encode_sixel(rgba: &[u8], width: usize, height: usize) -> String {
    if width == 0 || height == 0 {
        return String::new();
    }
    let mut ids = vec![0u16; width * height];
    let mut used = [false; 216];
    for (index, id_slot) in ids.iter_mut().enumerate() {
        let pixel = index * 4;
        let id = color_id(
            rgba.get(pixel).copied().unwrap_or(0),
            rgba.get(pixel + 1).copied().unwrap_or(0),
            rgba.get(pixel + 2).copied().unwrap_or(0),
        );
        *id_slot = id;
        used[id as usize] = true;
    }
    let mut out = String::from("\u{1b}Pq");
    for id in 0..216u16 {
        if !used[id as usize] {
            continue;
        }
        let (red, green, blue) = color_of(id);
        out.push_str(&format!(
            "#{id};2;{};{};{}",
            u16::from(red) * 100 / 255,
            u16::from(green) * 100 / 255,
            u16::from(blue) * 100 / 255
        ));
    }
    let mut band_start = 0;
    while band_start < height {
        let band_height = (height - band_start).min(6);
        let mut colors = Vec::new();
        for id in 0..216u16 {
            if (0..band_height)
                .any(|row| (0..width).any(|col| ids[(band_start + row) * width + col] == id))
            {
                colors.push(id);
            }
        }
        for id in colors {
            out.push_str(&format!("#{id}"));
            let mut col = 0;
            while col < width {
                let bits = |column: usize| {
                    let mut bits = 0u8;
                    for row in 0..band_height {
                        if ids[(band_start + row) * width + column] == id {
                            bits |= 1 << row;
                        }
                    }
                    bits
                };
                let value = bits(col);
                let mut run = 1;
                while col + run < width && bits(col + run) == value && run < 255 {
                    run += 1;
                }
                let sixel = char::from(0x3f + value);
                if run >= 3 {
                    out.push('!');
                    out.push_str(&run.to_string());
                    out.push(sixel);
                } else {
                    for _ in 0..run {
                        out.push(sixel);
                    }
                }
                col += run;
            }
            out.push('$');
        }
        out.push('-');
        band_start += 6;
    }
    out.push_str("\u{1b}\\");
    out
}

fn color_id(red: u8, green: u8, blue: u8) -> u16 {
    u16::from(quantize(red)) * 36 + u16::from(quantize(green)) * 6 + u16::from(quantize(blue))
}

fn quantize(channel: u8) -> u8 {
    ((u16::from(channel) * 5) / 255) as u8
}

fn color_of(id: u16) -> (u8, u8, u8) {
    let red = (id / 36) as u8;
    let green = ((id / 6) % 6) as u8;
    let blue = (id % 6) as u8;
    (red * 51, green * 51, blue * 51)
}

fn equals_ignore_ascii(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

impl super::Editor {
    pub(super) fn markdown_image_max_rows(&self) -> usize {
        let configured = self.config.markdown_image_max_height;
        if configured == 0 {
            16
        } else {
            configured.min(64)
        }
    }

    pub(super) fn markdown_images_enabled(&self, window: &crate::window::Window) -> bool {
        self.highlight_language_id_for_buffer_index(window.buffer_index)
            .as_deref()
            == Some("markdown")
    }

    pub(super) fn markdown_image_source_reveal(
        &self,
        window: &crate::window::Window,
    ) -> Option<std::ops::Range<usize>> {
        if !window.active || !self.markdown_images_enabled(window) {
            return None;
        }
        let line = window.vtop + window.cy;
        if self.has_multi_cursor_session() {
            return Some(0..self.current_buffer().len().saturating_add(1));
        }
        if self.is_visual() {
            let anchor = self.selection_start.map_or(line, |point| point.y);
            return Some(anchor.min(line)..anchor.max(line) + 1);
        }
        Some(line..line + 1)
    }

    pub(super) fn prepare_markdown_images(
        &self,
        window: &crate::window::Window,
    ) -> Option<ImageLayout> {
        if !self.markdown_images_enabled(window) {
            return None;
        }
        let buffer = self.buffer_manager.get(window.buffer_index)?;
        let base = buffer
            .file
            .as_deref()
            .and_then(|file| Path::new(file).parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::utils::get_workspace_path);
        let mut runtime = self.markdown_images.borrow_mut();
        runtime.sync_document(buffer.id(), buffer.revision(), &buffer.contents(), &base);
        let reveal = self.markdown_image_source_reveal(window);
        let layout = runtime.layout_for(
            buffer.id(),
            self.window_content_width(window),
            self.markdown_image_max_rows(),
            reveal.as_ref(),
            self.config.markdown_images,
        );
        (!layout.is_empty()).then_some(layout)
    }

    pub(super) fn markdown_image_epoch(&self) -> u64 {
        self.markdown_images.borrow().epoch()
    }

    pub(super) fn active_has_markdown_images(&self) -> bool {
        self.active_window_with_editor_view()
            .and_then(|window| self.prepare_markdown_images(&window))
            .is_some_and(|layout| layout.rows.values().any(|rows| rows.len() > 1))
    }

    pub(super) fn markdown_image_line_segments(
        &self,
        line: usize,
        _width: usize,
    ) -> Option<Vec<LineSegment>> {
        let window = self.active_window_with_editor_view()?;
        let layout = self.prepare_markdown_images(&window)?;
        layout.rendered(line).map(ToOwned::to_owned)
    }

    pub(super) fn image_line_is_raw(&self, line: usize) -> bool {
        let Some(window) = self.active_window_with_editor_view() else {
            return false;
        };
        self.prepare_markdown_images(&window)
            .is_some_and(|layout| layout.raw_lines.contains(&line))
    }

    pub(super) fn probe_terminal_graphics(&self) {
        if self.config.markdown_images == MarkdownImagesMode::Off
            || self.config.markdown_images == MarkdownImagesMode::Halfblocks
        {
            return;
        }
        let probe = probe_terminal();
        self.markdown_images.borrow_mut().note_probe(probe);
    }

    pub(super) fn poll_markdown_images(&self) -> bool {
        self.markdown_images.borrow_mut().poll()
    }

    pub(super) fn begin_markdown_image_frame(&self) {
        self.markdown_images.borrow_mut().begin_frame();
    }

    pub(super) fn markdown_graphics_payload(&self, force: bool) -> String {
        if !self.terminal_output_enabled {
            return String::new();
        }
        self.markdown_images.borrow_mut().graphics_payload(force)
    }

    pub(super) fn commit_markdown_image_visibility(
        &mut self,
        buffer: &crate::editor::RenderBuffer,
    ) {
        if self.markdown_images.borrow_mut().commit_visible(buffer) {
            // Sixel and iTerm2 have no delete command. Rewriting every cell
            // clears the previous pixels before the popup is shown, and again
            // when the image is placed back.
            self.force_full_redraw = true;
        }
    }

    pub(super) fn write_markdown_graphics(&mut self, force: bool) -> anyhow::Result<()> {
        let payload = self.markdown_graphics_payload(force);
        if payload.is_empty() {
            return Ok(());
        }
        std::io::Write::write_all(&mut self.stdout, payload.as_bytes())?;
        Ok(())
    }

    pub(super) fn markdown_image_shutdown_sequence(&self) -> String {
        self.markdown_images.borrow().shutdown_sequence()
    }

    pub(super) fn paint_markdown_image_segment(
        &self,
        buffer: &mut crate::editor::RenderBuffer,
        window: &crate::window::Window,
        segment: &LineSegment,
        origin_x: usize,
        origin_y: usize,
        content_width: usize,
    ) -> bool {
        let Some(span) = segment.image else {
            return false;
        };
        let editor_style = self.theme.style.clone();
        buffer.fill_rect(
            origin_x,
            origin_y,
            content_width,
            1,
            ' ',
            &editor_style,
            &self.theme,
        );
        let paint = {
            let runtime = self.markdown_images.borrow();
            let buffer_id = self.buffer_manager[window.buffer_index].id();
            runtime.paints.get(&(buffer_id, segment.line)).cloned()
        };
        let Some(paint) = paint else {
            return true;
        };
        let presentation = self
            .markdown_images
            .borrow()
            .presentation(self.config.markdown_images);
        if span.placeholder
            || paint.image.is_none()
            || matches!(presentation, ImagePresentation::Placeholder)
        {
            if span.index == 0 {
                let text = placeholder_text(
                    &paint.alt,
                    &paint.target,
                    paint.note.as_deref(),
                    content_width,
                );
                let style = self
                    .theme
                    .get_style("markup.underline.link.markdown")
                    .unwrap_or_else(|| crate::theme::Style {
                        fg: Some(Color::Rgb {
                            r: 120,
                            g: 170,
                            b: 220,
                        }),
                        italic: true,
                        ..crate::theme::Style::default()
                    });
                buffer.set_text(origin_x, origin_y, &text, &style);
            }
            return true;
        }
        let Some(image) = paint.image else {
            return true;
        };
        let draw_halfblocks = match presentation {
            ImagePresentation::Halfblocks => true,
            ImagePresentation::Graphics(_) => {
                self.markdown_images.borrow().hints.truecolor
                    || self.config.markdown_images == MarkdownImagesMode::Auto
            }
            ImagePresentation::Placeholder => false,
        };
        if draw_halfblocks {
            let background = theme_background(&self.theme.style);
            for col in 0..usize::from(span.cols).min(content_width) {
                let (fg, bg) = halfblock_colors(
                    &image,
                    u32::from(span.cols),
                    u32::from(span.rows),
                    col as u32,
                    u32::from(span.index),
                    background,
                );
                buffer.set_text(
                    origin_x + col,
                    origin_y,
                    "▀",
                    &crate::theme::Style {
                        fg: Some(fg),
                        bg: Some(bg),
                        ..crate::theme::Style::default()
                    },
                );
            }
        }
        if span.index == 0 {
            if let ImagePresentation::Graphics(protocol) = presentation {
                let fits =
                    segment.row + usize::from(span.rows) <= self.window_content_height(window);
                if fits {
                    let buffer_id = self.buffer_manager[window.buffer_index].id();
                    let mut runtime = self.markdown_images.borrow_mut();
                    let id = runtime.id_for(buffer_id, segment.line);
                    let payload = match protocol {
                        GraphicsProtocol::Kitty => kitty_transmit(id, &image, span.cols, span.rows),
                        GraphicsProtocol::Iterm2 => iterm_transmit(&image, span.cols, span.rows),
                        GraphicsProtocol::Sixel => sixel_transmit(&image, span.cols, span.rows),
                    };
                    if !payload.is_empty() {
                        let rect = crate::ui::ScreenRect {
                            x: origin_x,
                            y: origin_y,
                            width: usize::from(span.cols),
                            height: usize::from(span.rows),
                        };
                        runtime.stage_graphics(StagedImage {
                            id,
                            x: u16::try_from(origin_x).unwrap_or(u16::MAX),
                            y: u16::try_from(origin_y).unwrap_or(u16::MAX),
                            cols: span.cols,
                            rows: span.rows,
                            signature: image_signature(&image, span.cols, span.rows, protocol),
                            protocol,
                            payload,
                            cells: hash_region(buffer, rect),
                        });
                    }
                }
            }
        }
        true
    }
}

fn theme_background(style: &crate::theme::Style) -> [u8; 3] {
    match style.bg {
        Some(Color::Rgb { r, g, b }) | Some(Color::Rgba { r, g, b, .. }) => [r, g, b],
        None => [0, 0, 0],
    }
}

fn image_signature(image: &DecodedImage, cols: u16, rows: u16, protocol: GraphicsProtocol) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    image.width.hash(&mut hasher);
    image.height.hash(&mut hasher);
    image.rgba.len().hash(&mut hasher);
    image.rgba.first().hash(&mut hasher);
    image.rgba.last().hash(&mut hasher);
    cols.hash(&mut hasher);
    rows.hash(&mut hasher);
    protocol_tag(protocol).hash(&mut hasher);
    hasher.finish()
}

fn protocol_tag(protocol: GraphicsProtocol) -> u8 {
    match protocol {
        GraphicsProtocol::Kitty => 1,
        GraphicsProtocol::Iterm2 => 2,
        GraphicsProtocol::Sixel => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hints(vars: &[(&str, &str)]) -> TerminalHints {
        TerminalHints::from_env(|key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn parses_local_absolute_and_remote_image_lines() {
        let relative = parse_image_line("![alt](./pic.png)").unwrap();
        assert_eq!(relative.alt, "alt");
        assert!(
            matches!(relative.source, ImageSource::Local(ref path) if path.ends_with("pic.png"))
        );

        let absolute = parse_image_line("![shot](/tmp/shot.png)").unwrap();
        assert!(
            matches!(absolute.source, ImageSource::Local(ref path) if path == Path::new("/tmp/shot.png"))
        );

        let remote = parse_image_line("  ![hello](https://example.com/a.png \"title\")  ").unwrap();
        assert_eq!(remote.alt, "hello");
        assert!(
            matches!(remote.source, ImageSource::Remote(ref url) if url.starts_with("https://"))
        );

        let insecure = parse_image_line("![](http://example.com/a.jpg)").unwrap();
        assert!(matches!(insecure.source, ImageSource::Remote(_)));

        let spaced = parse_image_line("![a](<my file.png>)").unwrap();
        assert_eq!(spaced.target, "my file.png");

        assert!(parse_image_line("not an image").is_none());
        assert!(parse_image_line("![alt](pic.png) trailing").is_none());
        assert!(parse_image_line("![alt][ref]").is_none());
        assert!(parse_image_line("```").is_none());
    }

    #[test]
    fn fenced_images_are_not_document_images() {
        let source = "```\n![alt](pic.png)\n```\n![real](./pic.png)\n";
        let images = parse_document(source, Path::new("/notes"));
        assert_eq!(images.len(), 1);
        assert!(images.contains_key(&3));
        assert_eq!(images[&3].alt, "real");
    }

    #[test]
    fn reserves_rows_from_aspect_ratio_and_caps_height() {
        let (cols, rows) = image_cell_grid(200, 100, 80, 16);
        assert_eq!(rows, 16);
        assert_eq!(cols, 64);

        let (cols, rows) = image_cell_grid(100, 400, 80, 16);
        assert_eq!(rows, 16);
        assert_eq!(cols, 8);

        let (_, rows) = image_cell_grid(2, 2, 80, 16);
        assert_eq!(rows, 1);
        assert_eq!(image_cell_grid(0, 10, 80, 16), (0, 0));
    }

    #[test]
    fn virtual_rows_collapse_when_the_line_is_revealed() {
        let mut runtime = ImageRuntime::new();
        let id = BufferId::from_test_id(7);
        let source = "![alt](./missing.png)\nnext\n";
        runtime.sync_document(id, 1, source, Path::new("/tmp"));
        let hidden = runtime.layout_for(id, 40, 8, None, MarkdownImagesMode::Halfblocks);
        assert!(!hidden.rendered(0).unwrap().is_empty());
        assert!(hidden
            .rendered(0)
            .unwrap()
            .iter()
            .all(|row| row.image.is_some()));
        assert!(!hidden.raw_lines.contains(&0));

        let revealed = runtime.layout_for(id, 40, 8, Some(&(0..1)), MarkdownImagesMode::Halfblocks);
        assert!(revealed.rendered(0).is_none());
        assert!(revealed.raw_lines.contains(&0));
    }

    #[test]
    fn layout_reserves_vertical_space_for_an_image_line() {
        let image = DecodedImage {
            width: 200,
            height: 100,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        // The grid helper is what layout uses once pixels are known.
        let (cols, rows) = image_cell_grid(image.width, image.height, 80, 10);
        let segments = image_line_segments(2, rows, cols, false);
        assert_eq!(segments.len(), usize::from(rows));
        assert_eq!(segments[0].line, 2);
        assert_eq!(segments[0].start_col, 0);
        assert_eq!(segments.last().unwrap().start_col, usize::from(rows) - 1);
        assert!(segments[0].first_segment);
        assert!(segments.last().unwrap().last_segment);
        let visible: Vec<_> = segments
            .iter()
            .filter(|segment| segment.start_col >= 2)
            .collect();
        assert_eq!(visible.len(), usize::from(rows) - 2);
        assert_eq!(visible[0].start_col, 2);
    }

    #[test]
    fn probe_response_detects_kitty_ok_and_sixel_attribute() {
        let both = parse_probe_response(b"\x1b_Gi=31;OK\x1b\\\x1b[?64;1;2;4;6c");
        assert!(both.kitty);
        assert!(both.sixel);
        let vt100 = parse_probe_response(b"\x1b[?1;2c");
        assert!(!vt100.kitty);
        assert!(!vt100.sixel);
        assert!(da1_has_sixel(b"\x1b[?4c"));
        assert!(!da1_has_sixel(b"\x1b[?62;22c"));
        let rejected = parse_probe_response(b"\x1b_Gi=31;EINVAL\x1b\\");
        assert!(!rejected.kitty);
    }

    #[test]
    fn env_hints_prefer_kitty_then_iterm_then_sixel() {
        let kitty = hints(&[("KITTY_WINDOW_ID", "1"), ("COLORTERM", "truecolor")]);
        assert_eq!(
            detected_graphics(&kitty, Probe::default()),
            Some(GraphicsProtocol::Kitty)
        );
        let ghostty = hints(&[("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty")]);
        assert_eq!(
            detected_graphics(&ghostty, Probe::default()),
            Some(GraphicsProtocol::Kitty)
        );
        let wezterm = hints(&[("WEZTERM_EXECUTABLE", "/wezterm")]);
        assert_eq!(
            detected_graphics(&wezterm, Probe::default()),
            Some(GraphicsProtocol::Kitty)
        );
        let iterm = hints(&[("TERM_PROGRAM", "iTerm.app"), ("LC_TERMINAL", "iTerm2")]);
        assert_eq!(
            detected_graphics(&iterm, Probe::default()),
            Some(GraphicsProtocol::Iterm2)
        );
        let vscode = hints(&[("TERM_PROGRAM", "vscode")]);
        assert_eq!(
            detected_graphics(&vscode, Probe::default()),
            Some(GraphicsProtocol::Iterm2)
        );
        let foot = hints(&[("TERM", "foot")]);
        assert_eq!(
            detected_graphics(&foot, Probe::default()),
            Some(GraphicsProtocol::Sixel)
        );
        let queried = hints(&[("TERM", "xterm-256color")]);
        assert_eq!(
            detected_graphics(
                &queried,
                Probe {
                    kitty: true,
                    sixel: true
                }
            ),
            Some(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            detected_graphics(
                &queried,
                Probe {
                    kitty: false,
                    sixel: true
                }
            ),
            Some(GraphicsProtocol::Sixel)
        );
    }

    #[test]
    fn tmux_ignores_env_graphics_until_a_query_succeeds() {
        let kitty_tmux = hints(&[("TMUX", "/tmp/tmux-1"), ("KITTY_WINDOW_ID", "1")]);
        assert_eq!(detected_graphics(&kitty_tmux, Probe::default()), None);
        assert_eq!(
            detected_graphics(
                &kitty_tmux,
                Probe {
                    kitty: true,
                    sixel: false
                }
            ),
            Some(GraphicsProtocol::Kitty)
        );
        let wrapped = tmux_passthrough(KITTY_GRAPHICS_QUERY);
        assert!(wrapped.starts_with("\u{1b}Ptmux;\u{1b}"));
        assert!(wrapped.contains("\u{1b}\u{1b}_G"));
        assert!(wrapped.ends_with("\u{1b}\\"));
        assert!(probe_query(true).contains("Ptmux"));
        assert!(probe_query(false).starts_with(KITTY_GRAPHICS_QUERY));
        assert!(probe_query(false).contains(DA1_QUERY));
    }

    #[test]
    fn auto_falls_back_to_halfblocks_then_placeholder() {
        let plain = hints(&[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]);
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Auto, &plain, Probe::default()),
            ImagePresentation::Halfblocks
        );
        let dumb = hints(&[("TERM", "dumb")]);
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Auto, &dumb, Probe::default()),
            ImagePresentation::Placeholder
        );
        assert_eq!(
            choose_presentation(
                MarkdownImagesMode::Off,
                &plain,
                Probe {
                    kitty: true,
                    sixel: true
                }
            ),
            ImagePresentation::Placeholder
        );
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Kitty, &dumb, Probe::default()),
            ImagePresentation::Graphics(GraphicsProtocol::Kitty)
        );
    }

    #[test]
    fn placeholder_names_the_image_and_the_error() {
        assert_eq!(
            placeholder_text("alt", "pic.png", None, 80),
            "[image: alt (pic.png)]"
        );
        assert_eq!(
            placeholder_text("alt", "pic.png", Some("not found"), 80),
            "[image: alt (pic.png) - not found]"
        );
        let clipped = placeholder_text("alt", "pic.png", None, 8);
        assert!(display_width(&clipped) <= 8);
    }

    #[test]
    fn halfblocks_sample_top_and_bottom_pixels() {
        let image = DecodedImage {
            width: 1,
            height: 2,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        let (fg, bg) = halfblock_colors(&image, 1, 1, 0, 0, [0, 0, 0]);
        assert_eq!(fg, Color::Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(bg, Color::Rgb { r: 0, g: 0, b: 255 });
    }

    #[test]
    fn kitty_sixel_and_iterm_payloads_carry_the_image() {
        let image = DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        };
        let kitty = kitty_transmit(7, &image, 1, 1);
        assert!(kitty.contains("i=7"));
        assert!(kitty.contains("f=32"));
        assert!(kitty.contains("z=-1"));
        assert!(kitty.starts_with("\u{1b}_G"));
        assert!(kitty.contains("/wAA/w==") || kitty.contains("/wAA/w"));
        assert!(kitty_delete(7).contains("a=d"));
        assert!(kitty_delete_all().contains("d=A"));

        let sixel = sixel_transmit(&image, 1, 1);
        assert!(sixel.starts_with("\u{1b}Pq"));
        assert!(sixel.ends_with("\u{1b}\\"));

        let iterm = iterm_transmit(&image, 2, 2);
        assert!(iterm.starts_with("\u{1b}]1337;File=inline=1;"));
        assert!(iterm.contains("width=2"));
        assert!(iterm.ends_with('\u{7}'));
    }

    #[test]
    fn decodes_a_generated_png() {
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[0, 255, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let decoded = decode_image_bytes(&png).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(&decoded.rgba[..3], &[0, 255, 0]);
    }

    #[test]
    fn display_layout_reserves_image_rows_and_scrolls_inside_them() {
        use super::super::display_layout::{
            layout_lines_with_tables, BreakIndentOptions, LayoutConfig,
        };

        let plan = ImageLayout {
            rows: BTreeMap::from([(0, image_line_segments(0, 4, 10, false))]),
            raw_lines: BTreeSet::new(),
        };
        let lines = vec!["![alt](pic.png)\n".to_string(), "after\n".to_string()];
        let config = LayoutConfig {
            content_width: 20,
            height: 10,
            wrap: true,
            vtop: 0,
            vleft: 0,
            skipcol: 0,
            break_indent: BreakIndentOptions::disabled(),
        };
        let layout =
            layout_lines_with_tables(&lines, 2, config, None, None, None, None, Some(&plan));
        assert_eq!(layout.rows.len(), 5);
        assert!(layout.rows[..4]
            .iter()
            .all(|row| row.line == 0 && row.image.is_some()));
        assert_eq!(layout.rows[4].line, 1);
        assert_eq!(layout.rows[4].row, 4);

        let scrolled = layout_lines_with_tables(
            &lines,
            2,
            LayoutConfig {
                skipcol: 2,
                ..config
            },
            None,
            None,
            None,
            None,
            Some(&plan),
        );
        let image_rows: Vec<_> = scrolled.rows.iter().filter(|row| row.line == 0).collect();
        assert_eq!(image_rows.len(), 2);
        assert_eq!(image_rows[0].start_col, 2);
        assert_eq!(scrolled.rows.last().unwrap().line, 1);
    }

    #[test]
    fn normal_mode_draws_a_placeholder_until_the_cursor_lands_on_the_image() {
        use crate::{
            buffer::Buffer,
            config::Config,
            editor::render_buffer::RenderBuffer,
            theme::{Style, Theme},
        };

        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        config.scrolloff = Some(0);
        config.markdown_images = MarkdownImagesMode::Off;
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = crate::editor::Editor::with_size(
            lsp,
            80,
            12,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some("/tmp/notes.md".into()),
                "hello\n![alt](missing.png)\n".into(),
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();

        let mut frame = RenderBuffer::new(80, 12, &Style::default());
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(screen.contains("hello"), "{screen}");
        assert!(
            screen.contains("[image: alt (missing.png) - not found]"),
            "{screen}"
        );
        assert!(!screen.contains("![alt]"), "{screen}");

        editor.cy = 1;
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(screen.contains("![alt](missing.png)"), "{screen}");
        assert!(!screen.contains("[image:"), "{screen}");
    }

    #[test]
    fn halfblocks_paint_away_from_the_cursor_and_source_returns_on_the_line() {
        use crate::{
            buffer::Buffer,
            config::Config,
            editor::render_buffer::RenderBuffer,
            theme::{Style, Theme},
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swatch.png");
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let index = (y * 8 + x) * 4;
                rgba[index + usize::from(y >= 4) * 2] = 255;
                rgba[index + 3] = 255;
            }
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, 8, 8, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&path, &png).unwrap();

        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        config.scrolloff = Some(0);
        config.markdown_images = MarkdownImagesMode::Halfblocks;
        let source = format!("hello\n![swatch]({})\n", path.display());
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = crate::editor::Editor::with_size(
            lsp,
            80,
            16,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some(dir.path().join("notes.md").display().to_string()),
                source,
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        let mut frame = RenderBuffer::new(80, 16, &Style::default());
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        let swatch = frame.cells.iter().find(|cell| cell.text == "▀").unwrap();
        assert!(
            matches!(
                (swatch.style.fg, swatch.style.bg),
                (
                    Some(crate::color::Color::Rgb { r, g, b }),
                    Some(crate::color::Color::Rgb { .. })
                ) if r > 150 && g < 220 && b < 180
            ),
            "half-block color {:?} screen {screen}",
            swatch.style
        );
        assert!(screen.contains('▀'), "{screen}");
        assert!(!screen.contains("![swatch]"), "{screen}");

        editor.cy = 1;
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(screen.contains("![swatch]"), "{screen}");
        assert!(!screen.contains('▀'), "{screen}");
    }

    #[test]
    fn overlay_rects_hide_an_intersecting_image_and_ignore_the_rest() {
        use crate::ui::ScreenRect;
        let image = ScreenRect {
            x: 4,
            y: 2,
            width: 20,
            height: 8,
        };
        let popup = ScreenRect {
            x: 18,
            y: 8,
            width: 12,
            height: 4,
        };
        let elsewhere = ScreenRect {
            x: 0,
            y: 20,
            width: 6,
            height: 3,
        };
        let touching = ScreenRect {
            x: 24,
            y: 2,
            width: 4,
            height: 2,
        };
        assert!(image_hidden_by_overlays(image, &[elsewhere, popup]));
        assert!(!image_hidden_by_overlays(image, &[elsewhere, touching]));
        assert!(!image_hidden_by_overlays(
            ScreenRect {
                x: 0,
                y: 0,
                width: 0,
                height: 4,
            },
            &[image]
        ));
    }

    #[test]
    fn covered_graphics_are_removed_and_restored_when_the_overlay_closes() {
        use crate::theme::Style;
        let mut runtime = ImageRuntime::new();
        let mut buffer = crate::editor::RenderBuffer::new(20, 8, &Style::default());
        let rect = crate::ui::ScreenRect {
            x: 2,
            y: 1,
            width: 4,
            height: 2,
        };
        let stage = |buffer: &crate::editor::RenderBuffer| StagedImage {
            id: 3,
            x: 2,
            y: 1,
            cols: 4,
            rows: 2,
            signature: 9,
            protocol: GraphicsProtocol::Kitty,
            payload: "PLACE".to_string(),
            cells: hash_region(buffer, rect),
        };

        runtime.begin_frame();
        runtime.stage_graphics(stage(&buffer));
        assert!(runtime.commit_visible(&buffer));
        let shown = runtime.graphics_payload(false);
        assert!(shown.contains("PLACE"));
        assert!(!shown.contains("a=d"));

        buffer.set_text(
            3,
            2,
            "popup",
            &Style {
                fg: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
                ..Style::default()
            },
        );
        buffer.note_overlay(crate::ui::ScreenRect {
            x: 3,
            y: 2,
            width: 5,
            height: 1,
        });
        runtime.begin_frame();
        runtime.stage_graphics(stage(&buffer));
        // The fingerprint above includes the popup. Stage the pre-popup hash
        // so the cell change and the overlay rect both hide the image.
        runtime.staged[0].cells = runtime.staged[0].cells.wrapping_add(1);
        assert!(runtime.commit_visible(&buffer));
        let hidden = runtime.graphics_payload(false);
        assert!(!hidden.contains("PLACE"));
        assert!(hidden.contains("a=d"));

        buffer = crate::editor::RenderBuffer::new(20, 8, &Style::default());
        runtime.begin_frame();
        runtime.stage_graphics(stage(&buffer));
        assert!(runtime.commit_visible(&buffer));
        let restored = runtime.graphics_payload(false);
        assert!(restored.contains("PLACE"));
    }

    #[test]
    fn keymap_popup_covers_halfblocks_and_the_image_returns() {
        use std::collections::HashMap;

        use crate::{
            buffer::Buffer,
            config::{Config, KeyAction},
            editor::render_buffer::RenderBuffer,
            theme::{Style, Theme},
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swatch.png");
        let mut rgba = vec![0u8; 8 * 40 * 4];
        for (index, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            pixel[usize::from(index % 2 == 0)] = 220;
            pixel[3] = 255;
        }
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, 8, 40, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&path, &png).unwrap();

        let mut config: Config = toml::from_str(include_str!("../../default_config.toml")).unwrap();
        config.lsp.servers.clear();
        config.clipboard.enabled = false;
        config.scrolloff = Some(0);
        config.markdown_images = MarkdownImagesMode::Halfblocks;
        config.markdown_image_max_height = 14;
        let source = format!("![swatch]({})\nnotes\n", path.display());
        let lsp = Box::new(crate::lsp::LspManager::new(config.lsp.clone()));
        let mut editor = crate::editor::Editor::with_size(
            lsp,
            80,
            18,
            config,
            Theme::default(),
            vec![Buffer::new(
                Some(dir.path().join("notes.md").display().to_string()),
                source,
            )],
        )
        .unwrap();
        editor.test_disable_terminal_output();
        editor.cy = 1;
        let mut frame = RenderBuffer::new(80, 18, &Style::default());
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(screen.contains('▀'), "{screen}");
        assert!(!screen.contains("keymaps"), "{screen}");

        editor.keymap_hint_prefix = vec!["g".to_string()];
        editor.waiting_key_action = Some(KeyAction::Nested(HashMap::from([(
            "j".to_string(),
            KeyAction::Single(crate::editor::Action::MoveScreenLineDown),
        )])));
        editor.keymap_hints_visible = true;
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(
            screen.contains("g · keymaps"),
            "popup should paint over the image cells: {screen}"
        );
        assert!(
            image_hidden_by_overlays(
                crate::ui::ScreenRect {
                    x: 0,
                    y: 0,
                    width: frame.width,
                    height: frame.height,
                },
                &frame.overlay_rects,
            ),
            "the keymap popup should count as an overlay"
        );
        assert!(screen.contains('▀'), "{screen}");

        editor.keymap_hints_visible = false;
        editor.waiting_key_action = None;
        editor.keymap_hint_prefix.clear();
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(!screen.contains("keymaps"), "{screen}");
        assert!(screen.contains('▀'), "{screen}");
    }

    fn frame_text(frame: &crate::editor::RenderBuffer) -> String {
        frame
            .cells
            .chunks(frame.width)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
