//! Generates a stress-test `SQLite` database file: several related tables with
//! enough rows to be useful for manual browsing and perf testing of the
//! results grid / schema sidebar. Run via `scripts/sqlite-stress-db.sh`, or
//! directly with `cargo run -p zsql-sqlite --example gen_stress_db -- <path>`.
//!
//! This is a standalone generation script, not part of the driver's runtime
//! query path: rows are built as inline SQL literals (never end-user input)
//! for simplicity, rather than through sqlx's bound-argument API.

use std::fmt::Write as _;
use std::time::Instant;

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Executor as _, Row as _, SqliteConnection};

/// Default output path when no argument is given, relative to wherever the
/// script is invoked from.
const DEFAULT_OUTPUT_PATH: &str = "dev/stress.sqlite3";

const CUSTOMER_COUNT: u32 = 20_000;
const ORDER_COUNT: u32 = 100_000;
const ORDER_ITEM_COUNT: u32 = 300_000;
const EVENT_COUNT: u32 = 50_000;

/// Rows per multi-row `INSERT` statement. Rows are inlined as literals here
/// (not sqlx bound arguments), so this bound exists only to keep any single
/// statement's generated SQL text a manageable size, not to respect a
/// parameter-count ceiling.
const INSERT_CHUNK_SIZE: u32 = 500;

const ORDER_STATUSES: [&str; 5] = ["pending", "paid", "shipped", "delivered", "cancelled"];

fn main() {
    let mut args = std::env::args().skip(1);
    let output_path = args
        .next()
        .unwrap_or_else(|| DEFAULT_OUTPUT_PATH.to_owned());

    futures::executor::block_on(generate(&output_path)).expect("stress db generation failed");
}

async fn generate(output_path: &str) -> sqlx::Result<()> {
    let path = std::path::Path::new(output_path);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).expect("failed to create output directory");
    }
    // Start from a clean file every run so the printed counts are exact.
    let _ = std::fs::remove_file(path);

    let started = Instant::now();
    let dsn = format!("sqlite://{}?mode=rwc", path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&dsn)
        .await?;

    // Durability is irrelevant for a throwaway stress fixture; trade it for
    // generation speed.
    pool.execute(AssertSqlSafe(
        "PRAGMA journal_mode = MEMORY; PRAGMA synchronous = OFF;".to_owned(),
    ))
    .await?;

    create_schema(&pool).await?;

    let mut conn = pool.acquire().await?;
    insert_customers(&mut conn, CUSTOMER_COUNT).await?;
    insert_orders(&mut conn, ORDER_COUNT, CUSTOMER_COUNT).await?;
    insert_order_items(&mut conn, ORDER_ITEM_COUNT, ORDER_COUNT).await?;
    insert_events(&mut conn, EVENT_COUNT, CUSTOMER_COUNT).await?;
    drop(conn);

    let elapsed = started.elapsed();
    let file_size = std::fs::metadata(path).map_or(0, |m| m.len());

    println!("generated stress database: {}", path.display());
    println!("file size: {} bytes ({:.1} MiB)", file_size, mib(file_size));
    for table in ["customers", "orders", "order_items", "events"] {
        let count = row_count(&pool, table).await?;
        println!("  {table}: {count} rows");
    }
    println!("elapsed: {:.2}s", elapsed.as_secs_f64());

    pool.close().await;
    Ok(())
}

fn mib(bytes: u64) -> f64 {
    f64::from(u32::try_from(bytes).unwrap_or(u32::MAX)) / (1024.0 * 1024.0)
}

async fn create_schema(pool: &SqlitePool) -> sqlx::Result<()> {
    pool.execute(AssertSqlSafe(
        "CREATE TABLE customers (\
             id INTEGER PRIMARY KEY, \
             name TEXT NOT NULL, \
             email TEXT NOT NULL, \
             created_at TEXT NOT NULL, \
             balance REAL NOT NULL\
         ); \
         CREATE TABLE orders (\
             id INTEGER PRIMARY KEY, \
             customer_id INTEGER NOT NULL REFERENCES customers(id), \
             placed_at TEXT NOT NULL, \
             status TEXT NOT NULL, \
             total REAL NOT NULL\
         ); \
         CREATE TABLE order_items (\
             id INTEGER PRIMARY KEY, \
             order_id INTEGER NOT NULL REFERENCES orders(id), \
             sku TEXT NOT NULL, \
             quantity INTEGER NOT NULL, \
             unit_price REAL NOT NULL\
         ); \
         CREATE TABLE events (\
             id INTEGER PRIMARY KEY, \
             customer_id INTEGER REFERENCES customers(id), \
             payload BLOB, \
             occurred_at TEXT NOT NULL\
         ); \
         CREATE INDEX orders_customer_id ON orders(customer_id); \
         CREATE INDEX order_items_order_id ON order_items(order_id); \
         CREATE VIEW recent_orders AS \
             SELECT * FROM orders WHERE placed_at >= '2024-01-01'"
            .to_owned(),
    ))
    .await?;
    Ok(())
}

