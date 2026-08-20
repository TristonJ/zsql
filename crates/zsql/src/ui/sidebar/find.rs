//! The sidebar's quick-find session: a compact find row that borrows the
//! database row's slot (see [`super::db_row`]), filtering whichever pane is
//! active by a live query. Owns the query input and reacts to it; the pure
//! filtering itself lives in [`super::filter`].

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    ClickEvent, Context, Div, Entity, Focusable, Stateful, Subscription, Window, div, prelude::*,
    px, rgb, uniform_list,
};
use zsql_core::SchemaTree;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollable::{Axis, ScrollSource, WithScrollbars};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState, TextFieldStyle};
use zsql_ui::theme::{ActiveTheme, Theme};
use zsql_ui::tree::{META_TEXT_SIZE, disclosure_glyph, row_meta, row_shell};

use super::filter::{self, FilteredRow, MatchRange};
use super::model::SidebarPane;
use super::{SidebarView, sidebar_tree_content_height};
use crate::session::SchemaState;
use crate::ui::theme;

/// The key context the find row's own bindings (Esc) are scoped to,
/// active whenever the row's query input holds window focus.
pub(super) const KEY_CONTEXT: &str = "SidebarFind";

/// The leading magnifying-glass glyph the find row renders.
const GLASS_GLYPH: &str = "\u{2315}";

/// The schema tree filtered for one query, cached so a single query change
/// pays for one full-tree scan rather than one per reader (the find row's
/// own counter and the filtered tree body both want the same rows).
struct CachedSchemaFilter {
    schema_generation: u64,
    query: String,
    rows: Rc<Vec<FilteredRow>>,
}

/// The live find session: the query input and the subscriptions that react
/// to it.
pub(super) struct SidebarFind {
    input: Entity<TextFieldState>,
    last_query: String,
    schema_filter_cache: RefCell<Option<CachedSchemaFilter>>,
    _query_changed: Subscription,
    _submit: Subscription,
}

impl SidebarFind {
    fn new(placeholder: &'static str, window: &Window, cx: &mut Context<SidebarView>) -> Self {
        let input = cx.new(|cx| {
            TextFieldState::new(placeholder, None, cx).style(TextFieldStyle {
                height: theme::SIDEBAR_FIND_INPUT_HEIGHT,
                padding_x: theme::SIDEBAR_FIND_INPUT_PADDING_X,
                padding_y: px(0.0),
                border_w: px(0.0),
                text_size: px(theme::SIDEBAR_DB_ROW_NAME_TEXT_SIZE),
                ..Default::default()
            })
        });
        let query_changed = cx.observe(&input, |view: &mut SidebarView, field, cx| {
            let value = field.read(cx).value().to_string();
            let Some(find) = &mut view.find else {
                return;
            };
            if find.last_query == value {
                return;
            }
            find.last_query.clone_from(&value);
            apply_query_change(view, cx);
        });
        let submit = cx.subscribe_in(&input, window, |view, _input, event, window, cx| {
            if matches!(event, TextFieldEvent::Submit) {
                open_first_visible_match(view, window, cx);
            }
        });
        Self {
            input,
            last_query: String::new(),
            schema_filter_cache: RefCell::new(None),
            _query_changed: query_changed,
            _submit: submit,
        }
    }

    /// `tree` filtered for `query`, reusing the last computed result when
    /// both `query` and `schema_generation` match it exactly.
    fn filtered_schema_rows(
        &self,
        tree: &SchemaTree,
        schema_generation: u64,
        query: &str,
    ) -> Rc<Vec<FilteredRow>> {
        let mut cache = self.schema_filter_cache.borrow_mut();
        if let Some(cached) = cache.as_ref()
            && cached.schema_generation == schema_generation
            && cached.query == query
        {
            return cached.rows.clone();
        }
        let rows = Rc::new(filter::flatten_schema_tree_filtered(tree, query));
        *cache = Some(CachedSchemaFilter {
            schema_generation,
            query: query.to_owned(),
            rows: rows.clone(),
        });
        rows
    }
}

