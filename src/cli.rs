//! Command parsing and human-readable or JSON presentation.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

use schemars::generate::{Contract, SchemaSettings};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::id::OperationId;

/// The schema version emitted by the programmatic CLI interface.
const OUTPUT_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SchemaVersion;

impl Serialize for SchemaVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u16(OUTPUT_SCHEMA_VERSION)
    }
}

impl JsonSchema for SchemaVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CliSchemaVersion".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "integer",
            "const": OUTPUT_SCHEMA_VERSION
        })
    }
}

/// A stable process-exit category for CLI invocations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum ExitCategory {
    /// The command completed successfully.
    Success = 0,
    /// Coterie could not complete the command because of an internal failure.
    Internal = 1,
    /// The command line or another caller-supplied value was invalid.
    Usage = 2,
    /// Configuration was invalid or incompatible with the active run.
    Configuration = 3,
    /// A requested resource does not exist.
    NotFound = 4,
    /// Current state does not satisfy the operation's preconditions.
    Conflict = 5,
    /// Authentication or authorization failed.
    Permission = 6,
    /// A required service or provider is temporarily unavailable.
    Unavailable = 7,
}

impl ExitCategory {
    /// Every stable category in numeric order.
    pub(crate) const ALL: [Self; 8] = [
        Self::Success,
        Self::Internal,
        Self::Usage,
        Self::Configuration,
        Self::NotFound,
        Self::Conflict,
        Self::Permission,
        Self::Unavailable,
    ];

    /// Returns the stable process exit code for this category.
    #[must_use]
    pub(crate) const fn code(self) -> u8 {
        self as u8
    }

    /// Returns the stable machine-readable name for this category.
    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Internal => "internal",
            Self::Usage => "usage",
            Self::Configuration => "configuration",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Permission => "permission",
            Self::Unavailable => "unavailable",
        }
    }
}

impl From<ExitCategory> for std::process::ExitCode {
    fn from(category: ExitCategory) -> Self {
        Self::from(category.code())
    }
}

/// A stable, machine-readable CLI error code.
#[derive(Clone, Copy, Debug, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCode {
    /// A command-line argument or request value is invalid.
    InvalidArgument,
    /// Configuration is invalid or incompatible with the requested action.
    InvalidConfiguration,
    /// A requested resource does not exist.
    NotFound,
    /// Current state does not satisfy the operation's preconditions.
    Conflict,
    /// The caller did not present valid credentials.
    Unauthenticated,
    /// The authenticated caller lacks the required capability.
    PermissionDenied,
    /// A required local service or provider cannot currently respond.
    Unavailable,
    /// Durable state violates an invariant or cannot be decoded.
    CorruptState,
    /// Coterie encountered an otherwise unclassified internal failure.
    Internal,
}

impl ErrorCode {
    /// Returns the process-exit category associated with this error.
    #[must_use]
    pub(crate) const fn exit_category(self) -> ExitCategory {
        match self {
            Self::InvalidArgument => ExitCategory::Usage,
            Self::InvalidConfiguration => ExitCategory::Configuration,
            Self::NotFound => ExitCategory::NotFound,
            Self::Conflict => ExitCategory::Conflict,
            Self::Unauthenticated | Self::PermissionDenied => {
                ExitCategory::Permission
            }
            Self::Unavailable => ExitCategory::Unavailable,
            Self::CorruptState | Self::Internal => ExitCategory::Internal,
        }
    }
}

/// A diagnostic suitable for human or structured presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Diagnostic {
    code: ErrorCode,
    message: String,
    details: BTreeMap<String, Value>,
    operation_id: Option<OperationId>,
}

impl Diagnostic {
    /// Creates a diagnostic without structured details.
    pub(crate) fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: BTreeMap::new(),
            operation_id: None,
        }
    }

    /// Adds one machine-readable detail to the diagnostic.
    #[must_use]
    pub(crate) fn with_detail(
        mut self,
        key: impl Into<String>,
        value: impl Into<Value>,
    ) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    /// Associates the diagnostic with an allocated mutation operation.
    #[must_use]
    pub(crate) fn for_operation(mut self, operation_id: OperationId) -> Self {
        self.operation_id = Some(operation_id);
        self
    }

    /// Returns the process-exit category for this diagnostic.
    #[must_use]
    pub(crate) const fn exit_category(&self) -> ExitCategory {
        self.code.exit_category()
    }
}

#[derive(JsonSchema, Serialize)]
#[schemars(title = "Coterie CLI success response v1", deny_unknown_fields)]
struct SuccessEnvelope<'a, T: ?Sized> {
    schema_version: SchemaVersion,
    data: &'a T,
}

#[derive(JsonSchema, Serialize)]
#[schemars(
    title = "Coterie CLI mutation success response v1",
    deny_unknown_fields
)]
struct MutationSuccessEnvelope<'a, T: ?Sized> {
    schema_version: SchemaVersion,
    operation_id: OperationId,
    data: &'a T,
}

