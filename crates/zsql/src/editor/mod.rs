//! Text buffer and editing model for the SQL editor: pure logic, with no
//! gpui dependency and no database access. Owns multiline text plus a
//! cursor and an optional selection, and implements the movement, selection
//! and editing operations a code editor needs. Rendering and input plumbing
//! belong to a later, gpui-dependent layer built on top of this module.

mod buffer;
mod highlighter;

pub use buffer::{Position, Selection, TextBuffer};
pub use highlighter::{Highlighter, PlainHighlighter, StyleSpan};
