use std::collections::{BTreeMap, HashMap};

use tracing::error;

/// `SELECT COUNT(*)` for a table. A query error logs and yields `0` rather than
/// failing the caller — used by the fail-soft metrics scrape where a single
/// broken series should never take the whole endpoint down.
///
/// `table` is expected to be a hard-coded literal at the call site, never user
/// input, so the format!-built query carries no injection surface.
pub(crate) async fn table_count(pool: &sqlx::SqlitePool, table: &str) -> u64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    match sqlx::query_scalar::<_, i64>(&sql).fetch_one(pool).await {
        Ok(count) => count.max(0) as u64,
        Err(e) => {
            error!("query: counting {} failed: {}", table, e);
            0
        }
    }
}

/// `SELECT col, COUNT(*) ... GROUP BY col` as a label→count map. Same fail-soft
/// contract as [`table_count`]. `table`/`column` are expected to be hard-coded
/// literals at the call site.
pub(crate) async fn group_count(
    pool: &sqlx::SqlitePool,
    table: &str,
    column: &str,
) -> BTreeMap<String, u64> {
    let sql = format!("SELECT {column}, COUNT(*) FROM {table} GROUP BY {column}");
    match sqlx::query_as::<_, (String, i64)>(&sql)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|(label, count)| (label, count.max(0) as u64))
            .collect(),
        Err(e) => {
            error!("query: grouping {}.{} failed: {}", table, column, e);
            BTreeMap::new()
        }
    }
}

pub(crate) fn build_filtered_query(
    base_query: &str,
    filters: &HashMap<String, Vec<String>>,
    allowed_columns: &[&str],
) -> (String, Vec<String>) {
    let mut query = String::from(base_query);
    let mut all_values: Vec<String> = Vec::new();

    if !filters.is_empty() {
        let conditions: Vec<String> = filters
            .iter()
            .filter(|(k, v)| !v.is_empty() && allowed_columns.contains(&k.as_str()))
            .map(|(column, values)| {
                let placeholders = values.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                all_values.extend(values.clone());
                format!("{} IN({})", column, placeholders)
            })
            .collect();

        if !conditions.is_empty() {
            query += &format!(" WHERE {}", conditions.join(" AND "));
        }
    }

    (query, all_values)
}
