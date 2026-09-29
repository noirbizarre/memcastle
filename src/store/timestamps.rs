//! Rewriting timestamps stored by earlier versions into the canonical form.
//!
//! Only for `crate::migrate`. Every timestamp write now goes through
//! [`super::stored`]; this is the one-off pass that brings rows written
//! before that (which used `DateTime::to_rfc3339`, with variable precision
//! and a `+00:00` suffix) up to the same form, so lexical comparison of the
//! `option<string>` columns is valid across old and new rows alike. See
//! `docs/adr/005-timestamp-representation.md`.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::error::{Error, Result};

use super::SurrealStore;

/// Every `option<string>` timestamp column earlier versions wrote. The
/// `datetime` columns need no rewrite: the database owns their format.
const OPTIONAL_TIMESTAMP_COLUMNS: &[(&str, &str)] = &[
    ("drawer", "valid_to"),
    ("relates_to", "valid_to"),
    ("job", "started_at"),
    ("job", "completed_at"),
    ("job", "lease_expires_at"),
];

impl SurrealStore {
    /// Rewrite every stored optional timestamp that is not already in the
    /// canonical form, returning how many rows changed. Rows already
    /// canonical are not written, so re-running changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StoreMalformed`] naming the record if a stored value
    /// is not a timestamp at all — corruption a migration must not paper
    /// over — or a store error.
    pub(crate) async fn canonicalize_optional_timestamps(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Row {
            id: String,
            value: String,
        }

        let mut rewritten = 0;
        for (table, field) in OPTIONAL_TIMESTAMP_COLUMNS {
            // Table and field come from the constant above, never from a
            // caller, so formatting them into the statement is not an
            // injection path (SurrealQL cannot bind identifiers).
            let mut response = self
                .db
                .query(format!(
                    "SELECT record::id(id) AS id, {field} AS value FROM {table} WHERE {field}"
                ))
                .await?;
            let rows: Vec<Row> = super::take_rows(&mut response, 0)?;
            for row in rows {
                let parsed = DateTime::parse_from_rfc3339(&row.value).map_err(|source| {
                    Error::store_malformed(format!(
                        "{table}:{} has a {field} that is not a timestamp (`{}`): {source}",
                        row.id, row.value
                    ))
                })?;
                let canonical = super::stored(parsed.with_timezone(&Utc));
                if canonical == row.value {
                    continue;
                }
                self.db
                    .query(format!(
                        "UPDATE type::record('{table}', $id) SET {field} = $value"
                    ))
                    .bind(("id", row.id))
                    .bind(("value", canonical))
                    .await?
                    .check()?;
                rewritten += 1;
            }
        }
        Ok(rewritten)
    }

    /// Write raw SurrealQL, so a migration test can seed a row exactly as an
    /// older version wrote it — something the current write paths, which
    /// only emit the canonical form, can no longer do.
    #[cfg(test)]
    pub(crate) async fn execute_for_tests(&self, sql: &str) {
        self.db
            .query(sql.to_string())
            .await
            .expect("query")
            .check()
            .expect("statement accepted");
    }
}
