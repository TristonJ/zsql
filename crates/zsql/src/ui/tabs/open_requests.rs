//! Open/Browse request routing

use gpui::{Context, EventEmitter};

use super::TabModel;

/// What pressing Open script or Browse files asks the embedding app to do
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRequested {
    /// Open the Open Script picker, seeded with the active connection's open
    /// named tabs and the library.
    OpenPicker,
    /// Go straight to the platform open-file dialog.
    BrowseFiles,
}

impl EventEmitter<OpenRequested> for TabModel {}

impl TabModel {
    // Taking self matches the shape `Entity::update` expects, letting the
    // editor's open/browse requesters pass the method itself rather than a
    // wrapping closure.
    #[allow(clippy::unused_self)]
    pub(super) fn request_open(&mut self, cx: &mut Context<Self>) {
        cx.emit(OpenRequested::OpenPicker);
    }

    // Taking self matches the shape `Entity::update` expects, letting the
    // editor's open/browse requesters pass the method itself rather than a
    // wrapping closure.
    #[allow(clippy::unused_self)]
    pub(super) fn request_browse(&mut self, cx: &mut Context<Self>) {
        cx.emit(OpenRequested::BrowseFiles);
    }
}
