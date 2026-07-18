//! Reusable, layout-agnostic `gpui` building blocks shared across zsql's
//! views: the base color palette and small presentational div builders for
//! grid cells and tree rows. Nothing in this crate knows about the app's
//! domain, driver, or session state -- every function here takes only
//! primitives (colors, pixel widths, strings) and returns an `Element`, so
//! the caller owns all app-specific data and layout.

pub mod colors;
pub mod grid;
pub mod tree;
