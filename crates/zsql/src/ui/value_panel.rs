mod bindings;
mod body;
pub mod data;
mod json_tree;
pub(crate) mod view;

pub(crate) use bindings::ValuePanelBindings;
pub use view::{ValuePanel, init};
