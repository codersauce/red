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
/// Report cell size in pixels. The reply is `CSI 6 ; height ; width t`.
pub(super) const CELL_SIZE_QUERY: &str = "\u{1b}[16t";
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
    pub herdr: bool,
    pub zellij: bool,
    /// GNU screen does not relay Kitty or Sixel. A failed query stays on half-blocks.
    pub screen: bool,
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
        // Herdr sanitizes TERM_PROGRAM but panes keep HERDR_ENV=1. A leaked
        // GHOSTTY_RESOURCES_DIR must not override that and select Kitty.
        let herdr =
            var("HERDR_ENV").as_deref() == Some("1") || equals_ignore_ascii(&term_program, "herdr");
        let zellij = var("ZELLIJ").is_some()
            || var("ZELLIJ_SESSION_NAME").is_some()
            || equals_ignore_ascii(&term_program, "zellij");
        let screen = var("STY").is_some() || term == "screen" || term.starts_with("screen.");
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
            herdr,
            zellij,
            screen,
            truecolor,
        }
    }

    /// Multiplexers whose environment lies about the outer terminal.
    ///
    /// Herdr, tmux, and Zellij can relay graphics, but only an answered query
    /// proves it. GNU screen does not relay Kitty or Sixel, so a failed query
    /// is the fallback. Leaked `GHOSTTY_RESOURCES_DIR` or `KITTY_WINDOW_ID`
    /// must not choose a protocol by itself.
    fn query_required(&self) -> bool {
        self.tmux || self.herdr || self.zellij || self.screen
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

/// Cell size in pixels. `10×20` is the stand-in when a multiplexer reports 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CellPixels {
    pub width: u16,
    pub height: u16,
}

impl Default for CellPixels {
    fn default() -> Self {
        Self {
            width: 10,
            height: 20,
        }
    }
}