/// The active pane's schema tree filtered for `query`, sharing one
/// computed row list across every reader within the same render pass. See
/// [`SidebarFind::filtered_schema_rows`].
fn cached_filtered_schema_rows(
    view: &SidebarView,
    tree: &SchemaTree,
    query: &str,
    cx: &Context<SidebarView>,
) -> Rc<Vec<FilteredRow>> {
    let schema_generation = view.session.read(cx).schema_generation();
    match &view.find {
        Some(find) => find.filtered_schema_rows(tree, schema_generation, query),
        None => Rc::new(filter::flatten_schema_tree_filtered(tree, query)),
    }
}

/// [`super::OpenFind`]'s handler: open the find row and focus its input, or
/// refocus it if already open.
#[tracing::instrument(name = "sidebar_open_find", skip_all)]
pub(super) fn open(view: &mut SidebarView, window: &mut Window, cx: &mut Context<SidebarView>) {
    if let Some(find) = &view.find {
        window.focus(&find.input.read(cx).focus_handle(cx));
        return;
    }
    view.close_db_switcher(cx);
    let session = SidebarFind::new(placeholder_for(view.active_pane), window, cx);
    window.focus(&session.input.read(cx).focus_handle(cx));
    view.find = Some(session);
    cx.notify();
}

/// The placeholder text the find row's input shows while `pane` is active.
fn placeholder_for(pane: SidebarPane) -> &'static str {
    match pane {
        SidebarPane::Schema => "Find in schema...",
        SidebarPane::Scripts => "Find in scripts...",
    }
}

/// Keep an open find session's input placeholder matching `pane`: a filter
/// that survives a pane switch must not keep showing the previous pane's
/// placeholder.
pub(super) fn sync_placeholder_for_pane(
    view: &SidebarView,
    pane: SidebarPane,
    cx: &mut Context<SidebarView>,
) {
    let Some(find) = &view.find else {
        return;
    };
    find.input.update(cx, |input, _cx| {
        input.set_placeholder_quiet(placeholder_for(pane));
    });
}

/// [`super::CloseFind`]'s handler: close the row, clear the query, and
/// restore the collapse state captured before filtering began.
pub(super) fn close(view: &mut SidebarView, window: &mut Window, cx: &mut Context<SidebarView>) {
    if view.find.take().is_none() {
        return;
    }
    restore_collapse_snapshot(view, cx);
    window.focus(&view.focus_handle);
    cx.notify();
}

/// Reacts to every actual query change: captures the pre-filter collapse
/// snapshot on the first keystroke, auto-expands every catalog/schema on
/// the path to a match, and restores the snapshot once the query empties
/// back out (without closing the row).
fn apply_query_change(view: &mut SidebarView, cx: &mut Context<SidebarView>) {
    let query = current_query(view, cx);
    if query.trim().is_empty() {
        restore_collapse_snapshot(view, cx);
        cx.notify();
        return;
    }
    if view.pre_filter_collapse.is_none() {
        view.pre_filter_collapse = Some(filter::CollapseSnapshot::capture(
            &view.collapsed_catalogs,
            &view.collapsed_schemas,
        ));
    }
    let expand = match view.session.read(cx).schema() {
        SchemaState::Ready(tree) => Some(filter::expanded_ancestors_for_query(tree, query.trim())),
        SchemaState::NotLoaded | SchemaState::Loading | SchemaState::Error(_) => None,
    };
    if let Some((catalogs, schemas)) = expand {
        view.collapsed_catalogs
            .retain(|name| !catalogs.contains(name));
        view.collapsed_schemas.retain(|key| !schemas.contains(key));
    }
    cx.notify();
}

/// Restore the pre-filter collapse snapshot, if one was captured, and drop
/// it, rebuilding the unfiltered row list to match. A no-op if the query
/// never went non-empty.
fn restore_collapse_snapshot(view: &mut SidebarView, cx: &mut Context<SidebarView>) {
    if let Some(snapshot) = view.pre_filter_collapse.take() {
        let (catalogs, schemas) = snapshot.into_parts();
        view.collapsed_catalogs = catalogs;
        view.collapsed_schemas = schemas;
        view.sync_rows(cx);
    }
}

