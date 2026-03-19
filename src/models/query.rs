use std::collections::HashMap;

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
