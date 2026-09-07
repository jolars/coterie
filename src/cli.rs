//! Command parsing and human-readable or JSON presentation.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use schemars::generate::{Contract, SchemaSettings};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

use crate::id::OperationId;

/// Coterie's public command-line interface and private process entrypoints.
#[derive(Debug, Parser)]
#[command(
    name = "coterie",
    version,
    about = "Project-native orchestration for coding agents"
)]
pub(crate) struct Arguments {
    /// Emit the versioned machine-readable response.
    #[arg(long, global = true)]
    pub(crate) json: bool,

    /// Reuse this operation ID when retrying a foreground launch.
    #[arg(long)]
    pub(crate) operation_id: Option<OperationId>,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

/// One operator- or agent-facing action.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Inspect the active run, agents, and tasks.
    Status,
    /// Report the authenticated caller's identity.
    Whoami,
    /// Reconstruct the caller's current orchestration context.
    Prime,
    /// Create, inspect, or close durable tasks.
    Task(TaskArguments),
    /// Launch one configured role for a ready task.
    Spawn(SpawnArguments),
    /// Finish the caller's active assignment.
    Finish(FinishArguments),
    /// Send a durable message to another agent.
    Send(SendArguments),
    /// Read durable messages addressed to the caller.
    Inbox(InboxArguments),
    /// Read an agent's provider transcript.
    Logs(LogsArguments),
    /// Read the run's typed event stream.
    Events(EventsArguments),
    /// Stop the active run safely.
    Stop(MutationArguments),

    #[command(name = "__supervisor", hide = true)]
    Supervisor(PrivateSupervisorArguments),
    #[command(name = "__supervisor-connect", hide = true)]
    SupervisorConnect,
    #[command(name = "__supervisor-shutdown", hide = true)]
    SupervisorShutdown,
}

/// Task commands.
#[derive(Debug, Args)]
pub(crate) struct TaskArguments {
    #[command(subcommand)]
    pub(crate) command: TaskCommand,
}

/// One task-graph action.
#[derive(Debug, Subcommand)]
pub(crate) enum TaskCommand {
    /// Create a task in an attached project.
    Create(TaskCreateArguments),
    /// List tasks that can be claimed now.
    Ready,
    /// Close a submitted task after validation.
    Close(TaskCloseArguments),
}

/// Common options for idempotent mutations.
#[derive(Debug, Args)]
pub(crate) struct MutationArguments {
    /// Reuse this operation ID when retrying an uncertain mutation.
    #[arg(long)]
    pub(crate) operation_id: Option<OperationId>,
}

/// Inputs for `task create`.
#[derive(Debug, Args)]
pub(crate) struct TaskCreateArguments {
    /// A concise task title.
    pub(crate) title: String,
    /// A longer task description. Defaults to the title.
    #[arg(long)]
    pub(crate) description: Option<String>,
    /// The attached target project's alias.
    #[arg(long, default_value = "primary")]
    pub(crate) project: String,
    /// An optional task-group name.
    #[arg(long)]
    pub(crate) group: Option<String>,
    /// A task that must close before this task becomes ready.
    #[arg(long = "after")]
    pub(crate) dependencies: Vec<crate::id::TaskId>,
    #[command(flatten)]
    pub(crate) mutation: MutationArguments,
}

/// Inputs for `task close`.
#[derive(Debug, Args)]
pub(crate) struct TaskCloseArguments {
    pub(crate) task_id: crate::id::TaskId,
    /// A concise account of the validation performed.
    #[arg(long)]
    pub(crate) summary: String,
    #[command(flatten)]
    pub(crate) mutation: MutationArguments,
}

/// Inputs for `spawn`.
#[derive(Debug, Args)]
pub(crate) struct SpawnArguments {
    /// A role declared by the active archetype.
    pub(crate) role: String,
    /// The ready task assigned to the new agent.
    #[arg(long)]
    pub(crate) task: crate::id::TaskId,
    #[command(flatten)]
    pub(crate) mutation: MutationArguments,
}

/// Inputs for `finish`.
#[derive(Debug, Args)]
pub(crate) struct FinishArguments {
    #[arg(long, value_enum)]
    pub(crate) status: FinishStatus,
    #[arg(long)]
    pub(crate) summary: String,
    #[command(flatten)]
    pub(crate) mutation: MutationArguments,
}

/// The assignment outcome reported by an agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum FinishStatus {
    Completed,
    Failed,
}

/// Inputs for `send`.
#[derive(Debug, Args)]
pub(crate) struct SendArguments {
    /// An agent ID or run-local agent name.
    pub(crate) recipient: String,
    /// Message text, passed as data without shell interpretation.
    pub(crate) message: String,
    #[command(flatten)]
    pub(crate) mutation: MutationArguments,
}

/// Inputs for `inbox`.
#[derive(Debug, Args)]
pub(crate) struct InboxArguments {
    /// Return only messages after this recipient-local sequence.
    #[arg(long, default_value_t = 0)]
    pub(crate) after: u64,
}

/// Inputs for `logs`.
#[derive(Debug, Args)]
pub(crate) struct LogsArguments {
    /// An agent ID or run-local agent name.
    pub(crate) agent: String,
}

/// Inputs for `events`.
#[derive(Debug, Args)]
pub(crate) struct EventsArguments {
    /// Return only events after this run-local sequence.
    #[arg(long, default_value_t = 0)]
    pub(crate) after: u64,
    /// Bound the number of returned events.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub(crate) limit: u16,
}

/// The private child-supervisor invocation.
#[derive(Debug, Args)]
pub(crate) struct PrivateSupervisorArguments {
    pub(crate) run_id: crate::id::RunId,
    pub(crate) project_id: crate::id::ProjectId,
    pub(crate) project_path: PathBuf,
}

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
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "verified by golden tests")
    )]
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
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "verified by golden tests")
    )]
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
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "all stable error codes remain in the v1 contract"
    )
)]
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
    #[cfg_attr(not(test), allow(dead_code, reason = "used by golden tests"))]
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
#[cfg_attr(not(test), allow(dead_code, reason = "verified by golden tests"))]
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
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by schema golden tests")
    )]
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

#[cfg_attr(not(test), allow(dead_code, reason = "used by schema golden tests"))]
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

/// Writes a human-readable successful response to standard output's writer.
pub(crate) fn render_human_success<T, Stdout, Stderr>(
    stdout: &mut Stdout,
    _stderr: &mut Stderr,
    data: &T,
) -> Result<ExitCategory, RenderError>
where
    T: Serialize + ?Sized,
    Stdout: Write,
    Stderr: Write,
{
    serde_json::to_writer_pretty(&mut *stdout, data)?;
    stdout.write_all(b"\n")?;
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

/// Writes a human-readable diagnostic to standard error's writer.
pub(crate) fn render_human_error<Stdout, Stderr>(
    _stdout: &mut Stdout,
    stderr: &mut Stderr,
    diagnostic: &Diagnostic,
) -> Result<ExitCategory, RenderError>
where
    Stdout: Write,
    Stderr: Write,
{
    writeln!(stderr, "coterie: {}", diagnostic.message)?;
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
