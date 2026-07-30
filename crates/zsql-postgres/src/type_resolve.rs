//! Resolves a Postgres column's dynamically-assigned type OID -- `citext`,
//! `hstore`, a user-defined enum/domain/composite, or any other type whose
//! OID is not one of Postgres's compile-time-stable built-ins -- into its
//! catalog `typname`, for columns sqlx itself cannot name from its own
//! static type tables.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgColumn, PgPool};
use sqlx::{Column as _, Row as _, TypeInfo as _};
use zsql_core::ColumnMeta;

/// sqlx reports a column's type name as this placeholder when it knows only
/// the column's OID, not its catalog name (`PgType::DeclareWithOid` in
/// sqlx-postgres reports both `name()` and `display_name()` this way).
const UNRESOLVED_TYPE_NAME: &str = "?";

/// Per-connection context for the Postgres driver's column-resolution hook:
/// a pool independent of the one streaming the current query's rows (that
/// pool's connection is mutably borrowed for the whole query, so a lookup
/// issued on it would have to wait behind the very rows it is trying to
/// label), plus a memo of OIDs already resolved. One resolver backs exactly
/// one zsql connection, hence exactly one database, and a database's OIDs
/// are stable for as long as that connection stays open, so the memo never
/// needs to expire entries.
#[derive(Clone)]
pub struct PgColumnResolver {
    lookup_pool: PgPool,
    memo: Arc<Mutex<HashMap<u32, String>>>,
}