#[derive(JsonSchema, Serialize)]
#[schemars(title = "Coterie CLI error response v1", deny_unknown_fields)]
struct ErrorEnvelope<'a> {
    schema_version: SchemaVersion,
    error: ErrorBody<'a>,
}

#[derive(JsonSchema, Serialize)]
#[schemars(
    title = "Coterie CLI mutation error response v1",
    deny_unknown_fields
)]
struct MutationErrorEnvelope<'a> {
    schema_version: SchemaVersion,
    operation_id: OperationId,
    error: ErrorBody<'a>,
}

#[derive(JsonSchema, Serialize)]
#[schemars(deny_unknown_fields)]
struct ErrorBody<'a> {
    code: ErrorCode,
    message: &'a str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    details: &'a BTreeMap<String, Value>,
}

/// A generated schema for one versioned CLI response shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputSchema {
    /// A successful read-only response.
    Success,
    /// A successful response from a mutating command.
    MutationSuccess,
    /// A failed read-only response.
    Error,
    /// A failed mutating command with an allocated operation ID.
    MutationError,
}

impl OutputSchema {
    /// Generates the JSON Schema from the response's typed representation.
    #[must_use]
    pub(crate) fn generate(self) -> Schema {
        match self {
            Self::Success => {
                generated_schema_for::<SuccessEnvelope<'static, Value>>()
            }
            Self::MutationSuccess => generated_schema_for::<
                MutationSuccessEnvelope<'static, Value>,
            >(),
            Self::Error => generated_schema_for::<ErrorEnvelope<'static>>(),
            Self::MutationError => {
                generated_schema_for::<MutationErrorEnvelope<'static>>()
            }
        }
    }
}

fn generated_schema_for<T: JsonSchema + ?Sized>() -> Schema {
    SchemaSettings::draft2020_12()
        .with(|settings| settings.contract = Contract::Serialize)
        .into_generator()
        .into_root_schema_for::<T>()
}

impl fmt::Display for OutputSchema {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Success => "success",
            Self::MutationSuccess => "mutation-success",
            Self::Error => "error",
            Self::MutationError => "mutation-error",
        };
        formatter.write_str(name)
    }
}

/// An error encountered while serializing or writing CLI output.
#[derive(Debug, Error)]
pub(crate) enum RenderError {
    /// A response could not be represented as JSON.
    #[error("could not serialize CLI output: {0}")]
    Serialize(#[from] serde_json::Error),
    /// A response could not be written to its designated stream.
    #[error("could not write CLI output: {0}")]
    Write(#[from] io::Error),
}

/// Writes a successful read-only response to standard output's writer.
pub(crate) fn render_json_success<T, Stdout, Stderr>(
    stdout: &mut Stdout,
    _stderr: &mut Stderr,
    data: &T,
) -> Result<ExitCategory, RenderError>
where
    T: Serialize + ?Sized,
    Stdout: Write,
    Stderr: Write,
{
    write_json_line(
        stdout,
        &SuccessEnvelope {
            schema_version: SchemaVersion,
            data,
        },
    )?;
    Ok(ExitCategory::Success)
}

/// Writes a successful mutation response with its operation ID to standard output.
pub(crate) fn render_json_mutation_success<T, Stdout, Stderr>(
    stdout: &mut Stdout,
    _stderr: &mut Stderr,
    operation_id: OperationId,
    data: &T,
) -> Result<ExitCategory, RenderError>
where
    T: Serialize + ?Sized,
    Stdout: Write,
    Stderr: Write,
{
    write_json_line(
        stdout,
        &MutationSuccessEnvelope {
            schema_version: SchemaVersion,
            operation_id,
            data,
        },
    )?;
    Ok(ExitCategory::Success)
}

/// Writes a structured diagnostic to standard error's writer.
pub(crate) fn render_json_error<Stdout, Stderr>(
    _stdout: &mut Stdout,
    stderr: &mut Stderr,
    diagnostic: &Diagnostic,
) -> Result<ExitCategory, RenderError>
where
    Stdout: Write,
    Stderr: Write,
{
    let error = ErrorBody {
        code: diagnostic.code,
        message: &diagnostic.message,
        details: &diagnostic.details,
    };
    if let Some(operation_id) = diagnostic.operation_id {
        write_json_line(
            stderr,
            &MutationErrorEnvelope {
                schema_version: SchemaVersion,
                operation_id,
                error,
            },
        )?;
    } else {
        write_json_line(
            stderr,
            &ErrorEnvelope {
                schema_version: SchemaVersion,
                error,
            },
        )?;
    }
    Ok(diagnostic.exit_category())
}

fn write_json_line(
    writer: &mut impl Write,
    value: &impl Serialize,
) -> Result<(), RenderError> {
    let mut encoded = serde_json::to_vec(value)?;
    encoded.push(b'\n');
    writer.write_all(&encoded)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        Diagnostic, ErrorCode, ExitCategory, OutputSchema, render_json_error,
        render_json_mutation_success, render_json_success,
    };

    const OPERATION_ID: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FAV";

    #[test]
    fn query_success_matches_the_v1_golden_contract() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit = render_json_success(
            &mut stdout,
            &mut stderr,
            &json!({
                "run_id": "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "status": "active",
            }),
        )
        .expect("the response should render");

        assert_eq!(exit, ExitCategory::Success);
        assert_eq!(
            String::from_utf8(stdout).expect("JSON should be UTF-8"),
            include_str!("../tests/golden/cli-success-v1.json")
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn mutation_success_requires_an_operation_id_and_matches_the_v1_contract() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit = render_json_mutation_success(
            &mut stdout,
            &mut stderr,
            OPERATION_ID.parse().expect("the operation ID should parse"),
            &json!({"task_id": "ct-01ARZ3NDEKTSV4RRFFQ69G5FAV"}),
        )
        .expect("the response should render");

        assert_eq!(exit, ExitCategory::Success);
        assert_eq!(
            String::from_utf8(stdout).expect("JSON should be UTF-8"),
            include_str!("../tests/golden/cli-mutation-success-v1.json")
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn diagnostic_matches_the_v1_golden_contract_and_only_uses_stderr() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let diagnostic =
            Diagnostic::new(ErrorCode::InvalidArgument, "task ID is invalid")
                .with_detail("argument", "task_id");

        let exit = render_json_error(&mut stdout, &mut stderr, &diagnostic)
            .expect("the diagnostic should render");

        assert_eq!(exit, ExitCategory::Usage);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).expect("JSON should be UTF-8"),
            include_str!("../tests/golden/cli-error-v1.json")
        );
    }