impl CellPixels {
    pub(super) fn measured(width: u16, height: u16) -> Self {
        if width == 0 || height == 0 {
            Self::default()
        } else {
            Self {
                width: width.clamp(1, 128),
                height: height.clamp(1, 256),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Probe {
    pub kitty: bool,
    pub sixel: bool,
    /// From `CSI 16 t` (`CSI 6 ; height ; width t`). Zero means the terminal
    /// did not answer.
    pub cell_width: u16,
    pub cell_height: u16,
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct Placement {
    x: u16,
    y: u16,
    cols: u16,
    rows: u16,
    signature: u64,
    protocol: GraphicsProtocol,
    source: Option<KittySourceRect>,
    placement_id: u32,
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
    source: Option<KittySourceRect>,
    placement_id: u32,
}

struct StagedImage {
    id: u32,
    x: u16,
    y: u16,
    cols: u16,
    /// Visible rows. This is the full image when nothing is clipped.
    rows: u16,
    total_rows: u16,
    first_index: u16,
    /// How many of this image's rows have been painted in the current frame.
    painted: u16,
    signature: u64,
    protocol: GraphicsProtocol,
    payload: String,
    source: Option<KittySourceRect>,
    pixel_w: u32,
    pixel_h: u32,
    decoded: Option<Arc<DecodedImage>>,
    /// Cell fingerprint taken after every image row is painted, before overlays.
    cells: Option<u64>,
}

struct ObservedGraphics {
    id: u32,
    x: u16,
    y: u16,
    cols: u16,
    total_rows: u16,
    index: u16,
    signature: u64,
    protocol: GraphicsProtocol,
    image: Arc<DecodedImage>,
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
    placed: HashMap<(u32, u32), Placement>,
    /// Image id to the signature last transmitted with `a=t`. Placements can
    /// come and go without sending the pixels again.
    transmitted: HashMap<u32, u64>,
    visible_ids: Vec<(u32, u32)>,
    ids: HashMap<(BufferId, usize), u32>,
    next_id: u32,
    frame_open: bool,
    cell: CellPixels,
    text_grid: Option<(u64, u16, u16, Arc<DecodedImage>)>,
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
            transmitted: HashMap::new(),
            visible_ids: Vec::new(),
            ids: HashMap::new(),
            next_id: 0,
            frame_open: false,
            cell: CellPixels::default(),
            text_grid: None,
        }
    }

    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(super) fn note_probe(&mut self, probe: Probe) {
        let cell = if probe.cell_width > 0 && probe.cell_height > 0 {
            CellPixels::measured(probe.cell_width, probe.cell_height)
        } else {
            ioctl_cell_pixels()
                .map(|(width, height)| CellPixels::measured(width, height))
                .unwrap_or_default()
        };
        if probe != self.probe || cell != self.cell {
            self.probe = probe;
            self.cell = cell;
            self.text_grid = None;
            self.epoch = self.epoch.wrapping_add(1);
        }
    }

    pub(super) fn presentation(&self, mode: MarkdownImagesMode) -> ImagePresentation {
        choose_presentation(mode, &self.hints, self.probe)
    }

    fn text_samples(&mut self, image: &DecodedImage, cols: u16, rows: u16) -> Arc<DecodedImage> {
        let key = text_sample_key(image);
        if let Some((cached, cached_cols, cached_rows, grid)) = &self.text_grid {
            if *cached == key && *cached_cols == cols && *cached_rows == rows {
                return Arc::clone(grid);
            }
        }
        let width = u32::from(cols).saturating_mul(2).max(1);
        let height = u32::from(rows).saturating_mul(2).max(1);
        let rgba = resample_rgba(image, width, height);
        let grid = Arc::new(DecodedImage {
            width,
            height,
            rgba,
        });
        self.text_grid = Some((key, cols, rows, Arc::clone(&grid)));
        grid
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
            let (cols, image_rows, placeholder) = match (&pixels, presentation) {
                (Some(image), ImagePresentation::Halfblocks | ImagePresentation::Graphics(_)) => {
                    let (cols, image_rows) =
                        image_cell_grid(image.width, image.height, width, max_rows, self.cell);
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

    #[cfg(test)]
    fn stage_graphics(&mut self, image: StagedImage) {
        self.staged.push(image);
    }

    fn observe_graphics_row(&mut self, row: ObservedGraphics) {
        if !self.frame_open {
            return;
        }
        if let Some(existing) = self.staged.iter_mut().find(|image| image.id == row.id) {
            if row.index < existing.first_index {
                existing.x = row.x;
                existing.y = row.y;
                existing.first_index = row.index;
            }
            existing.painted = existing.painted.saturating_add(1);
            return;
        }
        self.staged.push(StagedImage {
            id: row.id,
            x: row.x,
            y: row.y,
            cols: row.cols,
            rows: row.total_rows,
            total_rows: row.total_rows,
            first_index: row.index,
            painted: 1,
            signature: row.signature,
            protocol: row.protocol,
            payload: String::new(),
            source: None,
            pixel_w: 0,
            pixel_h: 0,
            decoded: Some(row.image),
            cells: None,
        });
    }

    /// Hashes each image after its visible rows exist, and builds a cropped
    /// Kitty payload when the block does not fit in the viewport.
    pub(super) fn seal_fingerprints(&mut self, buffer: &crate::editor::RenderBuffer) {
        if !self.frame_open {
            return;
        }
        self.staged
            .retain(|image| image.decoded.is_none() || image.painted > 0);
        for image in &mut self.staged {
            if let Some(decoded) = image.decoded.clone() {
                let visible = image.painted.max(1);
                match image.protocol {
                    GraphicsProtocol::Kitty => {
                        let (pixel_w, pixel_h) =
                            graphics_pixel_size(&decoded, image.cols, image.total_rows, self.cell);
                        image.source = kitty_source_rect(
                            pixel_w,
                            pixel_h,
                            image.total_rows,
                            image.first_index,
                            visible,
                        );
                        image.rows = visible;
                        image.pixel_w = pixel_w;
                        image.pixel_h = pixel_h;
                        image.payload = kitty_transmit_data(
                            image.id,
                            &decoded,
                            image.cols,
                            image.total_rows,
                            self.cell,
                        );
                    }
                    GraphicsProtocol::Iterm2 | GraphicsProtocol::Sixel => {
                        let (pixel_w, pixel_h) =
                            graphics_pixel_size(&decoded, image.cols, image.total_rows, self.cell);
                        let source = kitty_source_rect(
                            pixel_w,
                            pixel_h,
                            image.total_rows,
                            image.first_index,
                            visible,
                        );
                        let rgba = resample_rgba(&decoded, pixel_w, pixel_h);
                        let (width, height, cropped) = crop_rgba(&rgba, pixel_w, pixel_h, source);
                        image.rows = visible;
                        image.pixel_w = width;
                        image.pixel_h = height;
                        image.source = source;
                        image.payload = if image.protocol == GraphicsProtocol::Iterm2 {
                            iterm_transmit_rgba(&cropped, width, height, image.cols, visible)
                        } else {
                            encode_sixel(&cropped, width as usize, height as usize)
                        };
                    }
                }
            }
            if image.cells.is_none() && image.painted > 0 && image.painted >= image.rows {
                let rect = crate::ui::ScreenRect {
                    x: usize::from(image.x),
                    y: usize::from(image.y),
                    width: usize::from(image.cols),
                    height: usize::from(image.rows.max(1)),
                };
                image.cells = Some(hash_region(buffer, rect));
            }
        }
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
            if image.protocol == GraphicsProtocol::Kitty {
                // Herdr draws Kitty images above cells, so a popup cannot cover
                // them with a background color. Split the placement instead.
                let mut pieces = uncovered_cell_rects(rect, &buffer.overlay_rects);
                pieces.sort_by_key(|piece| (piece.y, piece.x));
                let full = pieces.len() == 1 && pieces[0] == rect;
                let pixel_w = if image.pixel_w == 0 {
                    u32::from(image.cols.max(1))
                } else {
                    image.pixel_w
                };
                let pixel_h = if image.pixel_h == 0 {
                    u32::from(image.rows.max(1))
                } else {
                    image.pixel_h
                };
                let base = image.source.unwrap_or(KittySourceRect {
                    x: 0,
                    y: 0,
                    w: pixel_w,
                    h: pixel_h,
                });
                for (index, piece) in pieces.iter().enumerate() {
                    let placement_id = if full {
                        1
                    } else {
                        u32::try_from(index).unwrap_or(0) + 2
                    };
                    let source = source_for_piece(rect, *piece, base, pixel_h);
                    visible.push((image.id, placement_id));
                    self.pending.push(PendingDraw {
                        id: image.id,
                        x: u16::try_from(piece.x).unwrap_or(u16::MAX),
                        y: u16::try_from(piece.y).unwrap_or(u16::MAX),
                        cols: u16::try_from(piece.width).unwrap_or(u16::MAX),
                        rows: u16::try_from(piece.height).unwrap_or(u16::MAX),
                        signature: image.signature,
                        protocol: image.protocol,
                        payload: image.payload.clone(),
                        source,
                        placement_id,
                    });
                }
                continue;
            }
            let covered = image
                .cells
                .is_some_and(|cells| hash_region(buffer, rect) != cells)
                || image_hidden_by_overlays(rect, &buffer.overlay_rects);
            if covered {
                continue;
            }
            visible.push((image.id, 1));
            self.pending.push(PendingDraw {
                id: image.id,
                x: image.x,
                y: image.y,
                cols: image.cols,
                rows: image.rows,
                signature: image.signature,
                protocol: image.protocol,
                payload: image.payload,
                source: image.source,
                placement_id: 1,
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
        let mut out = String::new();
        // A forced redraw used to drop `placed` before emitting deletes, so the
        // ids were gone and the last image stayed on screen forever.
        if force {
            let mut images = Vec::new();
            for ((image_id, placement_id), placed) in &self.placed {
                if placed.protocol == GraphicsProtocol::Kitty {
                    out.push_str(&wrap_protocol(
                        self.hints.tmux,
                        &kitty_delete_placement(*image_id, *placement_id),
                    ));
                    if !images.contains(image_id) {
                        images.push(*image_id);
                    }
                }
            }
            for image_id in images {
                self.transmitted.remove(&image_id);
            }
            self.placed.clear();
        }
        let mut next = HashMap::new();
        let mut saw_transmit = std::collections::HashSet::new();
        for draw in &pending {
            let key = (draw.id, draw.placement_id);
            let previous = self.placed.get(&key).copied();
            let unchanged = previous.is_some_and(|placed| {
                placed.x == draw.x
                    && placed.y == draw.y
                    && placed.cols == draw.cols
                    && placed.rows == draw.rows
                    && placed.signature == draw.signature
                    && placed.protocol == draw.protocol
                    && placed.source == draw.source
            });
            if draw.protocol == GraphicsProtocol::Kitty {
                let needs_pixels = self.transmitted.get(&draw.id) != Some(&draw.signature);
                if needs_pixels && saw_transmit.insert(draw.id) && !draw.payload.is_empty() {
                    out.push_str(&wrap_protocol(self.hints.tmux, &draw.payload));
                    self.transmitted.insert(draw.id, draw.signature);
                }
                if unchanged && !force && !needs_pixels {
                    if let Some(placed) = previous {
                        next.insert(key, placed);
                    }
                    continue;
                }
                out.push_str(&format!("\u{1b}[{};{}H", draw.y + 1, draw.x + 1));
                out.push_str(&wrap_protocol(
                    self.hints.tmux,
                    &kitty_place(
                        draw.id,
                        draw.placement_id,
                        draw.cols,
                        draw.rows,
                        draw.source,
                    ),
                ));
            } else if unchanged && !force {
                if let Some(placed) = previous {
                    next.insert(key, placed);
                }
                continue;
            } else {
                out.push_str(&format!("\u{1b}[{};{}H", draw.y + 1, draw.x + 1));
                out.push_str(&wrap_protocol(self.hints.tmux, &draw.payload));
            }
            next.insert(
                key,
                Placement {
                    x: draw.x,
                    y: draw.y,
                    cols: draw.cols,
                    rows: draw.rows,
                    signature: draw.signature,
                    protocol: draw.protocol,
                    source: draw.source,
                    placement_id: draw.placement_id,
                },
            );
        }
        for ((image_id, placement_id), placed) in &self.placed {
            if next.contains_key(&(*image_id, *placement_id))
                || placed.protocol != GraphicsProtocol::Kitty
            {
                continue;
            }
            out.push_str(&wrap_protocol(
                self.hints.tmux,
                &kitty_delete_placement(*image_id, *placement_id),
            ));
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
    read_bounded_body(response, MAX_IMAGE_BYTES).await
}

/// Reads the body in chunks and stops as soon as it passes `max_bytes`.
async fn read_bounded_body(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "download failed".to_string())?
    {
        if !append_bounded(&mut bytes, &chunk, max_bytes) {
            return Err("too large".to_string());
        }
    }
    Ok(bytes)
}

fn append_bounded(bytes: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> bool {
    if bytes.len().saturating_add(chunk.len()) > max_bytes {
        return false;
    }
    bytes.extend_from_slice(chunk);
    true
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
            } else if hints.truecolor {
                ImagePresentation::Halfblocks
            } else {
                ImagePresentation::Placeholder
            }
        }
    }
}

/// Kitty, then iTerm2, then Sixel.
///
/// A successful Kitty or Sixel query wins inside every multiplexer, including
/// Herdr, tmux, and Zellij. Those three parse or relay the protocol, but their
/// environment variables describe the wrong terminal, so hints are used only
/// after the query fails and only outside a multiplexer. GNU screen does not
/// relay graphics; a failed query there stays on half-blocks.
pub(super) fn detected_graphics(hints: &TerminalHints, probe: Probe) -> Option<GraphicsProtocol> {
    if probe.kitty {
        return Some(GraphicsProtocol::Kitty);
    }
    if hints.query_required() {
        return probe.sixel.then_some(GraphicsProtocol::Sixel);
    }
    if hints.prefer_kitty() {
        return Some(GraphicsProtocol::Kitty);
    }
    if hints.prefer_iterm() {
        return Some(GraphicsProtocol::Iterm2);
    }
    if probe.sixel || hints.prefer_sixel() {
        return Some(GraphicsProtocol::Sixel);
    }
    None
}

pub(super) fn parse_probe_response(bytes: &[u8]) -> Probe {
    let text = String::from_utf8_lossy(bytes);
    let (cell_height, cell_width) = parse_cell_pixels(bytes).unwrap_or((0, 0));
    Probe {
        kitty: text.contains("i=31;OK") || text.contains("_Gi=31;OK"),
        sixel: da1_has_sixel(bytes),
        cell_width,
        cell_height,
    }
}

/// `CSI 6 ; height ; width t` from `CSI 16 t`.
pub(super) fn parse_cell_pixels(bytes: &[u8]) -> Option<(u16, u16)> {
    let mut index = 0;
    while index + 3 < bytes.len() {
        if bytes[index] != 0x1b || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let mut cursor = index + 2;
        let mut numbers = Vec::new();
        let mut number = 0u16;
        let mut saw_digit = false;
        let mut valid = true;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'0'..=b'9' => {
                    let digit = u16::from(bytes[cursor] - b'0');
                    number = number.saturating_mul(10).saturating_add(digit);
                    saw_digit = true;
                }
                b';' if saw_digit => {
                    numbers.push(number);
                    number = 0;
                    saw_digit = false;
                }
                b't' if saw_digit => {
                    numbers.push(number);
                    if numbers.first() == Some(&6) && numbers.len() >= 3 {
                        let height = numbers[1];
                        let width = numbers[2];
                        if width > 0 && height > 0 {
                            return Some((height, width));
                        }
                    }
                    break;
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
    None
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
    let parts = [KITTY_GRAPHICS_QUERY, DA1_QUERY, CELL_SIZE_QUERY];
    if tmux {
        parts.into_iter().map(tmux_passthrough).collect()
    } else {
        parts.concat()
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
    let mut probe = parse_probe_response(&read_probe_response(PROBE_TIMEOUT));
    if probe.cell_width == 0 || probe.cell_height == 0 {
        if let Some((width, height)) = ioctl_cell_pixels() {
            probe.cell_width = width;
            probe.cell_height = height;
        }
    }
    probe
}

fn ioctl_cell_pixels() -> Option<(u16, u16)> {
    #[cfg(unix)]
    {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
        if rc != 0 || size.ws_col == 0 || size.ws_row == 0 {
            return None;
        }
        // Multiplexers often report a character grid and zero pixel size.
        if size.ws_xpixel == 0 || size.ws_ypixel == 0 {
            return None;
        }
        let width = size.ws_xpixel / size.ws_col;
        let height = size.ws_ypixel / size.ws_row;
        (width > 0 && height > 0).then_some((width, height))
    }
    #[cfg(not(unix))]
    {
        None
    }
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
    cell: CellPixels,
) -> (u16, u16) {
    if pixel_w == 0 || pixel_h == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let max_cols = max_cols.min(usize::from(u16::MAX));
    let max_rows = max_rows.min(64);
    let cell_w = u64::from(cell.width.max(1));
    let cell_h = u64::from(cell.height.max(1));
    // Use the full viewport width when the picture is at least that wide.
    // A narrower picture stays at its pixel width so a tiny icon is not stretched
    // across the window.
    let mut cols = max_cols.min(pixel_w as usize).max(1);
    let mut rows = ((u64::from(pixel_h) * cols as u64 * cell_w) / (u64::from(pixel_w) * cell_h))
        .max(1) as usize;
    if rows > max_rows {
        rows = max_rows;
        cols = ((rows as u64 * u64::from(pixel_w) * cell_h) / (u64::from(pixel_h) * cell_w)).max(1)
            as usize;
        cols = cols.min(max_cols).max(1);
    }
    (cols as u16, rows.min(max_rows).max(1) as u16)
}

/// Pixel size of a graphics payload. Never larger than the source, and never
/// a cell-count bitmap. Kitty `c`/`r` fit this raster into the cell rectangle.
pub(super) fn graphics_pixel_size(
    image: &DecodedImage,
    cols: u16,
    rows: u16,
    cell: CellPixels,
) -> (u32, u32) {
    let max_w = u32::from(cols)
        .saturating_mul(u32::from(cell.width.max(1)))
        .clamp(1, 4096);
    let max_h = u32::from(rows)
        .saturating_mul(u32::from(cell.height.max(1)))
        .clamp(1, 4096);
    fit_inside(image.width, image.height, max_w, max_h)
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

#[cfg(test)]
pub(super) fn halfblock_colors(
    image: &DecodedImage,
    cols: u32,
    rows: u32,
    col: u32,
    row: u32,
    background: [u8; 3],
) -> (Color, Color) {
    let top = area_sample(
        image,
        cols,
        rows.saturating_mul(2),
        col,
        row.saturating_mul(2),
        background,
    );
    let bottom = area_sample(
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

/// One text cell. Quadrant block elements (same Unicode block as `▀`) give a
/// 2×2 sample. Sextants stay unused: the Legacy Computing block is missing
/// from many fonts, including some Nerd Fonts.
pub(super) fn quadrant_cell(grid: &DecodedImage, col: u32, row: u32) -> (char, Color, Color) {
    let samples = [
        grid_pixel(grid, col * 2, row * 2),
        grid_pixel(grid, col * 2 + 1, row * 2),
        grid_pixel(grid, col * 2, row * 2 + 1),
        grid_pixel(grid, col * 2 + 1, row * 2 + 1),
    ];
    let mut darkest = samples[0];
    let mut lightest = samples[0];
    for sample in samples {
        if luminance(sample) < luminance(darkest) {
            darkest = sample;
        }
        if luminance(sample) > luminance(lightest) {
            lightest = sample;
        }
    }
    if color_distance(darkest, lightest) < 24 * 24 {
        return (
            ' ',
            Color::Rgb {
                r: lightest[0],
                g: lightest[1],
                b: lightest[2],
            },
            Color::Rgb {
                r: darkest[0],
                g: darkest[1],
                b: darkest[2],
            },
        );
    }
    let mut mask = 0u8;
    for (bit, sample) in samples.iter().enumerate() {
        if color_distance(*sample, lightest) <= color_distance(*sample, darkest) {
            mask |= 1 << bit;
        }
    }
    (
        quadrant_char(mask),
        Color::Rgb {
            r: lightest[0],
            g: lightest[1],
            b: lightest[2],
        },
        Color::Rgb {
            r: darkest[0],
            g: darkest[1],
            b: darkest[2],
        },
    )
}

fn quadrant_char(mask: u8) -> char {
    match mask & 0b1111 {
        0b0001 => '▘',
        0b0010 => '▝',
        0b0100 => '▖',
        0b1000 => '▗',
        0b0011 => '▀',
        0b1100 => '▄',
        0b0101 => '▌',
        0b1010 => '▐',
        0b1001 => '▚',
        0b0110 => '▞',
        0b0111 => '▛',
        0b1011 => '▜',
        0b1101 => '▙',
        0b1110 => '▟',
        0b1111 => '█',
        _ => ' ',
    }
}

fn grid_pixel(grid: &DecodedImage, x: u32, y: u32) -> [u8; 3] {
    if grid.width == 0 || grid.height == 0 {
        return [0, 0, 0];
    }
    let x = x.min(grid.width - 1);
    let y = y.min(grid.height - 1);
    let index = ((y * grid.width + x) * 4) as usize;
    let pixel = grid.rgba.get(index..index + 4).unwrap_or(&[0, 0, 0, 255]);
    [pixel[0], pixel[1], pixel[2]]
}

fn luminance(pixel: [u8; 3]) -> u16 {
    u16::from(pixel[0]) * 3 + u16::from(pixel[1]) * 6 + u16::from(pixel[2])
}

fn color_distance(left: [u8; 3], right: [u8; 3]) -> u32 {
    let red = i32::from(left[0]) - i32::from(right[0]);
    let green = i32::from(left[1]) - i32::from(right[1]);
    let blue = i32::from(left[2]) - i32::from(right[2]);
    (red * red + green * green + blue * blue) as u32
}

#[cfg(test)]
fn area_sample(
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
    let x0 = u64::from(x) * u64::from(image.width) / u64::from(dest_w);
    let x1 = (u64::from(x) + 1) * u64::from(image.width) / u64::from(dest_w);
    let y0 = u64::from(y) * u64::from(image.height) / u64::from(dest_h);
    let y1 = (u64::from(y) + 1) * u64::from(image.height) / u64::from(dest_h);
    let x1 = x1.max(x0 + 1).min(u64::from(image.width));
    let y1 = y1.max(y0 + 1).min(u64::from(image.height));
    let mut red = 0u64;
    let mut green = 0u64;
    let mut blue = 0u64;
    let mut count = 0u64;
    for sy in y0..y1 {
        for sx in x0..x1 {
            let index = ((sy as u32 * image.width + sx as u32) * 4) as usize;
            let pixel = image.rgba.get(index..index + 4).unwrap_or(&[0, 0, 0, 0]);
            let composed = composite(pixel, background);
            red += u64::from(composed[0]);
            green += u64::from(composed[1]);
            blue += u64::from(composed[2]);
            count += 1;
        }
    }
    if count == 0 {
        return background;
    }
    [
        (red / count) as u8,
        (green / count) as u8,
        (blue / count) as u8,
    ]
}

#[cfg(test)]
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

/// Cell rectangles of `image` that do not intersect any overlay.
///
/// One popup yields at most four rectangles: above, below, left, and right.
pub(super) fn uncovered_cell_rects(
    image: crate::ui::ScreenRect,
    overlays: &[crate::ui::ScreenRect],
) -> Vec<crate::ui::ScreenRect> {
    let mut rects = vec![image];
    for overlay in overlays {
        let mut next = Vec::new();
        for rect in rects {
            next.extend(subtract_cell_rect(rect, *overlay));
        }
        rects = next;
    }
    rects.retain(|rect| rect.width > 0 && rect.height > 0);
    rects
}

fn subtract_cell_rect(
    image: crate::ui::ScreenRect,
    overlay: crate::ui::ScreenRect,
) -> Vec<crate::ui::ScreenRect> {
    if !rects_overlap(image, overlay) {
        return vec![image];
    }
    let image_right = image.x.saturating_add(image.width);
    let image_bottom = image.y.saturating_add(image.height);
    let overlap_left = overlay.x.max(image.x);
    let overlap_top = overlay.y.max(image.y);
    let overlap_right = overlay.x.saturating_add(overlay.width).min(image_right);
    let overlap_bottom = overlay.y.saturating_add(overlay.height).min(image_bottom);
    let mut pieces = Vec::new();
    if overlap_top > image.y {
        pieces.push(crate::ui::ScreenRect {
            x: image.x,
            y: image.y,
            width: image.width,
            height: overlap_top - image.y,
        });
    }
    if image_bottom > overlap_bottom {
        pieces.push(crate::ui::ScreenRect {
            x: image.x,
            y: overlap_bottom,
            width: image.width,
            height: image_bottom - overlap_bottom,
        });
    }
    let middle_height = overlap_bottom.saturating_sub(overlap_top);
    if overlap_left > image.x && middle_height > 0 {
        pieces.push(crate::ui::ScreenRect {
            x: image.x,
            y: overlap_top,
            width: overlap_left - image.x,
            height: middle_height,
        });
    }
    if image_right > overlap_right && middle_height > 0 {
        pieces.push(crate::ui::ScreenRect {
            x: overlap_right,
            y: overlap_top,
            width: image_right - overlap_right,
            height: middle_height,
        });
    }
    pieces
}

fn source_for_piece(
    image: crate::ui::ScreenRect,
    piece: crate::ui::ScreenRect,
    base: KittySourceRect,
    raster_h: u32,
) -> Option<KittySourceRect> {
    if image.width == 0 || image.height == 0 || base.w == 0 || base.h == 0 {
        return None;
    }
    if piece.x == image.x
        && piece.y == image.y
        && piece.width == image.width
        && piece.height == image.height
    {
        // A crop that starts at y=0 is still a crop. Dropping it makes Kitty
        // scale the whole transmitted image into the visible rows.
        let full_raster = base.x == 0 && base.y == 0 && base.h >= raster_h.max(1);
        return (!full_raster).then_some(base);
    }
    let x0 = base.x
        + u32::try_from(piece.x.saturating_sub(image.x)).unwrap_or(0) * base.w
            / u32::try_from(image.width).unwrap_or(1).max(1);
    let x1 = base.x
        + u32::try_from(piece.x.saturating_add(piece.width).saturating_sub(image.x)).unwrap_or(0)
            * base.w
            / u32::try_from(image.width).unwrap_or(1).max(1);
    let y0 = base.y
        + u32::try_from(piece.y.saturating_sub(image.y)).unwrap_or(0) * base.h
            / u32::try_from(image.height).unwrap_or(1).max(1);
    let y1 = base.y
        + u32::try_from(piece.y.saturating_add(piece.height).saturating_sub(image.y)).unwrap_or(0)
            * base.h
            / u32::try_from(image.height).unwrap_or(1).max(1);
    Some(KittySourceRect {
        x: x0.min(base.x.saturating_add(base.w.saturating_sub(1))),
        y: y0.min(base.y.saturating_add(base.h.saturating_sub(1))),
        w: x1.saturating_sub(x0).max(1).min(base.w),
        h: y1.saturating_sub(y0).max(1).min(base.h),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KittySourceRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[cfg(test)]
pub(super) fn kitty_transmit(id: u32, image: &DecodedImage, cols: u16, rows: u16) -> String {
    kitty_transmit_with_cell(id, image, cols, rows, CellPixels::default())
}

#[cfg(test)]
pub(super) fn kitty_transmit_with_cell(
    id: u32,
    image: &DecodedImage,
    cols: u16,
    rows: u16,
    cell: CellPixels,
) -> String {
    kitty_transmit_placed(id, image, cols, rows, rows, None, cell)
}

#[cfg(test)]
fn kitty_transmit_placed(
    id: u32,
    image: &DecodedImage,
    cols: u16,
    total_rows: u16,
    visible_rows: u16,
    source: Option<KittySourceRect>,
    cell: CellPixels,
) -> String {
    let (width, height) = graphics_pixel_size(image, cols, total_rows.max(1), cell);
    let rgba = resample_rgba(image, width, height);
    let payload = base64::engine::general_purpose::STANDARD.encode(&rgba);
    let mut control = format!("a=T,f=32,s={width},v={height},i={id},p={id}");
    if let Some(source) = source {
        control.push_str(&format!(
            ",x={},y={},w={},h={}",
            source.x, source.y, source.w, source.h
        ));
    }
    control.push_str(&format!(",c={cols},r={visible_rows},q=2,t=d,z=-1"));
    chunk_kitty(&control, &payload)
}

/// Source rectangle for the visible slice of an image, in transmitted pixels.
///
/// `None` means the whole image is on screen and no crop is required.
pub(super) fn kitty_source_rect(
    pixel_w: u32,
    pixel_h: u32,
    total_rows: u16,
    first_index: u16,
    visible_rows: u16,
) -> Option<KittySourceRect> {
    if total_rows == 0 || visible_rows == 0 || pixel_h == 0 {
        return None;
    }
    if first_index == 0 && visible_rows >= total_rows {
        return None;
    }
    // One scale for every scroll offset: hidden rows × pixels-per-row.
    let per_row = (pixel_h / u32::from(total_rows).max(1)).max(1);
    let y = u32::from(first_index.min(total_rows)).saturating_mul(per_row);
    let visible = u32::from(visible_rows.min(total_rows.saturating_sub(first_index))).max(1);
    let y = y.min(pixel_h.saturating_sub(1));
    let h = visible
        .saturating_mul(per_row)
        .min(pixel_h.saturating_sub(y))
        .max(1);
    Some(KittySourceRect {
        x: 0,
        y,
        w: pixel_w.max(1),
        h,
    })
}

fn kitty_transmit_data(
    id: u32,
    image: &DecodedImage,
    cols: u16,
    rows: u16,
    cell: CellPixels,
) -> String {
    let (width, height) = graphics_pixel_size(image, cols, rows.max(1), cell);
    let rgba = resample_rgba(image, width, height);
    let payload = base64::engine::general_purpose::STANDARD.encode(&rgba);
    chunk_kitty(
        &format!("a=t,f=32,s={width},v={height},i={id},q=2"),
        &payload,
    )
}

pub(super) fn kitty_place(
    id: u32,
    placement_id: u32,
    cols: u16,
    rows: u16,
    source: Option<KittySourceRect>,
) -> String {
    let mut control = format!("a=p,i={id},p={placement_id}");
    if let Some(source) = source {
        control.push_str(&format!(
            ",x={},y={},w={},h={}",
            source.x, source.y, source.w, source.h
        ));
    }
    control.push_str(&format!(",c={cols},r={rows},q=2,z=-1"));
    format!("\u{1b}_G{control}\u{1b}\\")
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

#[cfg(test)]
pub(super) fn kitty_delete(id: u32) -> String {
    format!("\u{1b}_Ga=d,d=i,i={id},q=2\u{1b}\\")
}

pub(super) fn kitty_delete_placement(image_id: u32, placement_id: u32) -> String {
    // Lowercase `d=i` removes one placement and keeps the transmitted pixels.
    format!("\u{1b}_Ga=d,d=i,i={image_id},p={placement_id},q=2\u{1b}\\")
}

pub(super) fn kitty_delete_all() -> String {
    "\u{1b}_Ga=d,d=A,q=2\u{1b}\\".to_string()
}

#[cfg(test)]
pub(super) fn iterm_transmit(image: &DecodedImage, cols: u16, rows: u16) -> String {
    iterm_transmit_rgba(
        &image.rgba,
        image.width.max(1),
        image.height.max(1),
        cols,
        rows,
    )
}

fn iterm_transmit_rgba(rgba: &[u8], width: u32, height: u32, cols: u16, rows: u16) -> String {
    let mut png = Vec::new();
    let encoded = image::codecs::png::PngEncoder::new(&mut png).write_image(
        rgba,
        width.max(1),
        height.max(1),
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

fn crop_rgba(
    rgba: &[u8],
    width: u32,
    height: u32,
    source: Option<KittySourceRect>,
) -> (u32, u32, Vec<u8>) {
    let Some(source) = source else {
        return (width.max(1), height.max(1), rgba.to_vec());
    };
    if width == 0 || height == 0 || source.w == 0 || source.h == 0 {
        return (width.max(1), height.max(1), rgba.to_vec());
    }
    let x = source.x.min(width.saturating_sub(1));
    let y = source.y.min(height.saturating_sub(1));
    let crop_w = source.w.min(width - x).max(1);
    let crop_h = source.h.min(height - y).max(1);
    let mut cropped = Vec::with_capacity((crop_w * crop_h * 4) as usize);
    for row in y..y + crop_h {
        let start = ((row * width + x) * 4) as usize;
        let end = start + (crop_w * 4) as usize;
        if let Some(slice) = rgba.get(start..end) {
            cropped.extend_from_slice(slice);
        }
    }
    (crop_w, crop_h, cropped)
}

#[cfg(test)]
pub(super) fn sixel_transmit(
    image: &DecodedImage,
    cols: u16,
    rows: u16,
    cell: CellPixels,
) -> String {
    let (width, height) = graphics_pixel_size(image, cols, rows, cell);
    let rgba = resample_rgba(image, width, height);
    encode_sixel(&rgba, width as usize, height as usize)
}

fn fit_inside(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let max_w = max_w.max(1);
    let max_h = max_h.max(1);
    if src_w == 0 || src_h == 0 {
        return (1, 1);
    }
    let width = src_w.min(max_w).max(1);
    let height = src_h.min(max_h).max(1);
    if u64::from(src_w) * u64::from(height) > u64::from(src_h) * u64::from(width) {
        let height = ((u64::from(width) * u64::from(src_h)) / u64::from(src_w).max(1))
            .clamp(1, u64::from(max_h)) as u32;
        (width, height)
    } else {
        let width = ((u64::from(height) * u64::from(src_w)) / u64::from(src_h).max(1))
            .clamp(1, u64::from(max_w)) as u32;
        (width, height)
    }
}

fn resample_rgba(image: &DecodedImage, width: u32, height: u32) -> Vec<u8> {
    let width = width.max(1);
    let height = height.max(1);
    if image.width == 0 || image.height == 0 {
        return vec![0; (width * height * 4) as usize];
    }
    if width == image.width && height == image.height {
        return image.rgba.clone();
    }
    let Some(buffer) = image::RgbaImage::from_raw(image.width, image.height, image.rgba.clone())
    else {
        return nearest_rgba(image, width, height);
    };
    // Lanczos when shrinking, Catmull-Rom if a caller ever asks for a larger
    // raster. Graphics payloads only shrink.
    let filter = if width < image.width || height < image.height {
        image::imageops::FilterType::Lanczos3
    } else {
        image::imageops::FilterType::CatmullRom
    };
    image::imageops::resize(&buffer, width, height, filter).into_raw()
}

fn nearest_rgba(image: &DecodedImage, width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let sx = (u64::from(x) * u64::from(image.width) / u64::from(width))
                .min(u64::from(image.width.saturating_sub(1))) as u32;
            let sy = (u64::from(y) * u64::from(image.height) / u64::from(height))
                .min(u64::from(image.height.saturating_sub(1))) as u32;
            let from = ((sy * image.width + sx) * 4) as usize;
            let to = ((y * width + x) * 4) as usize;
            if let Some(pixel) = image.rgba.get(from..from + 4) {
                rgba[to..to + 4].copy_from_slice(pixel);
            }
        }
    }
    rgba
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
        if self.config.markdown_images == MarkdownImagesMode::Off {
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

    pub(super) fn seal_markdown_image_fingerprints(&self, buffer: &crate::editor::RenderBuffer) {
        self.markdown_images.borrow_mut().seal_fingerprints(buffer);
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
            // Sixel and iTerm2 have no z-order. Rewriting every cell clears
            // their pixels when a popup covers them, and again when they return.
            // A switch away from block glyphs also needs the whole image region
            // sent, not only the cells whose text happened to differ.
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
        // Block glyphs have their own background. Under a z=-1 graphic those
        // cells punch holes in the picture, and hiding the placement for a
        // popup leaves only the blocks. Graphics mode keeps the rectangle blank.
        if matches!(presentation, ImagePresentation::Halfblocks) {
            let grid = self
                .markdown_images
                .borrow_mut()
                .text_samples(&image, span.cols, span.rows);
            for col in 0..usize::from(span.cols).min(content_width) {
                let (glyph, fg, bg) = quadrant_cell(&grid, col as u32, u32::from(span.index));
                let mut encoded = [0u8; 4];
                let text = glyph.encode_utf8(&mut encoded);
                buffer.set_text(
                    origin_x + col,
                    origin_y,
                    text,
                    &crate::theme::Style {
                        fg: Some(fg),
                        bg: Some(bg),
                        ..crate::theme::Style::default()
                    },
                );
            }
        }
        if let ImagePresentation::Graphics(protocol) = presentation {
            let (Ok(x), Ok(y)) = (u16::try_from(origin_x), u16::try_from(origin_y)) else {
                return true;
            };
            let buffer_id = self.buffer_manager[window.buffer_index].id();
            let mut runtime = self.markdown_images.borrow_mut();
            let id = runtime.id_for(buffer_id, segment.line);
            runtime.observe_graphics_row(ObservedGraphics {
                id,
                x,
                y,
                cols: span.cols,
                total_rows: span.rows,
                index: span.index,
                signature: image_signature(&image, span.cols, span.rows, protocol),
                protocol,
                image: Arc::clone(&image),
            });
        }
        true
    }
}

fn text_sample_key(image: &DecodedImage) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    image.width.hash(&mut hasher);
    image.height.hash(&mut hasher);
    image.rgba.len().hash(&mut hasher);
    image.rgba.first().hash(&mut hasher);
    image.rgba.last().hash(&mut hasher);
    hasher.finish()
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
        let (cols, rows) = image_cell_grid(200, 100, 80, 16, CellPixels::default());
        assert_eq!(rows, 16);
        assert_eq!(cols, 64);

        let (cols, rows) = image_cell_grid(100, 400, 80, 16, CellPixels::default());
        assert_eq!(rows, 16);
        assert_eq!(cols, 8);

        let (_, rows) = image_cell_grid(2, 2, 80, 16, CellPixels::default());
        assert_eq!(rows, 1);
        assert_eq!(
            image_cell_grid(0, 10, 80, 16, CellPixels::default()),
            (0, 0)
        );
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
        assert!(revealed.rendered(0).is_some());
        assert!(revealed.raw_lines.contains(&0));
        let hidden_rows = hidden.rendered(0).unwrap().len();
        let revealed_rows = revealed.rendered(0).unwrap().len();
        assert_eq!(hidden_rows, revealed_rows);
    }

    #[test]
    fn layout_reserves_vertical_space_for_an_image_line() {
        let image = DecodedImage {
            width: 200,
            height: 100,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
        };
        // The grid helper is what layout uses once pixels are known.
        let (cols, rows) =
            image_cell_grid(image.width, image.height, 80, 10, CellPixels::default());
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
                    sixel: true,
                    ..Probe::default()
                }
            ),
            Some(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            detected_graphics(
                &queried,
                Probe {
                    kitty: false,
                    sixel: true,
                    ..Probe::default()
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
                    sixel: false,
                    ..Probe::default()
                }
            ),
            Some(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            choose_presentation(
                MarkdownImagesMode::Auto,
                &kitty_tmux,
                Probe {
                    kitty: true,
                    sixel: false,
                    ..Probe::default()
                }
            ),
            ImagePresentation::Graphics(GraphicsProtocol::Kitty)
        );
        let wrapped = tmux_passthrough(KITTY_GRAPHICS_QUERY);
        assert!(wrapped.starts_with("\u{1b}Ptmux;\u{1b}"));
        assert!(wrapped.contains("\u{1b}\u{1b}_G"));
        assert!(wrapped.ends_with("\u{1b}\\"));
        assert!(probe_query(true).contains("Ptmux"));
        assert!(probe_query(false).starts_with(KITTY_GRAPHICS_QUERY));
        assert!(probe_query(false).contains(DA1_QUERY));
        assert!(probe_query(false).contains(CELL_SIZE_QUERY));
        assert!(probe_query(true).contains(CELL_SIZE_QUERY));
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
                    sixel: true,
                    ..Probe::default()
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

        let sixel = sixel_transmit(&image, 1, 1, CellPixels::default());
        assert!(sixel.starts_with("\u{1b}Pq"));
        assert!(sixel.ends_with("\u{1b}\\"));

        let iterm = iterm_transmit(&image, 2, 2);
        assert!(iterm.starts_with("\u{1b}]1337;File=inline=1;"));
        assert!(iterm.contains("width=2"));
        assert!(iterm.ends_with('\u{7}'));
    }

    #[test]
    fn graphics_rasters_use_cell_pixels_not_cell_counts() {
        let image = DecodedImage {
            width: 80,
            height: 40,
            rgba: vec![200u8; 80 * 40 * 4],
        };
        let cell = CellPixels {
            width: 10,
            height: 20,
        };
        // 4 columns by 2 rows is 40×40 pixels. The 80×40 source fits as 40×20,
        // not the old 4×8 cell-count bitmap.
        assert_eq!(graphics_pixel_size(&image, 4, 2, cell), (40, 20));
        let kitty = kitty_transmit_with_cell(3, &image, 4, 2, cell);
        let control = kitty.split(';').next().unwrap();
        assert!(control.contains("s=40"), "{control}");
        assert!(control.contains("v=20"), "{control}");
        assert!(control.contains("c=4"), "{control}");
        assert!(control.contains("r=2"), "{control}");
        assert!(!control.contains("s=16"), "{control}");
        let sixel = sixel_transmit(&image, 4, 2, cell);
        let tiny = sixel_transmit(
            &DecodedImage {
                width: 4,
                height: 2,
                rgba: vec![1u8; 4 * 2 * 4],
            },
            4,
            2,
            CellPixels {
                width: 1,
                height: 1,
            },
        );
        assert!(
            sixel.len() > tiny.len(),
            "sixel at cell-pixel size should carry more raster than a cell-count bitmap"
        );
    }

    #[test]
    fn resampling_mixes_neighbors_instead_of_copying_one_pixel() {
        let image = DecodedImage {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
            ],
        };
        let nearest = nearest_rgba(&image, 1, 1);
        let mixed = resample_rgba(&image, 1, 1);
        assert_eq!(&nearest[..3], &[255, 0, 0]);
        assert_ne!(&mixed[..3], &[255, 0, 0]);
        let grid = DecodedImage {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255, 255,
            ],
        };
        let (glyph, fg, bg) = quadrant_cell(&grid, 0, 0);
        assert_eq!(glyph, '▀');
        assert_eq!(fg, Color::Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(bg, Color::Rgb { r: 0, g: 0, b: 255 });
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
                rgba[index + usize::from((x + y) % 2 == 1) * 2] = 255;
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
        let source = format!("hello\n![swatch]({})\nafter\n", path.display());
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
        let swatch = frame
            .cells
            .iter()
            .find(|cell| {
                matches!(
                    cell.style.fg.or(cell.style.bg),
                    Some(crate::color::Color::Rgb { r, g, b }) if r > 150 && g < 80 && b < 80
                )
            })
            .unwrap();
        assert!(
            is_block_glyph(&swatch.text) || swatch.text == " ",
            "image cell {:?} screen {screen}",
            swatch.text
        );
        assert!(
            screen
                .chars()
                .any(|glyph| is_block_glyph(&glyph.to_string())),
            "{screen}"
        );
        assert!(!screen.contains("![swatch]"), "{screen}");

        let after_row = frame_row(&frame, "after");
        editor.cy = 1;
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(screen.contains("![swatch]"), "{screen}");
        assert_eq!(
            frame_row(&frame, "after"),
            after_row,
            "the declaration stays inside the image block: {screen}"
        );
        assert!(
            screen
                .chars()
                .any(|glyph| is_block_glyph(&glyph.to_string())),
            "the image stays under the declaration line: {screen}"
        );
    }

    #[test]
    fn kitty_image_cells_stay_blank_when_a_popup_opens() {
        use std::collections::HashMap;

        use crate::{
            buffer::Buffer,
            config::{Config, KeyAction},
            editor::render_buffer::RenderBuffer,
            theme::{Style, Theme},
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("swatch.png");
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for y in 0..8 {
            for x in 0..8 {
                let index = (y * 8 + x) * 4;
                rgba[index] = 220;
                rgba[index + 1] = if y > 4 { 180 } else { 20 };
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
        config.markdown_images = MarkdownImagesMode::Kitty;
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
        assert!(!screen.contains("![swatch]"), "{screen}");
        assert!(
            !screen
                .chars()
                .any(|glyph| is_block_glyph(&glyph.to_string())),
            "graphics mode must not paint block cells under the image: {screen}"
        );

        editor.keymap_hint_prefix = vec!["g".to_string()];
        editor.waiting_key_action = Some(KeyAction::Nested(HashMap::from([(
            "j".to_string(),
            KeyAction::Single(crate::editor::Action::MoveScreenLineDown),
        )])));
        editor.keymap_hints_visible = true;
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(screen.contains("g · keymaps"), "{screen}");
        assert!(
            !screen
                .chars()
                .any(|glyph| is_block_glyph(&glyph.to_string())),
            "a popup must not switch the image to block glyphs: {screen}"
        );
    }

    fn is_block_glyph(text: &str) -> bool {
        matches!(
            text,
            "▀" | "▄"
                | "▌"
                | "▐"
                | "█"
                | "▖"
                | "▗"
                | "▘"
                | "▝"
                | "▚"
                | "▞"
                | "▛"
                | "▜"
                | "▙"
                | "▟"
        )
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
        let popup = ScreenRect {
            x: 8,
            y: 4,
            width: 6,
            height: 3,
        };
        let pieces = uncovered_cell_rects(image, &[popup]);
        assert!(pieces.len() <= 4, "{pieces:?}");
        assert!(!pieces.is_empty());
        for piece in &pieces {
            assert!(!rects_overlap(*piece, popup), "{piece:?}");
        }
    }

    #[test]
    fn kitty_placement_stays_under_an_overlay_and_sixel_is_removed() {
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
            total_rows: 2,
            first_index: 0,
            painted: 2,
            signature: 9,
            protocol: GraphicsProtocol::Kitty,
            payload: "PLACE".to_string(),
            source: None,
            pixel_w: 40,
            pixel_h: 20,
            decoded: None,
            cells: Some(hash_region(buffer, rect)),
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
        runtime.staged[0].cells = runtime.staged[0].cells.map(|cells| cells.wrapping_add(1));
        assert!(runtime.commit_visible(&buffer));
        let split = runtime.graphics_payload(false);
        assert!(split.contains("a=p"), "{split}");
        assert!(split.contains("a=d"), "{split}");
        assert_placements_miss(
            &split,
            crate::ui::ScreenRect {
                x: 3,
                y: 2,
                width: 5,
                height: 1,
            },
        );

        let mut sixel = stage(&buffer);
        sixel.protocol = GraphicsProtocol::Sixel;
        sixel.id = 9;
        sixel.payload = "SIXEL".to_string();
        let mut sixel_runtime = ImageRuntime::new();
        sixel_runtime.begin_frame();
        sixel_runtime.stage_graphics(sixel);
        let _ = sixel_runtime.commit_visible(&buffer);
        let hidden = sixel_runtime.graphics_payload(false);
        assert!(!hidden.contains("SIXEL"), "{hidden}");
    }

    fn assert_placements_miss(payload: &str, popup: crate::ui::ScreenRect) {
        for (x, y, cols, rows) in placement_rects(payload) {
            let piece = crate::ui::ScreenRect {
                x: usize::from(x),
                y: usize::from(y),
                width: usize::from(cols),
                height: usize::from(rows),
            };
            assert!(
                !rects_overlap(piece, popup),
                "placement {piece:?} intersects popup {popup:?} in {payload}"
            );
        }
    }

    fn placement_rects(payload: &str) -> Vec<(u16, u16, u16, u16)> {
        let mut rects = Vec::new();
        let mut cursor_x = 1u16;
        let mut cursor_y = 1u16;
        for segment in payload.split('\u{1b}') {
            if let Some(rest) = segment.strip_prefix('[') {
                if let Some((position, _)) = rest.split_once('H') {
                    let mut parts = position.split(';');
                    if let (Some(y), Some(x)) = (parts.next(), parts.next()) {
                        cursor_y = y.parse().unwrap_or(1);
                        cursor_x = x.parse().unwrap_or(1);
                    }
                }
            }
            if segment.starts_with("_Ga=p") {
                let control = segment.split(';').next().unwrap_or(segment);
                let mut cols = 0u16;
                let mut rows = 0u16;
                for field in control.split(',') {
                    if let Some(value) = field.strip_prefix("c=") {
                        cols = value.parse().unwrap_or(0);
                    }
                    if let Some(value) = field.strip_prefix("r=") {
                        rows = value.parse().unwrap_or(0);
                    }
                }
                if cols > 0 && rows > 0 {
                    rects.push((
                        cursor_x.saturating_sub(1),
                        cursor_y.saturating_sub(1),
                        cols,
                        rows,
                    ));
                }
            }
        }
        rects
    }

    fn place_kitty(
        runtime: &mut ImageRuntime,
        buffer: &crate::editor::RenderBuffer,
        id: u32,
        y: u16,
    ) {
        runtime.begin_frame();
        runtime.stage_graphics(StagedImage {
            id,
            x: 2,
            y,
            cols: 4,
            rows: 2,
            total_rows: 2,
            first_index: 0,
            painted: 2,
            signature: 9,
            protocol: GraphicsProtocol::Kitty,
            payload: "PLACE".to_string(),
            source: None,
            pixel_w: 40,
            pixel_h: 20,
            decoded: None,
            cells: Some(hash_region(
                buffer,
                crate::ui::ScreenRect {
                    x: 2,
                    y: usize::from(y),
                    width: 4,
                    height: 2,
                },
            )),
        });
        runtime.commit_visible(buffer);
    }

    #[test]
    fn forced_redraw_deletes_kitty_ids_before_forgetting_them() {
        use crate::theme::Style;
        let mut runtime = ImageRuntime::new();
        let buffer = crate::editor::RenderBuffer::new(20, 12, &Style::default());
        place_kitty(&mut runtime, &buffer, 7, 4);
        let shown = runtime.graphics_payload(false);
        assert!(shown.contains("PLACE"), "{shown}");
        assert!(!shown.contains("a=d"), "{shown}");

        // PageDown takes the image off the staged set and asks for a full
        // redraw. The delete has to go out before `placed` is cleared.
        runtime.begin_frame();
        assert!(runtime.commit_visible(&buffer));
        let deleted = runtime.graphics_payload(true);
        assert!(deleted.contains("a=d"), "{deleted}");
        assert!(deleted.contains("i=7"), "{deleted}");
        assert!(!deleted.contains("PLACE"), "{deleted}");
        assert!(runtime.placed.is_empty());
    }

    #[test]
    fn scroll_deletes_a_placement_that_left_the_screen_and_moves_one_that_stayed() {
        use crate::theme::Style;
        let mut runtime = ImageRuntime::new();
        let buffer = crate::editor::RenderBuffer::new(20, 20, &Style::default());
        place_kitty(&mut runtime, &buffer, 4, 3);
        let _ = runtime.graphics_payload(false);

        runtime.begin_frame();
        assert!(runtime.commit_visible(&buffer));
        let gone = runtime.graphics_payload(false);
        assert!(gone.contains("a=d"), "{gone}");
        assert!(gone.contains("i=4"), "{gone}");
        assert!(!gone.contains("PLACE"), "{gone}");

        place_kitty(&mut runtime, &buffer, 4, 3);
        let _ = runtime.graphics_payload(false);
        place_kitty(&mut runtime, &buffer, 4, 9);
        let moved = runtime.graphics_payload(false);
        assert!(moved.contains("a=p"), "{moved}");
        assert!(moved.contains("i=4"), "{moved}");
        assert!(moved.contains("\u{1b}[10;3H"), "{moved}");
        assert!(!moved.contains("PLACE"), "{moved}");
    }

    #[test]
    fn tmux_passthrough_wraps_stale_placement_deletes() {
        use crate::theme::Style;
        let mut runtime = ImageRuntime::new();
        runtime.hints.tmux = true;
        let buffer = crate::editor::RenderBuffer::new(20, 12, &Style::default());
        place_kitty(&mut runtime, &buffer, 5, 2);
        let _ = runtime.graphics_payload(false);

        runtime.begin_frame();
        runtime.commit_visible(&buffer);
        let deleted = runtime.graphics_payload(true);
        assert!(deleted.contains("Ptmux"), "{deleted}");
        assert!(deleted.contains("a=d"), "{deleted}");
        assert!(deleted.contains("i=5"), "{deleted}");
        assert!(deleted.contains("\u{1b}\u{1b}_G"), "{deleted}");
    }

    #[test]
    fn multiplexers_use_kitty_when_the_query_succeeds() {
        let probe = Probe {
            kitty: true,
            sixel: false,
            cell_width: 10,
            cell_height: 22,
        };
        let herdr = hints(&[
            ("HERDR_ENV", "1"),
            ("TERM_PROGRAM", "ghostty"),
            ("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty"),
        ]);
        assert!(herdr.herdr && herdr.ghostty && herdr.query_required());
        assert_eq!(
            detected_graphics(&herdr, probe),
            Some(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Auto, &herdr, probe),
            ImagePresentation::Graphics(GraphicsProtocol::Kitty)
        );
        // A leaked Ghostty variable is not a protocol. Herdr answers the query
        // itself; until that answer arrives, auto mode stays on half-blocks.
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Auto, &herdr, Probe::default()),
            ImagePresentation::Halfblocks
        );

        let zellij = hints(&[("ZELLIJ_SESSION_NAME", "dev"), ("COLORTERM", "truecolor")]);
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Auto, &zellij, probe),
            ImagePresentation::Graphics(GraphicsProtocol::Kitty)
        );
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Auto, &zellij, Probe::default()),
            ImagePresentation::Halfblocks
        );

        let screen = hints(&[("STY", "123.pts-0.box"), ("KITTY_WINDOW_ID", "1")]);
        assert!(screen.screen);
        assert_eq!(
            choose_presentation(MarkdownImagesMode::Auto, &screen, Probe::default()),
            ImagePresentation::Halfblocks
        );
        let (height, width) = parse_cell_pixels(b"\x1b[6;22;10t").unwrap();
        assert_eq!((height, width), (22, 10));
        assert!(parse_cell_pixels(b"\x1b[6;0;0t").is_none());
    }

    #[test]
    fn fingerprint_covers_every_painted_image_row() {
        use crate::theme::Style;
        let mut runtime = ImageRuntime::new();
        let mut buffer = crate::editor::RenderBuffer::new(8, 4, &Style::default());
        let style = Style {
            fg: Some(Color::Rgb { r: 1, g: 2, b: 3 }),
            bg: Some(Color::Rgb { r: 4, g: 5, b: 6 }),
            ..Style::default()
        };
        buffer.set_text(1, 1, "abcd", &style);
        runtime.begin_frame();
        runtime.stage_graphics(StagedImage {
            id: 1,
            x: 1,
            y: 1,
            cols: 4,
            rows: 2,
            total_rows: 2,
            first_index: 0,
            painted: 1,
            signature: 1,
            protocol: GraphicsProtocol::Kitty,
            payload: "PLACE".to_string(),
            source: None,
            pixel_w: 40,
            pixel_h: 20,
            decoded: None,
            cells: None,
        });
        runtime.seal_fingerprints(&buffer);
        assert!(
            runtime.staged[0].cells.is_none(),
            "a fingerprint taken before the last row would not match the finished image"
        );

        buffer.set_text(1, 2, "wxyz", &style);
        runtime.staged[0].painted = 2;
        runtime.seal_fingerprints(&buffer);
        let fingerprint = runtime.staged[0].cells.expect("full image fingerprint");
        let rect = crate::ui::ScreenRect {
            x: 1,
            y: 1,
            width: 4,
            height: 2,
        };
        assert_eq!(fingerprint, hash_region(&buffer, rect));

        let mut early = crate::editor::RenderBuffer::new(8, 4, &Style::default());
        early.set_text(1, 1, "abcd", &style);
        assert_ne!(fingerprint, hash_region(&early, rect));

        assert!(runtime.commit_visible(&buffer));
        let shown = runtime.graphics_payload(false);
        assert!(shown.contains("PLACE"), "{shown}");
    }

    #[test]
    fn cropped_kitty_placement_uses_a_source_rect() {
        use crate::theme::Style;
        let mut runtime = ImageRuntime::new();
        let buffer = crate::editor::RenderBuffer::new(10, 6, &Style::default());
        let image = Arc::new(DecodedImage {
            width: 4,
            height: 16,
            rgba: vec![200u8; 4 * 16 * 4],
        });
        runtime.begin_frame();
        for (index, y) in [(2u16, 1u16), (3, 2)] {
            runtime.observe_graphics_row(ObservedGraphics {
                id: 8,
                x: 1,
                y,
                cols: 4,
                total_rows: 4,
                index,
                signature: 11,
                protocol: GraphicsProtocol::Kitty,
                image: Arc::clone(&image),
            });
        }
        runtime.seal_fingerprints(&buffer);
        let staged = &runtime.staged[0];
        assert_eq!(staged.rows, 2);
        assert_eq!(staged.first_index, 2);
        let source = staged.source.expect("partial image is cropped");
        assert!(source.y > 0, "{source:?}");
        assert!(source.h > 0, "{source:?}");
        let place = kitty_place(8, 1, staged.cols, staged.rows, Some(source));
        assert!(place.contains("r=2"), "{place}");
        assert!(!place.contains("r=4"), "{place}");
        assert!(place.contains(&format!("y={}", source.y)), "{place}");
        assert!(staged.payload.contains("a=t,"), "{}", staged.payload);
        assert!(!staged.payload.contains("a=T,"), "{}", staged.payload);
        assert!(kitty_source_rect(10, 40, 4, 0, 4).is_none());
        let top = kitty_source_rect(10, 40, 4, 1, 2).unwrap();
        assert_eq!((top.y, top.h), (10, 20));
    }

    #[test]
    fn cropped_placement_keeps_pixels_per_row_as_the_scroll_offset_changes() {
        let pixel_h = 120u32;
        let total_rows = 12u16;
        let visible = 5u16;
        let per_row = pixel_h / u32::from(total_rows);
        let mut heights = Vec::new();
        for first in [0u16, 2, 4, 7] {
            let source = kitty_source_rect(80, pixel_h, total_rows, first, visible)
                .expect("a partial slice is cropped");
            assert_eq!(source.y, u32::from(first) * per_row, "{source:?}");
            assert_eq!(source.h, u32::from(visible) * per_row, "{source:?}");
            heights.push(source.h);
        }
        assert!(heights.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn a_top_crop_is_placed_instead_of_scaling_the_whole_image() {
        use crate::theme::Style;
        let mut runtime = ImageRuntime::new();
        let buffer = crate::editor::RenderBuffer::new(10, 6, &Style::default());
        let image = Arc::new(DecodedImage {
            width: 4,
            height: 16,
            rgba: vec![180u8; 4 * 16 * 4],
        });
        runtime.begin_frame();
        for (index, y) in [(0u16, 1u16), (1, 2)] {
            runtime.observe_graphics_row(ObservedGraphics {
                id: 9,
                x: 1,
                y,
                cols: 4,
                total_rows: 4,
                index,
                signature: 12,
                protocol: GraphicsProtocol::Kitty,
                image: Arc::clone(&image),
            });
        }
        runtime.seal_fingerprints(&buffer);
        let source = runtime.staged[0]
            .source
            .expect("top crop keeps a source rect");
        assert_eq!((source.y, source.h), (0, 8));
        assert!(runtime.commit_visible(&buffer));
        let shown = runtime.graphics_payload(false);
        assert!(
            shown.contains("x=0,y=0,w=4,h=8"),
            "visible rows must crop the raster, not rescale it: {shown}"
        );
        assert!(shown.contains("r=2"), "{shown}");
        assert!(!shown.contains("r=4"), "{shown}");
    }

    #[test]
    fn sixel_and_iterm_crop_the_bitmap_to_the_visible_rows() {
        use crate::theme::Style;
        let image = Arc::new(DecodedImage {
            width: 4,
            height: 16,
            rgba: vec![90u8; 4 * 16 * 4],
        });
        for protocol in [GraphicsProtocol::Sixel, GraphicsProtocol::Iterm2] {
            let mut runtime = ImageRuntime::new();
            let buffer = crate::editor::RenderBuffer::new(10, 6, &Style::default());
            runtime.begin_frame();
            for (index, y) in [(2u16, 1u16), (3, 2)] {
                runtime.observe_graphics_row(ObservedGraphics {
                    id: 4,
                    x: 1,
                    y,
                    cols: 4,
                    total_rows: 4,
                    index,
                    signature: 13,
                    protocol,
                    image: Arc::clone(&image),
                });
            }
            runtime.seal_fingerprints(&buffer);
            let staged = &runtime.staged[0];
            assert_eq!(staged.rows, 2, "{protocol:?}");
            assert_eq!(staged.pixel_h, 8, "{protocol:?}");
            assert!(staged.pixel_h < 16, "{protocol:?}");
            if protocol == GraphicsProtocol::Iterm2 {
                assert!(
                    staged.payload.contains("height=2;"),
                    "{protocol:?} {}",
                    staged.payload
                );
                assert!(!staged.payload.contains("height=4;"), "{}", staged.payload);
            } else {
                assert!(
                    staged.payload.contains("SIXEL") || staged.payload.contains("\u{1b}P"),
                    "{protocol:?}"
                );
            }
        }
    }

    #[test]
    fn declaration_line_keeps_the_image_block_the_same_height() {
        use super::super::display_layout::{
            layout_lines_with_tables, BreakIndentOptions, LayoutConfig,
        };

        let plan = ImageLayout {
            rows: BTreeMap::from([(0, image_line_segments(0, 6, 10, false))]),
            raw_lines: BTreeSet::new(),
        };
        let lines = vec!["![alt](pic.png)\n".to_string(), "after\n".to_string()];
        let config = LayoutConfig {
            content_width: 40,
            height: 20,
            wrap: true,
            vtop: 0,
            vleft: 0,
            skipcol: 0,
            break_indent: BreakIndentOptions::disabled(),
        };
        let hidden =
            layout_lines_with_tables(&lines, 2, config, None, None, None, None, Some(&plan));
        let mut revealed_plan = plan;
        revealed_plan.raw_lines.insert(0);
        let revealed = layout_lines_with_tables(
            &lines,
            2,
            config,
            None,
            None,
            None,
            None,
            Some(&revealed_plan),
        );
        assert_eq!(hidden.rows.len(), revealed.rows.len());
        assert!(revealed.rows[0].image.is_none());
        assert!(revealed.rows[0].line == 0);
        assert!(revealed.rows[1].image.is_some());
        assert_eq!(revealed.rows[1].image.unwrap().index, 1);
        assert_eq!(hidden.rows.last().unwrap().line, 1);
        assert_eq!(revealed.rows.last().unwrap().line, 1);
        assert_eq!(
            hidden.rows.last().unwrap().row,
            revealed.rows.last().unwrap().row
        );
    }

    #[test]
    fn bounded_append_stops_before_the_cap() {
        let mut bytes = vec![1, 2, 3];
        assert!(append_bounded(&mut bytes, &[4, 5], 5));
        assert_eq!(bytes, vec![1, 2, 3, 4, 5]);
        assert!(!append_bounded(&mut bytes, &[6], 5));
        assert_eq!(bytes, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn remote_body_stops_when_it_exceeds_the_image_cap() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = socket.read(&mut buf).await;
            let body = "x".repeat(48);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!("http://{addr}/a.png"))
            .send()
            .await
            .unwrap();
        let error = read_bounded_body(response, 16).await.unwrap_err();
        assert_eq!(error, "too large");
        server.await.unwrap();
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
        assert!(
            screen
                .chars()
                .any(|glyph| is_block_glyph(&glyph.to_string())),
            "{screen}"
        );
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
        assert!(
            screen
                .chars()
                .any(|glyph| is_block_glyph(&glyph.to_string())),
            "{screen}"
        );
        for rect in &frame.overlay_rects {
            for y in rect.y..rect.y.saturating_add(rect.height) {
                for x in rect.x..rect.x.saturating_add(rect.width) {
                    let cell = &frame.cells[y * frame.width + x];
                    assert!(
                        matches!(cell.style.bg, Some(Color::Rgb { .. })),
                        "popup cell {x},{y} over the image needs an explicit background, got {:?}",
                        cell.style.bg
                    );
                }
            }
        }

        editor.keymap_hints_visible = false;
        editor.waiting_key_action = None;
        editor.keymap_hint_prefix.clear();
        editor.render(&mut frame).unwrap();
        let screen = frame_text(&frame);
        assert!(!screen.contains("keymaps"), "{screen}");
        assert!(
            screen
                .chars()
                .any(|glyph| is_block_glyph(&glyph.to_string())),
            "{screen}"
        );
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

    fn frame_row(frame: &crate::editor::RenderBuffer, needle: &str) -> usize {
        frame
            .cells
            .chunks(frame.width)
            .position(|row| {
                row.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
                    .contains(needle)
            })
            .unwrap_or_else(|| panic!("missing {needle} in {}", frame_text(frame)))
    }
}
