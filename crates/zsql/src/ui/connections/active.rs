use crate::connections::StoredConnection;

/// The name + URL of whichever connection the session is currently pointed
/// at, tracked independently of [`crate::session::Session`] (which only knows the connected
/// URL, not which saved [`StoredConnection`] -- if any -- it came from, nor
/// a display name for a `DATABASE_URL` fallback connection that was never
/// saved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveConnection {
    /// The display name shown in the footer and the modal's active row.
    pub name: String,
    /// The connection URL this name was resolved for.
    pub url: String,
}

/// What the connection footer (see [`super::super::footer`]) should render, derived
/// from the session's lifecycle state and whichever connection (if any) is
/// currently tracked as active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FooterDisplay {
    /// Show the active connection's name and host, with a filled status dot.
    Connected {
        /// The active connection's display name.
        name: String,
        /// A `host[:port]`-shaped label derived from the active connection's
        /// URL.
        host: String,
    },
    /// Show the "not connected, click to connect" prompt with a hollow dot.
    Disconnected,
}

/// The connection footer's display, given whether a live connection is
/// currently held (see [`crate::session::Session::is_connected`]) and whichever connection
/// is tracked as active. Connected only counts when both hold: the session
/// actually holds a live connection *and* an active connection is tracked --
/// a connected session with no tracked active connection (which should not
/// normally happen, since every connect path threads one through) still
/// falls back to the disconnected prompt rather than showing a blank name.
///
/// Deliberately takes connection liveness rather than [`crate::session::SessionState`]
/// itself: a query error moves `state` to [`crate::session::SessionState::Error`] without
/// dropping the underlying connection, and the footer must keep showing the
/// still-connected database through that, not fall back to "Not connected".
#[must_use]
pub fn footer_display(
    session_is_connected: bool,
    active: Option<&ActiveConnection>,
) -> FooterDisplay {
    match (session_is_connected, active) {
        (true, Some(active)) => FooterDisplay::Connected {
            name: active.name.clone(),
            host: host_label(&active.url),
        },
        _ => FooterDisplay::Disconnected,
    }
}

/// Determine the active-connection label for a freshly connected `url`: the
/// name of whichever [`StoredConnection`] in `saved` has a matching url
/// (first match wins), or -- when `url` matches no saved connection, e.g. a
/// `DATABASE_URL`/`Config` fallback connection -- a name derived from the
/// url's host via [`host_label`], so the footer always has something
/// sensible to show instead of a blank label.
#[must_use]
pub fn active_connection_for_url(url: &str, saved: &[StoredConnection]) -> ActiveConnection {
    let name = saved
        .iter()
        .find(|connection| connection.url == url)
        .map_or_else(|| host_label(url), |connection| connection.name.clone());
    ActiveConnection {
        name,
        url: url.to_owned(),
    }
}

/// Extract a `host[:port]`-shaped label from a connection URL for display,
/// e.g. `postgres://user:pass@localhost:5432/db` -> `localhost:5432`. Falls
/// back to the scheme-stripped remainder of the URL if no host segment can
/// be isolated (e.g. a `sqlite:` path), so even an unusual URL still renders
/// something instead of an empty label.
#[must_use]
pub fn host_label(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let after_userinfo = after_scheme
        .rsplit_once('@')
        .map_or(after_scheme, |(_, rest)| rest);
    let host = after_userinfo
        .split(['/', '?'])
        .next()
        .unwrap_or(after_userinfo);
    if host.is_empty() {
        after_scheme.to_owned()
    } else {
        host.to_owned()
    }
}
