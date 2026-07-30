//! The results bar's sort/pager controls for a live generated preview:
//! clickable column headers and the page navigator. Both stay visible but
//! render inert the moment the active tab is not a live, unedited generated
//! preview (see [`PreviewControls`]).

use std::rc::Rc;

use gpui::{AnyElement, App, Div, ElementId, SharedString, Window, div, prelude::*, px, rgb};
use zsql_core::preview_state::PreviewQueryState;
use zsql_core::{ColumnMeta, ESTIMATE_MARKER, SortDirection};
use zsql_ui::grid;
use zsql_ui::theme::Theme;

use crate::ui::theme as app_theme;

/// One pager/sort interaction a live generated preview's controls can
/// dispatch. Carried as data (rather than several separate closures) so
/// every control in the results bar and grid header routes through a
/// single callback on [`PreviewControls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreviewAction {
    /// Sort by (or, if already active, flip the direction of) the named
    /// column. The column name comes from [`ColumnMeta::name`].
    Sort(String),
    FirstPage,
    PrevPage,
    NextPage,
    LastPage,
    /// Advance to the next configured page size, wrapping around.
    CyclePageSize,
}

/// The callback every sort/pager control routes its clicks through.
pub(crate) type PreviewDispatch = Rc<dyn Fn(PreviewAction, &mut Window, &mut App)>;

/// The active generated tab's current sort/page snapshot, plus the
/// dispatcher every pager/sort control routes its clicks through.
/// `ResultsView` holds this as `Option<PreviewControls>`
/// ([`crate::ui::results::ResultsView::set_preview_controls`]): `None`
/// whenever the active tab is not a live, unedited generated preview, which
/// is what renders every control inert without hiding the grid itself.
#[derive(Clone)]
pub(crate) struct PreviewControls {
    pub state: PreviewQueryState,
    pub dispatch: PreviewDispatch,
}

