//! Reusable, layout-agnostic `gpui` building blocks shared across zsql's
//! views: the base color palette, a type-name badge and status dots, small
//! presentational div builders for tree rows, pure scrollbar geometry, the
//! [`scrollable`] module's scroll/drag abstraction, and the [`table`]
//! module's virtualized two-pane grid. Nothing in this crate knows about
//! the app's domain, driver, or session state: callers own all app-specific
//! data, and most functions here take only primitives (colors, pixel
//! widths, extents, strings) and return either an `Element` or a plain
//! value.
//! [`scrollable::ScrollableState`] and [`table::TableState`] are the
//! exceptions -- mechanical (non-domain) scroll/drag and table state that
//! would otherwise be duplicated at every call site is owned by this crate
//! via `Entity`, rebuilt fresh from caller-supplied configuration every
//! render.

pub mod colors;
pub mod grid;
pub mod icon;
pub mod scrollable;
pub mod scrollbar;
pub mod table;
pub mod tabs;
pub mod text_field;
pub mod tree;
