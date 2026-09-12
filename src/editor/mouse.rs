//! Editor-owned mouse gestures share the keyboard Visual selection and its operators.
//!
//! A press captures one stable window and buffer. Only that press may authorize drag
//! or release events; crossing another surface never transfers the gesture to it.

use super::*;

const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(45);
const MULTICLICK_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct MouseSelectionOwner {
    window: WindowId,
    buffer: BufferId,
}

#[derive(Clone, Copy)]
pub(super) struct MouseClick {
    owner: MouseSelectionOwner,
    pointer: Point,
    at: Instant,
    count: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionUnit {
    Character,
    Word,
    Line,
    Block,
}

impl SelectionUnit {
    fn mode(self) -> Mode {
        match self {
            Self::Character | Self::Word => Mode::Visual,
            Self::Line => Mode::VisualLine,
            Self::Block => Mode::VisualBlock,
        }
    }
}

#[derive(Clone)]
pub(super) struct MouseGesture {
    owner: MouseSelectionOwner,
    revision: u64,
    anchor: Point,
    anchor_end: Point,
    pointer: Point,
    started: bool,
    activate_on_press: bool,
    press_pointer: Point,
    reverse_initial: bool,
    from_insert: bool,
    unit: SelectionUnit,
    last_scroll: Instant,
}

#[derive(Default)]
pub(super) struct MouseSelectionState {
    pub(super) gesture: Option<MouseGesture>,
    pub(super) last_click: Option<MouseClick>,
    return_to_insert: Option<MouseSelectionOwner>,
    visual_owner: Option<MouseSelectionOwner>,
}

impl Editor {
    fn mouse_owner_matches(&self, owner: MouseSelectionOwner) -> bool {
        self.window_manager.active_stable_window_id() == Some(owner.window)
            && self.current_buffer().id() == owner.buffer
    }

    pub(super) fn validate_mouse_selection_owner(&mut self) {
        if self
            .mouse_selection
            .gesture
            .as_ref()
            .is_some_and(|gesture| {
                !self.mouse_owner_matches(gesture.owner)
                    || self.current_buffer().revision() != gesture.revision
                    || self.panel_manager.has_focused_panel()
                    || self.workspace_manager.is_active()
                    || self
                        .current_dialog
                        .as_ref()
                        .is_some_and(|dialog| !dialog.allows_event_passthrough())
                    || gesture.started && !self.is_visual()
            })
        {
            self.mouse_selection.gesture = None;
        }
        if self
            .mouse_selection
            .return_to_insert
            .is_some_and(|owner| !self.mouse_owner_matches(owner))
        {
            self.mouse_selection.return_to_insert = None;
        }
        if self
            .mouse_selection
            .visual_owner
            .is_some_and(|owner| !self.mouse_owner_matches(owner))
        {
            self.mouse_selection.visual_owner = None;
        }
    }

    pub(super) fn mouse_visual_allows_line_end(&self) -> bool {
        self.is_visual()
            && self
                .mouse_selection
                .visual_owner
                .is_some_and(|owner| self.mouse_owner_matches(owner))
    }

    pub(super) fn restore_mouse_visual_line_end(&mut self, first: Point, last: Point) {
        if first.x == self.length_for_line(first.y) || last.x == self.length_for_line(last.y) {
            self.mouse_selection.visual_owner =
                self.window_manager
                    .active_stable_window_id()
                    .map(|window| MouseSelectionOwner {
                        window,
                        buffer: self.current_buffer().id(),
                    });
        }
    }

    pub(super) fn take_mouse_insert_return(&mut self, new_mode: Mode) -> bool {
        if self.is_visual()
            && !matches!(
                new_mode,
                Mode::Visual | Mode::VisualLine | Mode::VisualBlock
            )
        {
            self.mouse_selection.gesture = None;
            self.mouse_selection.visual_owner = None;
            return self
                .mouse_selection
                .return_to_insert
                .take()
                .is_some_and(|owner| new_mode == Mode::Normal && self.mouse_owner_matches(owner));
        }
        false
    }

