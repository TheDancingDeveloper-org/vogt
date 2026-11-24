//! Shared query shapes for the SQLite stores: the `FromRow` trait that names an
//! entity's table and columns, and the `Filter` builder for dynamic WHERE
//! clauses.
//!
//! Nothing here interpolates caller text into SQL. Every fragment is a
//! `&'static str` (so only literals compile), and every value is bound.

use std::borrow::Cow;

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

/// Values per `IN (...)` statement; far below SQLite's bound-variable limit.
const IN_CHUNK: usize = 500;

/// `IN (?, ?, ...)` for `n` bound values. Only placeholders are generated.
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Select every row whose `column` is in `values`, in chunked queries with every
/// value bound. `column` is a `&'static str`, so only literals compile.
pub(super) fn select_in<T: FromRow>(
    conn: &Connection,
    column: &'static str,
    values: &[String],
) -> Result<Vec<T>, VogtError> {
    let sql_err = |err: rusqlite::Error| VogtError::from_sqlite(&err);
    let mut out = Vec::new();
    for chunk in values.chunks(IN_CHUNK) {
        let sql = format!(
            "SELECT {} FROM {} WHERE {column} IN ({})",
            T::COLUMNS,
            T::TABLE,
            placeholders(chunk.len())
        );
        let mut statement = conn.prepare(&sql).map_err(sql_err)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(chunk.iter()), T::from_row)
            .map_err(sql_err)?;
        for row in rows {
            out.push(row.map_err(sql_err)?);
        }
    }
    Ok(out)
}

/// `(id, ref, title, state)` of work items whose `column` is in `values`.
pub(super) fn select_work_briefs(
    conn: &Connection,
    column: &'static str,
    values: &[String],
) -> Result<Vec<(String, String, String, String)>, VogtError> {
    let sql_err = |err: rusqlite::Error| VogtError::from_sqlite(&err);
    let mut out = Vec::new();
    for chunk in values.chunks(IN_CHUNK) {
        let sql = format!(
            "SELECT id, ref, title, state FROM work_items WHERE {column} IN ({})",
            placeholders(chunk.len())
        );
        let mut statement = conn.prepare(&sql).map_err(sql_err)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(sql_err)?;
        for row in rows {
            out.push(row.map_err(sql_err)?);
        }
    }
    Ok(out)
}

/// A list of AND-ed conditions with their bound values. A condition is a
/// `&'static str` holding one `?` per value; the values are never part of the
/// SQL text.
#[derive(Default)]
pub(super) struct Filter {
    clauses: Vec<Cow<'static, str>>,
    values: Vec<Box<dyn ToSql>>,
}

impl Filter {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Add a condition with one placeholder, such as `"kind = ?"`.
    pub(super) fn and(&mut self, clause: &'static str, value: impl ToSql + 'static) {
        self.clauses.push(Cow::Borrowed(clause));
        self.values.push(Box::new(value));
    }

    /// Add a condition when the value is present.
    pub(super) fn and_some<V: ToSql + 'static>(&mut self, clause: &'static str, value: Option<V>) {
        if let Some(value) = value {
            self.and(clause, value);
        }
    }

    /// Add a condition with several placeholders, one value for each `?`.
    pub(super) fn and_values(&mut self, clause: &'static str, values: Vec<Box<dyn ToSql>>) {
        self.clauses.push(Cow::Borrowed(clause));
        self.values.extend(values);
    }

    /// `column IN (?, ?, ...)`, binding each element. `column` is a literal;
    /// the list text is only question marks. An empty list adds nothing.
    pub(super) fn and_in<V: ToSql + 'static>(
        &mut self,
        column: &'static str,
        values: impl IntoIterator<Item = V>,
    ) {
        self.in_list(column, " IN ", values);
    }

    /// `column NOT IN (?, ?, ...)`, as `and_in`.
    pub(super) fn and_not_in<V: ToSql + 'static>(
        &mut self,
        column: &'static str,
        values: impl IntoIterator<Item = V>,
    ) {
        self.in_list(column, " NOT IN ", values);
    }

    fn in_list<V: ToSql + 'static>(
        &mut self,
        column: &'static str,
        operator: &'static str,
        values: impl IntoIterator<Item = V>,
    ) {
        let values: Vec<V> = values.into_iter().collect();
        if values.is_empty() {
            return;
        }
        let marks = vec!["?"; values.len()].join(", ");
        self.clauses
            .push(Cow::Owned(format!("{column}{operator}({marks})")));
        self.values
            .extend(values.into_iter().map(|v| Box::new(v) as Box<dyn ToSql>));
    }

    /// Add a condition that has no placeholder, such as `"stopped_at IS NULL"`.
    pub(super) fn and_fixed(&mut self, clause: &'static str) {
        self.clauses.push(Cow::Borrowed(clause));
    }

    /// `WHERE a AND b ` (with a trailing space), or an empty string.
    pub(super) fn where_sql(&self) -> String {
        if self.clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {} ", self.clauses.join(" AND "))
        }
    }

    /// `WHERE a AND b` without a trailing space (or an empty string) and the
    /// bound values, for queries that splice the text into a larger statement.
    pub(super) fn into_parts(self) -> (String, Vec<Box<dyn ToSql>>) {
        let sql = self.where_sql().trim_end().to_string();
        (sql, self.values)
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

    #[test]
    fn in_lists_bind_each_element() {
        let mut filter = Filter::new();
        filter.and_in("k", Vec::<String>::new());
        filter.and_in("w.kind", vec!["a'); --".to_string(), "b".to_string()]);
        filter.and_not_in("w.state", ["x", "y", "z"].map(String::from));
        let (sql, values) = filter.into_parts();
        assert_eq!(sql, "WHERE w.kind IN (?, ?) AND w.state NOT IN (?, ?, ?)");
        assert_eq!(values.len(), 5);
    }
}