/// The find row's current query, `""` when no find session is open.
pub(super) fn current_query(view: &SidebarView, cx: &gpui::App) -> String {
    view.find
        .as_ref()
        .map(|find| find.input.read(cx).value().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn input_focus_handle_for_test(
    view: &SidebarView,
    cx: &gpui::App,
) -> Option<gpui::FocusHandle> {
    view.find
        .as_ref()
        .map(|find| find.input.read(cx).focus_handle(cx))
}

/// Activate (open the tab for, or focus the script tab of) the first row
/// the active pane's live filter currently shows. A no-op with an empty
/// query or zero visible matches.
fn open_first_visible_match(
    view: &mut SidebarView,
    window: &mut Window,
    cx: &mut Context<SidebarView>,
) {
    let query = current_query(view, cx);
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return;
    }
    match view.active_pane {
        SidebarPane::Schema => {
            let SchemaState::Ready(tree) = view.session.read(cx).schema() else {
                return;
            };
            let tree = tree.clone();
            let rows = filter::flatten_schema_tree_filtered(&tree, trimmed);
            let first_relation = rows.into_iter().find_map(|row| match row {
                FilteredRow::Relation { schema, name, .. } => Some((schema, name)),
                FilteredRow::Catalog { .. } | FilteredRow::Schema { .. } => None,
            });
            if let Some((schema, name)) = first_relation {
                view.preview(&schema, &name, window, cx);
            }
        }
        SidebarPane::Scripts => {
            let matches = filter::filter_script_rows(&view.script_rows, trimmed);
            if let Some(first) = matches.first() {
                let target = view.script_rows[first.index].target.clone();
                view.open_script_row(target, window, cx);
            }
        }
    }
}

/// The sidebar's top slot, under the pane tabs: the find row while a
/// session is open, otherwise the database row (see
/// [`super::db_row::render_db_row`]).
pub(super) fn render_top_slot(
    view: &SidebarView,
    cx: &mut Context<SidebarView>,
) -> Option<gpui::AnyElement> {
    if view.find.is_some() {
        Some(render_find_row(view, cx).into_any_element())
    } else {
        super::db_row::render_db_row(view, cx).map(IntoElement::into_any_element)
    }
}

/// The find row rendered in the database row's slot.
fn render_find_row(view: &SidebarView, cx: &mut Context<SidebarView>) -> Stateful<Div> {
    let Some(find) = &view.find else {
        return div().id("sidebar-find-row-empty");
    };
    let active_theme = cx.theme();
    let query = find.input.read(cx).value().to_string();
    let trimmed = query.trim();
    let counter = (!trimmed.is_empty()).then(|| find_row_counter(view, trimmed, cx));

    div()
        .id("sidebar-find-row")
        .key_context(KEY_CONTEXT)
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(theme::SIDEBAR_DB_ROW_GAP)
        .h(theme::SIDEBAR_DB_ROW_HEIGHT)
        .pl(theme::SIDEBAR_DB_ROW_PADDING_X)
        .pr(theme::SIDEBAR_FIND_ROW_PADDING_RIGHT)
        .border_b_1()
        .border_color(rgb(active_theme.colors.border_soft))
        .child(
            div()
                .flex_shrink_0()
                .text_size(theme::SIDEBAR_FIND_ROW_ICON_SIZE)
                .text_color(rgb(active_theme.colors.text_tertiary))
                .child(GLASS_GLYPH),
        )
        .child(div().flex_1().min_w_0().child(find.input.clone()))
        .children(counter)
        .child(
            div()
                .id("sidebar-find-close")
                .flex_shrink_0()
                .cursor_pointer()
                .text_color(rgb(active_theme.colors.text_tertiary))
                .hover(|el| el.text_color(rgb(active_theme.colors.text_primary)))
                .on_click(cx.listener(|view, _event: &ClickEvent, window, cx| {
                    close(view, window, cx);
                }))
                .child(icon(
                    IconName::Close,
                    theme::SIDEBAR_FIND_ROW_ICON_SIZE,
                    active_theme.colors.text_tertiary,
                )),
        )
}

/// The find row's own "n of m" counter: how many rows in the active pane
/// currently match `query`, of how many exist in total.
fn find_row_counter(view: &SidebarView, query: &str, cx: &Context<SidebarView>) -> Div {
    let active_theme = cx.theme();
    let (matched, total, empty) = match view.active_pane {
        SidebarPane::Schema => match view.session.read(cx).schema() {
            SchemaState::Ready(tree) => {
                let rows = cached_filtered_schema_rows(view, tree, query, cx);
                (
                    filter::matched_label_count(&rows),
                    filter::total_relation_count(tree),
                    rows.is_empty(),
                )
            }
            SchemaState::NotLoaded | SchemaState::Loading | SchemaState::Error(_) => (0, 0, true),
        },
        SidebarPane::Scripts => {
            let matches = filter::filter_script_rows(&view.script_rows, query);
            let empty = matches.is_empty();
            (matches.len(), view.script_rows.len(), empty)
        }
    };
    div()
        .flex_shrink_0()
        .font_family(&active_theme.fonts.data)
        .text_size(px(META_TEXT_SIZE))
        .text_color(rgb(if empty {
            active_theme.colors.status_error
        } else {
            active_theme.colors.text_tertiary
        }))
        .child(format!("{matched} of {total}"))
}

/// The schema pane's body while a filter is live: the filtered tree, or,
/// with an empty query, the normal placeholder/tree ([`SidebarView`]'s own
/// [`super::SidebarView::render_body`]).
pub(super) fn render_schema_body(
    view: &mut SidebarView,
    window: &mut Window,
    cx: &mut Context<SidebarView>,
) -> gpui::AnyElement {
    let query = current_query(view, cx);
    let trimmed = query.trim();
    if !trimmed.is_empty()
        && let SchemaState::Ready(tree) = view.session.read(cx).schema()
    {
        let tree = tree.clone();
        return render_filtered_tree(view, &tree, trimmed, cx);
    }
    view.render_body(window, cx)
}

/// The filtered schema tree: every surviving row from
/// [`filter::flatten_schema_tree_filtered`], or a small empty state with
/// zero matches.
fn render_filtered_tree(
    view: &mut SidebarView,
    tree: &SchemaTree,
    query: &str,
    cx: &mut Context<SidebarView>,
) -> gpui::AnyElement {
    let rows = cached_filtered_schema_rows(view, tree, query, cx);
    if rows.is_empty() {
        let query = query.to_owned();
        return div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(render_filter_empty_state(&query, cx))
            .into_any_element();
    }

    let row_count = rows.len();
    let content_height = f32::from(sidebar_tree_content_height(row_count));
    let tree_scroll_handle = view.tree_scroll_handle.clone();
    view.scroll.update(cx, |scroll, _cx| {
        scroll.vertical(Axis::new(
            ScrollSource::UniformList(tree_scroll_handle),
            content_height,
        ));
    });

    let list_rows = rows.clone();
    let list = uniform_list(
        "sidebar-filtered-rows",
        row_count,
        cx.processor(move |view, range: std::ops::Range<usize>, _window, cx| {
            range
                .map(|ix| render_filtered_row(view, &list_rows[ix], ix, cx))
                .collect::<Vec<_>>()
        }),
    )
    .flex_1()
    .track_scroll(view.tree_scroll_handle.clone());

    div()
        .id("sidebar-tree")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .py(px(theme::SIDEBAR_TREE_PADDING_Y))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(list)
                .with_scrollbars(
                    &view.scroll,
                    SidebarView::tree_scrollbar_style(cx.theme()),
                    cx,
                ),
        )
        .into_any_element()
}

