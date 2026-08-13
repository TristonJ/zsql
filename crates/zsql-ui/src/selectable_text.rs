//! A block of plain text with mouse-driven range selection, shaded with a
//! caller-supplied background color. The selection is a byte offset range
//! into the exact text the element was built from.

use std::ops::Range;

use gpui::{
    App, Div, Font, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, StyledText, TextRun, Window,
    prelude::*,
};

/// Byte-offset mouse selection over one block of plain text: an anchor and
/// a cursor, kept in the order they were set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectableTextState {
    selection: Option<(usize, usize)>,
    dragging: bool,
}

impl SelectableTextState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the selection cursor to `offset`. When `extend` is true the
    /// existing anchor is kept and no drag is armed; otherwise the
    /// selection re-anchors at `offset` and arms
    /// [`Self::extend_while_dragging`].
    pub fn begin(&mut self, offset: usize, extend: bool) {
        let anchor = if extend {
            self.selection.map_or(offset, |(anchor, _)| anchor)
        } else {
            offset
        };
        self.selection = Some((anchor, offset));
        self.dragging = !extend;
    }

    /// Move the selection cursor to `offset` while a drag begun by
    /// [`Self::begin`] is in progress, keeping the existing anchor. A no-op
    /// (returning `false`) once the drag has ended or the cursor is already
    /// at `offset`.
    #[must_use]
    pub fn extend_while_dragging(&mut self, offset: usize) -> bool {
        if !self.dragging {
            return false;
        }
        let Some((anchor, cursor)) = self.selection else {
            return false;
        };
        if cursor == offset {
            return false;
        }
        self.selection = Some((anchor, offset));
        true
    }

    /// End a selection drag, leaving the selection itself in place. Returns
    /// `false` (a no-op) if nothing was being dragged.
    #[must_use]
    pub fn end_drag(&mut self) -> bool {
        if !self.dragging {
            return false;
        }
        self.dragging = false;
        true
    }

    /// Clear the selection and any in-progress drag.
    pub fn clear(&mut self) {
        self.selection = None;
        self.dragging = false;
    }

    /// The selection's ordered byte range, or `None` if nothing is selected
    /// or the selection is collapsed (a plain click with no drag).
    #[must_use]
    pub fn range(&self) -> Option<Range<usize>> {
        let (a, b) = self.selection?;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        (start < end).then_some(start..end)
    }
}

/// The font/color a selectable text block paints its text with, and the
/// background its selected range (if any) is shaded with.
#[derive(Debug, Clone)]
pub struct SelectableTextStyle {
    pub font: Font,
    pub color: Hsla,
    pub selection_bg: Hsla,
}

/// `text`'s `TextRun`s at `style`'s font/color, with `selection`'s ordered
/// byte range (if any and non-empty) additionally shaded with
/// `style.selection_bg`.
#[must_use]
pub fn selection_runs(
    text: &str,
    style: &SelectableTextStyle,
    selection: Option<Range<usize>>,
) -> Vec<TextRun> {
    let len = text.len();
    let run = |range: Range<usize>, background_color: Option<Hsla>| TextRun {
        len: range.end - range.start,
        font: style.font.clone(),
        color: style.color,
        background_color,
        underline: None,
        strikethrough: None,
    };

    let Some(selection) = selection.filter(|range| !range.is_empty()) else {
        return vec![run(0..len, None)];
    };
    let start = selection.start.min(len);
    let end = selection.end.min(len);

    let mut runs = Vec::new();
    if start > 0 {
        runs.push(run(0..start, None));
    }
    if end > start {
        runs.push(run(start..end, Some(style.selection_bg)));
    }
    if end < len {
        runs.push(run(end..len, None));
    }
    if runs.is_empty() {
        runs.push(run(0..len, None));
    }
    runs
}

/// Attach mouse selection to `container`: the selected range (if any) is
/// painted with `style.selection_bg`, and `on_down` / `on_move` report the
/// clicked or dragged byte offset against `text`'s own layout. `on_down`
/// also receives whether the click extends the existing selection;
/// `on_move` fires on every hover move over `container`, not only during a
/// drag.
pub fn with_selectable_text(
    container: Div,
    text: &str,
    style: &SelectableTextStyle,
    selection: Option<Range<usize>>,
    on_down: impl Fn(usize, bool, &mut Window, &mut App) + 'static,
    on_move: impl Fn(usize, &mut Window, &mut App) + 'static,
) -> Div {
    let runs = selection_runs(text, style, selection);
    let styled = StyledText::new(text.to_owned()).with_runs(runs);
    let layout_for_down = styled.layout().clone();
    let layout_for_move = styled.layout().clone();

    container
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, app| {
                let offset = match layout_for_down.index_for_position(event.position) {
                    Ok(b) | Err(b) => b,
                };
                on_down(offset, event.modifiers.shift, window, app);
            },
        )
        .on_mouse_move(move |event: &MouseMoveEvent, window, app| {
            let offset = match layout_for_move.index_for_position(event.position) {
                Ok(b) | Err(b) => b,
            };
            on_move(offset, window, app);
        })
        .child(styled)
}

#[cfg(test)]
mod tests {
    use gpui::{font, rgb};

    use super::{SelectableTextState, SelectableTextStyle, selection_runs};

