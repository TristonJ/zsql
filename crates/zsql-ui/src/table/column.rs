//! A data column's width and header content, as the caller supplies it to
//! [`super::Table`].

use gpui::{AnyElement, IntoElement, Pixels};

/// One data column: its pixel width and the header content shown above it.
/// The table never inspects a column's semantics (name, type) -- that
/// content is entirely caller-built.
pub struct TableColumn {
    /// This column's width. Applied identically to its header cell and
    /// every body cell, so header and body stay aligned. For a [`grow`]able
    /// column this is a floor the column never shrinks below, not a fixed
    /// size.
    ///
    /// [`grow`]: TableColumn::grow
    pub width: Pixels,
    /// Whether this column expands to absorb a share of any horizontal space
    /// left over once every column has its [`width`]. When any column in a
    /// table grows, the table fills its container's width instead of leaving
    /// trailing empty space, and growable columns split the slack evenly.
    /// Off by default, which keeps a table's columns at exactly their fixed
    /// widths (and horizontally scrollable when they overflow).
    ///
    /// [`width`]: TableColumn::width
    pub grow: bool,
    /// The header cell's content.
    pub header: AnyElement,
}

impl TableColumn {
    /// A fixed-width column of `width` pixels, with `header` as its header
    /// cell's content.
    #[must_use]
    pub fn new(width: Pixels, header: impl IntoElement) -> Self {
        Self {
            width,
            grow: false,
            header: header.into_any_element(),
        }
    }

    /// Let this column grow past its `width` to help fill the table's
    /// container, treating `width` as a floor. See [`TableColumn::grow`].
    #[must_use]
    pub fn grow(mut self) -> Self {
        self.grow = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Empty, px};

    use super::TableColumn;

    #[test]
    fn new_carries_the_given_width() {
        let column = TableColumn::new(px(120.0), Empty);
        assert_eq!(column.width, px(120.0));
    }
}