async fn insert_customers(conn: &mut SqliteConnection, count: u32) -> sqlx::Result<()> {
    for (chunk_start, chunk_len) in chunks(count) {
        let mut sql =
            String::from("INSERT INTO customers (id, name, email, created_at, balance) VALUES ");
        for i in chunk_start..chunk_start + chunk_len {
            if i > chunk_start {
                sql.push(',');
            }
            let balance = f64::from(i % 100_000) / 100.0;
            let _ = write!(
                sql,
                "({id}, 'Customer {id}', 'customer{id}@example.test', '{date}', {balance})",
                id = i + 1,
                date = synthetic_date(i),
            );
        }
        conn.execute(AssertSqlSafe(sql)).await?;
    }
    Ok(())
}

async fn insert_orders(
    conn: &mut SqliteConnection,
    count: u32,
    customer_count: u32,
) -> sqlx::Result<()> {
    for (chunk_start, chunk_len) in chunks(count) {
        let mut sql =
            String::from("INSERT INTO orders (id, customer_id, placed_at, status, total) VALUES ");
        for i in chunk_start..chunk_start + chunk_len {
            if i > chunk_start {
                sql.push(',');
            }
            let customer_id = 1 + (i % customer_count);
            let status = ORDER_STATUSES[(i as usize) % ORDER_STATUSES.len()];
            let total = f64::from(i % 50_000) / 100.0;
            let _ = write!(
                sql,
                "({id}, {customer_id}, '{date}', '{status}', {total})",
                id = i + 1,
                date = synthetic_date(i),
            );
        }
        conn.execute(AssertSqlSafe(sql)).await?;
    }
    Ok(())
}

async fn insert_order_items(
    conn: &mut SqliteConnection,
    count: u32,
    order_count: u32,
) -> sqlx::Result<()> {
    for (chunk_start, chunk_len) in chunks(count) {
        let mut sql = String::from(
            "INSERT INTO order_items (id, order_id, sku, quantity, unit_price) VALUES ",
        );
        for i in chunk_start..chunk_start + chunk_len {
            if i > chunk_start {
                sql.push(',');
            }
            let order_id = 1 + (i % order_count);
            let sku_number = i % 500;
            let quantity = 1 + (i % 10);
            let unit_price = f64::from(i % 20_000) / 100.0;
            let _ = write!(
                sql,
                "({id}, {order_id}, 'SKU-{sku_number:05}', {quantity}, {unit_price})",
                id = i + 1,
            );
        }
        conn.execute(AssertSqlSafe(sql)).await?;
    }
    Ok(())
}

async fn insert_events(
    conn: &mut SqliteConnection,
    count: u32,
    customer_count: u32,
) -> sqlx::Result<()> {
    for (chunk_start, chunk_len) in chunks(count) {
        let mut sql =
            String::from("INSERT INTO events (id, customer_id, payload, occurred_at) VALUES ");
        for i in chunk_start..chunk_start + chunk_len {
            if i > chunk_start {
                sql.push(',');
            }
            let customer_id = 1 + (i % customer_count);
            // A small deterministic blob, hex-literal encoded.
            let _ = write!(
                sql,
                "({id}, {customer_id}, x'{b0:02x}{b1:02x}{b2:02x}{b3:02x}', '{date}T00:00:00')",
                id = i + 1,
                b0 = i & 0xff,
                b1 = (i >> 8) & 0xff,
                b2 = (i >> 16) & 0xff,
                b3 = i % 251,
                date = synthetic_date(i),
            );
        }
        conn.execute(AssertSqlSafe(sql)).await?;
    }
    Ok(())
}

/// A stable, made-up `YYYY-MM-DD` derived from `i`, spread across 2024.
fn synthetic_date(i: u32) -> String {
    format!("2024-{:02}-{:02}", 1 + (i % 12), 1 + (i % 28))
}

/// Split `total` into `(start, len)` pairs of at most [`INSERT_CHUNK_SIZE`]
/// rows each.
fn chunks(total: u32) -> impl Iterator<Item = (u32, u32)> {
    (0..total)
        .step_by(INSERT_CHUNK_SIZE as usize)
        .map(move |chunk_start| (chunk_start, INSERT_CHUNK_SIZE.min(total - chunk_start)))
}

async fn row_count(pool: &SqlitePool, table: &'static str) -> sqlx::Result<i64> {
    let sql = format!("SELECT count(*) AS n FROM {table}");
    let row = sqlx::query(AssertSqlSafe(sql)).fetch_one(pool).await?;
    row.try_get("n")
}