    fn test_style() -> SelectableTextStyle {
        SelectableTextStyle {
            font: font("monospace"),
            color: gpui::Hsla::from(rgb(0x00_ff_00)),
            selection_bg: gpui::Hsla::from(rgb(0xff_00_00)),
        }
    }

    // -- SelectableTextState -------------------------------------------

    #[test]
    fn begin_with_no_extend_starts_a_fresh_collapsed_selection() {
        let mut state = SelectableTextState::new();
        state.begin(3, false);
        assert_eq!(state.range(), None, "a plain click alone selects nothing");
    }

    #[test]
    fn extend_while_dragging_grows_the_selection_from_the_anchor() {
        let mut state = SelectableTextState::new();
        state.begin(2, false);
        assert!(
            state.extend_while_dragging(7),
            "a live drag must report a change"
        );
        assert_eq!(state.range(), Some(2..7));
    }

    #[test]
    fn extend_while_dragging_to_the_same_offset_reports_no_change() {
        let mut state = SelectableTextState::new();
        state.begin(2, false);
        assert!(state.extend_while_dragging(7));
        assert!(
            !state.extend_while_dragging(7),
            "extending to the cursor's current offset must report no change"
        );
    }

    #[test]
    fn shift_click_extends_from_the_original_anchor_not_the_last_cursor() {
        let mut state = SelectableTextState::new();
        state.begin(1, false);
        assert!(state.extend_while_dragging(4));
        state.begin(9, true);
        assert_eq!(
            state.range(),
            Some(1..9),
            "a shift-click must extend from the original anchor"
        );

        state.begin(0, false);
        assert_eq!(
            state.range(),
            None,
            "a plain click (no shift) must start a fresh selection"
        );
    }

    #[test]
    fn shift_click_does_not_arm_dragging() {
        let mut state = SelectableTextState::new();
        state.begin(2, false);
        state.begin(5, true);
        assert!(
            !state.extend_while_dragging(8),
            "a shift-click must not arm a subsequent drag"
        );
        assert_eq!(
            state.range(),
            Some(2..5),
            "a shift-click must not arm a subsequent drag"
        );
    }

    #[test]
    fn end_drag_stops_further_extension() {
        let mut state = SelectableTextState::new();
        state.begin(0, false);
        assert!(state.end_drag(), "a live drag must report that it ended");
        assert!(!state.end_drag(), "ending an already-ended drag is a no-op");
        assert!(!state.extend_while_dragging(5));
        assert_eq!(
            state.range(),
            None,
            "extending after the drag ended must be a no-op"
        );
    }

    #[test]
    fn clear_drops_the_selection_and_any_in_progress_drag() {
        let mut state = SelectableTextState::new();
        state.begin(0, false);
        assert!(state.extend_while_dragging(4));
        state.clear();
        assert_eq!(state.range(), None);

        assert!(!state.extend_while_dragging(9));
        assert_eq!(
            state.range(),
            None,
            "clearing must also disarm a drag in progress"
        );
    }

    #[test]
    fn range_normalizes_a_backward_drag() {
        let mut state = SelectableTextState::new();
        state.begin(6, false);
        assert!(state.extend_while_dragging(2));
        assert_eq!(
            state.range(),
            Some(2..6),
            "dragging backward must still yield an ascending range"
        );
    }

    // -- selection_runs ---------------------------------------------------

    #[test]
    fn selection_runs_with_no_selection_is_one_unshaded_run() {
        let style = test_style();
        let runs = selection_runs("hello", &style, None);

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len, 5);
        assert_eq!(runs[0].background_color, None);
    }

    #[test]
    fn selection_runs_shades_only_the_selected_byte_range() {
        let style = test_style();
        let runs = selection_runs("hello world", &style, Some(2..5));

        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].len, 2);
        assert_eq!(runs[0].background_color, None);
        assert_eq!(runs[1].len, 3);
        assert_eq!(runs[1].background_color, Some(style.selection_bg));
        assert_eq!(runs[2].len, 6);
        assert_eq!(runs[2].background_color, None);

        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, "hello world".len());
    }

    #[test]
    fn selection_runs_at_the_very_start_omits_the_leading_unselected_run() {
        let style = test_style();
        let runs = selection_runs("hello", &style, Some(0..3));

        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].background_color, Some(style.selection_bg));
        assert_eq!(runs[0].len, 3);
        assert_eq!(runs[1].background_color, None);
        assert_eq!(runs[1].len, 2);
    }

    #[test]
    fn selection_runs_covering_the_whole_text_is_one_shaded_run() {
        let style = test_style();
        let runs = selection_runs("hello", &style, Some(0..5));

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].background_color, Some(style.selection_bg));
        assert_eq!(runs[0].len, 5);
    }

    #[test]
    fn selection_runs_clamps_an_out_of_range_selection_to_the_text_length() {
        let style = test_style();
        let runs = selection_runs("hi", &style, Some(0..999));

        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, 2);
        assert!(
            runs.iter()
                .any(|r| r.background_color == Some(style.selection_bg))
        );
    }

    #[test]
    fn selection_runs_with_an_empty_range_is_one_unshaded_run() {
        let style = test_style();
        let runs = selection_runs("hello", &style, Some(3..3));

        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].background_color, None);
    }
}
