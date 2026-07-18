//! Mapping from `SQLite`'s runtime values to the engine-neutral
//! [`zsql_core::Value`].
//!
//! `SQLite` is dynamically typed: each *value* (not each column) carries one of
//! five storage classes - NULL, INTEGER, REAL, TEXT, BLOB - chosen by `SQLite`
//! itself at write time regardless of any declared column type. This module
//! dispatches on that per-value storage class rather than on a column's
//! declared type.
//!
//! Several [`Value`] variants the Postgres mapping produces are never
//! produced here, because `SQLite` has no matching native type: `Array` (no
//! array type - callers would see a BLOB or repeated rows instead), `Uuid`
//! (stored as plain TEXT), `Json` (the `json1` functions return TEXT, not a
//! distinct storage class), and `Numeric` (arbitrary-precision decimals are
//! not a `SQLite` storage class - the closest analog, `NUMERIC` column
//! affinity, just coerces the value into INTEGER, REAL, or TEXT). A JSON- or
//! UUID-shaped column therefore decodes to [`Value::Text`], which is correct
//! and lossless, just not one of those specialized variants.

use sqlx::sqlite::{SqliteColumn, SqliteRow};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};
use zsql_core::{ColumnMeta, Row as CoreRow, Value};

/// Build the `Columns` metadata for a prepared statement's output columns.
///
/// `SQLite` does not report per-query column nullability without inspecting
/// the schema `pragma_table_info` reports (and an expression column, e.g.
/// `SELECT 1`, has no such schema at all), so every column is conservatively
/// reported as nullable: it is never wrong to say a column *might* be null,
/// only to claim one *can't* be when it can.
pub(crate) fn column_metas(columns: &[SqliteColumn]) -> Vec<ColumnMeta> {
    columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name().to_owned(),
            type_name: column.type_info().name().to_owned(),
            nullable: true,
        })
        .collect()
}

/// Decode one `SQLite` row into an engine-neutral [`CoreRow`].
pub(crate) fn decode_row(row: &SqliteRow) -> CoreRow {
    CoreRow((0..row.len()).map(|idx| decode_value(row, idx)).collect())
}

/// Decode a single column of `row` into a [`Value`], dispatching on that
/// value's own runtime storage class (not the column's declared type).
/// Falls back to [`Value::Unknown`] for anything unrecognized, including a
/// raw-value read failure.
fn decode_value(row: &SqliteRow, idx: usize) -> Value {
    let Ok(raw) = row.try_get_raw(idx) else {
        return Value::Unknown(String::new());
    };
    // `is_null()` reads the value's actual runtime storage class; unlike
    // `type_info().name()` below it is never overridden by a NULL column's
    // declared type, so it is the only reliable way to detect SQL NULL here.
    if raw.is_null() {
        return Value::Null;
    }
    storage_class_value(row, idx, raw.type_info().name())
}

