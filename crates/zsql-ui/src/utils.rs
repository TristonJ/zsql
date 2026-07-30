use gpui::{
    App, Div, Element, SharedString, Stateful, StatefulInteractiveElement, Window,
    prelude::FluentBuilder,
};

/// Currently some styles can't be updated on hover, so this trait is intended to allow
/// those styles to be updated on hover.
pub trait OnHoverState: Sized {
    /// Implement a hover listener using global window state. Prefer to use the
    /// standard [`on_hover`] when you can, but this is useful for cases where typical
    /// styles can't be updated on hover, such as text color. The element must be
    /// assigned an id, otherwise the implementation may be a no-op.
    #[must_use]
    fn on_hover_state(
        self,
        window: &mut Window,
        cx: &mut App,
        listener: impl FnOnce(Self) -> Self,
    ) -> Self;
}

impl OnHoverState for Stateful<Div> {
    fn on_hover_state(
        self,
        window: &mut Window,
        cx: &mut App,
        listener: impl FnOnce(Self) -> Self,
    ) -> Self {
        let Some(id) = self.id() else {
            return self;
        };

        let hover_id = SharedString::from(format!("{}-global-hover", id));
        let hovered = window.use_keyed_state(hover_id, cx, |_w, _c| false);
        self.on_hover({
            let hovered = hovered.clone();
            move |now, _w, cx| {
                hovered.update(cx, |h, cx| {
                    *h = *now;
                    cx.notify();
                });
            }
        })
        .when(*hovered.read(cx), listener)
    }
}