    #[test]
    fn mutation_diagnostic_returns_its_operation_id() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let diagnostic = Diagnostic::new(
            ErrorCode::Conflict,
            "the task was modified concurrently",
        )
        .for_operation(
            OPERATION_ID.parse().expect("the operation ID should parse"),
        );

        let exit = render_json_error(&mut stdout, &mut stderr, &diagnostic)
            .expect("the diagnostic should render");

        assert_eq!(exit, ExitCategory::Conflict);
        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).expect("JSON should be UTF-8"),
            include_str!("../tests/golden/cli-mutation-error-v1.json")
        );
    }

    #[test]
    fn error_codes_map_to_stable_exit_categories() {
        let cases = [
            (ErrorCode::InvalidArgument, ExitCategory::Usage, 2),
            (
                ErrorCode::InvalidConfiguration,
                ExitCategory::Configuration,
                3,
            ),
            (ErrorCode::NotFound, ExitCategory::NotFound, 4),
            (ErrorCode::Conflict, ExitCategory::Conflict, 5),
            (ErrorCode::Unauthenticated, ExitCategory::Permission, 6),
            (ErrorCode::PermissionDenied, ExitCategory::Permission, 6),
            (ErrorCode::Unavailable, ExitCategory::Unavailable, 7),
            (ErrorCode::CorruptState, ExitCategory::Internal, 1),
            (ErrorCode::Internal, ExitCategory::Internal, 1),
        ];

        assert_eq!(ExitCategory::Success.code(), 0);
        for (error, expected_category, expected_code) in cases {
            let category = error.exit_category();
            assert_eq!(category, expected_category);
            assert_eq!(category.code(), expected_code);
        }
    }

    #[test]
    fn exit_categories_match_the_v1_golden_contract() {
        let contract = ExitCategory::ALL.map(|category| {
            json!({
                "category": category.name(),
                "code": category.code(),
            })
        });
        let mut generated = serde_json::to_string_pretty(&contract)
            .expect("the exit-code contract should serialize");
        generated.push('\n');

        assert_eq!(
            generated,
            include_str!("../tests/golden/cli-exit-codes-v1.json")
        );
    }

    #[test]
    fn generated_response_schemas_match_the_v1_golden_contracts() {
        let cases = [
            (
                OutputSchema::Success,
                include_str!("../schemas/cli-success-v1.schema.json"),
            ),
            (
                OutputSchema::MutationSuccess,
                include_str!("../schemas/cli-mutation-success-v1.schema.json"),
            ),
            (
                OutputSchema::Error,
                include_str!("../schemas/cli-error-v1.schema.json"),
            ),
            (
                OutputSchema::MutationError,
                include_str!("../schemas/cli-mutation-error-v1.schema.json"),
            ),
        ];

        let generated = cases.map(|(kind, expected)| {
            let mut schema = serde_json::to_string_pretty(&kind.generate())
                .expect("the generated schema should serialize");
            schema.push('\n');
            (kind, expected, schema)
        });

        for (kind, expected, generated) in generated {
            assert_eq!(generated, expected, "{kind} schema changed");
        }
    }
}
