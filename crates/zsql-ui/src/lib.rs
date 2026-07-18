//! Reusable, layout-agnostic `gpui` building blocks shared across zsql's
//! views: the base color palette, small presentational div builders for
//! grid cells and tree rows, and pure scrollbar geometry. Nothing in this
//! crate knows about the app's domain, driver, or session state -- every
//! function here takes only primitives (colors, pixel widths, extents,
//! strings) and returns either an `Element` or a plain value, so the caller
//! owns all app-specific data and layout.

pub mod colors;
pub mod grid;
pub mod icon;
pub mod scrollbar;
pub mod text_field;
pub mod tree;
