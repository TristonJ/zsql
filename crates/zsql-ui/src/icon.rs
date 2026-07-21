//! A tintable SVG icon component and its embedded asset source. Every icon's
//! bytes are compiled into the binary via `include_bytes!`, so there is no
//! runtime filesystem lookup: `IconAssetSource` (registered once at startup
//! via `gpui::Application::with_assets`) resolves the same logical paths
//! `icon()` passes to `gpui::svg().path(...)`, and gpui paints the SVG as an
//! alpha mask tinted by the caller's `color` -- there is no dependency on the
//! SVG's own fill/stroke color.

use std::borrow::Cow;

use gpui::{AssetSource, Pixels, SharedString, Svg, prelude::*, rgb, svg};

/// One embedded icon's identity. Every variant maps to exactly one SVG asset
/// under `assets/icons/`, embedded at compile time -- no runtime filesystem
/// access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    /// Tree disclosure: collapsed (points at the row it would expand).
    ChevronRight,
    /// Tree disclosure: expanded.
    ChevronDown,
    /// A catalog (database) row.
    Database,
    /// A schema (namespace) row.
    Schema,
    /// An ordinary table relation.
    Table,
    /// A view relation.
    View,
    /// A materialized view relation.
    MaterializedView,
    /// A partitioned table relation.
    PartitionedTable,
    /// Run/execute the current query.
    Run,
    /// Close a panel or modal.
    Close,
    /// Delete/remove an item.
    Delete,
    /// Add a new item.
    Add,
    /// Manually refresh a view. Vendored for a future affordance; no call
    /// site uses it yet.
    Refresh,
    /// Edit an existing item.
    Edit,
}

impl IconName {
    /// Every registered icon, in declaration order. Used to build the asset
    /// source's directory listing and to iterate every icon in tests.
    pub const ALL: [IconName; 14] = [
        IconName::ChevronRight,
        IconName::ChevronDown,
        IconName::Database,
        IconName::Schema,
        IconName::Table,
        IconName::View,
        IconName::MaterializedView,
        IconName::PartitionedTable,
        IconName::Run,
        IconName::Close,
        IconName::Delete,
        IconName::Add,
        IconName::Refresh,
        IconName::Edit,
    ];

    /// The logical asset path this icon resolves to, exactly as passed to
    /// `gpui::svg().path(...)` and returned by [`IconAssetSource::load`].
    #[must_use]
    pub const fn path(self) -> &'static str {
        match self {
            IconName::ChevronRight => "icons/chevron-right.svg",
            IconName::ChevronDown => "icons/chevron-down.svg",
            IconName::Database => "icons/database.svg",
            IconName::Schema => "icons/schema.svg",
            IconName::Table => "icons/table.svg",
            IconName::View => "icons/view.svg",
            IconName::MaterializedView => "icons/materialized-view.svg",
            IconName::PartitionedTable => "icons/partitioned-table.svg",
            IconName::Run => "icons/run.svg",
            IconName::Close => "icons/close.svg",
            IconName::Delete => "icons/delete.svg",
            IconName::Add => "icons/add.svg",
            IconName::Refresh => "icons/refresh.svg",
            IconName::Edit => "icons/edit.svg",
        }
    }

    /// This icon's SVG bytes, compiled into the binary.
    #[must_use]
    pub const fn bytes(self) -> &'static [u8] {
        match self {
            IconName::ChevronRight => {
                include_bytes!("../assets/icons/chevron-right.svg") as &[u8]
            }
            IconName::ChevronDown => include_bytes!("../assets/icons/chevron-down.svg") as &[u8],
            IconName::Database => include_bytes!("../assets/icons/database.svg") as &[u8],
            IconName::Schema => include_bytes!("../assets/icons/schema.svg") as &[u8],
            IconName::Table => include_bytes!("../assets/icons/table.svg") as &[u8],
            IconName::View => include_bytes!("../assets/icons/view.svg") as &[u8],
            IconName::MaterializedView => {
                include_bytes!("../assets/icons/materialized-view.svg") as &[u8]
            }
            IconName::PartitionedTable => {
                include_bytes!("../assets/icons/partitioned-table.svg") as &[u8]
            }
            IconName::Run => include_bytes!("../assets/icons/run.svg") as &[u8],
            IconName::Close => include_bytes!("../assets/icons/close.svg") as &[u8],
            IconName::Delete => include_bytes!("../assets/icons/delete.svg") as &[u8],
            IconName::Add => include_bytes!("../assets/icons/add.svg") as &[u8],
            IconName::Refresh => include_bytes!("../assets/icons/refresh.svg") as &[u8],
            IconName::Edit => include_bytes!("../assets/icons/edit.svg") as &[u8],
        }
    }

    /// The icon whose [`Self::path`] equals `path`, if any.
    fn from_path(path: &str) -> Option<IconName> {
        IconName::ALL.into_iter().find(|name| name.path() == path)
    }
}