/// Decode `row[idx]`, already known non-null, by its storage class name -
/// one of `"INTEGER"`, `"REAL"`, `"TEXT"`, `"BLOB"` for any value `SQLite`
/// itself produced. Anything else (unreachable in practice, since
/// `sqlite3_value_type` only ever returns one of those four codes for a
/// non-null value) degrades to [`Value::Unknown`] rather than panicking.
fn storage_class_value(row: &SqliteRow, idx: usize, storage_class: &str) -> Value {
    match storage_class {
        "INTEGER" => row
            .try_get::<i64, _>(idx)
            .map_or_else(|_| Value::Unknown(storage_class.to_owned()), Value::Int),
        "REAL" => row
            .try_get::<f64, _>(idx)
            .map_or_else(|_| Value::Unknown(storage_class.to_owned()), Value::Float),
        "TEXT" => row
            .try_get::<String, _>(idx)
            .map_or_else(|_| Value::Unknown(storage_class.to_owned()), Value::Text),
        "BLOB" => row
            .try_get::<Vec<u8>, _>(idx)
            .map_or_else(|_| Value::Unknown(storage_class.to_owned()), Value::Bytes),
        other => Value::Unknown(other.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use sqlx::Row as _;
    use sqlx::sqlite::SqlitePoolOptions;
    use zsql_core::Value;

    use super::{decode_row, storage_class_value};

    /// A private, non-shared in-memory database dedicated to this test
    /// module: each test opens its own pool of exactly one connection so
    /// `SQLite`'s per-connection `:memory:` isolation never leaks state
    /// between tests.
    async fn memory_pool() -> sqlx::sqlite::SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite connection must succeed")
    }

    fn select_one_row(sql: &str) -> zsql_core::Row {
        let sql = sql.to_owned();
        block_on(async {
            let pool = memory_pool().await;
            // Test-only fixture SQL, never user input: safe to assert.
            let row = sqlx::query(sqlx::AssertSqlSafe(sql))
                .fetch_one(&pool)
                .await
                .expect("query should succeed");
            decode_row(&row)
        })
    }

    #[test]
    fn decodes_null_storage_class() {
        let row = select_one_row("SELECT NULL AS c");
        assert_eq!(row.0[0], Value::Null);
    }

    #[test]
    fn decodes_integer_storage_class() {
        let row = select_one_row("SELECT 42 AS c");
        assert_eq!(row.0[0], Value::Int(42));
    }

    #[test]
    fn decodes_real_storage_class() {
        let row = select_one_row("SELECT 1.5 AS c");
        assert_eq!(row.0[0], Value::Float(1.5));
    }

    #[test]
    fn decodes_text_storage_class() {
        let row = select_one_row("SELECT 'hello' AS c");
        assert_eq!(row.0[0], Value::Text("hello".to_owned()));
    }

    #[test]
    fn decodes_blob_storage_class() {
        let row = select_one_row("SELECT x'0102' AS c");
        assert_eq!(row.0[0], Value::Bytes(vec![0x01, 0x02]));
    }

    #[test]
    fn json_shaped_text_decodes_as_plain_text_not_json_variant() {
        // SQLite has no JSON storage class: `json1` functions return TEXT.
        let row = select_one_row("SELECT json_object('a', 1) AS c");
        assert_eq!(row.0[0], Value::Text("{\"a\":1}".to_owned()));
    }

    #[test]
    fn uuid_shaped_text_decodes_as_plain_text_not_uuid_variant() {
        // SQLite has no UUID storage class: a UUID is just TEXT.
        let row = select_one_row("SELECT '11111111-1111-1111-1111-111111111111' AS c");
        assert_eq!(
            row.0[0],
            Value::Text("11111111-1111-1111-1111-111111111111".to_owned())
        );
    }

    #[test]
    fn column_metas_reports_every_column_as_nullable() {
        let metas = block_on(async {
            let pool = memory_pool().await;
            let row = sqlx::query("SELECT 1 AS a, 'x' AS b")
                .fetch_one(&pool)
                .await
                .expect("query should succeed");
            super::column_metas(row.columns())
        });
        assert_eq!(metas.len(), 2);
        assert!(metas.iter().all(|m| m.nullable));
        assert_eq!(metas[0].name, "a");
        assert_eq!(metas[1].name, "b");
    }

    #[test]
    fn unrecognized_storage_class_falls_back_to_unknown() {
        // `storage_class_value` is exercised directly here with a name no
        // real SQLite value ever reports (only "INTEGER"/"REAL"/"TEXT"/
        // "BLOB" are reachable for a non-null value in practice), to prove
        // the fallback arm is real and tested rather than dead code.
        let row = block_on(async {
            let pool = memory_pool().await;
            sqlx::query("SELECT 1 AS c")
                .fetch_one(&pool)
                .await
                .expect("query should succeed")
        });
        assert_eq!(
            storage_class_value(&row, 0, "SOMETHING_UNMAPPED"),
            Value::Unknown("SOMETHING_UNMAPPED".to_owned())
        );
    }
}
