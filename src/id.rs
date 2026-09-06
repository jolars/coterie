//! Stable, type-specific identifiers.

use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use ulid::Ulid;

/// An error returned when a typed identifier cannot be parsed.
#[derive(Debug, Eq, Error, PartialEq)]
pub(crate) enum ParseIdError {
    /// The identifier does not have the prefix required by its type.
    #[error("expected an identifier with the `{expected}` prefix")]
    InvalidPrefix { expected: &'static str },

    /// The identifier contains an invalid ULID.
    #[error("invalid ULID: {0}")]
    InvalidUlid(#[source] ulid::DecodeError),

    /// The ULID exceeds the canonical 128-bit representation.
    #[error("ULID is not in canonical 128-bit form")]
    NonCanonicalUlid,
}

macro_rules! define_id {
    ($(#[$metadata:meta])* $name:ident, $prefix:literal) => {
        $(#[$metadata])*
        #[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub(crate) struct $name(Ulid);

        impl $name {
            /// Generates a new identifier.
            #[must_use]
            pub(crate) fn generate() -> Self {
                Self(Ulid::generate())
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(input: &str) -> Result<Self, Self::Err> {
                let encoded = input.strip_prefix(concat!($prefix, "-")).ok_or(
                    ParseIdError::InvalidPrefix {
                        expected: concat!($prefix, "-"),
                    },
                )?;
                let ulid = encoded.parse::<Ulid>().map_err(ParseIdError::InvalidUlid)?;

                if !ulid.to_string().eq_ignore_ascii_case(encoded) {
                    return Err(ParseIdError::NonCanonicalUlid);
                }

                Ok(Self(ulid))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!($prefix, "-{}"), self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!(stringify!($name), "({})"), self)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn schema_id() -> std::borrow::Cow<'static, str> {
                concat!(module_path!(), "::", stringify!($name)).into()
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "pattern": concat!(
                        "^",
                        $prefix,
                        "-[0-7][0-9A-HJKMNP-TV-Z]{25}$"
                    )
                })
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value
                    .parse()
                    .map_err(<D::Error as serde::de::Error>::custom)
            }
        }
    };
}

define_id!(
    /// Identifies an orchestration run.
    RunId,
    "cr"
);
define_id!(
    /// Identifies a project attached to a run.
    ProjectId,
    "cp"
);
define_id!(
    /// Identifies an instantiated agent role.
    AgentId,
    "cg"
);
define_id!(
    /// Identifies a provider execution associated with an agent.
    SessionId,
    "cs"
);
define_id!(
    /// Identifies a durable unit of work.
    TaskId,
    "ct"
);
define_id!(
    /// Identifies the association between a task, agent, and workspace.
    AssignmentId,
    "ca"
);
define_id!(
    /// Identifies a durable message.
    MessageId,
    "cm"
);
define_id!(
    /// Identifies an idempotent mutation.
    OperationId,
    "co"
);
define_id!(
    /// Identifies an immutable event.
    EventId,
    "ce"
);

#[cfg(test)]
mod tests {
    use std::fmt::{Debug, Display};
    use std::hash::Hash;
    use std::str::FromStr;

    use serde::{Deserialize, Serialize};
    use ulid::DecodeError;

    use super::{
        AgentId, AssignmentId, EventId, MessageId, OperationId, ParseIdError,
        ProjectId, RunId, SessionId, TaskId,
    };

    const ULID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

    fn assert_common_traits<T>()
    where
        T: Copy
            + Debug
            + Display
            + Eq
            + Hash
            + Ord
            + FromStr<Err = ParseIdError>
            + Serialize
            + for<'de> Deserialize<'de>,
    {
    }

    macro_rules! assert_id_contract {
        ($id_type:ty, $prefix:literal) => {{
            assert_common_traits::<$id_type>();

            let expected = concat!($prefix, "-01ARZ3NDEKTSV4RRFFQ69G5FAV");
            let id: $id_type =
                expected.parse().expect("the fixture ID should parse");
            assert_eq!(id.to_string(), expected);
            assert_eq!(
                format!("{id:?}"),
                format!("{}({expected})", stringify!($id_type))
            );

            let lowercase = expected.to_ascii_lowercase();
            let normalized: $id_type =
                lowercase.parse().expect("lowercase ULIDs should parse");
            assert_eq!(normalized, id);
            assert_eq!(normalized.to_string(), expected);

            let json =
                serde_json::to_string(&id).expect("the ID should serialize");
            assert_eq!(json, format!("\"{expected}\""));
            assert_eq!(
                serde_json::from_str::<$id_type>(&json)
                    .expect("the ID should deserialize"),
                id
            );

            let generated = <$id_type>::generate();
            let generated_text = generated.to_string();
            assert!(generated_text.starts_with(concat!($prefix, "-")));
            assert_eq!(generated_text.len(), 29);
            assert_eq!(
                generated_text
                    .parse::<$id_type>()
                    .expect("a generated ID should parse"),
                generated
            );
        }};
    }

    #[test]
    fn each_id_type_has_the_stable_contract() {
        assert_id_contract!(RunId, "cr");
        assert_id_contract!(ProjectId, "cp");
        assert_id_contract!(AgentId, "cg");
        assert_id_contract!(SessionId, "cs");
        assert_id_contract!(TaskId, "ct");
        assert_id_contract!(AssignmentId, "ca");
        assert_id_contract!(MessageId, "cm");
        assert_id_contract!(OperationId, "co");
        assert_id_contract!(EventId, "ce");
    }

    #[test]
    fn parsing_rejects_an_id_for_another_type() {
        assert!(matches!(
            format!("ca-{ULID}").parse::<TaskId>(),
            Err(ParseIdError::InvalidPrefix { expected: "ct-" })
        ));
    }

    #[test]
    fn parsing_requires_the_exact_prefix_and_delimiter() {
        assert!(matches!(
            format!("CT-{ULID}").parse::<TaskId>(),
            Err(ParseIdError::InvalidPrefix { expected: "ct-" })
        ));
        assert!(matches!(
            format!("ct{ULID}").parse::<TaskId>(),
            Err(ParseIdError::InvalidPrefix { expected: "ct-" })
        ));
    }

    #[test]
    fn parsing_rejects_a_malformed_ulid() {
        assert!(matches!(
            "ct-short".parse::<TaskId>(),
            Err(ParseIdError::InvalidUlid(DecodeError::InvalidLength))
        ));
        assert!(matches!(
            "ct-01ARZ3NDEKTSV4RRFFQ69G5FAI".parse::<TaskId>(),
            Err(ParseIdError::InvalidUlid(DecodeError::InvalidChar))
        ));
    }

    #[test]
    fn parsing_rejects_an_overflowing_ulid() {
        assert!(matches!(
            "ct-81ARZ3NDEKTSV4RRFFQ69G5FAV".parse::<TaskId>(),
            Err(ParseIdError::NonCanonicalUlid)
        ));
    }

    #[test]
    fn identifiers_sort_by_their_ulids() {
        let earlier = format!("ct-{ULID}")
            .parse::<TaskId>()
            .expect("the earlier ID should parse");
        let later = "ct-01ARZ3NDEKTSV4RRFFQ69G5FAW"
            .parse::<TaskId>()
            .expect("the later ID should parse");

        assert!(earlier < later);
    }

    #[test]
    fn deserialization_rejects_invalid_ids() {
        let error =
            serde_json::from_str::<TaskId>("\"ca-01ARZ3NDEKTSV4RRFFQ69G5FAV\"")
                .expect_err("a mismatched prefix should fail");

        assert!(error.to_string().contains("`ct-` prefix"));
    }
}
