use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Div, FocusHandle, KeyBinding, Render,
    Stateful, Window, actions, div, prelude::*, px, rgb,
};
use zsql_core::{ColumnMeta, Value};
use zsql_ui::theme::{ActiveTheme, Theme};

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
    text: String,
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
    /// Configuration options for the value panel
    config: ValuePanelConfig,
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

/// Register the results grid's and value panel's key bindings. Call once at
/// startup, before any window that hosts a [`ValuePanel`] is opened.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", TreeUp, Some(VALUE_PANEL_KEY_CONTEXT)),
        KeyBinding::new("down", TreeDown, Some(VALUE_PANEL_KEY_CONTEXT)),
        KeyBinding::new("left", TreeCollapse, Some(VALUE_PANEL_KEY_CONTEXT)),
        KeyBinding::new("right", TreeExpand, Some(VALUE_PANEL_KEY_CONTEXT)),
        KeyBinding::new(
            "secondary-c",
            CopyTreeNodeValue,
            Some(VALUE_PANEL_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "shift-secondary-c",
            CopyTreeNodePath,
            Some(VALUE_PANEL_KEY_CONTEXT),
        ),
        KeyBinding::new("escape", ClosePanelFromPanel, Some(VALUE_PANEL_KEY_CONTEXT)),
        KeyBinding::new("tab", FocusGridFromPanel, Some(VALUE_PANEL_KEY_CONTEXT)),
    ]);
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
                    .child(self.render_json_body(active_theme, cx))
                    .child(self.render_json_footer(active_theme, cx));
            }
            RendererKind::Text => {
                let text = match value {
                    Value::Text(text) | Value::Uuid(text) => text.clone(),
                    _ => String::new(),
                };
                root = root
                    .child(Self::render_static_subbar(
                        format!("Raw - {} chars", text.chars().count()).as_str(),
                        active_theme,
                    ))
                    .child(Self::render_text_body(&text, active_theme));
            }
            RendererKind::Bytes => {
                let bytes = match value {
                    Value::Bytes(bytes) => bytes.clone(),
                    _ => Vec::new(),
                };
                root = root
                    .child(self.render_bytes_subbar(active_theme, cx))
                    .child(Self::render_bytes_body(
                        &bytes,
                        self.state.bytes_mode(),
                        self.config.hex_bytes_per_row,
                        active_theme,
                    ));
            }
            RendererKind::Number => {
                let text = data::number_raw_text(value).unwrap_or_default();
                root = root
                    .child(Self::render_static_subbar(
                        "Raw - full precision",
                        active_theme,
                    ))
                    .child(Self::render_mono_body(&text, active_theme));
            }
            RendererKind::Timestamp => {
                root = root
                    .child(self.render_timestamp_subbar(active_theme, cx))
                    .child(Self::render_timestamp_body(
                        value,
                        self.state.timestamp_mode(),
                        active_theme,
                    ));
            }
            RendererKind::Bool => {
                let text = match value {
                    Value::Bool(b) => b.to_string(),
                    _ => String::new(),
                };
                root = root.child(Self::render_mono_body(&text, active_theme));
            }
            RendererKind::Null => {
                root = root.child(Self::render_null_body(active_theme));
            }
            RendererKind::Unknown { type_name } => {
                let text = format_value(value).text;
                root = root
                    .child(Self::render_static_subbar(&type_name, active_theme))
                    .child(Self::render_mono_body(&text, active_theme));
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

    fn render_json_body(&self, active_theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let Some(cache) = self.json.as_ref() else {
            return div().flex_1().min_h_0().into_any_element();
        };
        match &cache.load {
            JsonLoad::Invalid(_) => {
                Self::render_scroll_mono_body(&cache.text, active_theme).into_any_element()
            }
            JsonLoad::Oversized {
                preview,
                total_bytes,
            } => Self::render_oversized_json_body(preview, *total_bytes, active_theme, cx)
                .into_any_element(),
            JsonLoad::Parsed(node) => match self.state.json_mode() {
                JsonMode::Raw => {
                    Self::render_scroll_mono_body(&cache.text, active_theme).into_any_element()
                }
                JsonMode::Pretty => {
                    let pretty = serde_json::to_string_pretty(&data::json_node_to_serde(node))
                        .unwrap_or_default();
                    Self::render_scroll_mono_body(&pretty, active_theme).into_any_element()
                }
                JsonMode::Tree => self
                    .render_json_tree(node, active_theme, cx)
                    .into_any_element(),
            },
        }
    }

    fn render_oversized_json_body(
        preview: &str,
        total_bytes: usize,
        active_theme: &Theme,
        cx: &Context<Self>,
    ) -> Div {
        let colors = active_theme.colors;
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .p(theme::VALUE_PANEL_PADDING_X)
            .child(
                div()
                    .text_size(px(theme::VALUE_PANEL_LABEL_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(format!(
                        "{total_bytes} bytes -- past the eager-parse threshold; showing the \
                             first {} bytes",
                        preview.len()
                    )),
            )
            .child(
                Self::mono_text(preview, active_theme)
                    .id("value-panel-oversized-preview")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll(),
            )
            .child(
                div()
                    .id("value-panel-load-full")
                    .cursor_pointer()
                    .flex_shrink_0()
                    .px_2()
                    .h(theme::VALUE_PANEL_BUTTON_HEIGHT)
                    .flex()
                    .items_center()
                    .rounded(px(theme::VALUE_PANEL_BUTTON_RADIUS))
                    .bg(theme::sidebar_selected_bg(active_theme))
                    .text_color(rgb(colors.accent))
                    .text_size(px(theme::VALUE_PANEL_LABEL_TEXT_SIZE))
                    .child("Load full value")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.load_full_json_value(cx);
                    })),
            )
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

    fn render_text_body(text: &str, active_theme: &Theme) -> Stateful<Div> {
        let colors = active_theme.colors;
        div()
            .id("value-panel-text-body")
            .flex_1()
            .min_h_0()
            .p(theme::VALUE_PANEL_PADDING_X)
            .overflow_y_scroll()
            .text_size(px(theme::VALUE_PANEL_TEXT_SIZE))
            .text_color(rgb(colors.text_primary))
            .font_family(&active_theme.fonts.data)
            .child(text.to_owned())
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

    fn render_bytes_body(
        bytes: &[u8],
        mode: BytesMode,
        bytes_per_row: usize,
        active_theme: &Theme,
    ) -> Stateful<Div> {
        let text = match mode {
            BytesMode::Hex => data::format_hex_dump(bytes, bytes_per_row),
            BytesMode::Base64 => format::base64_encode(bytes),
        };
        Self::render_scroll_mono_body(&text, active_theme)
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

    fn render_timestamp_body(value: &Value, mode: TimestampMode, active_theme: &Theme) -> Div {
        let Some(raw) = data::timestamp_raw_text(value) else {
            return Self::render_mono_body("", active_theme);
        };
        let text = match mode {
            TimestampMode::Raw => raw.to_owned(),
            TimestampMode::Utc => data::timestamp_utc_text(raw)
                .unwrap_or_else(|| format!("{raw} (could not be parsed as a timestamp)")),
        };
        Self::render_mono_body(&text, active_theme)
    }

    fn render_null_body(active_theme: &Theme) -> Div {
        let colors = active_theme.colors;
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .text_color(rgb(colors.value_null))
            .child("NULL")
    }

    fn mono_text(text: &str, active_theme: &Theme) -> Div {
        div()
            .font_family(&active_theme.fonts.data)
            .text_size(px(theme::VALUE_PANEL_TEXT_SIZE))
            .text_color(rgb(active_theme.colors.text_primary))
            .child(text.to_owned())
    }

    fn render_mono_body(text: &str, active_theme: &Theme) -> Div {
        Self::mono_text(text, active_theme)
            .flex_1()
            .min_h_0()
            .p(theme::VALUE_PANEL_PADDING_X)
    }

    fn render_scroll_mono_body(text: &str, active_theme: &Theme) -> Stateful<Div> {
        Self::render_mono_body(text, active_theme)
            .id("value-panel-scroll-body")
            .overflow_y_scroll()
    }

    /// The "Load full value" action on an oversized JSON preview: parses the
    /// complete source text on a background executor (never on the render
    /// path), then updates the cache once it finishes. A no-op unless the
    /// panel is currently showing an [`JsonLoad::Oversized`] value.
    #[tracing::instrument(name = "results_value_panel_load_full_json", skip_all)]
    fn load_full_json_value(&mut self, cx: &mut Context<Self>) {
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
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Cmd/Ctrl-C while the panel has focus: copy the selected tree node's
    /// own value (its JSON text) when a JSON tree is showing, else fall back
    /// to the panel's own target cell -- the same text `Copy value`/
    /// `copy_focused_cell` would copy -- so Cmd/Ctrl-C always copies
    /// something while the panel is focused, regardless of which renderer
    /// (or an unparsed json/jsonb cell) it is currently showing.
    #[tracing::instrument(name = "results_value_panel_copy_tree_node_value", skip_all)]
    fn copy_tree_node_value(
        &mut self,
        _: &CopyTreeNodeValue,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        });

        panel.update(cx, ValuePanel::load_full_json_value);
        cx.run_until_parked();

        panel.read_with(cx, |p, _cx| {
            assert!(
                matches!(p.json_load_for_test(), Some(JsonLoad::Parsed(_))),
                "Load full value must upgrade the cache from Oversized to Parsed"
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
        cx.add_window_view(|window, cx| {
            let parent = cx.focus_handle();
            let panel = ValuePanel::new(parent, ValuePanelConfig::default(), cx);
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
}