/// One filtered tree row: catalog/schema rows show the "n of m" descendant
/// count and never toggle (the filter itself owns what is expanded);
/// relation rows reuse [`SidebarView`]'s own row renderer, so a filtered
/// relation is a real row: it opens the same preview and context menu an
/// unfiltered click would.
fn render_filtered_row(
    view: &SidebarView,
    row: &FilteredRow,
    ix: usize,
    cx: &Context<SidebarView>,
) -> Stateful<Div> {
    let active_theme = cx.theme();
    match row {
        FilteredRow::Catalog {
            name,
            matched_schemas,
            total_schemas,
            label_match,
        } => row_shell(theme::SIDEBAR_INDENT_L0, active_theme)
            .id(ix)
            .child(disclosure_glyph(true, active_theme))
            .child(icon(
                IconName::Database,
                theme::SIDEBAR_ROW_ICON_SIZE,
                active_theme.colors.text_tertiary,
            ))
            .child(highlighted_row_label(
                name,
                label_match.as_ref(),
                active_theme,
            ))
            .child(row_meta(
                format!("{matched_schemas} of {total_schemas}"),
                active_theme,
            )),
        FilteredRow::Schema {
            name,
            matched_relations,
            total_relations,
            label_match,
            ..
        } => row_shell(theme::SIDEBAR_INDENT_L1, active_theme)
            .id(ix)
            .child(disclosure_glyph(true, active_theme))
            .child(icon(
                IconName::Schema,
                theme::SIDEBAR_ROW_ICON_SIZE,
                active_theme.colors.text_tertiary,
            ))
            .child(highlighted_row_label(
                name,
                label_match.as_ref(),
                active_theme,
            ))
            .child(row_meta(
                format!("{matched_relations} of {total_relations}"),
                active_theme,
            )),
        FilteredRow::Relation {
            schema,
            name,
            kind,
            column_count,
            label_match,
        } => view.render_relation_row(
            ix,
            schema,
            name,
            *kind,
            *column_count,
            label_match.as_ref(),
            cx,
        ),
    }
}

