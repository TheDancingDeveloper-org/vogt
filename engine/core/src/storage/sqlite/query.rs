//! Shared query shapes for the SQLite stores: the `FromRow` trait that names an
//! entity's table and columns, and the `Filter` builder for dynamic WHERE
//! clauses.
//!
//! Nothing here interpolates caller text into SQL. Every fragment is a
//! `&'static str` (so only literals compile), and every value is bound.

use rusqlite::types::ToSql;
use rusqlite::{Connection, OptionalExtension, Row};

use crate::errors::VogtError;

/// A stored entity: the table it lives in, the columns its mapper reads, and
/// the mapper. Queries build their select list from `COLUMNS`, so a schema
/// change shows up in one place instead of hiding behind `SELECT *`.
pub(super) trait FromRow: Sized {
    const TABLE: &'static str;
    const COLUMNS: &'static str;
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self>;
}

/// `tail` is a `&'static str` on purpose: it holds only constant SQL, and user
/// text travels as bound parameters.
fn select_sql<T: FromRow>(tail: &'static str) -> String {
    format!("SELECT {} FROM {} {tail}", T::COLUMNS, T::TABLE)
}

pub(super) fn select_one<T: FromRow>(
    conn: &Connection,
    tail: &'static str,
    params: impl rusqlite::Params,
) -> Result<Option<T>, VogtError> {
    conn.query_row(&select_sql::<T>(tail), params, T::from_row)
        .optional()
        .map_err(|err| VogtError::from_sqlite(&err))
}

pub(super) fn select_many<T: FromRow>(
    conn: &Connection,
    tail: &'static str,
    params: impl rusqlite::Params,
) -> Result<Vec<T>, VogtError> {
    let sql_err = |err: rusqlite::Error| VogtError::from_sqlite(&err);
    let mut statement = conn.prepare(&select_sql::<T>(tail)).map_err(sql_err)?;
    let rows = statement.query_map(params, T::from_row).map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

/// Select every row matching `filter`. `tail` (ORDER BY, LIMIT) is static SQL;
/// its placeholders are filled from `tail_params`, after the filter's values.
pub(super) fn select_filtered<T: FromRow>(
    conn: &Connection,
    filter: Filter,
    tail: &'static str,
    tail_params: Vec<Box<dyn ToSql>>,
) -> Result<Vec<T>, VogtError> {
    let sql = format!(
        "SELECT {} FROM {} {}{tail}",
        T::COLUMNS,
        T::TABLE,
        filter.where_sql()
    );
    let values = filter.params(tail_params);
    let sql_err = |err: rusqlite::Error| VogtError::from_sqlite(&err);
    let mut statement = conn.prepare(&sql).map_err(sql_err)?;
    let rows = statement
        .query_map(
            rusqlite::params_from_iter(values.iter().map(|value| value.as_ref())),
            T::from_row,
        )
        .map_err(sql_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(sql_err)
}

/// A list of AND-ed conditions with their bound values. A condition is a
/// `&'static str` holding one `?` per value; the values are never part of the
/// SQL text.
#[derive(Default)]
pub(super) struct Filter {
    clauses: Vec<&'static str>,
    values: Vec<Box<dyn ToSql>>,
}

impl Filter {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Add a condition with one placeholder, such as `"kind = ?"`.
    pub(super) fn and(&mut self, clause: &'static str, value: impl ToSql + 'static) {
        self.clauses.push(clause);
        self.values.push(Box::new(value));
    }

    /// Add a condition when the value is present.
    pub(super) fn and_some<V: ToSql + 'static>(&mut self, clause: &'static str, value: Option<V>) {
        if let Some(value) = value {
            self.and(clause, value);
        }
    }

    /// Add a condition that has no placeholder, such as `"stopped_at IS NULL"`.
    pub(super) fn and_fixed(&mut self, clause: &'static str) {
        self.clauses.push(clause);
    }

    /// `WHERE a AND b ` (with a trailing space), or an empty string.
    pub(super) fn where_sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {} ", self.clauses.join(" AND "))
        }
    }

    /// The bound values, in clause order, followed by `tail`.
    pub(super) fn params(self, tail: Vec<Box<dyn ToSql>>) -> Vec<Box<dyn ToSql>> {
        let mut values = self.values;
        values.extend(tail);
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_filter_has_no_where() {
        assert_eq!(Filter::new().where_sql(), "");
    }

    #[test]
    fn clauses_join_with_and_and_values_stay_out_of_the_sql() {
        let mut filter = Filter::new();
        filter.and("kind = ?", "x'; DROP TABLE t; --".to_string());
        filter.and_some::<String>("project_id = ?", None);
        filter.and("promoted = ?", 1_i64);
        assert_eq!(filter.where_sql(), "WHERE kind = ? AND promoted = ? ");
        assert_eq!(filter.params(vec![Box::new(5_i64)]).len(), 3);
    }
}
