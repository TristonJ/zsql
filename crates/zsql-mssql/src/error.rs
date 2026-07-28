//! Mapping from `tiberius::error::Error` (and the raw `std::io::Error` a TCP
//! connect can fail with before tiberius is ever involved) to the
//! driver-agnostic [`zsql_core::CoreError`].

use zsql_core::CoreError;

/// Convert an I/O error encountered while opening the TCP connection into a
/// [`CoreError::Connection`].
pub(crate) fn map_io_connect_error(err: &std::io::Error) -> CoreError {
    CoreError::connection(format!("network error: {err}"), true)
}

/// Convert a `tiberius::Error` encountered while establishing (or logging
/// into) a connection into a [`CoreError::Connection`].
pub(crate) fn map_connect_error(err: tiberius::error::Error) -> CoreError {
    CoreError::Connection {
        message: describe(&err),
        transient: is_transient(&err),
        source: Some(std::sync::Arc::new(err)),
    }
}

/// Convert a `tiberius::Error` encountered while streaming a query's results
/// into a [`CoreError::Query`].
pub(crate) fn map_query_error(err: tiberius::error::Error) -> CoreError {
    CoreError::Query {
        message: describe(&err),
        code: err.code().map(|c| c.to_string()),
        position: None,
        source: Some(std::sync::Arc::new(err)),
    }
}

/// Convert a `tiberius::Error` encountered while introspecting the schema
/// into a [`CoreError::Introspection`].
pub(crate) fn map_introspect_error(err: tiberius::error::Error) -> CoreError {
    CoreError::Introspection {
        message: describe(&err),
        source: Some(std::sync::Arc::new(err)),
    }
}

/// Return whether or not this triberius error is plausibly transient
pub(crate) fn is_transient(err: &tiberius::error::Error) -> bool {
    use tiberius::error::Error;
    match err {
        Error::Io { kind, .. } => matches!(
            kind,
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::WouldBlock
        ),
        Error::Tls(_) => true,
        _ => false,
    }
}

/// Render a short, useful description of a `tiberius::Error`. Never includes
/// connection-string text: `tiberius::Error` carries none of it (that lives
/// only in the URL this crate parses, never handed to `tiberius::Error`).
fn describe(err: &tiberius::error::Error) -> String {
    use tiberius::error::Error;
    match err {
        Error::Io { message, .. } => format!("network error: {message}"),
        Error::Tls(message) => format!("TLS error: {message}"),
        Error::Server(token) => format!("server rejected request: {}", token.message()),
        Error::Protocol(message) => format!("protocol error: {message}"),
        other => format!("mssql error: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{map_connect_error, map_introspect_error, map_io_connect_error, map_query_error};
    use zsql_core::CoreError;

    fn sample_io_error() -> tiberius::error::Error {
        tiberius::error::Error::Io {
            kind: std::io::ErrorKind::ConnectionRefused,
            message: "connection refused".to_owned(),
        }
    }

    #[test]
    fn io_error_maps_to_connection_error_with_network_prefix() {
        let mapped = map_connect_error(sample_io_error());
        match mapped {
            CoreError::Connection { message, .. } => {
                assert_eq!(message, "network error: connection refused");
            }
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn io_error_maps_to_query_error() {
        let mapped = map_query_error(sample_io_error());
        assert!(matches!(mapped, CoreError::Query { .. }));
    }

    #[test]
    fn io_error_maps_to_introspection_error() {
        let mapped = map_introspect_error(sample_io_error());
        assert!(matches!(mapped, CoreError::Introspection { .. }));
    }

    #[test]
    fn io_connect_error_maps_to_connection_error_with_network_prefix() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let mapped = map_io_connect_error(&io_err);
        match mapped {
            CoreError::Connection { message, .. } => assert!(message.starts_with("network error:")),
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn tls_error_maps_with_a_tls_prefix() {
        let mapped = map_connect_error(tiberius::error::Error::Tls("bad cert".to_owned()));
        match mapped {
            CoreError::Connection { message, .. } => assert!(message.contains("TLS error")),
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }
}
