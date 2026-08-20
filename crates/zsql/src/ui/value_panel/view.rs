use std::ops::Range;

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Div, FocusHandle, MouseButton,
    MouseUpEvent, Render, Stateful, Window, actions, div, prelude::*, px, rgb,
};
use zsql_core::{ColumnMeta, Value};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::ValuePanelBindings;
use super::body::{MonoBody, NullBody, OversizedJsonBody, TextBody};
use super::data::{
    self, BytesMode, JsonLoad, JsonMode, RendererKind, TimestampMode, ValuePanelState,
};
use crate::config::ValuePanelConfig;
use crate::ui::format::{self, format_value, format_value_for_clipboard};
use crate::ui::theme;
use crate::ui::value_panel::data::ValuePanelContent;

/// The key context the value panel's own key bindings are scoped to, so
/// they only fire while keyboard focus has moved onto the panel
pub const VALUE_PANEL_KEY_CONTEXT: &str = "ValuePanel";

/// The value panel's cached load of a `Value::Json` cell's source text: the
/// cell it belongs to, the exact source text (for Raw mode -- never
/// reconstructed from the parsed tree, so Raw always matches what the
/// driver returned), and the parse outcome.
#[derive(Debug, Clone)]
pub(super) struct ValuePanelJsonCache {
    id: usize,
    pub(super) text: String,
    /// Read by `json_tree`'s keyboard navigation to reach the parsed tree
    /// without re-parsing on every keypress.
    pub(super) load: JsonLoad,
}

pub struct ValuePanel {
    /// Read and mutated by `json_tree`'s tree navigation, which shares the
    /// panel's pin/mode/selection state rather than keeping its own copy.
    pub(super) state: ValuePanelState,
    /// The parsed (or failed/oversized) JSON document for the value the
    /// panel is currently showing, cached so navigating the tree does not
    /// re-parse on every render. Invalidated (see `sync_value_panel_json`)
    /// whenever the panel starts showing a different cell. Read by
    /// `json_tree`'s keyboard navigation to reach the parsed tree.
    pub(super) json: Option<ValuePanelJsonCache>,
    /// Our focus handle
    focus_handle: FocusHandle,
    /// The parent focus handle, so we can return focus when the panel is
    /// explicitly closed.
    parent_focus_handle: FocusHandle,
    pub(super) config: ValuePanelConfig,
}

actions!(
    zsql_value_panel,
    [
        TreeUp,
        TreeDown,
        TreeCollapse,
        TreeExpand,
        CopyTreeNodeValue,
        CopyTreeNodePath,
        ClosePanelFromPanel,
        FocusGridFromPanel,
    ]
);

/// Register the value panel's key bindings from `bindings`. Call once at
/// startup, before any window that hosts a [`ValuePanel`] is opened.
pub fn init(cx: &mut App, bindings: &ValuePanelBindings) {
    use crate::keybindings::bind_all;

    let mut keys = Vec::new();
    bind_all(
        &mut keys,
        &bindings.tree_up,
        &TreeUp,
        VALUE_PANEL_KEY_CONTEXT,
    );
    bind_all(
        &mut keys,
        &bindings.tree_down,
        &TreeDown,
        VALUE_PANEL_KEY_CONTEXT,
    );
    bind_all(
        &mut keys,
        &bindings.tree_collapse,
        &TreeCollapse,
        VALUE_PANEL_KEY_CONTEXT,
    );
    bind_all(
        &mut keys,
        &bindings.tree_expand,
        &TreeExpand,
        VALUE_PANEL_KEY_CONTEXT,
    );
    bind_all(
        &mut keys,
        &bindings.copy_tree_node_value,
        &CopyTreeNodeValue,
        VALUE_PANEL_KEY_CONTEXT,
    );
    bind_all(
        &mut keys,
        &bindings.copy_tree_node_path,
        &CopyTreeNodePath,
        VALUE_PANEL_KEY_CONTEXT,
    );
    bind_all(
        &mut keys,
        &bindings.close_panel_from_panel,
        &ClosePanelFromPanel,
        VALUE_PANEL_KEY_CONTEXT,
    );
    bind_all(
        &mut keys,
        &bindings.focus_grid_from_panel,
        &FocusGridFromPanel,
        VALUE_PANEL_KEY_CONTEXT,
    );
    let registered = keys.len();
    cx.bind_keys(keys);
    tracing::debug!(registered, "value panel keybindings registered");
}

