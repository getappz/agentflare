//! Typed errors, validation hooks and allowed-choice checks.
//!
//! Generated methods keep returning `sqlx::Result` (so existing callers compile
//! unchanged); [`CrudError`] is an additive, typed view over that error via
//! `CrudError::from(sqlx_error)`. Validation failures travel inside
//! `sqlx::Error::Encode` (the input could not be encoded for the database) and are
//! recovered as [`CrudError::Validation`].

use std::fmt;

/// A failed pre-write check (`Validate` hook or `enum_values` choice list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: Option<&'static str>,
    pub message: String,
}

impl ValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            field: None,
            message: message.into(),
        }
    }

    pub fn field(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field: Some(field),
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.field {
            Some(field) => write!(f, "invalid {field}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ValidationError {}

impl From<ValidationError> for sqlx::Error {
    fn from(e: ValidationError) -> Self {
        sqlx::Error::Encode(Box::new(e))
    }
}

/// Implemented by the caller for `{Entity}New` / `{Entity}Patch` and enabled with
/// `#[crud(validate)]`; runs before every insert/update/upsert.
pub trait Validate {
    fn validate(&self) -> Result<(), ValidationError>;
}

/// String-like values an `enum_values(...)` check can look at.
pub trait AsChoice {
    fn as_choice(&self) -> Option<&str>;
}

impl AsChoice for String {
    fn as_choice(&self) -> Option<&str> {
        Some(self)
    }
}

impl AsChoice for Option<String> {
    fn as_choice(&self) -> Option<&str> {
        self.as_deref()
    }
}

/// `NULL` (or unset) passes; any other value must be one of `choices`.
pub fn check_choice<T: AsChoice>(
    field: &'static str,
    value: &T,
    choices: &[&str],
) -> Result<(), ValidationError> {
    match value.as_choice() {
        Some(v) if !choices.contains(&v) => Err(ValidationError::field(
            field,
            format!("`{v}` is not one of: {}", choices.join(", ")),
        )),
        _ => Ok(()),
    }
}

/// Database failures classified the way callers usually want to react to them.
#[derive(Debug)]
pub enum CrudError {
    /// No row matched (`sqlx::Error::RowNotFound`).
    NotFound,
    /// Unique / primary-key violation.
    Conflict {
        table: Option<String>,
        cols: Vec<String>,
        values: Vec<String>,
    },
    /// `NOT NULL` violation.
    NotNull {
        table: Option<String>,
        col: Option<String>,
    },
    /// Foreign-key violation.
    ForeignKey { constraint: Option<String> },
    /// A `Validate` / `enum_values` check failed.
    Validation(ValidationError),
    /// Anything else.
    Other(sqlx::Error),
}

impl fmt::Display for CrudError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrudError::NotFound => f.write_str("record not found"),
            CrudError::Conflict {
                table,
                cols,
                values,
            } => {
                write!(f, "{} ", table.as_deref().unwrap_or("record"))?;
                if values.is_empty() {
                    write!(f, "with unique key ({}) already exists", cols.join(", "))
                } else {
                    write!(
                        f,
                        "with {}: {} already exists",
                        cols.join(", "),
                        values.join(", ")
                    )
                }
            }
            CrudError::NotNull { table, col } => write!(
                f,
                "cannot set field '{}' of {} to null",
                col.as_deref().unwrap_or("?"),
                table.as_deref().unwrap_or("record")
            ),
            CrudError::ForeignKey { constraint } => match constraint {
                Some(c) => write!(f, "foreign key constraint '{c}' violated"),
                None => f.write_str("foreign key constraint violated"),
            },
            CrudError::Validation(e) => e.fmt(f),
            CrudError::Other(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for CrudError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CrudError::Validation(e) => Some(e),
            CrudError::Other(e) => Some(e),
            _ => None,
        }
    }
}

/// Text after `prefix` in `msg` (SQLite: `UNIQUE constraint failed: t.a, t.b`).
fn sqlite_columns(msg: &str, prefix: &str) -> (Option<String>, Vec<String>) {
    let Some(rest) = msg.split(prefix).nth(1) else {
        return (None, Vec::new());
    };
    let mut table = None;
    let cols = rest
        .split(',')
        .map(|part| {
            let part = part.trim();
            match part.split_once('.') {
                Some((t, c)) => {
                    table = Some(t.to_string());
                    c.to_string()
                }
                None => part.to_string(),
            }
        })
        .collect();
    (table, cols)
}

/// `Key (a, b)=(x, y) already exists.` (Postgres detail) -> (["a","b"], ["x","y"]).
#[cfg_attr(not(feature = "postgres"), allow(dead_code))]
fn postgres_key_detail(detail: &str) -> (Vec<String>, Vec<String>) {
    let split = |s: &str| s.split(',').map(|p| p.trim().to_string()).collect();
    let Some(rest) = detail.strip_prefix("Key (") else {
        return (Vec::new(), Vec::new());
    };
    let Some((cols, rest)) = rest.split_once(")=(") else {
        return (Vec::new(), Vec::new());
    };
    let values = rest.split(')').next().unwrap_or("");
    (split(cols), split(values))
}

impl From<sqlx::Error> for CrudError {
    fn from(err: sqlx::Error) -> Self {
        use sqlx::error::ErrorKind;
        match err {
            sqlx::Error::RowNotFound => CrudError::NotFound,
            sqlx::Error::Encode(inner) => match inner.downcast::<ValidationError>() {
                Ok(v) => CrudError::Validation(*v),
                Err(inner) => CrudError::Other(sqlx::Error::Encode(inner)),
            },
            sqlx::Error::Database(db) => {
                let msg = db.message().to_string();
                match db.kind() {
                    ErrorKind::UniqueViolation => {
                        #[cfg(feature = "postgres")]
                        if let Some(pg) = db.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
                            let (cols, values) =
                                pg.detail().map(postgres_key_detail).unwrap_or_default();
                            return CrudError::Conflict {
                                table: pg.table().map(str::to_string),
                                cols,
                                values,
                            };
                        }
                        let (table, cols) = sqlite_columns(&msg, "UNIQUE constraint failed: ");
                        CrudError::Conflict {
                            table,
                            cols,
                            values: Vec::new(),
                        }
                    }
                    ErrorKind::NotNullViolation => {
                        let (table, cols) = sqlite_columns(&msg, "NOT NULL constraint failed: ");
                        #[cfg(feature = "postgres")]
                        if let Some(pg) = db.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
                            return CrudError::NotNull {
                                table: pg.table().map(str::to_string),
                                col: pg.column().map(str::to_string),
                            };
                        }
                        CrudError::NotNull {
                            table,
                            col: cols.into_iter().next(),
                        }
                    }
                    ErrorKind::ForeignKeyViolation => CrudError::ForeignKey {
                        constraint: db.constraint().map(str::to_string),
                    },
                    _ => CrudError::Other(sqlx::Error::Database(db)),
                }
            }
            other => CrudError::Other(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_unique_message_is_split_into_table_and_columns() {
        let (t, c) = sqlite_columns(
            "UNIQUE constraint failed: users.email, users.org",
            "UNIQUE constraint failed: ",
        );
        assert_eq!(t.as_deref(), Some("users"));
        assert_eq!(c, ["email", "org"]);
    }

    #[test]
    fn postgres_detail_is_split_into_columns_and_values() {
        let (c, v) = postgres_key_detail("Key (email, org)=(a@b.c, acme) already exists.");
        assert_eq!(c, ["email", "org"]);
        assert_eq!(v, ["a@b.c", "acme"]);
    }

    #[test]
    fn row_not_found_and_validation_are_recovered() {
        assert!(matches!(
            CrudError::from(sqlx::Error::RowNotFound),
            CrudError::NotFound
        ));
        let e: sqlx::Error = ValidationError::field("status", "bad").into();
        let CrudError::Validation(v) = CrudError::from(e) else {
            panic!("expected Validation");
        };
        assert_eq!(v.to_string(), "invalid status: bad");
    }

    #[test]
    fn choices_reject_unknown_and_accept_null() {
        assert!(check_choice("s", &"a".to_string(), &["a", "b"]).is_ok());
        assert!(check_choice("s", &"c".to_string(), &["a", "b"]).is_err());
        assert!(check_choice("s", &None::<String>, &["a"]).is_ok());
    }

    #[test]
    fn conflict_message_is_readable() {
        let e = CrudError::Conflict {
            table: Some("users".into()),
            cols: vec!["email".into()],
            values: vec!["a@b.c".into()],
        };
        assert_eq!(e.to_string(), "users with email: a@b.c already exists");
    }
}
