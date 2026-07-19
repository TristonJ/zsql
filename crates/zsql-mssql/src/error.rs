//! Mapping from `tiberius::error::Error` (and the raw `std::io::Error` a TCP
//! connect can fail with before tiberius is ever involved) to the
//! driver-agnostic [`zsql_core::CoreError`].

use zsql_core::CoreError;

/// Convert an I/O error encountered while opening the TCP connection into a
/// [`CoreError::Connection`].
pub(crate) fn map_io_connect_error(err: &std::io::Error) -> CoreError {
    CoreError::Connection(format!("network error: {err}"))
}

/// Convert a `tiberius::Error` encountered while establishing (or logging
/// into) a connection into a [`CoreError::Connection`].
pub(crate) fn map_connect_error(err: tiberius::error::Error) -> CoreError {
    CoreError::Connection(describe(err))
}

/// Convert a `tiberius::Error` encountered while streaming a query's results
/// into a [`CoreError::Query`].
pub(crate) fn map_query_error(err: tiberius::error::Error) -> CoreError {
    CoreError::Query(describe(err))
}

/// Convert a `tiberius::Error` encountered while introspecting the schema
/// into a [`CoreError::Introspection`].
pub(crate) fn map_introspect_error(err: tiberius::error::Error) -> CoreError {
    CoreError::Introspection(describe(err))
}

/// Render a short, useful description of a `tiberius::Error`. Never includes
/// connection-string text: `tiberius::Error` carries none of it (that lives
/// only in the DSN this crate parses, never handed to `tiberius::Error`).
fn describe(err: tiberius::error::Error) -> String {
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
            CoreError::Connection(msg) => {
                assert_eq!(msg, "network error: connection refused");
            }
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn io_error_maps_to_query_error() {
        let mapped = map_query_error(sample_io_error());
        assert!(matches!(mapped, CoreError::Query(_)));
    }

    #[test]
    fn io_error_maps_to_introspection_error() {
        let mapped = map_introspect_error(sample_io_error());
        assert!(matches!(mapped, CoreError::Introspection(_)));
    }

    #[test]
    fn io_connect_error_maps_to_connection_error_with_network_prefix() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let mapped = map_io_connect_error(&io_err);
        match mapped {
            CoreError::Connection(msg) => assert!(msg.starts_with("network error:")),
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn tls_error_maps_with_a_tls_prefix() {
        let mapped = map_connect_error(tiberius::error::Error::Tls("bad cert".to_owned()));
        match mapped {
            CoreError::Connection(msg) => assert!(msg.contains("TLS error")),
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }
}