impl Render for ValuePanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_theme = cx.theme();
        let colors = active_theme.colors;

        let body = match self.state.content() {
            Some(c) => self.render_value_panel_content(&c.value, &c.column, cx),
            None => div()
                .flex()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_color(rgb(colors.text_tertiary))
                .child("No cell selected"),
        };

        div()
            .id("value-panel")
            .key_context(VALUE_PANEL_KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::tree_up))
            .on_action(cx.listener(Self::tree_down))
            .on_action(cx.listener(Self::tree_collapse))
            .on_action(cx.listener(Self::tree_expand))
            .on_action(cx.listener(Self::copy_tree_node_value))
            .on_action(cx.listener(Self::copy_tree_node_path))
            .on_action(cx.listener(Self::close_panel_from_panel))
            .on_action(cx.listener(Self::focus_parent_from_panel))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _window, cx| {
                    if view.state.text_selection_mut().end_drag() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _window, cx| {
                    if view.state.text_selection_mut().end_drag() {
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w_full()
            .h_full()
            .bg(rgb(colors.bg_panel))
            .child(body)
    }
}

impl ValuePanel {
    #[must_use]
    pub fn new(
        parent_focus_handle: FocusHandle,
        config: ValuePanelConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            state: ValuePanelState::new(),
            json: None,
            focus_handle: cx.focus_handle(),
            parent_focus_handle,
            config,
        }
    }

    /// Set the configuration for the panel
    pub fn set_config(&mut self, config: ValuePanelConfig) {
        self.config = config;
        self.state.clear_text_selection();
    }

    /// Toggle the visibility of the panel
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.state.is_open() {
            self.state.close();
        } else {
            self.state.open();
        }
        cx.notify();
    }

    /// Open the panel
    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.state.open();
        cx.notify();
    }

    /// Close the panel
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.state.close();
        cx.notify();
    }

    /// Whether the panel should be open or not
    pub fn is_open(&self) -> bool {
        self.state.is_open()
    }

    /// Quickly check whether the panel would update if the given content were set.
    /// This is useful if you want to avoid cloning the content before deciding to call
    /// `update_content`.
    pub fn would_update_content(&self, id: Option<usize>) -> bool {
        match (id, self.state.content()) {
            (Some(n), Some(c)) if n == c.id => false,
            (None, _) => self.state.content().is_some(),
            (Some(_), _) => !self.state.is_pinned(),
        }
    }

    /// Set the content of this value panel. Ignored if the panel is in the pinned state
    /// This is safe to call repeatedly, as it only updates the content if it's id is
    /// different from the current content id.
    pub fn update_content(&mut self, content: Option<ValuePanelContent>) {
        let update = self.would_update_content(content.as_ref().map(|c| c.id));
        if !update {
            return;
        }
        self.state.set_content(content);
        self.state.reset_tree();

        // Update the JSON cache if the new content is a JSON value
        let (id, text) = match &self.state.content() {
            Some(c) => match &c.value {
                Value::Json(text) => (c.id, text),
                _ => return,
            },
            None => return,
        };
        let span = tracing::info_span!("results_value_panel_parse_json", id = id, len = text.len());
        let _guard = span.enter();
        let load = data::load_json(text, &self.config);
        match &load {
            JsonLoad::Invalid(failure) => tracing::warn!(
                error = %failure.message,
                "value panel: JSON cell failed to parse; falling back to Raw"
            ),
            JsonLoad::Oversized { total_bytes, .. } => tracing::info!(
                total_bytes,
                "value panel: JSON cell exceeds the eager-parse threshold; opened in Raw \
                 with a preview"
            ),
            JsonLoad::Parsed(_) => {
                tracing::debug!("value panel: parsed a JSON cell into its tree");
            }
        }
        self.json = Some(ValuePanelJsonCache {
            id,
            text: text.clone(),
            load,
        });
    }

    /// The focus handle for this panel
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus_handle
    }

    /// Read-only access to the panel's [`ValuePanelState`], for tests that
    /// need to observe its open/pin/mode/tree state directly.
    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> &ValuePanelState {
        &self.state
    }

    /// Mutable access to the panel's [`ValuePanelState`], for tests that need
    /// to drive its modes, pin, or tree selection (which the component only
    /// exposes through render-time controls otherwise).
    #[cfg(test)]
    pub(crate) fn state_mut_for_test(&mut self) -> &mut ValuePanelState {
        &mut self.state
    }

    /// The parse outcome of the panel's currently cached JSON cell, for tests
    /// that assert on the private JSON cache.
    #[cfg(test)]
    pub(crate) fn json_load_for_test(&self) -> Option<&JsonLoad> {
        self.json.as_ref().map(|c| &c.load)
    }

    /// The header + renderer-specific sub-bar/body/footer for `value`
    /// (`column`'s cell), keyed off [`data::renderer_for`].
    fn render_value_panel_content(
        &self,
        value: &Value,
        column: &ColumnMeta,
        cx: &mut Context<Self>,
    ) -> Div {
        let active_theme = cx.theme();
        let renderer = data::renderer_for(value, &column.type_name);
        let selection = self.state.text_selection().range();
        let body_text = self.current_body_text();
        let text = body_text.as_deref().unwrap_or_default();

        let mut root = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.render_value_panel_header(column, active_theme, cx));

        match renderer {
            RendererKind::Json => {
                root = root
                    .child(self.render_json_subbar(active_theme, cx))
                    .child(self.render_json_body(text, active_theme, selection, cx))
                    .child(self.render_json_footer(active_theme, cx));
            }
            RendererKind::Text => {
                let label = format!("Raw - {} chars", text.chars().count());
                root = root
                    .child(Self::render_static_subbar(&label, active_theme))
                    .child(TextBody::new(cx.entity(), text, selection));
            }
            RendererKind::Bytes => {
                root = root
                    .child(self.render_bytes_subbar(active_theme, cx))
                    .child(MonoBody::new(cx.entity(), text, selection).scrollable());
            }
            RendererKind::Number => {
                root = root
                    .child(Self::render_static_subbar(
                        "Raw - full precision",
                        active_theme,
                    ))
                    .child(MonoBody::new(cx.entity(), text, selection));
            }
            RendererKind::Timestamp => {
                root = root
                    .child(self.render_timestamp_subbar(active_theme, cx))
                    .child(MonoBody::new(cx.entity(), text, selection));
            }
            RendererKind::Bool => {
                root = root.child(MonoBody::new(cx.entity(), text, selection));
            }
            RendererKind::Null => {
                root = root.child(NullBody);
            }
            RendererKind::Unknown { type_name } => {
                root = root
                    .child(Self::render_static_subbar(&type_name, active_theme))
                    .child(MonoBody::new(cx.entity(), text, selection));
            }
        }
        root
    }

    /// The panel header: column name, pin/expand/close controls.
    fn render_value_panel_header(
        &self,
        column: &ColumnMeta,
        active_theme: &Theme,
        cx: &Context<Self>,
    ) -> Div {
        let colors = active_theme.colors;
        let pinned = self.state.is_pinned();
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::VALUE_PANEL_HEADER_HEIGHT)
            .px(theme::VALUE_PANEL_PADDING_X)
            .gap_2()
            .border_b_1()
            .border_color(rgb(colors.border_soft))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.text_primary))
                    .child(column.name.clone()),
            )
            .child(Self::mode_button(
                cx,
                "Pin",
                pinned,
                false,
                active_theme,
                |view, _window, cx| {
                    view.state.toggle_pinned();
                    cx.notify();
                },
            ))
            .child(Self::mode_button(
                cx,
                "Close",
                false,
                false,
                active_theme,
                |view, window, cx| {
                    view.state.close();
                    window.focus(&view.parent_focus_handle);
                    cx.notify();
                },
            ))
    }

    /// One mode-switcher/header toggle button: `label`, highlighted when
    /// `active`, dimmed and inert when `disabled`.
    fn mode_button(
        cx: &Context<Self>,
        label: &'static str,
        active: bool,
        disabled: bool,
        active_theme: &Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let colors = active_theme.colors;
        let mut btn = div()
            .id(label)
            .debug_selector(move || format!("value-panel-mode-button-{label}"))
            .px_2()
            .h(theme::VALUE_PANEL_BUTTON_HEIGHT)
            .flex()
            .items_center()
            .rounded(px(theme::VALUE_PANEL_BUTTON_RADIUS))
            .text_size(px(theme::VALUE_PANEL_LABEL_TEXT_SIZE))
            .child(label);
        if disabled {
            btn = btn.text_color(theme::value_panel_disabled_button_text(active_theme));
        } else if active {
            btn = btn
                .cursor_pointer()
                .bg(theme::sidebar_selected_bg(active_theme))
                .text_color(rgb(colors.accent))
                .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                    on_click(view, window, cx);
                }));
        } else {
            btn = btn
                .cursor_pointer()
                .text_color(rgb(colors.text_tertiary))
                .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                    on_click(view, window, cx);
                }));
        }
        btn
    }

    fn render_static_subbar(label: &str, active_theme: &Theme) -> Div {
        let colors = active_theme.colors;
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::VALUE_PANEL_SUBBAR_HEIGHT)
            .px(theme::VALUE_PANEL_PADDING_X)
            .border_b_1()
            .border_color(rgb(colors.border_soft))
            .text_size(px(theme::VALUE_PANEL_LABEL_TEXT_SIZE))
            .text_color(rgb(colors.text_tertiary))
            .child(label.to_owned())
    }

    fn render_json_subbar(&self, active_theme: &Theme, cx: &Context<Self>) -> Div {
        let colors = active_theme.colors;
        let current_mode = self.state.json_mode();
        let parsed = matches!(
            self.json.as_ref().map(|c| &c.load),
            Some(JsonLoad::Parsed(_))
        );
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::VALUE_PANEL_SUBBAR_HEIGHT)
            .px(theme::VALUE_PANEL_PADDING_X)
            .gap_2()
            .border_b_1()
            .border_color(rgb(colors.border_soft));
        for mode in data::JSON_MODES {
            let label = match mode {
                JsonMode::Tree => "Tree",
                JsonMode::Pretty => "Pretty",
                JsonMode::Raw => "Raw",
            };
            let disabled = matches!(mode, JsonMode::Tree | JsonMode::Pretty) && !parsed;
            bar = bar.child(Self::mode_button(
                cx,
                label,
                current_mode == mode,
                disabled,
                active_theme,
                move |view, _window, cx| {
                    view.state.set_json_mode(mode);
                    cx.notify();
                },
            ));
        }
        bar
    }

    fn render_json_body(
        &self,
        text: &str,
        active_theme: &Theme,
        selection: Option<Range<usize>>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(cache) = self.json.as_ref() else {
            return div().flex_1().min_h_0().into_any_element();
        };
        match &cache.load {
            JsonLoad::Invalid(_) => MonoBody::new(cx.entity(), text, selection)
                .scrollable()
                .into_any_element(),
            JsonLoad::Oversized { total_bytes, .. } => {
                OversizedJsonBody::new(cx.entity(), text, *total_bytes, selection)
                    .into_any_element()
            }
            JsonLoad::Parsed(node) => match self.state.json_mode() {
                JsonMode::Raw | JsonMode::Pretty => MonoBody::new(cx.entity(), text, selection)
                    .scrollable()
                    .into_any_element(),
                JsonMode::Tree => self
                    .render_json_tree(node, active_theme, cx)
                    .into_any_element(),
            },
        }
    }

    /// The panel's currently displayed body text, or `None` for JSON Tree
    /// mode and any state with no flat text body (`Null`, or no content).
    pub(super) fn current_body_text(&self) -> Option<String> {
        let content = self.state.content()?;
        let renderer = data::renderer_for(&content.value, &content.column.type_name);
        match renderer {
            RendererKind::Json => {
                let cache = self.json.as_ref()?;
                match &cache.load {
                    JsonLoad::Invalid(_) => Some(cache.text.clone()),
                    JsonLoad::Oversized { preview, .. } => Some(preview.clone()),
                    JsonLoad::Parsed(node) => match self.state.json_mode() {
                        JsonMode::Raw => Some(cache.text.clone()),
                        JsonMode::Pretty => {
                            serde_json::to_string_pretty(&data::json_node_to_serde(node)).ok()
                        }
                        JsonMode::Tree => None,
                    },
                }
            }
            RendererKind::Text => match &content.value {
                Value::Text(text) | Value::Uuid(text) => Some(text.clone()),
                _ => None,
            },
            RendererKind::Bytes => {
                let bytes = match &content.value {
                    Value::Bytes(bytes) => bytes.as_slice(),
                    _ => &[],
                };
                Some(match self.state.bytes_mode() {
                    BytesMode::Hex => data::format_hex_dump(bytes, self.config.hex_bytes_per_row),
                    BytesMode::Base64 => format::base64_encode(bytes),
                })
            }
            RendererKind::Number => data::number_raw_text(&content.value),
            RendererKind::Timestamp => {
                let raw = data::timestamp_raw_text(&content.value)?;
                Some(match self.state.timestamp_mode() {
                    TimestampMode::Raw => raw.to_owned(),
                    TimestampMode::Utc => data::timestamp_utc_text(raw)
                        .unwrap_or_else(|| format!("{raw} (could not be parsed as a timestamp)")),
                })
            }
            RendererKind::Bool => match &content.value {
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            },
            RendererKind::Null => None,
            RendererKind::Unknown { .. } => Some(format_value(&content.value).text),
        }
    }

    fn render_json_footer(&self, active_theme: &Theme, cx: &Context<Self>) -> Div {
        let colors = active_theme.colors;
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::VALUE_PANEL_FOOTER_HEIGHT)
            .px(theme::VALUE_PANEL_PADDING_X)
            .gap_2()
            .border_t_1()
            .border_color(rgb(colors.border_soft))
            .text_size(px(theme::VALUE_PANEL_LABEL_TEXT_SIZE));

        match self.json.as_ref().map(|c| &c.load) {
            Some(JsonLoad::Invalid(failure)) => {
                bar = bar
                    .text_color(rgb(colors.status_error))
                    .child(failure.message.clone());
            }
            Some(JsonLoad::Parsed(_)) => {
                let path = data::json_path_string(self.state.selected_tree_path());
                bar = bar
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(rgb(colors.text_tertiary))
                            .child(path.clone()),
                    )
                    .child(
                        div()
                            .id("value-panel-copy-path")
                            .cursor_pointer()
                            .text_color(rgb(colors.accent))
                            .child("Copy path")
                            .on_click(cx.listener(
                                move |_view, _event: &ClickEvent, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                                },
                            )),
                    );
            }
            Some(JsonLoad::Oversized { .. }) | None => {}
        }
        bar
    }

    fn render_bytes_subbar(&self, active_theme: &Theme, cx: &Context<Self>) -> Div {
        let colors = active_theme.colors;
        let current_mode = self.state.bytes_mode();
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::VALUE_PANEL_SUBBAR_HEIGHT)
            .px(theme::VALUE_PANEL_PADDING_X)
            .gap_2()
            .border_b_1()
            .border_color(rgb(colors.border_soft));
        for mode in data::BYTES_MODES {
            let label = match mode {
                BytesMode::Hex => "Hex",
                BytesMode::Base64 => "Base64",
            };
            bar = bar.child(Self::mode_button(
                cx,
                label,
                current_mode == mode,
                false,
                active_theme,
                move |view, _window, cx| {
                    view.state.set_bytes_mode(mode);
                    cx.notify();
                },
            ));
        }
        bar
    }

    fn render_timestamp_subbar(&self, active_theme: &Theme, cx: &Context<Self>) -> Div {
        let colors = active_theme.colors;
        let current_mode = self.state.timestamp_mode();
        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::VALUE_PANEL_SUBBAR_HEIGHT)
            .px(theme::VALUE_PANEL_PADDING_X)
            .gap_2()
            .border_b_1()
            .border_color(rgb(colors.border_soft));
        for mode in data::TIMESTAMP_MODES {
            let label = match mode {
                TimestampMode::Raw => "Raw",
                TimestampMode::Utc => "UTC",
            };
            bar = bar.child(Self::mode_button(
                cx,
                label,
                current_mode == mode,
                false,
                active_theme,
                move |view, _window, cx| {
                    view.state.set_timestamp_mode(mode);
                    cx.notify();
                },
            ));
        }
        bar
    }

    /// The "Load full value" action on an oversized JSON preview: parses the
    /// complete source text on a background executor (never on the render
    /// path), then updates the cache once it finishes. A no-op unless the
    /// panel is currently showing an [`JsonLoad::Oversized`] value.
    #[tracing::instrument(name = "results_value_panel_load_full_json", skip_all)]
    pub(super) fn load_full_json_value(&mut self, cx: &mut Context<Self>) {
        let Some(cache) = &self.json else {
            return;
        };
        if !matches!(cache.load, JsonLoad::Oversized { .. }) {
            return;
        }
        let id = cache.id;
        let text = cache.text.clone();
        tracing::info!(
            id = id,
            len = text.len(),
            "value panel: loading the full oversized JSON value"
        );
        cx.spawn(async move |this, cx| {
            let load = cx
                .background_spawn(async move { data::load_json_full(&text) })
                .await;
            this.update(cx, |this, cx| {
                if this.json.as_ref().map(|c| c.id) == Some(id) {
                    let text = this
                        .json
                        .as_ref()
                        .map(|c| c.text.clone())
                        .unwrap_or_default();
                    this.json = Some(ValuePanelJsonCache { id, text, load });
                    this.state.clear_text_selection();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Cmd/Ctrl-C while the panel has focus: copy an active text selection's
    /// exact substring when one exists, else the selected tree node's own
    /// value (its JSON text) when a JSON tree is showing, else the panel's
    /// whole formatted target cell. Something is always copied, regardless
    /// of which renderer the panel is currently showing.
    #[tracing::instrument(name = "results_value_panel_copy_tree_node_value", skip_all)]
    fn copy_tree_node_value(
        &mut self,
        _: &CopyTreeNodeValue,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(range) = self.state.text_selection().range()
            && let Some(body_text) = self.current_body_text()
            && let Some(selected) = body_text.get(range)
        {
            tracing::debug!("copied the value panel's active text selection to the clipboard");
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_owned()));
            return;
        }

        if let Some(JsonLoad::Parsed(root)) = self.json.as_ref().map(|c| &c.load)
            && let Some(node) = data::node_at_path(root, self.state.selected_tree_path())
        {
            let text = serde_json::to_string(&data::json_node_to_serde(node)).unwrap_or_default();
            tracing::debug!("copied the value panel's focused tree node's value to the clipboard");
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            return;
        }

        let Some(content) = self.state.content() else {
            return;
        };
        let text = format_value_for_clipboard(&content.value);
        tracing::debug!(
            "copied the value panel's target cell to the clipboard (no parsed json tree)"
        );
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Cmd/Ctrl-Shift-C while the panel has focus: copy the selected tree
    /// node's JSONPath-style path (e.g. `$.items[0].sku`).
    #[tracing::instrument(name = "results_value_panel_copy_tree_node_path", skip_all)]
    fn copy_tree_node_path(
        &mut self,
        _: &CopyTreeNodePath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = data::json_path_string(self.state.selected_tree_path());
        tracing::debug!(path = %path, "copied the value panel's focused tree node's path to the clipboard");
        cx.write_to_clipboard(ClipboardItem::new_string(path));
    }

    /// `esc` while the panel has focus
    fn close_panel_from_panel(
        &mut self,
        _: &ClosePanelFromPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.close();
        window.focus(&self.parent_focus_handle);
        cx.notify();
    }

    /// `tab` while the panel has focus: move keyboard focus back
    fn focus_parent_from_panel(
        &mut self,
        _: &FocusGridFromPanel,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        window.focus(&self.parent_focus_handle);
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use zsql_core::value::UnknownValue;
    use zsql_core::{ColumnMeta, Value};

    use super::{
        CopyTreeNodePath, CopyTreeNodeValue, JsonLoad, TreeCollapse, TreeDown, TreeExpand, TreeUp,
        ValuePanel, ValuePanelContent,
    };
    use crate::config::ValuePanelConfig;
    use crate::ui::format;
    use crate::ui::theme;
    use crate::ui::value_panel::data::{self, PathSegment};

    fn json_content(id: usize, text: &str) -> ValuePanelContent {
        ValuePanelContent::new(
            id,
            Value::Json(text.to_owned()),
            ColumnMeta {
                name: "payload".to_owned(),
                type_name: "jsonb".to_owned(),
                nullable: true,
            },
        )
    }

    fn new_panel(cx: &mut gpui::TestAppContext) -> gpui::Entity<ValuePanel> {
        new_panel_with_config(cx, ValuePanelConfig::default())
    }

    fn new_panel_with_config(
        cx: &mut gpui::TestAppContext,
        config: ValuePanelConfig,
    ) -> gpui::Entity<ValuePanel> {
        cx.new(|cx| {
            let parent = cx.focus_handle();
            ValuePanel::new(parent, config, cx)
        })
    }

    #[gpui::test]
    fn load_full_json_value_upgrades_an_oversized_cache_to_parsed(cx: &mut gpui::TestAppContext) {
        let oversized = format!(
            r#"{{"padding":"{}","items":[{{"sku":"A1"}}]}}"#,
            "x".repeat(100)
        );
        let panel = new_panel_with_config(
            cx,
            ValuePanelConfig {
                json_eager_parse_threshold_bytes: 16,
                json_oversized_preview_bytes: 8,
                ..ValuePanelConfig::default()
            },
        );

        panel.update(cx, |p, _cx| {
            p.update_content(Some(json_content(0, &oversized)));
            assert!(
                matches!(p.json_load_for_test(), Some(JsonLoad::Oversized { .. })),
                "a value past the configured threshold must open Oversized"
            );
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(3);
        });

        panel.update(cx, ValuePanel::load_full_json_value);
        cx.run_until_parked();

        panel.read_with(cx, |p, _cx| {
            assert!(
                matches!(p.json_load_for_test(), Some(JsonLoad::Parsed(_))),
                "Load full value must upgrade the cache from Oversized to Parsed"
            );
            assert_eq!(
                p.state_for_test().text_selection().range(),
                None,
                "upgrading the cache must clear a selection made against the oversized preview"
            );
        });
    }

    /// A new JSON target at the panel re-parses the cache rather than keeping
    /// the prior value on screen: switching the panel's content (e.g. after a
    /// new result set) must reflect the new source text.
    #[gpui::test]
    fn update_content_reparses_the_json_cache_for_a_new_target(cx: &mut gpui::TestAppContext) {
        let panel = new_panel(cx);

        panel.update(cx, |p, _cx| {
            p.update_content(Some(json_content(0, r#"{"v":1}"#)));
            assert_eq!(
                p.json.as_ref().map(|c| c.text.as_str()),
                Some(r#"{"v":1}"#),
                "the panel caches the first target's JSON"
            );
        });

        panel.update(cx, |p, _cx| {
            p.update_content(Some(json_content(1, r#"{"v":2}"#)));
            assert_eq!(
                p.json.as_ref().map(|c| c.text.as_str()),
                Some(r#"{"v":2}"#),
                "a new target's JSON must re-parse, not keep showing the prior value"
            );
        });
    }

    /// A focused, docked-open panel in its own test window, so the keyboard
    /// actions its render registers can be dispatched to it directly.
    fn panel_window(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<ValuePanel>, &mut gpui::VisualTestContext) {
        panel_window_with_config(cx, ValuePanelConfig::default())
    }

    fn panel_window_with_config(
        cx: &mut gpui::TestAppContext,
        config: ValuePanelConfig,
    ) -> (gpui::Entity<ValuePanel>, &mut gpui::VisualTestContext) {
        cx.add_window_view(|window, cx| {
            let parent = cx.focus_handle();
            let panel = ValuePanel::new(parent, config, cx);
            window.focus(panel.focus_handle());
            panel
        })
    }

    fn content(id: usize, value: Value, type_name: &str) -> ValuePanelContent {
        ValuePanelContent::new(
            id,
            value,
            ColumnMeta {
                name: "col".to_owned(),
                type_name: type_name.to_owned(),
                nullable: true,
            },
        )
    }

    #[gpui::test]
    fn tree_keyboard_actions_move_selection_and_expand_collapse_nodes(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(0, r#"{"items":[{"sku":"A1"}]}"#)));
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().selected_tree_path(),
                &[] as &[PathSegment]
            );
        });

        vcx.dispatch_action(TreeExpand);
        panel.read_with(vcx, |p, _cx| {
            assert!(
                p.state_for_test()
                    .is_tree_node_expanded(&[] as &[PathSegment]),
                "TreeExpand on the root object must expand it"
            );
        });

        vcx.dispatch_action(TreeDown);
        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().selected_tree_path(),
                [PathSegment::Key("items".to_owned())],
                "TreeDown must move selection to the root's first revealed child"
            );
        });

        vcx.dispatch_action(TreeExpand);
        panel.read_with(vcx, |p, _cx| {
            assert!(
                p.state_for_test()
                    .is_tree_node_expanded(&[PathSegment::Key("items".to_owned())])
            );
        });

        vcx.dispatch_action(TreeCollapse);
        panel.read_with(vcx, |p, _cx| {
            assert!(
                !p.state_for_test()
                    .is_tree_node_expanded(&[PathSegment::Key("items".to_owned())]),
                "TreeCollapse must collapse the selected node"
            );
        });

        vcx.dispatch_action(TreeUp);
        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().selected_tree_path(),
                &[] as &[PathSegment],
                "TreeUp must move selection back to the previous visible row"
            );
        });
    }

    #[gpui::test]
    fn copy_tree_node_value_and_path_target_the_selected_nested_node(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(0, r#"{"items":[{"sku":"A1"}]}"#)));
            p.state_mut_for_test().select_tree_path(vec![
                PathSegment::Key("items".to_owned()),
                PathSegment::Index(0),
                PathSegment::Key("sku".to_owned()),
            ]);
        });
        vcx.run_until_parked();

        vcx.dispatch_action(CopyTreeNodeValue);
        let copied_value = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied_value.as_deref(),
            Some("\"A1\""),
            "Cmd/Ctrl-C must copy the selected node's own value, not the whole document"
        );

        vcx.dispatch_action(CopyTreeNodePath);
        let copied_path = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied_path.as_deref(), Some("$.items[0].sku"));
    }

    // -- Text-body mouse selection and its interaction with copy -----------

    /// The pixel point that hit-tests back to `text`'s byte `offset`, shaped
    /// with the same font/size/padding a value-panel body renders with --
    /// lets a test drive `simulate_event` with real, shape-derived
    /// coordinates against a body painted at `bounds` (from
    /// [`gpui::VisualTestContext::debug_bounds`]) instead of guessed pixel
    /// offsets.
    fn click_point_for_offset(
        vcx: &mut gpui::VisualTestContext,
        bounds: gpui::Bounds<gpui::Pixels>,
        text: &str,
        offset: usize,
    ) -> gpui::Point<gpui::Pixels> {
        let font_size = gpui::px(theme::VALUE_PANEL_TEXT_SIZE);
        let run = gpui::TextRun {
            len: text.len(),
            font: gpui::font(zsql_ui::theme::Theme::default().fonts.data),
            color: gpui::Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = vcx.update(|window, _cx| {
            window
                .text_system()
                .layout_line(text, font_size, &[run], None)
        });
        let padding = theme::VALUE_PANEL_PADDING_X;
        gpui::point(
            bounds.origin.x + padding + line.x_for_index(offset),
            bounds.origin.y + padding + gpui::px(4.0),
        )
    }

    /// Simulate a click-and-drag over a value-panel body's rendered text
    /// from byte `from` to byte `to`, through the panel's real mouse
    /// listeners (not a direct [`data::ValuePanelState`] mutation), ending
    /// the drag with a mouse-up as a real drag would.
    fn simulate_drag_over_body(
        vcx: &mut gpui::VisualTestContext,
        selector: &'static str,
        text: &str,
        from: usize,
        to: usize,
    ) {
        let bounds = vcx
            .debug_bounds(selector)
            .expect("the body must be painted before it can be dragged over");
        let down_point = click_point_for_offset(vcx, bounds, text, from);
        let drag_point = click_point_for_offset(vcx, bounds, text, to);

        vcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: down_point,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseMoveEvent {
            position: drag_point,
            pressed_button: Some(gpui::MouseButton::Left),
            modifiers: gpui::Modifiers::default(),
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: drag_point,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        });
    }

    #[gpui::test]
    fn dragging_over_text_sets_a_selection_spanning_the_dragged_range(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        let text = "hello world";
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(0, Value::Text(text.to_owned()), "text")));
        });
        vcx.run_until_parked();

        simulate_drag_over_body(vcx, "value-panel-text-body", text, 0, 5);

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().text_selection().range(),
                Some(0..5),
                "a real click-and-drag over the rendered text must select exactly the dragged \
                 byte range through the panel's own mouse listeners"
            );
        });
    }

    #[gpui::test]
    fn shift_click_extends_the_selection_from_its_original_anchor(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        let text = "hello world";
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(0, Value::Text(text.to_owned()), "text")));
        });
        vcx.run_until_parked();

        simulate_drag_over_body(vcx, "value-panel-text-body", text, 0, 5);

        let bounds = vcx
            .debug_bounds("value-panel-text-body")
            .expect("the body must still be painted");
        let shift_point = click_point_for_offset(vcx, bounds, text, 9);
        vcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: shift_point,
            modifiers: gpui::Modifiers {
                shift: true,
                ..gpui::Modifiers::default()
            },
            click_count: 1,
            first_mouse: false,
        });

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().text_selection().range(),
                Some(0..9),
                "a real shift-click must extend the selection from its original anchor, not the \
                 last cursor"
            );
        });
    }

    #[gpui::test]
    fn copy_with_an_active_selection_copies_only_the_selected_text(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Text("hello world".to_owned()),
                "text",
            )));
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(5);
        });
        vcx.run_until_parked();

        vcx.dispatch_action(CopyTreeNodeValue);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("hello"),
            "Cmd/Ctrl-C with an active selection must copy exactly the selected substring"
        );
    }

    #[gpui::test]
    fn copy_with_no_selection_still_copies_the_whole_cell_value(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Text("hello world".to_owned()),
                "text",
            )));
        });
        vcx.run_until_parked();

        vcx.dispatch_action(CopyTreeNodeValue);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("hello world"),
            "Cmd/Ctrl-C with no selection must fall back to the whole cell value, unchanged"
        );
    }

    #[gpui::test]
    fn copy_with_selection_copies_the_selected_substring_of_a_bytes_hex_body(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Bytes(vec![0x00, 0x41, 0xff, 0x10, 0xab, 0xcd]),
                "bytea",
            )));
        });
        vcx.run_until_parked();

        let text = panel
            .read_with(vcx, |p, _cx| p.current_body_text())
            .expect("a Bytes cell must have hex-dump body text");
        assert!(
            text.len() >= 8,
            "the hex dump must be long enough to select a sub-range from"
        );

        panel.update(vcx, |p, cx| {
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(8);
            cx.notify();
        });

        vcx.dispatch_action(CopyTreeNodeValue);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(&text[0..8]),
            "Cmd/Ctrl-C with a selection over a Bytes hex body must copy exactly the same \
             substring the body renders"
        );
    }

    #[gpui::test]
    fn copy_with_selection_copies_the_selected_substring_of_a_json_pretty_body(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(0, r#"{"items":[{"sku":"A1"}]}"#)));
            p.state_mut_for_test().set_json_mode(data::JsonMode::Pretty);
            cx.notify();
        });
        vcx.run_until_parked();

        let text = panel
            .read_with(vcx, |p, _cx| p.current_body_text())
            .expect("Pretty mode must have body text");
        assert!(
            text.len() >= 6,
            "the pretty-printed document must be long enough to select a sub-range from"
        );

        panel.update(vcx, |p, cx| {
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(6);
            cx.notify();
        });

        vcx.dispatch_action(CopyTreeNodeValue);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(&text[0..6]),
            "Cmd/Ctrl-C with a selection over a JSON Pretty body must copy exactly the same \
             substring the body renders"
        );
    }

    #[gpui::test]
    fn copy_with_selection_copies_the_selected_substring_of_an_oversized_json_preview(
        cx: &mut gpui::TestAppContext,
    ) {
        let oversized = format!(r#"{{"padding":"{}"}}"#, "x".repeat(100));
        let (panel, vcx) = panel_window_with_config(
            cx,
            ValuePanelConfig {
                json_eager_parse_threshold_bytes: 16,
                json_oversized_preview_bytes: 8,
                ..ValuePanelConfig::default()
            },
        );
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(0, &oversized)));
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert!(
                matches!(p.json_load_for_test(), Some(JsonLoad::Oversized { .. })),
                "a value past the configured threshold must open Oversized"
            );
        });

        let text = panel
            .read_with(vcx, |p, _cx| p.current_body_text())
            .expect("an Oversized cell must have preview body text");
        assert!(
            text.len() >= 4,
            "the preview must be long enough to select a sub-range from"
        );

        panel.update(vcx, |p, cx| {
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(4);
            cx.notify();
        });

        vcx.dispatch_action(CopyTreeNodeValue);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(&text[0..4]),
            "Cmd/Ctrl-C with a selection over an oversized-JSON preview must copy exactly the \
             same substring the preview renders"
        );
    }

    #[gpui::test]
    fn set_config_clears_a_prior_text_selection(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Text("hello world".to_owned()),
                "text",
            )));
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(5);
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert!(p.state_for_test().text_selection().range().is_some());
        });

        panel.update(vcx, |p, _cx| {
            p.set_config(ValuePanelConfig {
                hex_bytes_per_row: 32,
                ..ValuePanelConfig::default()
            });
        });

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().text_selection().range(),
                None,
                "changing the panel's config must clear a selection made against the previous \
                 body text"
            );
        });
    }

    #[gpui::test]
    fn a_new_cell_clears_a_prior_text_selection(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Text("hello world".to_owned()),
                "text",
            )));
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(5);
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert!(p.state_for_test().text_selection().range().is_some());
        });

        panel.update(vcx, |p, _cx| {
            p.update_content(Some(content(1, Value::Text("goodbye".to_owned()), "text")));
        });
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().text_selection().range(),
                None,
                "a new cell must clear a selection made against the previous cell's text"
            );
        });
    }

    /// A JSON cell's panel renders in every mode without panicking, covering
    /// the JSON tree/pretty/raw paths and the invalid-JSON fallback path
    /// together in one render smoke test.
    #[gpui::test]
    fn renders_the_panel_for_every_json_state_without_panicking(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(
                0,
                r#"{"items":[{"sku":"A1"},{"sku":"B2"}]}"#,
            )));
        });
        for mode in data::JSON_MODES {
            panel.update(vcx, |p, cx| {
                p.state_mut_for_test().set_json_mode(mode);
                cx.notify();
            });
            vcx.run_until_parked();
        }

        // A `Value::Json` that fails to parse must still render (the Raw
        // fallback) rather than panicking.
        panel.update(vcx, |p, _cx| {
            p.update_content(Some(json_content(1, "not json")));
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert!(matches!(p.json_load_for_test(), Some(JsonLoad::Invalid(_))));
        });
    }

    /// The non-JSON renderers (Bytes, Timestamp, Bool, Unknown, Null, and an
    /// empty-text cell) each render in every mode without panicking, mirroring
    /// [`renders_the_panel_for_every_json_state_without_panicking`]'s coverage
    /// of the JSON renderer.
    #[gpui::test]
    fn renders_the_panel_for_non_json_cells_without_panicking(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, super::ValuePanel::open);

        panel.update(vcx, |p, _cx| {
            p.update_content(Some(content(
                0,
                Value::Bytes(vec![0x00, 0x41, 0xff, 0x10]),
                "bytea",
            )));
        });
        for mode in data::BYTES_MODES {
            panel.update(vcx, |p, cx| {
                p.state_mut_for_test().set_bytes_mode(mode);
                cx.notify();
            });
            vcx.run_until_parked();
        }

        panel.update(vcx, |p, _cx| {
            p.update_content(Some(content(
                1,
                Value::Timestamp("2026-07-14T09:12:31+02:00".to_owned()),
                "timestamptz",
            )));
        });
        for mode in data::TIMESTAMP_MODES {
            panel.update(vcx, |p, cx| {
                p.state_mut_for_test().set_timestamp_mode(mode);
                cx.notify();
            });
            vcx.run_until_parked();
        }

        for (id, value, type_name) in [
            (2usize, Value::Bool(true), "bool"),
            (
                3,
                Value::Unknown(UnknownValue::Text("(1,2)".to_owned())),
                "point",
            ),
            (4, Value::Null, "text"),
            (5, Value::Text(String::new()), "text"),
            (6, Value::Unknown(UnknownValue::None), "point"),
        ] {
            panel.update(vcx, |p, _cx| {
                p.update_content(Some(content(id, value, type_name)));
            });
            vcx.run_until_parked();
        }

        panel.read_with(vcx, |p, _cx| {
            assert!(
                p.state_for_test().is_open(),
                "the panel must stay open across every non-JSON cell it renders"
            );
        });
    }

    // -- current_body_text: every renderer/mode combination -----------------

    #[gpui::test]
    fn current_body_text_for_invalid_json_returns_the_raw_source_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        let raw = "not json";
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(0, raw)));
        });
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert!(matches!(p.json_load_for_test(), Some(JsonLoad::Invalid(_))));
            assert_eq!(
                p.current_body_text().as_deref(),
                Some(raw),
                "invalid JSON must show its exact raw source, not an empty or reformatted body"
            );
        });
    }

    #[gpui::test]
    fn current_body_text_for_json_raw_mode_returns_the_raw_source_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        let raw = r#"{"v":1}"#;
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(0, raw)));
            p.state_mut_for_test().set_json_mode(data::JsonMode::Raw);
        });
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(p.current_body_text().as_deref(), Some(raw));
        });
    }

    #[gpui::test]
    fn current_body_text_for_bytes_base64_mode_returns_the_base64_encoding(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        let bytes = vec![0x00, 0x41, 0xff, 0x10, 0xab, 0xcd];
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(0, Value::Bytes(bytes.clone()), "bytea")));
            p.state_mut_for_test()
                .set_bytes_mode(data::BytesMode::Base64);
        });
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.current_body_text().as_deref(),
                Some(format::base64_encode(&bytes).as_str())
            );
        });
    }

    #[gpui::test]
    fn current_body_text_for_number_values_returns_their_raw_text(cx: &mut gpui::TestAppContext) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
        });

        for (id, value, expected) in [
            (0usize, Value::Int(42), "42"),
            (1, Value::Float(1.5), "1.5"),
            (2, Value::Numeric("3.140".to_owned()), "3.140"),
        ] {
            panel.update(vcx, |p, _cx| {
                p.update_content(Some(content(id, value, "numeric")));
            });
            vcx.run_until_parked();
            panel.read_with(vcx, |p, _cx| {
                assert_eq!(p.current_body_text().as_deref(), Some(expected));
            });
        }
    }

    #[gpui::test]
    fn current_body_text_for_timestamp_modes_returns_raw_and_utc_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        let raw = "2026-07-14T09:12:31+02:00";
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Timestamp(raw.to_owned()),
                "timestamptz",
            )));
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.current_body_text().as_deref(),
                Some(raw),
                "Raw mode must show the driver's exact text"
            );
        });

        panel.update(vcx, |p, _cx| {
            p.state_mut_for_test()
                .set_timestamp_mode(data::TimestampMode::Utc);
        });
        panel.read_with(vcx, |p, _cx| {
            let expected = data::timestamp_utc_text(raw).expect("a well-formed offset must parse");
            assert_eq!(p.current_body_text(), Some(expected));
        });
    }

    #[gpui::test]
    fn current_body_text_for_an_unparseable_raw_timestamp_falls_back_to_a_could_not_be_parsed_message(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        let raw = "not a timestamp";
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Timestamp(raw.to_owned()),
                "timestamptz",
            )));
            p.state_mut_for_test()
                .set_timestamp_mode(data::TimestampMode::Utc);
        });
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.current_body_text().as_deref(),
                Some("not a timestamp (could not be parsed as a timestamp)")
            );
        });
    }

    #[gpui::test]
    fn current_body_text_for_bool_and_unknown_values_returns_their_formatted_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(0, Value::Bool(true), "bool")));
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert_eq!(p.current_body_text().as_deref(), Some("true"));
        });

        let unknown = Value::Unknown(UnknownValue::Text("(1,2)".to_owned()));
        panel.update(vcx, |p, _cx| {
            p.update_content(Some(content(1, unknown.clone(), "point")));
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.current_body_text(),
                Some(format::format_value(&unknown).text)
            );
        });
    }

    // -- mode switches clear a prior text selection, through the panel's
    // own subbar buttons rather than a direct state mutation -------------

    #[gpui::test]
    fn switching_json_mode_via_the_subbar_button_clears_a_prior_text_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(json_content(0, r#"{"v":1}"#)));
            p.state_mut_for_test().set_json_mode(data::JsonMode::Pretty);
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(3);
            cx.notify();
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert!(p.state_for_test().text_selection().range().is_some());
        });

        let bounds = vcx
            .debug_bounds("value-panel-mode-button-Raw")
            .expect("the JSON subbar's Raw button must be painted");
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().json_mode(),
                data::JsonMode::Raw,
                "the click must have actually switched the JSON mode"
            );
            assert_eq!(
                p.state_for_test().text_selection().range(),
                None,
                "clicking a JSON subbar mode button must clear a selection made against the \
                 previous mode's text"
            );
        });
    }

    #[gpui::test]
    fn switching_bytes_mode_via_the_subbar_button_clears_a_prior_text_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Bytes(vec![0x00, 0x41, 0xff, 0x10]),
                "bytea",
            )));
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(3);
            cx.notify();
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert!(p.state_for_test().text_selection().range().is_some());
        });

        let bounds = vcx
            .debug_bounds("value-panel-mode-button-Base64")
            .expect("the Bytes subbar's Base64 button must be painted");
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().bytes_mode(),
                data::BytesMode::Base64,
                "the click must have actually switched the Bytes mode"
            );
            assert_eq!(
                p.state_for_test().text_selection().range(),
                None,
                "clicking the Bytes subbar's Base64 button must clear a selection made against \
                 the previous mode's text"
            );
        });
    }

    #[gpui::test]
    fn switching_timestamp_mode_via_the_subbar_button_clears_a_prior_text_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let (panel, vcx) = panel_window(cx);
        panel.update(vcx, |p, cx| {
            p.open(cx);
            p.update_content(Some(content(
                0,
                Value::Timestamp("2026-07-14T09:12:31+02:00".to_owned()),
                "timestamptz",
            )));
            p.state_mut_for_test().text_selection_mut().begin(0, false);
            let _ = p
                .state_mut_for_test()
                .text_selection_mut()
                .extend_while_dragging(3);
            cx.notify();
        });
        vcx.run_until_parked();
        panel.read_with(vcx, |p, _cx| {
            assert!(p.state_for_test().text_selection().range().is_some());
        });

        let bounds = vcx
            .debug_bounds("value-panel-mode-button-UTC")
            .expect("the Timestamp subbar's UTC button must be painted");
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();

        panel.read_with(vcx, |p, _cx| {
            assert_eq!(
                p.state_for_test().timestamp_mode(),
                data::TimestampMode::Utc,
                "the click must have actually switched the Timestamp mode"
            );
            assert_eq!(
                p.state_for_test().text_selection().range(),
                None,
                "clicking the Timestamp subbar's UTC button must clear a selection made against \
                 the previous mode's text"
            );
        });
    }
}
