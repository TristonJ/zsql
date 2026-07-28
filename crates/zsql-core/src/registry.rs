//! Driver selection: map a connection URL's scheme to one of a caller-
//! supplied set of registered [`Driver`]s.

use std::sync::Arc;

use crate::driver::Driver;
use crate::error::CoreError;

/// Map a URL scheme to the canonical `Driver::id()` that should handle it.
/// Several schemes may alias the same driver (e.g. `postgres`/`postgresql`).
fn driver_id_for_scheme(drivers: &[Arc<dyn Driver>], scheme: &str) -> Option<&'static str> {
    for driver in drivers {
        if driver.url_schemes().contains(&scheme) {
            return Some(driver.id());
        }
    }
    None
}

/// Extract the lowercased scheme prefix from `url` (the text before its
/// first `:`), or `None` if `url` has no scheme at all.
fn scheme_of(url: &str) -> Option<String> {
    let (scheme, _rest) = url.split_once(':')?;
    if scheme.is_empty() {
        return None;
    }
    Some(scheme.to_lowercase())
}

/// Select the registered driver whose [`Driver::id`] matches `url`'s scheme.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` has no recognizable scheme, the
/// scheme is not one this function knows how to route, or no driver with the
/// matching id is present in `drivers`. The error message names the
/// offending scheme only, never the full `url` (which may embed
/// credentials).
pub fn select_driver<'a>(
    drivers: &'a [Arc<dyn Driver>],
    url: &str,
) -> Result<&'a Arc<dyn Driver>, CoreError> {
    let scheme = scheme_of(url).ok_or_else(|| CoreError::Url("missing URL scheme".to_owned()))?;
    let driver_id = driver_id_for_scheme(drivers, &scheme)
        .ok_or_else(|| CoreError::Url(format!("unrecognized URL scheme '{scheme}'")))?;
    drivers
        .iter()
        .find(|driver| driver.id() == driver_id)
        .ok_or_else(|| CoreError::Url(format!("no driver registered for scheme '{scheme}'")))
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::{Arc, select_driver};
    use crate::config::ConnConfig;
    use crate::driver::{Connection, Driver};
    use crate::error::CoreError;

    /// A `Driver` double identified only by a fixed id, used to exercise
    /// selection without depending on any real backend crate.
    struct FakeDriver(&'static str);

    #[async_trait]
    impl Driver for FakeDriver {
        fn id(&self) -> &'static str {
            self.0
        }

        fn display_name(&self) -> &'static str {
            self.0
        }

        fn default_port(&self) -> Option<u16> {
            Some(1234)
        }

        fn url_schemes(&self) -> &[&'static str] {
            match self.0 {
                "postgres" => &["postgres", "postgresql"],
                "sqlite" => &["sqlite", "file"],
                "mssql" => &["mssql", "sqlserver"],
                "mysql" => &["mysql", "mariadb"],
                _ => &[],
            }
        }

        fn parse_url(&self, url: &str) -> Result<ConnConfig, CoreError> {
            ConnConfig::from_url(url)
        }

        async fn connect(&self, _cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
            Err(CoreError::Connection {
                message: "fake driver never connects".to_owned(),
                transient: false,
                source: None,
            })
        }
    }

    fn registry() -> Vec<Arc<dyn Driver>> {
        vec![
            Arc::new(FakeDriver("postgres")),
            Arc::new(FakeDriver("sqlite")),
            Arc::new(FakeDriver("mssql")),
            Arc::new(FakeDriver("mysql")),
        ]
    }

    #[test]
    fn postgres_and_postgresql_schemes_select_the_postgres_driver() {
        let drivers = registry();
        for url in ["postgres://user@host/db", "postgresql://user@host/db"] {
            let driver = select_driver(&drivers, url).expect("scheme should resolve");
            assert_eq!(driver.id(), "postgres");
        }
    }

    #[test]
    fn sqlite_and_file_schemes_select_the_sqlite_driver() {
        let drivers = registry();
        for url in [
            "sqlite://path/to/db.sqlite3",
            "sqlite::memory:",
            "file:./local.db",
        ] {
            let driver = select_driver(&drivers, url).expect("scheme should resolve");
            assert_eq!(driver.id(), "sqlite");
        }
    }

    #[test]
    fn mssql_and_sqlserver_schemes_select_the_mssql_driver() {
        let drivers = registry();
        for url in ["mssql://user@host/db", "sqlserver://user@host/db"] {
            let driver = select_driver(&drivers, url).expect("scheme should resolve");
            assert_eq!(driver.id(), "mssql");
        }
    }

    #[test]
    fn mysql_and_mariadb_schemes_select_the_same_registered_mysql_driver() {
        let drivers = registry();
        for url in ["mysql://root@host/db", "mariadb://root@host/db"] {
            let driver = select_driver(&drivers, url).expect("scheme should resolve");
            assert_eq!(driver.id(), "mysql");
        }
    }

    #[test]
    fn an_unrecognized_scheme_is_a_typed_error_naming_the_scheme_not_the_url() {
        let drivers = registry();
        let url = "cassandra://secret-password@host/db";
        let result = select_driver(&drivers, url);
        match result {
            Err(CoreError::Url(message)) => {
                assert!(message.contains("cassandra"), "message: {message}");
                assert!(
                    !message.contains("secret-password"),
                    "the offending scheme's message must not leak the full URL: {message}"
                );
            }
            Err(other) => panic!("expected CoreError::Url, got {other:?}"),
            Ok(_) => panic!("expected an error, got a resolved driver"),
        }
    }

    #[test]
    fn a_url_with_no_scheme_at_all_is_a_typed_error() {
        let drivers = registry();
        assert!(matches!(
            select_driver(&drivers, "not-a-url"),
            Err(CoreError::Url(_))
        ));
        assert!(matches!(
            select_driver(&drivers, ""),
            Err(CoreError::Url(_))
        ));
    }

    #[test]
    fn a_url_with_an_empty_scheme_before_the_first_colon_is_a_typed_error() {
        // ":memory:" splits into an empty scheme and "memory:" -- this must
        // hit the same "no scheme" error path as a URL with no colon at all,
        // not be treated as a scheme that happens to be the empty string.
        let drivers = registry();
        assert!(matches!(
            select_driver(&drivers, ":memory:"),
            Err(CoreError::Url(_))
        ));
    }

    #[test]
    fn a_recognized_scheme_with_no_matching_registered_driver_is_a_typed_error() {
        // Only a sqlite driver registered; a postgres URL has nowhere to go.
        let drivers: Vec<Arc<dyn Driver>> = vec![Arc::new(FakeDriver("sqlite"))];
        let result = select_driver(&drivers, "postgres://user@host/db");
        assert!(matches!(result, Err(CoreError::Url(_))));
    }

    #[test]
    fn scheme_matching_is_case_insensitive() {
        let drivers = registry();
        let driver = select_driver(&drivers, "POSTGRES://user@host/db").expect("should resolve");
        assert_eq!(driver.id(), "postgres");
    }
}