/// A data column's header content: its name, type-name badge, and (while
/// `controls` is `Some`) a click-to-sort affordance. The active sort
/// column carries a persistent arrow and a tinted background; every other
/// header shows a neutral arrow only while the pointer hovers it, per
/// `window.use_keyed_state`'s per-element hover tracking -- `gpui`'s
/// declarative `.hover()` only restyles the hovering element itself, not a
/// child's visibility, so this tracks hover state explicitly instead.
pub(crate) fn sortable_column_header(
    column: &ColumnMeta,
    active_theme: &Theme,
    controls: Option<&PreviewControls>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let colors = active_theme.colors;
    let is_sorted = controls.is_some_and(|c| c.state.sort_column() == Some(column.name.as_str()));

    let hover_key = SharedString::from(format!("results-header-hover-{}", column.name));
    let hovered = window.use_keyed_state(hover_key, cx, |_window, _cx| false);
    let is_hovered = *hovered.read(cx);

    let header_id = ElementId::from(SharedString::from(format!(
        "results-header-sort-{}",
        column.name
    )));
    let mut header = div()
        .id(header_id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .w_full()
        .h_full()
        .on_hover(move |now, _window, cx| {
            hovered.update(cx, |value, cx| {
                if *value != *now {
                    *value = *now;
                    cx.notify();
                }
            });
        });

    if let Some(controls) = controls {
        let dispatch = controls.dispatch.clone();
        let column_name = column.name.clone();
        header = header.cursor_pointer().on_click(move |_event, window, cx| {
            dispatch(PreviewAction::Sort(column_name.clone()), window, cx);
        });
    }

    header = header
        .child(
            div()
                .text_color(rgb(colors.text_primary))
                .child(column.name.clone()),
        )
        .child(grid::type_tag_tertiary(&column.type_name, active_theme));

    if is_sorted {
        let controls = controls.expect("is_sorted implies controls is Some");
        let arrow = match controls.state.sort_direction() {
            SortDirection::Asc => "\u{25B2}",
            SortDirection::Desc => "\u{25BC}",
        };
        header = header.child(
            div()
                .text_size(px(app_theme::PAGER_TEXT_SIZE))
                .text_color(rgb(colors.accent))
                .child(arrow),
        );
    } else if controls.is_some() && is_hovered {
        header = header.child(
            div()
                .text_size(px(app_theme::PAGER_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child("\u{2195}"),
        );
    }

    header.into_any_element()
}

/// The results bar's pager: a "rows X-Y" window readout, first/prev/next/
/// last controls, a "page N / total" readout, and a page-size cycle
/// control. `displayed_row_count` is however many rows the grid is actually
/// showing right now, for the window readout; `controls` being `None`
/// renders every control disabled with placeholder text instead of hiding
/// the pager.
pub(crate) fn render_pager_bar(
    controls: Option<&PreviewControls>,
    displayed_row_count: usize,
    active_theme: &Theme,
) -> Div {
    let colors = active_theme.colors;

    let window_text = match controls {
        Some(controls) if displayed_row_count > 0 => {
            let start = controls.state.offset() + 1;
            let end = controls.state.offset() + displayed_row_count as u64;
            format!("rows {start}-{end}")
        }
        _ => "rows -".to_owned(),
    };

    let page_text = match controls {
        Some(controls) => page_readout_text(&controls.state),
        None => "page -".to_owned(),
    };

    let size_text = match controls {
        Some(controls) => format!("{} / page", controls.state.page_size()),
        None => "- / page".to_owned(),
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(app_theme::PAGER_GROUP_GAP)
        .font_family(&active_theme.fonts.data)
        .text_size(px(app_theme::PAGER_TEXT_SIZE))
        .child(
            div()
                .text_color(rgb(colors.text_tertiary))
                .child(window_text),
        )
        .child(pager_button(
            "results-pager-first",
            "\u{ab}",
            active_pager_action(
                controls,
                |c| !c.state.is_first_page(),
                PreviewAction::FirstPage,
            ),
            active_theme,
        ))
        .child(pager_button(
            "results-pager-prev",
            "\u{2039} Prev",
            active_pager_action(
                controls,
                |c| !c.state.is_first_page(),
                PreviewAction::PrevPage,
            ),
            active_theme,
        ))
        .child(
            div()
                .text_color(rgb(colors.text_secondary))
                .child(page_text),
        )
        .child(pager_button(
            "results-pager-next",
            "Next \u{203a}",
            active_pager_action(
                controls,
                |c| !c.state.is_last_page(),
                PreviewAction::NextPage,
            ),
            active_theme,
        ))
        .child(pager_button(
            "results-pager-last",
            "\u{bb}",
            active_pager_action(
                controls,
                |c| !c.state.is_last_page(),
                PreviewAction::LastPage,
            ),
            active_theme,
        ))
        .child(pager_button(
            "results-pager-page-size",
            size_text,
            active_pager_action(controls, |_c| true, PreviewAction::CyclePageSize),
            active_theme,
        ))
}

/// The "page N / M" readout for a live preview's pager. An estimated total
/// makes the last-page number approximate, so it is marked with
/// [`ESTIMATE_MARKER`] exactly as an estimated row count is marked elsewhere;
/// an unknown total shows only the current page number.
fn page_readout_text(state: &PreviewQueryState) -> String {
    match state.last_page_number() {
        Some(last) => {
            let marker = if state.total_is_estimated() {
                ESTIMATE_MARKER
            } else {
                ""
            };
            format!("page {} / {marker}{last}", state.page())
        }
        None => format!("page {}", state.page()),
    }
}

/// `Some((dispatch, action))` when `controls` is `Some` and `enabled` holds
/// for it, else `None`: the shared "is this control clickable right now"
/// gate every pager button goes through.
fn active_pager_action(
    controls: Option<&PreviewControls>,
    enabled: impl FnOnce(&PreviewControls) -> bool,
    action: PreviewAction,
) -> Option<(PreviewDispatch, PreviewAction)> {
    let controls = controls?;
    if !enabled(controls) {
        return None;
    }
    Some((controls.dispatch.clone(), action))
}

/// One pager control: clickable and normally styled when `action` is
/// `Some`, otherwise disabled and muted.
fn pager_button(
    id: &'static str,
    label: impl Into<SharedString>,
    action: Option<(PreviewDispatch, PreviewAction)>,
    active_theme: &Theme,
) -> gpui::Stateful<Div> {
    let colors = active_theme.colors;
    let button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(app_theme::PAGER_BUTTON_HEIGHT)
        .px(app_theme::PAGER_BUTTON_PADDING_X)
        .rounded(px(app_theme::PAGER_BUTTON_RADIUS))
        .border_1()
        .border_color(rgb(colors.border))
        .child(label.into());

    match action {
        Some((dispatch, action)) => button
            .cursor_pointer()
            .text_color(rgb(colors.text_primary))
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .on_click(move |_event, window, cx| dispatch(action.clone(), window, cx)),
        None => button
            .text_color(rgb(colors.text_tertiary))
            .opacity(app_theme::PAGER_DISABLED_OPACITY),
    }
}

#[cfg(test)]
mod tests {
    use super::page_readout_text;
    use zsql_core::RowCount;
    use zsql_core::preview_state::PreviewQueryState;

    #[test]
    fn page_readout_shows_the_current_page_alone_when_the_total_is_unknown() {
        let state = PreviewQueryState::new(200);
        assert_eq!(page_readout_text(&state), "page 1");
    }

    #[test]
    fn page_readout_shows_an_exact_last_page_without_a_marker() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(450)));
        assert_eq!(page_readout_text(&state), "page 1 / 3");
    }

    #[test]
    fn page_readout_marks_an_estimated_last_page() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Estimated(450)));
        assert_eq!(page_readout_text(&state), "page 1 / ~3");
    }
}
