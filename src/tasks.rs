//! Native task graphs, claims, groups, comments, and assignments.

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{
    FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::id::TaskId;

/// A task's durable lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
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

    /// Reports whether the lifecycle permits a move to `next`.
    #[must_use]
    pub(crate) const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Open, Self::InProgress | Self::Canceled)
                | (
                    Self::InProgress,
                    Self::Open | Self::Submitted | Self::Canceled
                )
                | (Self::Submitted, Self::Open | Self::Closed | Self::Canceled)
        )
    }

    /// Applies a lifecycle action that does not create a claim.
    #[must_use]
    pub(crate) const fn transition(
        self,
        transition: TaskTransition,
    ) -> Option<Self> {
        match (self, transition) {
            (Self::InProgress | Self::Submitted, TaskTransition::Reopen) => {
                Some(Self::Open)
            }
            (Self::InProgress, TaskTransition::Submit) => Some(Self::Submitted),
            (Self::Submitted, TaskTransition::Close) => Some(Self::Closed),
            (
                Self::Open | Self::InProgress | Self::Submitted,
                TaskTransition::Cancel,
            ) => Some(Self::Canceled),
            _ => None,
        }
    }
}

/// A lifecycle action other than the atomic task-claim transition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskTransition {
    Reopen,
    Submit,
    Close,
    Cancel,
}

impl TaskTransition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reopen => "reopen",
            Self::Submit => "submit",
            Self::Close => "close",
            Self::Cancel => "cancel",
        }
    }
}

impl fmt::Display for TaskTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The current facts from which a task's readiness is derived.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskReadiness {
    pub(crate) status: TaskStatus,
    pub(crate) unresolved_dependencies: Vec<TaskId>,
    pub(crate) has_active_claim: bool,
}

impl TaskReadiness {
    /// Reports whether the task can be claimed now.
    #[must_use]
    pub(crate) fn is_ready(&self) -> bool {
        self.status == TaskStatus::Open
            && self.unresolved_dependencies.is_empty()
            && !self.has_active_claim
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

#[cfg(test)]
mod tests {
    use super::{TaskStatus, TaskTransition};

    #[test]
    fn task_lifecycle_allows_exactly_the_defined_transitions() {
        let statuses = [
            TaskStatus::Open,
            TaskStatus::InProgress,
            TaskStatus::Submitted,
            TaskStatus::Closed,
            TaskStatus::Canceled,
        ];
        let allowed = [
            (TaskStatus::Open, TaskStatus::InProgress),
            (TaskStatus::Open, TaskStatus::Canceled),
            (TaskStatus::InProgress, TaskStatus::Open),
            (TaskStatus::InProgress, TaskStatus::Submitted),
            (TaskStatus::InProgress, TaskStatus::Canceled),
            (TaskStatus::Submitted, TaskStatus::Open),
            (TaskStatus::Submitted, TaskStatus::Closed),
            (TaskStatus::Submitted, TaskStatus::Canceled),
        ];

        for from in statuses {
            for to in statuses {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "unexpected lifecycle rule for {from} -> {to}",
                );
            }
        }
    }

    #[test]
    fn lifecycle_actions_select_the_expected_next_status() {
        let statuses = [
            TaskStatus::Open,
            TaskStatus::InProgress,
            TaskStatus::Submitted,
            TaskStatus::Closed,
            TaskStatus::Canceled,
        ];
        let transitions = [
            TaskTransition::Reopen,
            TaskTransition::Submit,
            TaskTransition::Close,
            TaskTransition::Cancel,
        ];
        let allowed = [
            (
                TaskStatus::InProgress,
                TaskTransition::Reopen,
                TaskStatus::Open,
            ),
            (
                TaskStatus::Submitted,
                TaskTransition::Reopen,
                TaskStatus::Open,
            ),
            (
                TaskStatus::InProgress,
                TaskTransition::Submit,
                TaskStatus::Submitted,
            ),
            (
                TaskStatus::Submitted,
                TaskTransition::Close,
                TaskStatus::Closed,
            ),
            (
                TaskStatus::Open,
                TaskTransition::Cancel,
                TaskStatus::Canceled,
            ),
            (
                TaskStatus::InProgress,
                TaskTransition::Cancel,
                TaskStatus::Canceled,
            ),
            (
                TaskStatus::Submitted,
                TaskTransition::Cancel,
                TaskStatus::Canceled,
            ),
        ];

        for status in statuses {
            for transition in transitions {
                let expected = allowed
                    .iter()
                    .find(|(from, action, _)| {
                        *from == status && *action == transition
                    })
                    .map(|(_, _, next)| *next);
                assert_eq!(
                    status.transition(transition),
                    expected,
                    "unexpected result for {status} and {transition}",
                );
            }
        }
    }

    #[test]
    fn readiness_requires_an_unclaimed_open_task_without_dependencies() {
        let ready = super::TaskReadiness {
            status: TaskStatus::Open,
            unresolved_dependencies: Vec::new(),
            has_active_claim: false,
        };

        assert!(ready.is_ready());

        for status in [
            TaskStatus::InProgress,
            TaskStatus::Submitted,
            TaskStatus::Closed,
            TaskStatus::Canceled,
        ] {
            assert!(
                !super::TaskReadiness {
                    status,
                    ..ready.clone()
                }
                .is_ready()
            );
        }
        assert!(
            !super::TaskReadiness {
                unresolved_dependencies: vec![
                    "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV"
                        .parse()
                        .expect("the task ID should parse"),
                ],
                ..ready.clone()
            }
            .is_ready()
        );
        assert!(
            !super::TaskReadiness {
                has_active_claim: true,
                ..ready
            }
            .is_ready()
        );
    }
}
