---
title: "Display Layout"
summary: "Display layout maps logical buffer lines to viewport rows using terminal-column width, wrapping, horizontal scrolling, and break indentation."
topics: [editor, rendering, unicode]
sources:
  - id: display-layout
    type: file
    path: src/editor/display_layout.rs
  - id: editor-core
    type: file
    path: src/editor.rs
  - id: movement-tests
    type: file
    path: tests/movement.rs
---

Display layout is Red's width-aware map from buffer lines to screen rows. It takes logical lines, viewport dimensions, wrap mode, horizontal scroll state, long-line skip state, and break-indent options, then produces immutable `LineSegment` rows with buffer line numbers, display-column ranges, grapheme ranges, byte ranges, source offsets, and virtual indentation [@display-layout]. The editor uses that map for drawing, cursor placement, mouse hit-testing, plugin viewport snapshots, and screen-line motion, while painting styles remain outside the layout module [@editor-core].

## Line Segments

`DisplayLayout` is a vector of `LineSegment` values. A segment names the buffer line, screen row, start and end display columns, start and end grapheme indices, start and end UTF-8 byte offsets, and whether it is the first visual segment of the logical line [@display-layout]. This gives callers enough information to slice text safely, place the cursor on a wrapped row, and report visible viewport rows to plugins without recomputing wrapping [@editor-core].

The module trims line endings before layout, so CRLF carriage returns do not consume display cells or appear as rendered text [@display-layout]. Tests in the layout module cover CRLF trimming, wide grapheme boundaries, tab-aware wrapping, and preserving byte ranges for composed graphemes and wide glyphs [@display-layout].

## Wrapping And Break Indent

Wrapped layout uses terminal display columns rather than byte or scalar counts. `wrap_line_segments` iterates grapheme clusters, expands tabs from the current display column, and refuses to split a wide grapheme across a row boundary [@display-layout]. Continuation rows can receive a virtual `visual_offset` derived from leading whitespace, which mirrors Vim-style break indent and is clamped so at least twenty text columns remain on a wrapped continuation row [@display-layout].

The visual offset is a screen-only property. A continuation row may start drawing at an indented screen column while retaining the logical `start_col` and `start_grapheme` of the wrapped content [@display-layout]. This distinction is why [Editor Coordinate Systems](coordinate-systems) treats display columns as a separate coordinate system from grapheme and scalar positions.

## Horizontal Scrolling

When wrapping is disabled, layout creates a single segment per logical line using `vleft` as the first visible display column and the window content width as the visible range [@display-layout]. The segment also records `start_grapheme_col`, which can be earlier than `start_col` when horizontal scrolling begins inside a tab or wide grapheme [@display-layout]. This preserves hit-testing and cursor drawing without pretending that a half-visible grapheme is an editable position.

Wrapped long-line scrolling uses `skipcol` only for the first viewport line. The tests assert that once the first wrapped row skips to a later display column, following logical lines start from column zero again [@display-layout]. The editor keeps `skipcol` separately from `vleft` because wrapped long lines scroll by visual row segments, while nowrap horizontal scrolling scrolls by left display column [@editor-core].

## Editor Consumers

The editor caches layouts by every input that can change them: buffer index, buffer revision, file path, viewport top, horizontal scroll, skip column, wrap mode, content width and height, insert-mode line-count override, and break-indent options [@editor-core]. The cache is therefore tied to the same inputs listed by the layout module's invariant comment [@display-layout].

Cursor placement converts the grapheme cursor into a display column, asks the layout for the segment containing that column, and then turns the display column into a screen column with `screen_col_for_display_col` [@editor-core]. Mouse clicks follow the reverse direction: a clicked viewport row selects a segment, subtracts gutter and virtual indent, turns the resulting display column into a grapheme index, and returns a buffer cursor action [@editor-core]. Movement tests cover screen-line behavior around wrapping and viewport placement, including cases where rendered cursor coordinates must track wrapped rows rather than logical line numbers [@movement-tests].

Display layout sits between [Editor Coordinate Systems](coordinate-systems) and the [Rendering Pipeline](../../architecture/editor/rendering-pipeline). It owns row segmentation and coordinate conversion for viewport geometry; rendering consumes those rows to paint cells.
