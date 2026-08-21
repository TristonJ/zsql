//! Pure divider-drag math for the workspace's resizable panes: no `gpui`
//! window is needed, so both functions are unit testable in isolation.

use gpui::Pixels;

/// New sidebar width after dragging its divider by `delta` from `current`,
/// clamped to `[min, max]`.
///
/// `max` is widened to `min` first: `Pixels::clamp` asserts `min <= max`,
/// and a misconfigured `sidebar_max_width < sidebar_min_width` must not
/// crash the app on the first drag.
#[must_use]
pub(super) fn clamp_sidebar_width(
    current: Pixels,
    delta: Pixels,
    min: Pixels,
    max: Pixels,
) -> Pixels {
    let max = max.max(min);
    (current + delta).clamp(min, max)
}

/// New editor-pane height after dragging the editor/results divider by
/// `delta` from `current`, given the column's total available height.
///
/// The editor is never allowed to grow past
/// `container_height - divider_thickness - min_results_height`, so the
/// results pane always keeps at least `min_results_height` regardless of how
/// far the drag requests. If the container itself is too small to fit both
/// panes' minimums, the editor's own minimum wins and the results pane
/// shrinks below its target -- there is no space left to honor both.
#[must_use]
pub(super) fn clamp_editor_height(
    container_height: Pixels,
    current: Pixels,
    delta: Pixels,
    min_editor_height: Pixels,
    min_results_height: Pixels,
    divider_thickness: Pixels,
) -> Pixels {
    let max_editor_height =
        (container_height - divider_thickness - min_results_height).max(min_editor_height);
    (current + delta).clamp(min_editor_height, max_editor_height)
}
