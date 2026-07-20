//! A data column's width and header content, as the caller supplies it to
//! [`super::Table`].

use gpui::{AnyElement, IntoElement, Pixels};

/// One data column: its pixel width and the header content shown above it.
/// The table never inspects a column's semantics (name, type) -- that
/// content is entirely caller-built.
pub struct TableColumn {
    /// This column's width. Applied identically to its header cell and
    /// every body cell, so header and body stay aligned.
    pub width: Pixels,
    /// The header cell's content.
    pub header: AnyElement,
}

impl TableColumn {
    /// A column of `width` pixels, with `header` as its header cell's
    /// content.
    #[must_use]
    pub fn new(width: Pixels, header: impl IntoElement) -> Self {
        Self {
            width,
            header: header.into_any_element(),
        }
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