    pub(super) fn handle_captured_mouse_selection(
        &mut self,
        event: &MouseEvent,
    ) -> Option<KeyAction> {
        match event.kind {
            MouseEventKind::Down(_) => {
                self.mouse_selection.gesture = None;
                None
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
                if self.mouse_selection.gesture.is_some() =>
            {
                Some(KeyAction::Single(Action::MouseDrag {
                    column: event.column,
                    row: event.row,
                    finish: matches!(event.kind, MouseEventKind::Up(_)),
                }))
            }
            _ => None,
        }
    }

    /// Maps terminal cells through the rendered layout, including tabs, graphemes,
    /// horizontal scrolling, wrapped continuations and inserted annotation rows.
    fn mouse_buffer_point(&self, window: &crate::window::Window, pointer: Point) -> Point {
        let top = window.position.y + self.window_content_top(window);
        let height = self.window_content_height(window).max(1);
        let row = pointer.y.clamp(top, top + height - 1) - top;
        let left = window.position.x + self.gutter_width_for_window(window) + 1;
        let width = self.window_content_width(window).max(1);
        let content_x = pointer.x.clamp(left, left + width - 1) - left;
        let layout = self.layout_for_window(window);
        let segment = layout.row(row).or_else(|| {
            // Annotation rows are not buffer characters. While dragging, snap to
            // the preceding text row instead of selecting annotation contents.
            layout.rows.iter().rev().find(|segment| segment.row <= row)
        });
        let Some(segment) = segment else {
            return Point::new(0, self.last_navigable_line());
        };
        let buffer = &self.buffer_manager[window.buffer_index];
        let line = buffer.get(segment.line).unwrap_or_default();
        let display_col = segment.start_col + content_x.saturating_sub(segment.visual_offset);
        let x = column_to_grapheme_with_tabs(
            trim_line_ending(&line),
            display_col,
            self.tab_width_for_buffer_index(window.buffer_index),
        );
        Point::new(x, segment.line.min(buffer.last_navigable_line()))
    }

    /// Mouse word selection follows keyword, whitespace and punctuation runs;
    /// punctuation also supports the same balanced-delimiter motion as `%`.
    fn mouse_word_span(&self, point: Point) -> (Point, Point, bool) {
        let line = self.current_buffer().get(point.y).unwrap_or_default();
        let graphemes = trim_line_ending(&line).graphemes(true).collect::<Vec<_>>();
        if graphemes.is_empty() {
            return (Point::new(0, point.y), Point::new(0, point.y), false);
        }
        let index = point.x.min(graphemes.len() - 1);
        let kind = |grapheme: &str| match grapheme.chars().next() {
            Some(character) if character.is_whitespace() => 0,
            Some(character) if is_keyword_char(character) => 1,
            _ => 2,
        };
        let class = kind(graphemes[index]);
        if class == 2 {
            let cursor = TextPosition::new(point.y, self.grapheme_to_char_on_line(index, point.y));
            if let Some(motion) = matchit::find_motion(
                &self.current_buffer().contents(),
                cursor,
                self.current_language_id().as_deref(),
                &self.config.matchit,
                MatchDirection::Forward,
            ) {
                let target = self.point_for_text_position(motion.target);
                let clicked = Point::new(index, point.y);
                return if clicked <= target {
                    (clicked, target, false)
                } else {
                    (target, clicked, true)
                };
            }
        }
        let mut first = index;
        let mut last = index;
        while first > 0 && kind(graphemes[first - 1]) == class {
            first -= 1;
        }
        while last + 1 < graphemes.len() && kind(graphemes[last + 1]) == class {
            last += 1;
        }
        (Point::new(first, point.y), Point::new(last, point.y), false)
    }

    fn mouse_extension_anchor(&self, clicked: Point) -> Point {
        let Some(selection) = self.selection else {
            return Point::new(self.cx, self.buffer_line());
        };
        let first = Point::new(selection.x0, selection.y0);
        let last = Point::new(selection.x1, selection.y1);
        let offset = |point: Point| {
            self.current_buffer()
                .position_to_char_idx(TextPosition::new(
                    point.y,
                    self.grapheme_to_char_on_line(point.x, point.y),
                ))
        };
        let clicked = offset(clicked);
        if clicked.abs_diff(offset(first)) <= clicked.abs_diff(offset(last)) {
            last
        } else {
            first
        }
    }

    #[inline(never)]
    pub(super) fn execute_mouse_press<'a>(
        &'a mut self,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            self.mouse_selection.gesture = None;
            let pointer = Point::new(usize::from(column), usize::from(row));
            let Some((index, window)) =
                self.window_manager.window_at_position(pointer.x, pointer.y)
            else {
                return Ok(());
            };
            let window = window.clone();
            let extends = modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
            let same_window = Some(window.id) == self.window_manager.active_stable_window_id();
            let previous_anchor = if modifiers.contains(KeyModifiers::SHIFT) && same_window {
                self.mouse_extension_anchor(self.mouse_buffer_point(&window, pointer))
            } else {
                Point::new(self.cx, self.buffer_line())
            };
            let previous_mode = self.mode;
            if self.is_visual() {
                self.execute_enter_mode(Mode::Normal, buffer, runtime)
                    .await?;
            }
            self.set_active_window(index);
            self.mouse_selection.return_to_insert = None;
            let local_y = pointer.y.saturating_sub(window.position.y);
            if local_y < self.window_content_top(&window) {
                let local_x = pointer.x.saturating_sub(window.position.x);
                if let Some(rendered) = self
                    .window_bar_manager
                    .render(window.id, window.inner_width())
                {
                    if let Some(region) = rendered.hit_regions.iter().find(|region| {
                        local_x >= region.start_column && local_x < region.end_column
                    }) {
                        self.execute(&Action::NotifyPlugins(
                        format!("window_bar:action:{}", rendered.bar_id),
                        json!({ "window_id": window.id.0, "segment_id": region.segment_id, "action": region.action }),
                    ), buffer, runtime).await?;
                    }
                }
                return Ok(());
            }
            let layout = self.layout_for_window(&window);
            let content_x = pointer
                .x
                .saturating_sub(window.position.x + self.gutter_width_for_window(&window) + 1);
            if let Some(comment) =
                layout.inline_comment_row(local_y - self.window_content_top(&window))
            {
                if let Some(action) = self.inline_comment_click_action(comment, content_x) {
                    self.execute(&action, buffer, runtime).await?;
                } else if let Some(group) = self.inline_job_on_comment_line(comment.line) {
                    self.execute(&Action::OpenInlineJob(group), buffer, runtime)
                        .await?;
                }
                return Ok(());
            }
            let point = self.mouse_buffer_point(&window, pointer);
            self.clear_multi_cursor();
            self.execute(&Action::SetCursor(point.x, point.y), buffer, runtime)
                .await?;
            let owner = MouseSelectionOwner {
                window: window.id,
                buffer: self.current_buffer().id(),
            };
            let count = if modifiers.is_empty() {
                self.mouse_selection
                    .last_click
                    .filter(|click| {
                        click.owner == owner
                            && click.pointer == pointer
                            && click.at.elapsed() < MULTICLICK_INTERVAL
                    })
                    .map_or(1, |click| click.count % 4 + 1)
            } else {
                1
            };
            self.mouse_selection.last_click = modifiers.is_empty().then_some(MouseClick {
                owner,
                pointer,
                at: Instant::now(),
                count,
            });
            let unit = if modifiers.contains(KeyModifiers::ALT) {
                SelectionUnit::Block
            } else if modifiers.contains(KeyModifiers::SHIFT) && same_window {
                match previous_mode {
                    Mode::VisualLine => SelectionUnit::Line,
                    Mode::VisualBlock => SelectionUnit::Block,
                    _ => SelectionUnit::Character,
                }
            } else {
                match count {
                    2 => SelectionUnit::Word,
                    3 => SelectionUnit::Line,
                    4 => SelectionUnit::Block,
                    _ => SelectionUnit::Character,
                }
            };
            let pressed = Point::new(self.cx, self.buffer_line());
            let (anchor, anchor_end, reverse_initial) = if extends && same_window {
                (previous_anchor, previous_anchor, false)
            } else if unit == SelectionUnit::Word {
                self.mouse_word_span(pressed)
            } else {
                (pressed, pressed, false)
            };
            let activate_on_press = extends || count > 1;
            self.mouse_selection.gesture = Some(MouseGesture {
                owner,
                revision: self.current_buffer().revision(),
                anchor,
                anchor_end,
                pointer,
                started: false,
                activate_on_press,
                press_pointer: pointer,
                reverse_initial,
                from_insert: self.is_insert(),
                unit,
                last_scroll: Instant::now() - AUTOSCROLL_INTERVAL,
            });
            if activate_on_press {
                self.execute_mouse_drag(column, row, /*finish*/ false, buffer, runtime)
                    .await?;
            }
            Ok(())
        })
    }

    #[inline(never)]
    pub(super) fn execute_mouse_drag<'a>(
        &'a mut self,
        column: u16,
        row: u16,
        finish: bool,
        buffer: &'a mut RenderBuffer,
        runtime: &'a mut Runtime,
    ) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            self.validate_mouse_selection_owner();
            let Some(mut gesture) = self.mouse_selection.gesture.clone() else {
                return Ok(());
            };
            let pointer = Point::new(usize::from(column), usize::from(row));
            if !gesture.started && !gesture.activate_on_press && pointer == gesture.pointer {
                if finish {
                    self.mouse_selection.gesture = None;
                }
                return Ok(());
            }
            if !gesture.started {
                if self.is_insert() {
                    self.execute_enter_mode(Mode::Normal, buffer, runtime)
                        .await?;
                }
                self.mouse_selection.visual_owner = Some(gesture.owner);
                self.execute_enter_mode(gesture.unit.mode(), buffer, runtime)
                    .await?;
                self.execute(
                    &Action::SetCursor(gesture.anchor.x, gesture.anchor.y),
                    buffer,
                    runtime,
                )
                .await?;
                gesture.anchor = Point::new(self.cx, self.buffer_line());
                gesture.revision = self.current_buffer().revision();
                gesture.started = true;
                if gesture.from_insert {
                    self.mouse_selection.return_to_insert = Some(gesture.owner);
                }
            }
            gesture.pointer = pointer;
            if !gesture.activate_on_press {
                self.mouse_selection.last_click = None;
            }
            if !finish && gesture.last_scroll.elapsed() >= AUTOSCROLL_INTERVAL {
                self.scroll_mouse_selection(pointer);
                gesture.last_scroll = Instant::now();
            }
            let Some(window) = self.active_window_with_editor_view() else {
                self.mouse_selection.gesture = None;
                return Ok(());
            };
            let mut point = self.mouse_buffer_point(&window, pointer);
            let anchor = if gesture.unit == SelectionUnit::Word {
                let (start, end, _) = self.mouse_word_span(point);
                if gesture.reverse_initial && pointer == gesture.press_pointer {
                    point = gesture.anchor;
                    gesture.anchor_end
                } else if point < gesture.anchor {
                    point = start;
                    gesture.anchor_end
                } else {
                    point = end;
                    gesture.anchor
                }
            } else {
                gesture.anchor
            };
            self.cx = point.x;
            self.cy = point.y.saturating_sub(self.vtop);
            self.check_bounds();
            self.selection_start = Some(anchor);
            self.update_selection_end(Point::new(self.cx, self.buffer_line()));
            self.refresh_cursor_goal();
            self.sync_to_window();
            self.mouse_selection.gesture = (!finish).then_some(gesture);
            self.render(buffer)?;
            Ok(())
        })
    }

    fn mouse_scroll_direction(&self, pointer: Point) -> (isize, isize) {
        let Some(window) = self.window_manager.active_window() else {
            return (0, 0);
        };
        let top = window.position.y + self.window_content_top(window);
        let bottom = top + self.window_content_height(window).max(1);
        let left = window.position.x + self.gutter_width_for_window(window) + 1;
        let right = left + self.window_content_width(window).max(1);
        let vertical = if pointer.y < top {
            -1
        } else if pointer.y >= bottom {
            1
        } else {
            0
        };
        let horizontal = if self.wrap {
            0
        } else if pointer.x < left {
            -1
        } else if pointer.x >= right {
            1
        } else {
            0
        };
        (horizontal, vertical)
    }

    fn scroll_mouse_selection(&mut self, pointer: Point) {
        let (horizontal, vertical) = self.mouse_scroll_direction(pointer);
        if self.wrap {
            if vertical < 0 {
                self.scroll_wrapped_viewport_up_one_screen_line();
            } else if vertical > 0 {
                self.scroll_wrapped_viewport_down_one_screen_line();
            }
        } else {
            self.vtop = self
                .vtop
                .saturating_add_signed(vertical)
                .min(self.last_navigable_line());
            let longest = self.line_display_width();
            self.vleft = self.vleft.saturating_add_signed(horizontal).min(longest);
        }
        self.sync_to_window();
    }

    pub(super) async fn service_mouse_selection(
        &mut self,
        buffer: &mut RenderBuffer,
        runtime: &mut Runtime,
    ) -> anyhow::Result<()> {
        self.validate_mouse_selection_owner();
        let Some(gesture) = self.mouse_selection.gesture.as_ref() else {
            return Ok(());
        };
        if !gesture.started
            || gesture.last_scroll.elapsed() < AUTOSCROLL_INTERVAL
            || self.mouse_scroll_direction(gesture.pointer) == (0, 0)
        {
            return Ok(());
        }
        let pointer = gesture.pointer;
        self.execute_mouse_drag(
            pointer.x as u16,
            pointer.y as u16,
            /*finish*/ false,
            buffer,
            runtime,
        )
        .await
    }
}
