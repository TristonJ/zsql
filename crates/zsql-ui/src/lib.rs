//! Reusable, layout-agnostic `gpui` building blocks shared across zsql's
//! views: the base color palette, small presentational div builders for
//! grid cells and tree rows, pure scrollbar geometry, and the
//! [`scrollable`] module's scroll/drag abstraction. Nothing in this crate
//! knows about the app's domain, driver, or session state: callers own all
//! app-specific data, and most functions here take only primitives (colors,
//! pixel widths, extents, strings) and return either an `Element` or a
//! plain value. [`scrollable::ScrollableState`] is the one exception --
//! mechanical (non-domain) scroll and drag state that would otherwise be
//! duplicated at every call site is owned by this crate via `Entity`,
//! rebuilt fresh from caller-supplied configuration every render.

pub mod colors;
pub mod grid;
pub mod icon;
pub mod scrollable;
pub mod scrollbar;
pub mod tabs;
pub mod text_field;
pub mod tree;