/// Render `name` at `size`, tinted with `color`. Paints via `text_color` on
/// the underlying `svg()` element: gpui rasterizes the SVG to an alpha mask
/// and paints it with this color, so the icon's own SVG fill is irrelevant.
#[must_use]
pub fn icon(name: IconName, size: Pixels, color: u32) -> Svg {
    svg().path(name.path()).size(size).text_color(rgb(color))
}

/// A `gpui::AssetSource` that resolves every [`IconName`] to its embedded
/// bytes. Register once via `Application::new().with_assets(IconAssetSource)`
/// before opening any window.
pub struct IconAssetSource;

impl AssetSource for IconAssetSource {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(IconName::from_path(path).map(|name| Cow::Borrowed(name.bytes())))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(IconName::ALL
            .iter()
            .map(|name| SharedString::from(name.path()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{IconAssetSource, IconName, icon};
    use gpui::{AssetSource as _, px};

    #[test]
    fn icon_builds_for_every_registered_name() {
        for name in IconName::ALL {
            let _element = icon(name, px(16.0), 0xff_ff_ff);
        }
    }

    #[test]
    fn the_asset_source_resolves_non_empty_bytes_for_every_registered_icon() {
        let source = IconAssetSource;
        for name in IconName::ALL {
            let loaded = source
                .load(name.path())
                .expect("loading a registered icon path must not error");
            let bytes = loaded.unwrap_or_else(|| {
                panic!(
                    "icon {name:?} at path {:?} must resolve to bytes",
                    name.path()
                )
            });
            assert!(
                !bytes.is_empty(),
                "icon {name:?} at path {:?} resolved to empty bytes",
                name.path()
            );
        }
    }

    #[test]
    fn the_asset_source_returns_none_for_an_unregistered_path() {
        let source = IconAssetSource;
        let loaded = source
            .load("icons/does-not-exist.svg")
            .expect("loading an unregistered path must not error");
        assert!(loaded.is_none());
    }

    #[test]
    fn the_asset_source_lists_every_registered_icon_path() {
        let source = IconAssetSource;
        let listed = source.list("icons").expect("list must not error");
        assert_eq!(listed.len(), IconName::ALL.len());
        for name in IconName::ALL {
            assert!(
                listed.iter().any(|path| path.as_ref() == name.path()),
                "list() must include {:?}",
                name.path()
            );
        }
    }

    #[test]
    fn every_icon_asset_is_ascii_with_a_single_opaque_path_on_a_24x24_viewbox() {
        for name in IconName::ALL {
            let text = std::str::from_utf8(name.bytes())
                .unwrap_or_else(|_| panic!("icon {name:?} svg must be valid utf8"));
            assert!(text.is_ascii(), "icon {name:?} must be ASCII-only source");
            assert_eq!(
                text.matches("<path").count(),
                1,
                "icon {name:?} must be a single <path> shape"
            );
            assert!(
                text.contains("viewBox=\"0 0 24 24\""),
                "icon {name:?} must use a 24x24 viewBox"
            );
            assert!(
                !text.contains("fill=\"none\""),
                "icon {name:?} must be an opaque shape, not fill=\"none\""
            );
        }
    }

    #[test]
    fn every_icon_name_has_a_distinct_path() {
        let mut paths: Vec<&str> = IconName::ALL.iter().map(|name| name.path()).collect();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(
            paths.len(),
            IconName::ALL.len(),
            "every icon must resolve to a distinct asset path"
        );
    }
}