impl PgColumnResolver {
    pub(crate) fn new(lookup_pool: PgPool) -> Self {
        Self {
            lookup_pool,
            memo: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Patch every column in `columns` whose type name sqlx could not resolve
/// (see [`UNRESOLVED_TYPE_NAME`]) with its catalog `typname`, looked up (and
/// memoized) via `resolver`. `raw_columns` and `columns` must be the same
/// statement's column list, in the same order -- `raw_columns` carries the
/// OID [`ColumnMeta`] itself does not.
///
/// A column whose OID cannot be resolved (lookup failure, or sqlx reporting
/// no OID at all) is left exactly as [`crate::driver::PostgresDriver::column_metas`]
/// produced it, so a resolution problem degrades the affected column's type
/// badge rather than failing the query.
#[tracing::instrument(name = "pg_resolve_columns", skip_all)]
pub(crate) async fn resolve_columns(
    resolver: &PgColumnResolver,
    raw_columns: &[PgColumn],
    columns: &mut [ColumnMeta],
) {
    let unresolved: Vec<(usize, u32)> = raw_columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.type_info().name() == UNRESOLVED_TYPE_NAME)
        .filter_map(|(idx, column)| column.type_info().oid().map(|oid| (idx, oid.0)))
        .collect();
    if unresolved.is_empty() {
        return;
    }

    let oids: Vec<u32> = unresolved.iter().map(|(_, oid)| *oid).collect();
    let lookup_pool = &resolver.lookup_pool;
    let typenames = resolve_oids(&resolver.memo, &oids, |missing| {
        lookup_typenames(lookup_pool, missing)
    })
    .await;

    for (idx, oid) in unresolved {
        if let Some(name) = typenames.get(&oid) {
            columns[idx].type_name = display_type_name(name);
        }
    }
}

/// Resolve `oids`, consulting `memo` first and calling `lookup` at most once
/// for whatever remains unresolved after that (a no-op call when nothing
/// is missing). Kept independent of the actual `pg_type` query -- `lookup`
/// is injected -- so the memoization contract (a later call for an
/// already-resolved OID never calls `lookup` again) is unit-testable
/// without a live database.
async fn resolve_oids<F, Fut>(
    memo: &Mutex<HashMap<u32, String>>,
    oids: &[u32],
    lookup: F,
) -> HashMap<u32, String>
where
    F: FnOnce(Vec<u32>) -> Fut,
    Fut: Future<Output = Result<Vec<(u32, String)>, sqlx::Error>>,
{
    let mut resolved = HashMap::new();
    let mut missing = Vec::new();
    {
        let cached = memo.lock().unwrap_or_else(PoisonError::into_inner);
        for &oid in oids {
            match cached.get(&oid) {
                Some(name) => {
                    resolved.insert(oid, name.clone());
                }
                None => missing.push(oid),
            }
        }
    }
    if missing.is_empty() {
        return resolved;
    }

    match lookup(missing).await {
        Ok(found) => {
            tracing::debug!(resolved = found.len(), "resolved postgres type oid(s)");
            let mut cached = memo.lock().unwrap_or_else(PoisonError::into_inner);
            for (oid, name) in found {
                cached.insert(oid, name.clone());
                resolved.insert(oid, name);
            }
        }
        Err(error) => {
            tracing::debug!(
                %error,
                "pg_type lookup failed; leaving the affected column(s) at their unresolved type name"
            );
        }
    }
    resolved
}

/// Batched `pg_type` lookup for `oids`, run on `pool` -- always a pool other
/// than the one streaming the current query's rows.
async fn lookup_typenames(
    pool: &PgPool,
    oids: Vec<u32>,
) -> Result<Vec<(u32, String)>, sqlx::Error> {
    let bound: Vec<Oid> = oids.into_iter().map(Oid).collect();
    let rows = sqlx::query("SELECT oid, typname FROM pg_type WHERE oid = ANY($1)")
        .bind(bound)
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|row| {
            let oid: Oid = row.try_get("oid")?;
            let typname: String = row.try_get("typname")?;
            Ok((oid.0, typname))
        })
        .collect()
}

/// Render a resolved catalog `typname` for display. Postgres names an
/// auto-generated array type after its element type with a leading
/// underscore (e.g. `_citext` backs `citext[]`) -- a stable catalog
/// convention, not an inference -- which this expands into the bracket form
/// the grid's other array-typed columns already use; every other name
/// (composite, enum, domain, scalar extension type) passes through
/// unchanged.
fn display_type_name(typname: &str) -> String {
    typname
        .strip_prefix('_')
        .map_or_else(|| typname.to_owned(), |elem| format!("{elem}[]"))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{display_type_name, resolve_oids};

    #[test]
    fn display_type_name_expands_the_leading_underscore_array_convention() {
        assert_eq!(display_type_name("_citext"), "citext[]");
        assert_eq!(display_type_name("_my_enum"), "my_enum[]");
    }

    #[test]
    fn display_type_name_passes_a_scalar_name_through_unchanged() {
        assert_eq!(display_type_name("citext"), "citext");
        assert_eq!(display_type_name("my_enum"), "my_enum");
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    #[test]
    fn resolve_oids_returns_the_lookups_result_and_memoizes_it() {
        let memo = Mutex::new(std::collections::HashMap::new());
        let calls = AtomicUsize::new(0);

        let resolved = block_on(resolve_oids(&memo, &[100], |missing| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(missing, vec![100]);
            async { Ok(vec![(100, "my_enum".to_owned())]) }
        }));

        assert_eq!(resolved.get(&100), Some(&"my_enum".to_owned()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            memo.lock().unwrap().get(&100),
            Some(&"my_enum".to_owned()),
            "a resolved oid must be written back into the memo"
        );
    }

    #[test]
    fn resolve_oids_never_calls_lookup_again_for_an_already_memoized_oid() {
        let memo = Mutex::new(std::collections::HashMap::new());
        memo.lock().unwrap().insert(100u32, "my_enum".to_owned());
        let calls = AtomicUsize::new(0);

        let resolved = block_on(resolve_oids(&memo, &[100], |_missing| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(Vec::new()) }
        }));

        assert_eq!(resolved.get(&100), Some(&"my_enum".to_owned()));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "an oid already in the memo must never trigger a second lookup"
        );
    }

    #[test]
    fn resolve_oids_only_looks_up_the_oids_missing_from_the_memo() {
        let memo = Mutex::new(std::collections::HashMap::new());
        memo.lock().unwrap().insert(100u32, "cached".to_owned());

        let resolved = block_on(resolve_oids(&memo, &[100, 200], |missing| {
            assert_eq!(missing, vec![200]);
            async { Ok(vec![(200, "fresh".to_owned())]) }
        }));

        assert_eq!(resolved.get(&100), Some(&"cached".to_owned()));
        assert_eq!(resolved.get(&200), Some(&"fresh".to_owned()));
    }

    #[test]
    fn resolve_oids_is_non_fatal_when_the_lookup_fails() {
        let memo = Mutex::new(std::collections::HashMap::new());

        let resolved = block_on(resolve_oids(&memo, &[100], |_missing| async {
            Err(sqlx::Error::PoolClosed)
        }));

        assert!(
            resolved.is_empty(),
            "a failed lookup must leave the affected oids unresolved, not panic"
        );
        assert!(
            memo.lock().unwrap().is_empty(),
            "a failed lookup must not memoize anything"
        );
    }

    #[test]
    fn resolve_oids_skips_the_lookup_entirely_when_nothing_is_missing() {
        let memo = Mutex::new(std::collections::HashMap::new());
        let calls = AtomicUsize::new(0);

        let resolved = block_on(resolve_oids(&memo, &[], |_missing| {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(Vec::new()) }
        }));

        assert!(resolved.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
