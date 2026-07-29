//! Enumerating the databases selectable on an MSSQL server, for
//! [`zsql_core::Connection::list_databases`].

use async_net::TcpStream;
use tiberius::Client;
use zsql_core::CoreError;

use crate::introspect::{run, text};

/// System databases every MSSQL instance carries, excluded from
/// [`list_databases`] by default: there is no user data to switch to in any
/// of them, and no v1 config knob to opt back in.
const EXCLUDED_SYSTEM_DATABASES: &[&str] = &["master", "model", "msdb", "tempdb"];

/// Render [`EXCLUDED_SYSTEM_DATABASES`] as a SQL `IN (...)` literal list.
/// Built purely from the fixed constant above, never from anything read at
/// runtime.
fn excluded_databases_sql_list() -> String {
    EXCLUDED_SYSTEM_DATABASES
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `SELECT name FROM sys.databases WHERE state = 0 AND name NOT IN (...)
/// ORDER BY name`, with [`EXCLUDED_SYSTEM_DATABASES`] spliced into the
/// `NOT IN` list.
fn list_databases_sql() -> String {
    format!(
        "SELECT name FROM sys.databases \
         WHERE state = 0 AND name NOT IN ({}) \
         ORDER BY name",
        excluded_databases_sql_list()
    )
}

/// Every online (`state = 0`), non-system database on the server, sorted by
/// name. `sys.databases` is an instance-wide catalog visible regardless of
/// which database the connection is attached to, so this reflects the whole
/// server, not just the connected database.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if the underlying query fails.
pub(crate) async fn list_databases(
    client: &mut Client<TcpStream>,
) -> Result<Vec<String>, CoreError> {
    let rows = run(client, list_databases_sql()).await?;
    rows.iter().map(|row| text(row, "name")).collect()
}

#[cfg(test)]
mod tests {
    use super::{EXCLUDED_SYSTEM_DATABASES, excluded_databases_sql_list, list_databases_sql};

    #[test]
    fn excludes_every_system_database_by_default() {
        let sql = list_databases_sql();
        for name in EXCLUDED_SYSTEM_DATABASES {
            assert!(
                sql.contains(&format!("'{name}'")),
                "missing {name} in {sql}"
            );
        }
    }

    #[test]
    fn excluded_databases_list_quotes_every_entry_and_joins_with_commas() {
        let sql = excluded_databases_sql_list();
        assert_eq!(sql, "'master', 'model', 'msdb', 'tempdb'");
    }

    #[test]
    fn the_query_filters_offline_databases_and_orders_by_name() {
        let sql = list_databases_sql();
        assert!(sql.contains("state = 0"));
        assert!(sql.to_uppercase().contains("ORDER BY NAME"));
    }
}
