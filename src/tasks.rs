//! Native task graphs, claims, groups, comments, and assignments.

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{
    FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef,
};
use thiserror::Error;

/// A task's durable lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TaskStatus {
    Open,
    InProgress,
    Submitted,
    Closed,
    Canceled,
}

impl TaskStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Submitted => "submitted",
            Self::Closed => "closed",
            Self::Canceled => "canceled",
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A task status not recognized by this schema version.
#[derive(Debug, Error)]
#[error("unknown task status `{0}`")]
pub(crate) struct ParseTaskStatusError(String);

impl FromStr for TaskStatus {
    type Err = ParseTaskStatusError;

    fn from_str(status: &str) -> Result<Self, Self::Err> {
        match status {
            "open" => Ok(Self::Open),
            "in_progress" => Ok(Self::InProgress),
            "submitted" => Ok(Self::Submitted),
            "closed" => Ok(Self::Closed),
            "canceled" => Ok(Self::Canceled),
            _ => Err(ParseTaskStatusError(status.to_owned())),
        }
    }
}

impl ToSql for TaskStatus {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Text(
            self.as_str().as_bytes(),
        )))
    }
}

impl FromSql for TaskStatus {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}