/// A row label with `match_range` (if any) washed in the shared quick-find
/// amber, otherwise identical to [`zsql_ui::tree::row_label`].
pub(super) fn highlighted_row_label(
    text: &str,
    match_range: Option<&MatchRange>,
    active_theme: &Theme,
) -> Div {
    highlighted_label(text, match_range, active_theme, true)
}

/// [`highlighted_row_label`], but never the greedy flex item: for a row
/// whose label sits beside its own trailing affordance (e.g. the scripts
/// pane's open-library dot) rather than directly against a row's meta text.
pub(super) fn highlighted_row_label_fixed(
    text: &str,
    match_range: Option<&MatchRange>,
    active_theme: &Theme,
) -> Div {
    highlighted_label(text, match_range, active_theme, false)
}

fn highlighted_label(
    text: &str,
    match_range: Option<&MatchRange>,
    active_theme: &Theme,
    grow: bool,
) -> Div {
    let Some(range) = match_range else {
        let label = div().min_w_0().truncate().child(text.to_owned());
        return if grow { label.flex_1() } else { label };
    };
    let before = text[..range.start].to_owned();
    let matched = text[range.start..range.end].to_owned();
    let after = text[range.end..].to_owned();
    let wrapper = div().min_w_0().overflow_hidden().flex().flex_row();
    let wrapper = if grow { wrapper.flex_1() } else { wrapper };
    wrapper
        .child(before)
        .child(
            div()
                .rounded(px(theme::QUICK_FIND_MATCH_RADIUS))
                .bg(theme::quick_find_match_bg(active_theme))
                .child(matched),
        )
        .child(after)
}

/// The small empty state a filtered pane shows with a non-empty query and
/// zero surviving rows, distinct from the pane's normal row list.
pub(super) fn render_filter_empty_state(query: &str, cx: &Context<SidebarView>) -> Stateful<Div> {
    let colors = cx.theme().colors;
    div()
        .id("sidebar-filter-empty")
        .debug_selector(|| "sidebar-filter-empty".to_owned())
        .flex_shrink_0()
        .mx(theme::SIDEBAR_SCRIPTS_EMPTY_MARGIN_X)
        .mt(theme::SIDEBAR_SCRIPTS_EMPTY_MARGIN_TOP)
        .mb(theme::SIDEBAR_SCRIPTS_EMPTY_MARGIN_BOTTOM)
        .px(theme::SIDEBAR_SCRIPTS_EMPTY_PADDING_X)
        .py(theme::SIDEBAR_SCRIPTS_EMPTY_PADDING_Y)
        .border_1()
        .border_dashed()
        .border_color(rgb(colors.border_soft))
        .rounded(px(theme::SIDEBAR_SCRIPTS_EMPTY_RADIUS))
        .flex()
        .flex_col()
        .items_center()
        .text_center()
        .gap(theme::SIDEBAR_SCRIPTS_EMPTY_GAP)
        .child(
            div()
                .text_size(px(theme::SIDEBAR_SCRIPTS_EMPTY_TITLE_TEXT_SIZE))
                .text_color(rgb(colors.text_secondary))
                .child(format!("Nothing matches \"{query}\"")),
        )
        .child(
            div()
                .text_size(px(theme::SIDEBAR_SCRIPTS_EMPTY_DETAIL_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child("Esc clears the filter and restores the tree."),
        )
}

#[cfg(test)]
mod tests {
    use zsql_ui::theme::Theme;

    use super::highlighted_row_label;

    #[test]
    fn highlighted_row_label_builds_with_and_without_a_match_range() {
        let theme = Theme::default();
        let _no_match = highlighted_row_label("orders", None, &theme);
        let _with_match = highlighted_row_label("orders", Some(&(0..3)), &theme);
    }
}
