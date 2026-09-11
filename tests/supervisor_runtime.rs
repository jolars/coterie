use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use git2::{Repository, Signature, StatusOptions};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde_json::Value;

#[path = "supervisor_runtime/finish.rs"]
mod finish;

#[path = "supervisor_runtime/closure_override.rs"]
mod closure_override;

const RUN_ID: &str = "cr-01ARZ3NDEKTSV4RRFFQ69G5FAV";
const PROJECT_ID: &str = "cp-01ARZ3NDEKTSV4RRFFQ69G5FAW";
const FAKE_CODEX: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'codex-cli 0.151.0\n'
  exit 0
fi
if [ "$1" = "--help" ]; then
  printf 'Usage: codex [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --ask-for-approval <APPROVAL_POLICY>\n'
  exit 0
fi
if [ "$1" = "exec" ] && [ "$2" = "--help" ]; then
  printf 'Usage: codex exec [OPTIONS] [PROMPT]\n  --config <key=value>\n  --cd <DIR>\n  --sandbox <SANDBOX_MODE>\n  --ask-for-approval <APPROVAL_POLICY>\n  --json\n'
  exit 0
fi
if [ "${COTERIE_FAKE_MODE-}" = "contract" ]; then
  {
    printf 'cwd=%s\0' "$PWD"
    for argument in "$@"; do
      printf 'arg=%s\0' "$argument"
    done
    for variable in COTERIE_PROJECT_ROOT COTERIE_PROJECT_ID COTERIE_PRIMARY_PROJECT_ROOT COTERIE_RUN_ID COTERIE_AGENT_ID COTERIE_SESSION_ID COTERIE_ROLE COTERIE_SOCKET COTERIE_TOKEN COTERIE_BIN; do
      eval "value=\${$variable}"
      printf 'env:%s=%s\0' "$variable" "$value"
    done
    if [ "${COTERIE_TASK_ID+x}" = "x" ]; then
      printf 'env:COTERIE_TASK_ID=%s\0' "$COTERIE_TASK_ID"
    fi
  } > "$COTERIE_FAKE_CAPTURE.pending"
  # Readers use the capture pathname as the readiness signal.
  mv "$COTERIE_FAKE_CAPTURE.pending" "$COTERIE_FAKE_CAPTURE"
  IFS= read -r input
  printf 'stdout:%s\n' "$input"
  printf 'stderr:%s\n' "$input" >&2
  exit 0
fi
if [ "${COTERIE_FAKE_MODE-}" = "signals" ]; then
  trap 'printf "winch\n" >> "$COTERIE_FAKE_CAPTURE"' WINCH
  trap 'printf "int\n" >> "$COTERIE_FAKE_CAPTURE"; exit 0' INT
  printf 'ready\n' > "$COTERIE_FAKE_READY"
  while :; do
    :
  done
fi
if [ "${COTERIE_FAKE_MODE-}" = "stop" ]; then
  trap 'printf "int\n" >> "$COTERIE_FAKE_CAPTURE"' INT
  trap 'printf "term\n" >> "$COTERIE_FAKE_CAPTURE"; exit 0' TERM
  printf 'ready\n' > "$COTERIE_FAKE_READY"
  while :; do
    :
  done
fi
if [ "${COTERIE_FAKE_MODE-}" = "phased-stop" ]; then
  trap 'printf "int\n" >> "$COTERIE_FAKE_CAPTURE"' INT
  trap 'printf "term\n" >> "$COTERIE_FAKE_CAPTURE"' TERM
  printf 'ready\n' > "$COTERIE_FAKE_READY"
  while :; do :; done
fi
if [ "${COTERIE_FAKE_MODE-}" = "restart" ]; then
  printf 'ready\n' > "$COTERIE_FAKE_READY"
  while [ ! -e "$COTERIE_FAKE_RELEASE" ]; do
    :
  done
  exit 23
fi
is_job=false
for argument in "$@"; do
  if [ "$argument" = "exec" ]; then
    is_job=true
  fi
done
if [ "$is_job" = true ]; then
  parent=$PPID
  printf '{"type":"thread.started","thread_id":"thread-1"}\n'
  while [ ! -e "$COTERIE_SOCKET.release" ]; do
    if [ ! -e "/proc/$parent" ]; then
      exit 0
    fi
  done
  coterie="$COTERIE_BIN"
  "$coterie" prime --json > /dev/null || exit 19
  "$coterie" inbox --json > /dev/null || exit 20
  "$coterie" inbox ack 1 --json > /dev/null || exit 21
  "$coterie" finish --status completed --summary "Implemented and tested." --json > /dev/null || exit 22
  printf '{"type":"turn.completed","usage":{}}\n'
  exit 0
fi
exit 0
"#;

#[path = "supervisor_runtime/progress.rs"]
mod progress;

fn write_global(fixture: &TestEnvironment, text: &str) {
    let directory = fixture.root.join("config/coterie");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("config.toml"), text).unwrap();
}

fn rejected(fixture: &TestEnvironment, arguments: &[&str], message: &str) {
    let output = run({
        let mut command = fixture.command();
        command.args(arguments);
        command
    });
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}

#[test]
fn project_attachment_discovers_one_run_and_retires_every_index() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let library = fixture.root.join("library");
    Repository::init(&library).unwrap();
    let alias_path = fixture.root.join("library-link");
    std::os::unix::fs::symlink(&library, &alias_path).unwrap();
    let operation = "co-01ARZ3NDEKTSV4RRFFQ69G5FAX";
    let args = [
        "project",
        "attach",
        alias_path.to_str().unwrap(),
        "--alias",
        "library",
        "--operation-id",
        operation,
        "--json",
    ];
    let attached = fixture.run_json(&args);
    assert_eq!(
        attached["data"]["project"]["root"],
        library.to_str().unwrap()
    );
    assert_eq!(fixture.run_json(&args), attached);
    assert_eq!(fixture.index_entry_count(), 2);
    let primary = fixture.run_json(&["status", "--json"]);
    let output = run({
        let mut command = fixture.command();
        command.current_dir(&library).args(["status", "--json"]);
        command
    });
    assert!(output.status.success(), "{output:?}");
    let secondary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(primary, secondary);
    let listed = fixture.run_json(&["project", "list", "--json"]);
    assert_eq!(listed["data"]["projects"].as_array().unwrap().len(), 2);
    rejected(
        &fixture,
        &[
            "project",
            "attach",
            library.to_str().unwrap(),
            "--alias",
            "primary",
            "--json",
        ],
        "alias",
    );
    let output = run({
        let mut command = fixture.command();
        command.current_dir(&library).args(["stop", "--json"]);
        command
    });
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn project_attachment_preserves_non_utf8_directory_identity() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = TestEnvironment::new_plain();
    fixture.launch(&[]);
    let directory = fixture
        .root
        .join(std::ffi::OsString::from_vec(b"directory-\xff".to_vec()));
    fs::create_dir(&directory).unwrap();
    let output = run({
        let mut command = fixture.command();
        command.args(["project", "attach"]).arg(&directory).args([
            "--alias",
            "directory",
            "--json",
        ]);
        command
    });
    assert!(output.status.success(), "{output:?}");
    let attached: Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = fixture.run_json(&["status", "--json"]);
    let database = fixture
        .state
        .join("coterie/runs")
        .join(status["data"]["run_id"].as_str().unwrap())
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open_with_flags(
        database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let root: Vec<u8> = connection
        .query_row(
            "SELECT canonical_path FROM projects WHERE id = ?1",
            [attached["data"]["project"]["id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(root, directory.as_os_str().as_bytes());
    let output = run({
        let mut command = fixture.command();
        command.current_dir(&directory).args(["project", "list"]);
        command
    });
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("directory"));
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn project_attachment_recovery_reacquires_all_leases_before_serving() {
    let fixture = TestEnvironment::new();
    let mut supervisor = fixture
        .command()
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("primary publication", || fixture.index_entry_count() == 1);
    let library = fixture.root.join("library");
    Repository::init(&library).unwrap();
    let attached = fixture.run_json(&[
        "project",
        "attach",
        library.to_str().unwrap(),
        "--json",
    ]);
    let before = fixture.run_json(&["status", "--json"]);
    let indexes = fs::read_dir(fixture.state.join("coterie/projects")).unwrap();
    let entry: Value = indexes
        .filter_map(Result::ok)
        .filter_map(|file| fs::read(file.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .find(|entry| entry["project_id"] == attached["data"]["project"]["id"])
        .unwrap();
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    let lease_path = fixture
        .runtime
        .join("coterie/projects")
        .join(format!("{}.lock", entry["project_key"].as_str().unwrap()));
    let lease = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lease_path)
        .unwrap();
    lease.try_lock().unwrap();
    let output = run({
        let mut command = fixture.command();
        command
            .args(["__supervisor", RUN_ID, PROJECT_ID])
            .arg(&fixture.project);
        command
    });
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("lease"));
    drop(lease);
    assert!(run(fixture.connect_command()).status.success());
    assert_eq!(fixture.run_json(&["status", "--json"]), before);
    let output = run({
        let mut command = fixture.command();
        command.current_dir(&library).args(["status", "--json"]);
        command
    });
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        before
    );
    fixture.run_json(&["stop", "--json"]);
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn project_attachment_enforces_agent_roots_and_capabilities() {
    for authorized in [true, false] {
        let fixture = TestEnvironment::new();
        let allowed = fixture.root.join("allowed");
        let library = allowed.join("library");
        Repository::init(&library).unwrap();
        let outside = fixture.root.join("outside");
        Repository::init(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, allowed.join("escape")).unwrap();
        let root_link = fixture.root.join("root-link");
        std::os::unix::fs::symlink(&allowed, &root_link).unwrap();
        let custom = if authorized {
            String::new()
        } else {
            include_str!("../examples/config/global.toml").to_owned()
        };
        write_global(
            &fixture,
            &format!("allowed_project_roots = [{root_link:?}]\n{custom}"),
        );
        let script = FAKE_CODEX.replace("is_job=false", r#"
if [ "${COTERIE_ROLE-}" = lead ] || [ "${COTERIE_ROLE-}" = coordinator ]; then
  coterie="${0%/*}/coterie"
  base="$COTERIE_PRIMARY_PROJECT_ROOT/.."
  "$coterie" project attach "$base/outside" --json > "$0.outside.out" 2> "$0.outside.err"
  [ "$?" = 6 ] || exit 91
  "$coterie" project attach "$base/allowed/escape" --alias escape --json > "$0.escape.out" 2> "$0.escape.err"
  [ "$?" = 6 ] || exit 92
  ln -sfn "$base/outside" "$base/root-link" || exit 93
  "$coterie" project attach "$base/root-link" --alias retargeted --json > "$0.retargeted.out" 2> "$0.retargeted.err"
  [ "$?" = 6 ] || exit 94
  "$coterie" project attach "$base/allowed/library" --json > "$0.allowed.out" 2> "$0.allowed.err"
  printf '%s' "$?" > "$0.allowed.status"
  exit 0
fi
is_job=false"#);
        let provider = fixture.root.join("bin/codex");
        fs::write(&provider, script).unwrap();
        fixture.launch(&[]);
        assert_eq!(
            fs::read_to_string(provider.with_extension("allowed.status"))
                .unwrap(),
            if authorized { "0" } else { "6" }
        );
        assert_eq!(fixture.index_entry_count(), if authorized { 2 } else { 1 });
        let denied: Value = serde_json::from_slice(
            &fs::read(provider.with_extension("outside.err")).unwrap(),
        )
        .unwrap();
        assert_eq!(denied["error"]["code"], "permission_denied");
        // An explicit operator attachment can grant a root outside the allowlist.
        fixture.run_json(&[
            "project",
            "attach",
            outside.to_str().unwrap(),
            "--json",
        ]);
        fixture.run_json(&["stop", "--json"]);
    }
}

#[test]
fn project_attachment_races_and_cross_run_leases_fail_without_waiting() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let second = fixture.root.join("second");
    let shared = fixture.root.join("shared");
    Repository::init(&second).unwrap();
    Repository::init(&shared).unwrap();
    let output = run({
        let mut command = fixture.connect_command();
        command.current_dir(&second);
        command
    });
    assert!(output.status.success(), "{output:?}");
    let first = fixture
        .command()
        .args(["project", "attach"])
        .arg(&shared)
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second_output = run({
        let mut command = fixture.command();
        command
            .current_dir(&second)
            .args(["project", "attach"])
            .arg(&shared)
            .arg("--json");
        command
    });
    let first_output = first.wait_with_output().unwrap();
    let status = first_output.status;
    assert_eq!(
        [status.success(), second_output.status.success()]
            .into_iter()
            .filter(|value| *value)
            .count(),
        1
    );
    assert!(
        status.code() == Some(5) || second_output.status.code() == Some(5),
        "{first_output:?}, {second_output:?}"
    );
    rejected(
        &fixture,
        &[
            "project",
            "attach",
            second.to_str().unwrap(),
            "--alias",
            "second",
            "--json",
        ],
        "lease",
    );
    let output = run({
        let mut command = fixture.command();
        command
            .current_dir(&second)
            .args(["project", "attach"])
            .arg(&fixture.project)
            .arg("--json");
        command
    });
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    fixture.run_json(&["stop", "--json"]);
    let output = run({
        let mut command = fixture.command();
        command.current_dir(&second).args(["stop", "--json"]);
        command
    });
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn project_attachment_keeps_linked_worktrees_distinct_and_validates_aliases() {
    let fixture = TestEnvironment::new();
    let linked = fixture.root.join("linked");
    let repository = Repository::open(&fixture.project).unwrap();
    repository.worktree("linked", &linked, None).unwrap();
    fixture.launch(&[]);
    let attached =
        fixture.run_json(&["project", "attach", "../linked", "--json"]);
    let list = fixture.run_json(&["project", "list", "--json"]);
    assert_ne!(
        attached["data"]["project"]["id"],
        list["data"]["projects"][0]["id"]
    );
    rejected(
        &fixture,
        &[
            "project",
            "attach",
            "../linked",
            "--alias",
            "renamed",
            "--json",
        ],
        "identity",
    );
    let output = run({
        let mut command = fixture.command();
        command.args([
            "project",
            "attach",
            "../linked",
            "--alias",
            "../invalid",
            "--json",
        ]);
        command
    });
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let output = run({
        let mut command = fixture.command();
        command.current_dir(&linked);
        command
    });
    assert_eq!(output.status.code(), Some(5), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("project belongs to run")
    );
    assert_eq!(fixture.index_entry_count(), 2);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn project_attachment_rejects_incompatible_restrictions_and_invalid_locks() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let library = fixture.root.join("library");
    Repository::init(&library).unwrap();
    fs::write(
        library.join("coterie.toml"),
        "[roles.worker]\nmax_instances = 1",
    )
    .unwrap();
    rejected(
        &fixture,
        &["project", "attach", library.to_str().unwrap(), "--json"],
        "restriction overlays",
    );
    fs::remove_file(library.join("coterie.toml")).unwrap();
    fs::write(library.join("coterie.lock"), "invalid lock").unwrap();
    let output = run({
        let mut command = fixture.command();
        command
            .args(["project", "attach"])
            .arg(&library)
            .arg("--json");
        command
    });
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(fixture.index_entry_count(), 1);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn configured_capabilities_authorize_calls_and_concrete_recipient_roles() {
    let fixture = TestEnvironment::new();
    let provider = fixture.root.join("bin/codex");
    let script = FAKE_CODEX.replace("is_job=false", r#"
if [ "${COTERIE_ROLE-}" = coordinator ]; then
  "${0%/*}/coterie" task create Forbidden --json > "$0.stdout" 2> "$0.denied"
  [ "$?" = 6 ] || exit 91
fi
if [ "${COTERIE_ROLE-}" = builder ]; then
  "${0%/*}/coterie" send coordinator 'Configured recipient' --json > "$0.sent" || exit 92
fi
is_job=false"#);
    fs::write(&provider, script).unwrap();
    write_global(
        &fixture,
        &include_str!("../examples/config/global.toml")
            .replace(", \"task:*\"", ""),
    );
    fixture.launch(&[]);
    let denied: Value = serde_json::from_slice(
        &fs::read(provider.with_extension("denied")).unwrap(),
    )
    .unwrap();
    assert_eq!(denied["error"]["code"], "permission_denied");
    let task = fixture.run_json(&[
        "task",
        "create",
        "Allowed operator task",
        "--json",
    ]);
    fixture.run_json(&[
        "spawn",
        "builder",
        "--task",
        task["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    wait_until("message to configured foreground role", || {
        fs::read(provider.with_extension("sent"))
            .is_ok_and(|bytes| serde_json::from_slice::<Value>(&bytes).is_ok())
    });
    let sent: Value = serde_json::from_slice(
        &fs::read(provider.with_extension("sent")).unwrap(),
    )
    .unwrap();
    assert!(sent["data"].is_object());
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn operator_overrides_are_bounded_snapshotted_and_reused_on_reconnect() {
    let fixture = TestEnvironment::new();
    write_global(&fixture, include_str!("../examples/config/global.toml"));
    fs::write(
        fixture.project.join("coterie.toml"),
        "archetype = 'builtin:standard@1'\n[roles.worker]\nmax_instances = 1",
    )
    .unwrap();
    let args = [
        "--archetype",
        "builtin:standard@1",
        "--role",
        "worker.max_instances=2",
        "--max-agents-per-run",
        "5",
    ];
    fixture.launch(&args);
    fixture.launch(&args);
    let status = fixture.run_json(&["status", "--json"]);
    let run_id = status["data"]["run_id"].as_str().unwrap();
    let connection = rusqlite::Connection::open(
        fixture
            .state
            .join("coterie/runs")
            .join(run_id)
            .join("state.sqlite3"),
    )
    .unwrap();
    let document: String = connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
    let snapshot: Value = serde_json::from_str(&document).unwrap();
    assert_eq!(snapshot["effective"]["roles"]["worker"]["max_instances"], 2);
    assert_eq!(snapshot["effective"]["limits"]["max_agents_per_run"], 5);
    assert_eq!(
        snapshot["provenance"]["roles.worker.max_instances"]["source"]["layer"],
        "operator"
    );
    assert_eq!(run(fixture.command()).status.code(), Some(3));
    fixture.run_json(&["stop", "--json"]);
    let mut invalid = fixture.command();
    invalid.args(["--role", "worker.max_instances=4"]);
    assert_eq!(run(invalid).status.code(), Some(3));
    let mut invalid = fixture.command();
    invalid.args(["--max-agents-per-run", "13"]);
    assert_eq!(run(invalid).status.code(), Some(3));
    let mut invalid = fixture.command();
    invalid.args(["--role", "worker.instructions=untrusted"]);
    assert_eq!(run(invalid).status.code(), Some(2));
    let mut invalid = fixture.command();
    invalid.args(["status", "--max-agents-per-run", "3", "--json"]);
    assert_eq!(run(invalid).status.code(), Some(2));
}

#[test]
fn foreground_controls_remain_available_when_configuration_files_change() {
    let fixture = TestEnvironment::new();
    let ready = fixture.root.join("ready");
    let capture = fixture.root.join("signals");
    let mut command = fixture.command();
    command
        .args(["--role", "worker.max_instances=1"])
        .env("COTERIE_FAKE_MODE", "stop")
        .env("COTERIE_FAKE_READY", &ready)
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut foreground = command.spawn().unwrap();
    wait_until("foreground with overrides", || ready.exists());
    fs::write(fixture.project.join("coterie.toml"), "invalid = true").unwrap();
    fixture.run_json(&["stop", "--json"]);
    wait_until("foreground reaped", || {
        foreground.try_wait().unwrap().is_some()
    });
    assert!(fs::read_to_string(capture).unwrap().contains("term"));
}

#[test]
fn runtime_snapshots_refuse_known_credentials_without_creating_state() {
    let fixture = TestEnvironment::new();
    let secret = "credential\nwith\"escaping";
    let global = format!(
        "[providers.codex]\ncommand = ['codex', {}]",
        serde_json::to_string(secret).unwrap()
    );
    write_global(&fixture, &global);
    let mut command = fixture.command();
    command.env("OPENAI_API_KEY", secret);
    let output = run(command);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("escaping"));
    assert!(!fixture.state.exists());
}

#[test]
fn configured_bindings_instructions_permissions_and_role_names_reach_providers()
{
    let fixture = TestEnvironment::new();
    let lead = fixture.root.join("bin/configured-lead");
    let job = fixture.root.join("bin/configured-job");
    let script = FAKE_CODEX.replace("#!/bin/sh", "#!/bin/sh\n[ \"$1\" = \"--binding-marker\" ] || exit 90\nshift\nif [ -n \"${COTERIE_RUN_ID-}\" ]; then printf '%s\\n' \"$@\" > \"$0.capture\"; fi");
    for executable in [&lead, &job] {
        fs::write(executable, &script).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    let global = include_str!("../examples/config/global.toml")
        .replace("[providers.codex]\ncommand = [\"codex\"]", &format!("[providers.codex]\ncommand = ['{}', '--binding-marker']\n[providers.jobs]\ncommand = ['{}', '--binding-marker']", lead.display(), job.display()))
        .replace("roles.builder]\nprovider = \"codex\"", "roles.builder]\nprovider = \"jobs\"")
        .replace("max_instances = 3", "max_instances = 3\ninstructions = 'Check the recorded acceptance criteria.'");
    write_global(&fixture, &global);
    fs::write(
        fixture.project.join("coterie.toml"),
        "[roles.builder]\nmax_instances = 1\npermission_profile = 'inspect'\n",
    )
    .unwrap();
    fixture.launch(&[]);
    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["agents"][0]["name"], "coordinator");
    let capture = fs::read_to_string(lead.with_extension("capture")).unwrap();
    assert!(
        capture.contains("Coordinate tasks through Coterie."),
        "{capture}"
    );
    let task = fixture.run_json(&["task", "create", "Inspect", "--json"]);
    let task_id = task["data"]["task"]["id"].as_str().unwrap();
    let spawned =
        fixture.run_json(&["spawn", "builder", "--task", task_id, "--json"]);
    assert_eq!(spawned["data"]["agent"]["name"], "builder-1");
    wait_until("configured job launch", || {
        job.with_extension("capture").exists()
    });
    let capture = fs::read_to_string(job.with_extension("capture")).unwrap();
    assert!(
        capture.contains("Check the recorded acceptance criteria."),
        "{capture}"
    );
    assert!(capture.contains("read-only"), "{capture}");
    assert!(capture.contains("never"), "{capture}");
    let next = fixture.run_json(&["task", "create", "Second", "--json"]);
    rejected(
        &fixture,
        &[
            "spawn",
            "builder",
            "--task",
            next["data"]["task"]["id"].as_str().unwrap(),
            "--json",
        ],
        "instance limit",
    );
    // Editing the file cannot raise the ceiling in the supervisor's saved policy.
    fs::write(
        fixture.project.join("coterie.toml"),
        "[roles.builder]\nmax_instances = 2\npermission_profile = 'inspect'\n",
    )
    .unwrap();
    rejected(
        &fixture,
        &[
            "spawn",
            "builder",
            "--task",
            next["data"]["task"]["id"].as_str().unwrap(),
            "--json",
        ],
        "instance limit",
    );
    let report = fixture.run_json(&["doctor", "--json"]);
    assert!(
        report["data"]["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["check"] == "configuration"
                && check["status"] == "error")
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn disabled_roles_run_limits_and_spawn_rates_use_effective_policy() {
    for (restriction, role, expected, first) in [
        (
            "[roles.worker]\nenabled = false",
            "worker",
            "disabled",
            false,
        ),
        (
            "[limits]\nmax_agents_per_run = 1",
            "worker",
            "agent limit",
            false,
        ),
        (
            "[limits]\nmax_concurrent_agents = 1",
            "worker",
            "agent limit",
            true,
        ),
        (
            "[limits]\nmax_spawns_per_minute = 1",
            "reviewer",
            "spawn rate",
            true,
        ),
    ] {
        let fixture = TestEnvironment::new();
        fs::write(fixture.project.join("coterie.toml"), restriction).unwrap();
        fixture.launch(&[]);
        if first {
            let task = fixture.run_json(&["task", "create", "First", "--json"]);
            fixture.run_json(&[
                "spawn",
                "worker",
                "--task",
                task["data"]["task"]["id"].as_str().unwrap(),
                "--json",
            ]);
        }
        let task = fixture.run_json(&["task", "create", "Next", "--json"]);
        rejected(
            &fixture,
            &[
                "spawn",
                role,
                "--task",
                task["data"]["task"]["id"].as_str().unwrap(),
                "--json",
            ],
            expected,
        );
        if first && expected == "agent limit" {
            rejected(&fixture, &[], "agent limit");
        }
        fixture.run_json(&["stop", "--json"]);
    }
}

#[test]
fn changed_host_bindings_are_incompatible_even_with_the_same_portable_lock() {
    let fixture = TestEnvironment::new();
    fixture.run_json(&["config", "lock", "--json"]);
    fixture.launch(&[]);
    write_global(&fixture, "[providers.codex]\ncommand = ['another-codex']");
    fixture.run_json(&["config", "check", "--json"]);
    let output = run(fixture.command());
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("providers"));
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn recovery_rejects_changed_policy_and_preserves_the_original_snapshot() {
    let fixture = TestEnvironment::new();
    write_global(&fixture, include_str!("../examples/config/global.toml"));
    let mut command = fixture.command();
    command
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut supervisor = command.spawn().unwrap();
    wait_until("configured supervisor", || fixture.index_entry_count() == 1);
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let snapshot = || {
        rusqlite::Connection::open(&database).unwrap().query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get::<_, String>(0)).unwrap()
    };
    let original = snapshot();
    fs::write(
        fixture.project.join("coterie.toml"),
        "[roles.builder]\nenabled = false",
    )
    .unwrap();
    let output = run(fixture.connect_command());
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert_eq!(snapshot(), original);
    fs::remove_file(fixture.project.join("coterie.toml")).unwrap();
    fixture.launch(&[]);
    assert_eq!(
        fixture.run_json(&["status", "--json"])["data"]["agents"][0]["name"],
        "coordinator"
    );
    assert_eq!(snapshot(), original);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn runtime_configuration_is_snapshotted_and_conflicting_restarts_fail() {
    let fixture = TestEnvironment::new();
    fs::write(
        fixture.project.join("coterie.toml"),
        "[roles.worker]\nmax_instances = 1\n",
    )
    .unwrap();
    fixture.launch(&[]);
    let status = fixture.run_json(&["status", "--json"])["data"].clone();
    let run_id = status["run_id"].as_str().unwrap();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(run_id)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let document: String = connection.query_row(
        "SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)
    ).unwrap();
    let snapshot: Value = serde_json::from_str(&document).unwrap();
    assert_eq!(snapshot["effective"]["roles"]["worker"]["max_instances"], 1);
    assert_eq!(
        snapshot["provenance"]["roles.worker.max_instances"]["source"]["layer"],
        "project"
    );
    fs::write(
        fixture.project.join("coterie.toml"),
        "schema_version = 1\n[roles.worker]\nmax_instances = 1\n",
    )
    .unwrap();
    fixture.launch(&[]);
    let unchanged: String = connection.query_row("SELECT document_json FROM configuration_snapshots WHERE scope = 'run'", [], |row| row.get(0)).unwrap();
    assert_eq!(unchanged, document);
    fs::write(
        fixture.project.join("coterie.toml"),
        "[roles.worker]\nmax_instances = 2\n",
    )
    .unwrap();
    let output = run(fixture.command());
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("snapshot"),
        "{output:?}"
    );
    assert_eq!(
        fixture.run_json(&["status", "--json"])["data"].clone()["run_id"],
        run_id
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn invalid_configuration_and_lock_fail_before_run_creation() {
    for text in ["[roles.worker]\nmax_instances = 4", "unknown = true"] {
        let fixture = TestEnvironment::new();
        fs::write(fixture.project.join("coterie.toml"), text).unwrap();
        let output = run(fixture.command());
        assert_eq!(output.status.code(), Some(3), "{output:?}");
        assert!(!fixture.state.exists());
    }
    let fixture = TestEnvironment::new();
    fs::write(fixture.project.join("coterie.lock"), "{}").unwrap();
    let output = run(fixture.command());
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(!fixture.state.exists());
}

#[test]
fn public_help_lists_the_minimum_delegation_commands() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.arg("--help");

    let output = run(command);

    assert!(output.status.success(), "help failed: {output:?}");
    let stdout =
        String::from_utf8(output.stdout).expect("help should be UTF-8");
    for command in [
        "status",
        "whoami",
        "prime",
        "task",
        "spawn",
        "finish",
        "send",
        "inbox",
        "logs",
        "workspace",
        "events",
        "stop",
    ] {
        assert!(
            stdout.contains(command),
            "help should list `{command}`:\n{stdout}"
        );
    }
    assert!(!stdout.contains("__supervisor"));

    let mut command = fixture.command();
    command.args(["task", "--help"]);
    let output = run(command);
    assert!(output.status.success(), "task help failed: {output:?}");
    let stdout =
        String::from_utf8(output.stdout).expect("task help should be UTF-8");
    for command in ["create", "ready", "close"] {
        assert!(
            stdout.contains(command),
            "task help should list `{command}`:\n{stdout}"
        );
    }

    let mut command = fixture.command();
    command.args(["workspace", "--help"]);
    let output = run(command);
    assert!(output.status.success(), "workspace help failed: {output:?}");
    let stdout = String::from_utf8(output.stdout)
        .expect("workspace help should be UTF-8");
    assert!(
        stdout.contains("integrate"),
        "workspace help should list `integrate`:\n{stdout}"
    );
}

#[test]
fn invalid_programmatic_arguments_use_the_versioned_error_contract() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.args(["status", "--unknown", "--json"]);

    let output = run(command);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the diagnostic should be one JSON response");
    assert_eq!(error["schema_version"], 1);
    assert_eq!(error["error"]["code"], "invalid_argument");
    assert!(error.get("operation_id").is_none());
}

#[test]
fn interactive_foreground_rejects_json_before_starting_a_run() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.arg("--json");

    let output = run(command);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the diagnostic should be one JSON response");
    assert_eq!(error["error"]["code"], "invalid_argument");
    assert!(error["operation_id"].as_str().is_some());
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn missing_codex_is_reported_as_unavailable() {
    let fixture = TestEnvironment::new();
    let empty_path = fixture.root.join("empty-path");
    fs::create_dir(&empty_path).expect("the empty PATH should be created");
    let mut command = fixture.command();
    command.env("PATH", empty_path);

    let output = run(command);

    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("could not execute a Codex probe")
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn mutation_connection_failures_preserve_the_allocated_operation_id() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command.args([
        "task",
        "create",
        "Unreachable mutation",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB4",
        "--json",
    ]);

    let output = run(command);

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the diagnostic should be one JSON response");
    assert_eq!(error["operation_id"], "co-01ARZ3NDEKTSV4RRFFQ69G5FB4");
    assert_eq!(error["error"]["code"], "not_found");
}

#[test]
fn status_does_not_create_a_run_and_partial_agent_identity_never_becomes_operator()
 {
    let fixture = TestEnvironment::new();
    let mut status = fixture.command();
    status.args(["status", "--json"]);

    let output = run(status);

    assert_eq!(output.status.code(), Some(4));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the missing-run diagnostic should be JSON");
    assert_eq!(error["error"]["code"], "not_found");
    assert_eq!(fixture.index_entry_count(), 0);

    fixture.launch(&[]);
    let mut inbox = fixture.command();
    inbox.args(["inbox", "--json"]);
    let output = run(inbox);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the operator inbox rejection should be JSON");
    assert_eq!(error["error"]["code"], "permission_denied");

    let mut acknowledge = fixture.command();
    acknowledge.args([
        "inbox",
        "ack",
        "1",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB9",
        "--json",
    ]);
    let output = run(acknowledge);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the acknowledgement rejection should be JSON");
    assert_eq!(error["operation_id"], "co-01ARZ3NDEKTSV4RRFFQ69G5FB9");
    assert_eq!(error["error"]["code"], "permission_denied");

    let mut finish = fixture.command();
    finish.args([
        "finish",
        "--status",
        "completed",
        "--summary",
        "No operator assignment.",
        "--json",
    ]);
    let output = run(finish);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the operator finish rejection should be JSON");
    assert_eq!(error["error"]["code"], "permission_denied");
    assert!(error["operation_id"].as_str().is_some());

    let mut whoami = fixture.command();
    whoami
        .args(["whoami", "--json"])
        .env("COTERIE_AGENT_ID", "cg-01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let output = run(whoami);
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the authentication diagnostic should be JSON");
    assert_eq!(error["error"]["code"], "unauthenticated");

    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn human_success_and_diagnostics_use_separate_streams() {
    let fixture = TestEnvironment::new();
    let launch = run(fixture.command());
    assert!(launch.status.success(), "launch failed: {launch:?}");
    assert!(launch.stdout.is_empty());
    assert!(launch.stderr.is_empty());
    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["agents"][0]["name"], "lead");
    assert_eq!(status["data"]["agents"][0]["state"], "exited");

    let mut invalid = fixture.command();
    invalid.args(["task", "close", "not-a-task"]);
    let invalid = run(invalid);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(invalid.stdout.is_empty());
    assert!(!invalid.stderr.is_empty());

    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn clean_git_launch_starts_and_reconnects_to_silent_foreground_leads() {
    let fixture = TestEnvironment::new();
    assert_repository_clean(&fixture.project);

    fixture.launch(&[]);
    let initial_status = fixture.run_json(&["status", "--json"]);
    let run_id = initial_status["data"]["run_id"]
        .as_str()
        .expect("status should identify the started run")
        .to_owned();
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    let socket_inode = fs::metadata(&socket)
        .expect("the started supervisor should publish its socket")
        .ino();

    assert_eq!(initial_status["data"]["agents"][0]["name"], "lead");
    assert_eq!(initial_status["data"]["agents"][0]["state"], "exited");

    fixture.launch(&[]);
    let reconnected_status = fixture.run_json(&["status", "--json"]);

    assert_eq!(reconnected_status["data"]["run_id"], run_id);
    assert_eq!(
        fs::metadata(&socket)
            .expect("the reconnected supervisor socket should remain live")
            .ino(),
        socket_inode
    );
    assert_eq!(reconnected_status["data"]["agents"][0]["name"], "lead");
    assert_eq!(reconnected_status["data"]["agents"][0]["state"], "exited");
    assert_repository_clean(&fixture.project);

    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn foreground_exit_preserves_an_active_worker_for_a_fresh_lead_session() {
    let fixture = TestEnvironment::new();
    let first_capture = fixture.root.join("first-lead-environment");
    let mut first_command = fixture.command();
    first_command
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &first_capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut first_foreground =
        first_command.spawn().expect("the first lead should start");
    wait_until("the first lead environment", || first_capture.exists());
    let first_environment = captured_environment(&first_capture);
    let first_identity =
        fixture.run_agent_json(&["whoami", "--json"], &first_environment);
    let run_id = first_identity["data"]["run_id"]
        .as_str()
        .expect("the first lead should identify the run")
        .to_owned();
    let lead_id = first_identity["data"]["agent"]["id"]
        .as_str()
        .expect("the first lead should identify itself")
        .to_owned();
    let first_session_id =
        environment_value(&first_environment, "COTERIE_SESSION_ID");
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    let socket_inode = fs::metadata(&socket)
        .expect("the active supervisor should publish its socket")
        .ino();

    let task = fixture.run_agent_json(
        &["task", "create", "Keep working", "--json"],
        &first_environment,
    );
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("task creation should return an ID")
        .to_owned();
    let spawn = fixture.run_agent_json(
        &["spawn", "worker", "--task", &task_id, "--json"],
        &first_environment,
    );
    let worker_session_id = spawn["data"]["session_id"]
        .as_str()
        .expect("spawn should return the worker session ID")
        .to_owned();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(&run_id)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the active run database should open");
    let worker_process_id: u32 = connection
        .query_row(
            "SELECT provider_session_id FROM sessions WHERE id = ?1",
            [&worker_session_id],
            |row| row.get::<_, String>(0),
        )
        .expect("the worker process identity should be durable")
        .strip_prefix("process:")
        .and_then(|process_id| process_id.parse().ok())
        .expect("the worker should have a process identity");
    drop(connection);
    wait_until("the active worker", || {
        fixture.run_json(&["status", "--json"])["data"]["agents"]
            .as_array()
            .is_some_and(|agents| {
                agents.iter().any(|agent| {
                    agent["name"] == "worker-1" && agent["state"] == "running"
                })
            })
    });

    first_foreground
        .stdin
        .take()
        .expect("the first lead stdin should be available")
        .write_all(b"exit\n")
        .expect("the first lead should accept terminal input");
    let first_output = first_foreground
        .wait_with_output()
        .expect("the first lead should finish");
    assert!(first_output.status.success());
    assert_eq!(first_output.stdout, b"stdout:exit\n");
    assert_eq!(first_output.stderr, b"stderr:exit\n");

    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["run_id"], run_id);
    assert_eq!(status["data"]["status"], "active");
    assert!(status["data"]["agents"].as_array().is_some_and(|agents| {
        agents
            .iter()
            .any(|agent| agent["id"] == lead_id && agent["state"] == "exited")
            && agents.iter().any(|agent| {
                agent["name"] == "worker-1" && agent["state"] == "running"
            })
    }));
    assert_eq!(
        fs::metadata(&socket)
            .expect("the supervisor should remain reachable")
            .ino(),
        socket_inode
    );
    assert!(
        Path::new("/proc")
            .join(worker_process_id.to_string())
            .exists(),
        "the worker process should outlive the foreground"
    );

    let second_capture = fixture.root.join("second-lead-environment");
    let mut second_command = fixture.command();
    second_command
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &second_capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut second_foreground =
        second_command.spawn().expect("the fresh lead should start");
    wait_until("the fresh lead environment", || second_capture.exists());
    let second_environment = captured_environment(&second_capture);
    let second_session_id =
        environment_value(&second_environment, "COTERIE_SESSION_ID");
    assert_ne!(second_session_id, first_session_id);

    let prime =
        fixture.run_agent_json(&["prime", "--json"], &second_environment);
    assert_eq!(prime["data"]["identity"]["run_id"], run_id);
    assert_eq!(prime["data"]["identity"]["channel"], "agent");
    assert_eq!(prime["data"]["identity"]["agent"]["id"], lead_id);
    assert!(prime["data"]["peers"].as_array().is_some_and(|peers| {
        peers.iter().any(|peer| {
            peer["name"] == "worker-1" && peer["state"] == "running"
        })
    }));
    assert!(prime["data"]["tasks"].as_array().is_some_and(|tasks| {
        tasks.iter().any(|task| {
            task["id"] == task_id && task["status"] == "in_progress"
        })
    }));

    second_foreground
        .stdin
        .take()
        .expect("the fresh lead stdin should be available")
        .write_all(b"exit\n")
        .expect("the fresh lead should accept terminal input");
    assert!(
        second_foreground
            .wait_with_output()
            .expect("the fresh lead should finish")
            .status
            .success()
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn foreground_codex_inherits_streams_directory_identity_and_agents_discovery() {
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("codex-contract");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("Coterie should start");
    child
        .stdin
        .take()
        .expect("the foreground stdin should be piped")
        .write_all(b"terminal input\n")
        .expect("the terminal input should be writable");

    let output = child
        .wait_with_output()
        .expect("the foreground process should finish");

    assert!(output.status.success(), "foreground failed: {output:?}");
    assert_eq!(output.stdout, b"stdout:terminal input\n");
    assert_eq!(output.stderr, b"stderr:terminal input\n");
    let records = fs::read(&capture)
        .expect("the Codex contract should be captured")
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| String::from_utf8(record.to_vec()).expect("UTF-8 record"))
        .collect::<Vec<_>>();
    assert!(records.contains(&format!("cwd={}", fixture.project.display())));
    let arguments = records
        .iter()
        .filter_map(|record| record.strip_prefix("arg="))
        .collect::<Vec<_>>();
    assert_eq!(arguments.len(), 10, "the bootstrap must not be a prompt");
    assert_eq!(arguments[0], "--sandbox");
    assert_eq!(arguments[1], "workspace-write");
    assert_eq!(arguments[2], "--ask-for-approval");
    assert_eq!(arguments[3], "on-request");
    assert_eq!(arguments[4], "--config");
    assert_eq!(arguments[5], "approvals_reviewer=\"user\"");
    assert_eq!(arguments[6], "--cd");
    assert_eq!(arguments[7], fixture.project.to_string_lossy());
    assert_eq!(arguments[8], "--config");
    assert!(arguments[9].starts_with("developer_instructions=\""));
    assert!(arguments[9].contains("COTERIE_BIN"));
    assert!(arguments[9].contains("AGENTS.md"));
    assert!(arguments[9].contains("validate the work and commit"));
    assert!(arguments[9].contains("coterie finish --status completed"));
    assert!(arguments[9].contains("no new commit"));
    for variable in [
        "COTERIE_PROJECT_ROOT",
        "COTERIE_PROJECT_ID",
        "COTERIE_PRIMARY_PROJECT_ROOT",
        "COTERIE_RUN_ID",
        "COTERIE_AGENT_ID",
        "COTERIE_SESSION_ID",
        "COTERIE_ROLE",
        "COTERIE_SOCKET",
        "COTERIE_TOKEN",
        "COTERIE_BIN",
    ] {
        assert!(
            records.iter().any(|record| {
                record
                    .strip_prefix(&format!("env:{variable}="))
                    .is_some_and(|value| !value.is_empty())
            }),
            "Codex should receive {variable}"
        );
    }
    assert!(
        !records
            .iter()
            .any(|record| record.starts_with("env:COTERIE_TASK_ID=")),
        "a foreground lead must not inherit a stale task identity"
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn foreground_signals_sent_to_coterie_reach_codex() {
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("codex-signals");
    let ready = fixture.root.join("codex-ready");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "signals")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .env("COTERIE_FAKE_READY", &ready)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("Coterie should start");
    let _open_stdin = child
        .stdin
        .take()
        .expect("the foreground stdin should stay open");
    wait_until("the fake Codex process", || ready.exists());
    let coterie_pid =
        Pid::from_raw(i32::try_from(child.id()).expect("PID should fit"));

    kill(coterie_pid, Signal::SIGWINCH).expect("SIGWINCH should be sent");
    wait_until("the resize signal", || {
        fs::read_to_string(&capture)
            .unwrap_or_default()
            .contains("winch")
    });
    kill(coterie_pid, Signal::SIGINT).expect("SIGINT should be sent");
    wait_until("the interrupted foreground", || {
        child
            .try_wait()
            .expect("status should be readable")
            .is_some()
    });
    let status = child.wait().expect("the foreground should be reaped");

    assert!(status.success());
    let signals = fs::read_to_string(&capture)
        .expect("the delivered signals should be recorded");
    assert!(signals.contains("winch\n"));
    assert!(signals.contains("int\n"));
    assert_eq!(fixture.index_entry_count(), 1);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn stop_terminates_and_reaps_the_foreground_codex_process() {
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("codex-stop");
    let ready = fixture.root.join("codex-ready");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "stop")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .env("COTERIE_FAKE_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut foreground = command.spawn().expect("Coterie should start");
    wait_until("the fake Codex process", || ready.exists());

    let stopped = fixture.run_json(&["stop", "--json"]);

    assert_eq!(stopped["data"]["status"], "stopped");
    wait_until("the foreground wrapper", || {
        foreground
            .try_wait()
            .expect("the wrapper status should be readable")
            .is_some()
    });
    assert!(
        foreground
            .wait()
            .expect("the wrapper should be reaped")
            .success()
    );
    assert_eq!(
        fs::read_to_string(&capture).expect("Codex should record termination"),
        "int\nterm\n"
    );
    assert_eq!(fixture.index_entry_count(), 0);
}

#[test]
fn phased_shutdown_resumes_after_a_supervisor_crash() {
    let fixture = TestEnvironment::new();
    let mut command = fixture.command();
    command
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut supervisor = command.spawn().unwrap();
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });
    let ready = fixture.root.join("ready");
    let capture = fixture.root.join("signals");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "phased-stop")
        .env("COTERIE_FAKE_READY", &ready)
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut foreground = command.spawn().unwrap();
    wait_until("foreground ready", || ready.exists());
    let operation_id = format!("co-{}", ulid::Ulid::generate());
    let mut stop = fixture.command();
    stop.args(["stop", "--json", "--operation-id", &operation_id])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut stop = stop.spawn().unwrap();
    wait_until("interrupt delivery", || {
        fs::read_to_string(&capture).is_ok_and(|text| text.contains("int"))
    });
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let deadline: i64 = connection
        .query_row("SELECT deadline_ms FROM run_shutdowns", [], |row| {
            row.get(0)
        })
        .unwrap();
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    stop.wait().unwrap();
    let mut restart = fixture.command();
    restart
        .args(["__supervisor", RUN_ID, PROJECT_ID])
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut restart = restart.spawn().unwrap();
    wait_until("completed shutdown retirement", || {
        fixture.index_entry_count() == 0
    });
    wait_until("foreground reap", || {
        foreground.try_wait().unwrap().is_some()
    });
    wait_until("replacement supervisor exit", || {
        restart.try_wait().unwrap().is_some()
    });
    assert_eq!(fs::read_to_string(&capture).unwrap(), "int\nterm\n");
    let state: (String, i64, String, String) = connection.query_row("SELECT phase, deadline_ms, runs.status, operation_id FROM run_shutdowns JOIN runs ON runs.id = run_shutdowns.run_id", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap();
    assert_eq!(
        state,
        ("completed".into(), deadline, "stopped".into(), operation_id)
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM events WHERE event_type = 'run.stopped'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn foreground_exit_is_recorded_after_the_supervisor_restarts() {
    let fixture = TestEnvironment::new();
    let mut supervisor = fixture.command();
    supervisor
        .arg("__supervisor")
        .arg(RUN_ID)
        .arg(PROJECT_ID)
        .arg(&fixture.project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut crashed = supervisor.spawn().expect("the supervisor should start");
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });

    let ready = fixture.root.join("codex-ready");
    let release = fixture.root.join("codex-release");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "restart")
        .env("COTERIE_FAKE_READY", &ready)
        .env("COTERIE_FAKE_RELEASE", &release)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let foreground = command.spawn().expect("Coterie should start");
    wait_until("the fake Codex process", || ready.exists());
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    wait_until("the durable foreground launch observation", || {
        rusqlite::Connection::open(&database).is_ok_and(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM sessions \
                     WHERE process_owner = 'foreground' \
                       AND provider_session_id LIKE 'process:%' \
                       AND state = 'running' \
                       AND reconciliation_state = 'observed'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .is_ok_and(|count| count == 1)
        })
    });

    crashed.kill().expect("the supervisor should crash");
    crashed
        .wait()
        .expect("the crashed supervisor should be reaped");
    let restarted = run(fixture.connect_command());
    assert!(restarted.status.success(), "restart failed: {restarted:?}");
    let connection = rusqlite::Connection::open(&database)
        .expect("the restarted run database should open");
    let (state, reconciliation, owner, active_credentials):
        (String, String, String, i64) = connection
        .query_row(
            "SELECT sessions.state, sessions.reconciliation_state, \
                    sessions.process_owner, \
                    COUNT(session_credentials.session_id) \
             FROM sessions \
             JOIN session_credentials ON session_credentials.session_id = sessions.id \
             WHERE sessions.provider_session_id LIKE 'process:%' \
               AND session_credentials.revoked_at IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the live foreground session should remain durable");
    assert_eq!(state, "unknown");
    assert_eq!(reconciliation, "unknown");
    assert_eq!(owner, "foreground");
    assert_eq!(active_credentials, 1);
    drop(connection);

    let mut contender = fixture.command();
    contender.args(["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FBE"]);
    let contender = run(contender);
    assert_eq!(contender.status.code(), Some(5));
    assert!(
        String::from_utf8_lossy(&contender.stderr).contains("already active")
    );

    fs::write(&release, b"exit").expect("Codex should be released");
    let output = foreground
        .wait_with_output()
        .expect("the foreground wrapper should finish");

    assert_eq!(
        output.status.code(),
        Some(7),
        "foreground output: {output:?}"
    );
    let connection = rusqlite::Connection::open(database)
        .expect("the restarted run database should open");
    let (state, reconciliation): (String, String) = connection
        .query_row(
            "SELECT state, reconciliation_state FROM sessions \
             WHERE provider_session_id LIKE 'process:%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the foreground exit should be durable");
    assert_eq!(state, "exited");
    assert_eq!(reconciliation, "observed");
    let exit_code: i64 = connection
        .query_row(
            "SELECT json_extract(payload_json, '$.data.details.code') \
             FROM events \
             WHERE event_type = 'session.lifecycle_changed' \
               AND actor = 'foreground' \
             ORDER BY sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("the definitive foreground exit code should be durable");
    assert_eq!(exit_code, 23);
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn operator_commands_drive_the_minimum_delegation_flow() {
    let fixture = TestEnvironment::new();

    const FIRST_LAUNCH: &str = "co-01ARZ3NDEKTSV4RRFFQ69G5FB0";
    fixture.launch(&["--operation-id", FIRST_LAUNCH]);
    let initial_status = fixture.run_json(&["status", "--json"]);
    let run_id = initial_status["data"]["run_id"]
        .as_str()
        .expect("status should identify the run")
        .to_owned();
    assert_eq!(initial_status["data"]["agents"][0]["name"], "lead");
    assert_eq!(initial_status["data"]["agents"][0]["state"], "exited");
    let mut replay = fixture.command();
    replay.args(["--operation-id", FIRST_LAUNCH]);
    let replay = run(replay);
    assert_eq!(replay.status.code(), Some(5));
    assert!(replay.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&replay.stderr)
            .contains("has already attempted its provider launch")
    );
    fixture.launch(&["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FBD"]);
    let reconnected = fixture.run_json(&["status", "--json"]);
    assert_eq!(reconnected["data"]["run_id"], run_id);
    assert_eq!(reconnected["data"]["agents"][0]["name"], "lead");
    assert_eq!(reconnected["data"]["agents"][0]["state"], "exited");

    let identity = fixture.run_json(&["whoami", "--json"]);
    assert_eq!(identity["data"]["run_id"], run_id);
    assert_eq!(identity["data"]["channel"], "operator");
    assert!(identity["data"]["agent"].is_null());

    let task = fixture.run_json(&[
        "task",
        "create",
        "Implement parser",
        "--description",
        "Add parsing tests first.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB1",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("task creation should return an ID")
        .to_owned();
    assert!(task["operation_id"].as_str().is_some());
    let retried_task = fixture.run_json(&[
        "task",
        "create",
        "Implement parser",
        "--description",
        "Add parsing tests first.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB1",
        "--json",
    ]);
    assert_eq!(retried_task, task);

    let ready = fixture.run_json(&["task", "ready", "--json"]);
    assert_eq!(ready["data"]["tasks"][0]["id"], task_id);
    assert_eq!(ready["data"]["tasks"][0]["project"], "primary");

    let downstream = fixture.run_json(&[
        "task",
        "create",
        "Document parser",
        "--after",
        &task_id,
        "--json",
    ]);
    let downstream_id = downstream["data"]["task"]["id"]
        .as_str()
        .expect("downstream task creation should return an ID");
    let ready = fixture.run_json(&["task", "ready", "--json"]);
    assert_eq!(ready["data"]["tasks"].as_array().map(Vec::len), Some(1));
    assert_eq!(ready["data"]["tasks"][0]["id"], task_id);
    assert_ne!(ready["data"]["tasks"][0]["id"], downstream_id);

    let mut premature_close = fixture.command();
    premature_close.args([
        "task",
        "close",
        &task_id,
        "--summary",
        "Not submitted yet.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB5",
        "--json",
    ]);
    let output = run(premature_close);
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the lifecycle rejection should be JSON");
    assert_eq!(error["operation_id"], "co-01ARZ3NDEKTSV4RRFFQ69G5FB5");
    assert_eq!(error["error"]["code"], "conflict");

    let spawn = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB2",
        "--json",
    ]);
    assert_eq!(spawn["data"]["agent"]["name"], "worker-1");
    assert_eq!(spawn["data"]["agent"]["state"], "running");
    assert_eq!(spawn["data"]["task_id"], task_id);
    assert!(spawn["data"]["assignment_id"].as_str().is_some());
    let session_id = spawn["data"]["session_id"]
        .as_str()
        .expect("spawn should return a session ID");
    let assignment_id = spawn["data"]["assignment_id"]
        .as_str()
        .expect("spawn should return an assignment ID");
    let database = fixture
        .state
        .join("coterie/runs")
        .join(&run_id)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the active run database should be readable");
    let (provider_session_id, session_reconciliation): (
        Option<String>,
        String,
    ) = connection
        .query_row(
            "SELECT provider_session_id, reconciliation_state \
                 FROM sessions WHERE id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the session observation should be durable");
    let worker_process_id = provider_session_id
        .as_deref()
        .and_then(|provider_id| provider_id.strip_prefix("process:"))
        .and_then(|process_id| process_id.parse::<u32>().ok())
        .expect("the Codex worker should have a process identity");
    assert_eq!(session_reconciliation, "observed");
    let (workspace_kind, workspace_path, workspace_reconciliation, base_commit): (
        String,
        Vec<u8>,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT kind, path, state, base_commit FROM workspaces WHERE assignment_id = ?1",
            [assignment_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("the workspace observation should be durable");
    assert_eq!(workspace_kind, "worktree");
    assert_eq!(workspace_reconciliation, "observed");
    let workspace_path =
        Path::new(std::ffi::OsStr::from_bytes(&workspace_path));
    assert!(
        workspace_path
            .starts_with(fixture.state.join("coterie/runs").join(&run_id)),
        "the assignment workspace should be rooted in private run state"
    );
    let project_repository = Repository::open(&fixture.project)
        .expect("the target repository should open");
    assert_eq!(
        project_repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("the target HEAD should resolve")
            .id()
            .to_string(),
        base_commit
    );
    let reference_name = format!("refs/heads/coterie/{run_id}/{assignment_id}");
    assert_eq!(
        project_repository
            .find_reference(&reference_name)
            .expect("the assignment reference should exist")
            .target()
            .map(|oid| oid.to_string())
            .as_deref(),
        Some(base_commit.as_str())
    );
    let workspace_repository = Repository::open(workspace_path)
        .expect("the assignment worktree should open");
    assert_eq!(
        workspace_repository
            .head()
            .expect("the assignment HEAD should resolve")
            .name()
            .expect("the assignment HEAD should be UTF-8"),
        reference_name
    );
    let retried_spawn = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB2",
        "--json",
    ]);
    assert_eq!(retried_spawn, spawn);

    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["run_id"], run_id);
    assert_eq!(status["data"]["status"], "active");
    assert_eq!(status["data"]["agents"].as_array().map(Vec::len), Some(2));
    assert_eq!(status["data"]["tasks"]["in_progress"], 1);

    let prime = fixture.run_json(&["prime", "--json"]);
    assert_eq!(prime["data"]["identity"]["channel"], "operator");
    assert_eq!(prime["data"]["projects"][0]["alias"], "primary");
    assert_eq!(prime["data"]["peers"].as_array().map(Vec::len), Some(2));
    let tasks = prime["data"]["tasks"]
        .as_array()
        .expect("prime should include durable tasks");
    assert_eq!(tasks.len(), 2);
    let downstream = tasks
        .iter()
        .find(|task| task["id"] == downstream_id)
        .expect("prime should include the blocked downstream task");
    assert_eq!(downstream["ready"], false);
    assert_eq!(downstream["unresolved_dependencies"][0], task_id);

    let sent = fixture.run_json(&[
        "send",
        "worker-1",
        "Check the parser edge cases.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB3",
        "--json",
    ]);
    assert_eq!(sent["data"]["recipient"]["name"], "worker-1");
    assert_eq!(sent["data"]["sequence"], 1);
    let retried_send = fixture.run_json(&[
        "send",
        "worker-1",
        "Check the parser edge cases.",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FB3",
        "--json",
    ]);
    assert_eq!(retried_send, sent);

    let mut logs = None;
    wait_until("the worker transcript", || {
        let observed = fixture.run_json(&["logs", "worker-1", "--json"]);
        let ready = observed["data"]["transcript"]
            .as_str()
            .is_some_and(|transcript| transcript.contains("thread.started"));
        if ready {
            logs = Some(observed);
        }
        ready
    });
    let logs = logs.expect("the worker transcript should be observed");
    assert_eq!(logs["data"]["agent"]["name"], "worker-1");
    assert!(
        logs["data"]["transcript"]
            .as_str()
            .is_some_and(|transcript| transcript.contains("thread.started"))
    );

    let events = fixture.run_json(&["events", "--json"]);
    let events = events["data"]["events"]
        .as_array()
        .expect("the typed event stream should be an array");
    assert!(!events.is_empty());
    assert!(events.windows(2).all(|events| {
        events[0]["sequence"].as_u64() < events[1]["sequence"].as_u64()
    }));
    assert!(events.iter().all(|event| {
        event["run_id"] == run_id && event["payload"]["schema_version"] == 1
    }));
    for event_type in [
        "run.started",
        "project.attached",
        "agent.created",
        "session.started",
        "session.reconciliation_changed",
        "session.lifecycle_changed",
        "task.created",
        "task.claimed",
        "assignment.created",
        "workspace.desired",
        "workspace.reconciliation_changed",
        "message.sent",
    ] {
        assert!(
            events.iter().any(|event| event["event_type"] == event_type),
            "the runtime should normalize `{event_type}`"
        );
    }

    fs::write(workspace_path.join("uncommitted.txt"), "recoverable work\n")
        .expect("the assignment worktree should become dirty");
    let stopped = fixture.run_json(&["stop", "--json"]);
    let drained = rusqlite::Connection::open(
        fixture
            .state
            .join("coterie/runs")
            .join(run_id.as_str())
            .join("state.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        drained
            .query_row(
                "SELECT count(*) FROM assignments WHERE state = 'draining'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(drained.query_row("SELECT count(*) FROM events WHERE event_type = 'assignment.lifecycle_changed' AND json_extract(payload_json, '$.data.state') = 'draining'", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(stopped["data"]["run_id"], run_id);
    assert_eq!(stopped["data"]["status"], "stopped");
    assert!(
        workspace_path.exists(),
        "stopping must preserve a dirty, unintegrated worker workspace"
    );
    assert_eq!(
        fs::read_to_string(workspace_path.join("uncommitted.txt"))
            .expect("recoverable work should remain readable"),
        "recoverable work\n"
    );
    assert!(
        project_repository.find_reference(&reference_name).is_ok(),
        "stopping must preserve the owned reference while work is recoverable"
    );
    assert!(
        !Path::new("/proc")
            .join(worker_process_id.to_string())
            .exists(),
        "stopping must terminate and reap the active Codex worker"
    );
}

#[test]
fn foreground_lead_completes_the_codex_worker_loop_through_validation() {
    let fixture = TestEnvironment::new();
    let capture = fixture.root.join("lead-environment");
    let mut command = fixture.command();
    command
        .env("COTERIE_FAKE_MODE", "contract")
        .env("COTERIE_FAKE_CAPTURE", &capture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut foreground = command.spawn().expect("Coterie should start");
    wait_until("the foreground lead environment", || capture.exists());
    let lead_environment = captured_environment(&capture);
    let identity =
        fixture.run_agent_json(&["whoami", "--json"], &lead_environment);
    let run_id = identity["data"]["run_id"]
        .as_str()
        .expect("the lead identity should identify the run")
        .to_owned();
    let lead_id = identity["data"]["agent"]["id"]
        .as_str()
        .expect("the lead identity should include its agent ID")
        .to_owned();
    assert_eq!(identity["data"]["channel"], "agent");
    assert_eq!(identity["data"]["agent"]["role"], "lead");

    let task = fixture.run_agent_json(
        &["task", "create", "Implement the worker result", "--json"],
        &lead_environment,
    );
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("task creation should return an ID")
        .to_owned();
    let spawn = fixture.run_agent_json(
        &["spawn", "worker", "--task", &task_id, "--json"],
        &lead_environment,
    );
    let assignment_id = spawn["data"]["assignment_id"]
        .as_str()
        .expect("spawn should return an assignment ID")
        .to_owned();

    fixture.run_agent_json(
        &[
            "send",
            "worker-1",
            "Include the requested result and tests.",
            "--json",
        ],
        &lead_environment,
    );
    let database = fixture
        .state
        .join("coterie/runs")
        .join(&run_id)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the active run database should open");
    let workspace_bytes: Vec<u8> = connection
        .query_row(
            "SELECT path FROM workspaces WHERE assignment_id = ?1",
            [&assignment_id],
            |row| row.get(0),
        )
        .expect("the assignment workspace should be durable");
    drop(connection);
    let workspace = Path::new(std::ffi::OsStr::from_bytes(&workspace_bytes));
    assert!(
        workspace.starts_with(fixture.state.join("coterie/runs").join(&run_id)),
        "the worker must run in private state, not the target worktree"
    );
    assert_ne!(workspace, fixture.project);
    assert!(
        Repository::open(workspace)
            .expect("the worker repository should open")
            .is_worktree(),
        "the worker workspace should be an isolated Git worktree"
    );
    let result_commit = commit_file(
        workspace,
        Path::new("result.txt"),
        "worker result\n",
        "implement worker result",
    );
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    fs::write(socket.with_extension("sock.release"), b"finish")
        .expect("the scripted worker should be released");

    let mut submitted = None;
    wait_until("the submitted assignment", || {
        let prime =
            fixture.run_agent_json(&["prime", "--json"], &lead_environment);
        submitted = prime["data"]["tasks"]
            .as_array()
            .expect("prime should return durable tasks")
            .iter()
            .find(|task| task["id"] == task_id && task["status"] == "submitted")
            .cloned();
        submitted.is_some()
    });
    let submitted = submitted.expect("prime should return the submitted task");
    assert_eq!(submitted["status"], "submitted");
    assert_eq!(submitted["result"]["status"], "completed");
    assert_eq!(submitted["result"]["summary"], "Implemented and tested.");
    assert_eq!(submitted["result"]["result_commit"], result_commit);

    let mut logs = None;
    wait_until("the complete worker transcript", || {
        let observed = fixture
            .run_agent_json(&["logs", "worker-1", "--json"], &lead_environment);
        let complete = observed["data"]["transcript"]
            .as_str()
            .is_some_and(|transcript| transcript.contains("turn.completed"));
        if complete {
            logs = Some(observed);
        }
        complete
    });
    let logs = logs.expect("the complete worker transcript should be observed");
    let transcript = logs["data"]["transcript"]
        .as_str()
        .expect("the worker transcript should be returned");
    assert!(transcript.contains("thread.started"));
    assert!(transcript.contains("turn.completed"));

    let mut premature_close = fixture.agent_command(&lead_environment);
    premature_close.args([
        "task",
        "close",
        &task_id,
        "--summary",
        "Validated before integration.",
        "--json",
    ]);
    let premature_close = run(premature_close);
    assert_eq!(premature_close.status.code(), Some(5));
    let error: Value = serde_json::from_slice(&premature_close.stderr)
        .expect("the integration requirement should be a JSON diagnostic");
    assert_eq!(error["error"]["code"], "conflict");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not been integrated"))
    );

    let integrated = fixture.run_agent_json(
        &[
            "workspace",
            "integrate",
            "--assignment",
            &assignment_id,
            "--json",
        ],
        &lead_environment,
    );
    assert_eq!(
        integrated["data"]["integration"]["result_commit"],
        result_commit
    );
    let target_commit = integrated["data"]["integration"]["target_commit"]
        .as_str()
        .expect("integration should return the target commit")
        .to_owned();
    assert_eq!(
        fs::read_to_string(fixture.project.join("result.txt"))
            .expect("the integrated result should be readable"),
        "worker result\n"
    );
    let target_repository = Repository::open(&fixture.project)
        .expect("the target repository should open for validation");
    assert_eq!(
        target_repository
            .head()
            .and_then(|head| head.peel_to_commit())
            .expect("the validated target commit should resolve")
            .id()
            .to_string(),
        target_commit
    );
    assert_repository_clean(&fixture.project);

    let closed = fixture.run_agent_json(
        &[
            "task",
            "close",
            &task_id,
            "--summary",
            "Integrated result and validated its tests.",
            "--json",
        ],
        &lead_environment,
    );
    assert_eq!(closed["data"]["task"]["status"], "closed");
    assert_eq!(
        closed["data"]["task"]["result"]["integration"]["target_commit"],
        target_commit
    );
    assert_eq!(
        closed["data"]["task"]["result"]["validation_summary"],
        "Integrated result and validated its tests."
    );

    let events = fixture.run_json(&["events", "--json"]);
    let events = events["data"]["events"]
        .as_array()
        .expect("events should be returned");
    for event_type in [
        "message.sent",
        "message.acknowledged",
        "workspace.integrated",
        "task.lifecycle_changed",
    ] {
        assert!(
            events.iter().any(|event| event["event_type"] == event_type),
            "the completed loop should contain `{event_type}`"
        );
    }
    for event_type in [
        "task.created",
        "task.claimed",
        "workspace.integration_desired",
        "workspace.integrated",
    ] {
        assert!(
            events.iter().any(|event| {
                event["event_type"] == event_type && event["actor"] == lead_id
            }),
            "the lead should author `{event_type}`"
        );
    }
    assert!(events.iter().any(|event| {
        event["event_type"] == "task.lifecycle_changed"
            && event["actor"] == lead_id
            && event["payload"]["data"]["status"] == "closed"
    }));

    foreground
        .stdin
        .take()
        .expect("the live lead stdin should be piped")
        .write_all(b"exit\n")
        .expect("the live lead should accept terminal input");
    let foreground = foreground
        .wait_with_output()
        .expect("the foreground lead should finish");
    assert!(foreground.status.success());
    assert_eq!(foreground.stdout, b"stdout:exit\n");
    assert_eq!(foreground.stderr, b"stderr:exit\n");
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn worktree_workers_fail_closed_for_non_git_projects() {
    let fixture = TestEnvironment::new_plain();
    fixture.launch(&[]);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Git-only work",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBA",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("the task ID should be returned");
    let mut spawn = fixture.command();
    spawn.args([
        "spawn",
        "worker",
        "--task",
        task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBB",
        "--json",
    ]);

    let output = run(spawn);

    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)
        .expect("the workspace rejection should be JSON");
    assert_eq!(error["error"]["code"], "unavailable");
    assert!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("not Git-backed"))
    );
    let status = fixture.run_json(&["status", "--json"]);
    assert_eq!(status["data"]["tasks"]["open"], 1);
    assert_eq!(status["data"]["agents"].as_array().map(Vec::len), Some(1));
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn private_supervisor_entrypoint_publishes_a_reachable_run() {
    let fixture = TestEnvironment::new();
    let log_path = fixture.root.join("supervisor.log");
    let log = fs::File::create(&log_path).expect("the log should be created");
    let mut command = fixture.command();
    command
        .arg("__supervisor")
        .arg(RUN_ID)
        .arg(PROJECT_ID)
        .arg(&fixture.project)
        .stderr(log);
    let mut child = command.spawn().expect("the supervisor should start");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fixture.index_entry_count() == 1 {
            break;
        }
        if let Some(status) =
            child.try_wait().expect("status should be readable")
        {
            panic!(
                "supervisor exited with {status}: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        assert!(Instant::now() < deadline, "supervisor startup timed out");
        thread::sleep(Duration::from_millis(20));
    }

    let foreground = run(fixture.connect_command());
    assert!(
        foreground.status.success(),
        "the indexed supervisor should be reachable: {foreground:?}"
    );
    let mut shutdown = fixture.command();
    shutdown.arg("__supervisor-shutdown");
    let shutdown = run(shutdown);
    assert!(
        shutdown.status.success(),
        "restart shutdown failed: {shutdown:?}"
    );
    assert!(
        child.wait().expect("the supervisor should exit").success(),
        "supervisor failed: {}",
        fs::read_to_string(log_path).unwrap_or_default()
    );
}

#[test]
fn concurrent_launches_share_one_supervisor_and_clean_shutdown_retires_it() {
    let fixture = TestEnvironment::new();
    let first = fixture.connect_command();
    let second = fixture.connect_command();
    let first = thread::spawn(move || run(first));
    let second = thread::spawn(move || run(second));

    let first = first.join().expect("the first launch should not panic");
    let second = second.join().expect("the second launch should not panic");
    assert!(
        first.status.success() && second.status.success(),
        "concurrent launches failed:\nfirst: {first:?}\nsecond: {second:?}\nfiles: {:?}",
        fixture.files()
    );

    let index_path = fixture.only_index_entry();
    assert_eq!(mode(&index_path), 0o600);
    let index: Value = serde_json::from_slice(
        &fs::read(&index_path).expect("the index should be readable"),
    )
    .expect("the index should contain JSON");
    let run_id = index["run_id"]
        .as_str()
        .expect("the index should identify its run");
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{run_id}.sock"));
    let database = fixture
        .state
        .join("coterie/runs")
        .join(run_id)
        .join("state.sqlite3");
    assert!(socket.exists(), "the supervisor socket should be live");
    assert!(
        database.is_file(),
        "the supervisor should own a durable run database"
    );
    assert_eq!(
        fs::read_dir(fixture.state.join("coterie/runs"))
            .expect("the run directory should be readable")
            .count(),
        1,
        "losing startup contenders must not leave run state"
    );

    let mut shutdown_command = fixture.command();
    shutdown_command.arg("__supervisor-shutdown");
    let shutdown = run(shutdown_command);
    assert!(shutdown.status.success(), "shutdown failed: {shutdown:?}");
    wait_until("supervisor retirement", || {
        !index_path.exists() && !socket.exists()
    });
    let connection = rusqlite::Connection::open(database)
        .expect("the stopped run database should open");
    let (status, stopped_at): (String, Option<i64>) = connection
        .query_row(
            "SELECT status, stopped_at FROM runs WHERE id = ?1",
            [run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("the durable run should be readable");
    assert_eq!(status, "stopped");
    assert!(stopped_at.is_some());
}

#[test]
fn completed_shutdown_retires_stale_coordination_before_a_new_run() {
    let fixture = TestEnvironment::new();
    assert!(run(fixture.connect_command()).status.success());
    let index_path = fixture.only_index_entry();
    let index_bytes = fs::read(&index_path).unwrap();
    let indexed: Value = serde_json::from_slice(&index_bytes).unwrap();
    let old_run = indexed["run_id"].as_str().unwrap();
    fixture.run_json(&["stop", "--json"]);
    let socket = fixture
        .runtime
        .join("coterie")
        .join(format!("{old_run}.sock"));
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    drop(listener);
    fs::write(&index_path, &index_bytes).unwrap();
    fs::set_permissions(&index_path, fs::Permissions::from_mode(0o600))
        .unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let report = fixture.run_json(&["doctor", "--json"]);
    assert!(
        report["data"]["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| {
                check["check"] == "operations" && check["status"] == "ok"
            }),
        "{report}"
    );
    fs::write(
        fixture.project.join("coterie.toml"),
        "[roles.worker]\nmax_instances = 1",
    )
    .unwrap();
    let result = run(fixture.connect_command());
    assert!(result.status.success(), "{result:?}");
    let current: Value =
        serde_json::from_slice(&fs::read(fixture.only_index_entry()).unwrap())
            .unwrap();
    assert_ne!(current["run_id"], indexed["run_id"]);
    assert!(!socket.exists());
    let connection = rusqlite::Connection::open(
        fixture
            .state
            .join("coterie/runs")
            .join(old_run)
            .join("state.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM events WHERE event_type = 'run.stopped'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn stale_index_and_socket_restart_the_same_durable_run() {
    let fixture = TestEnvironment::new();
    let log_path = fixture.root.join("crashed-supervisor.log");
    let log = fs::File::create(&log_path).expect("the log should be created");
    let mut command = fixture.command();
    command
        .arg("__supervisor")
        .arg(RUN_ID)
        .arg(PROJECT_ID)
        .arg(&fixture.project)
        .stdout(std::process::Stdio::null())
        .stderr(log);
    let mut crashed = command.spawn().expect("the supervisor should start");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fixture.index_entry_count() == 1 {
            break;
        }
        if let Some(status) = crashed
            .try_wait()
            .expect("the crashed fixture status should be readable")
        {
            panic!(
                "supervisor exited before publication with {status}: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "supervisor publication timed out"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let stale_index = fixture.only_index_entry();
    let stale_contents =
        fs::read(&stale_index).expect("the index should exist");

    let follow_path = fixture.root.join("restart-follow.jsonl");
    let mut follow_command = fixture.command();
    follow_command
        .args(["events", "--follow", "--json"])
        .stdout(fs::File::create(&follow_path).unwrap())
        .stderr(Stdio::piped());
    let mut follower = follow_command.spawn().unwrap();
    wait_until("initial follow page", || {
        fs::metadata(&follow_path).unwrap().len() > 0
    });

    crashed
        .kill()
        .expect("the owned fixture process should stop");
    crashed.wait().expect("the killed process should be reaped");

    let report = fixture.run_json(&["doctor", "--json"]);
    assert_eq!(report["data"]["report"]["run_id"], RUN_ID);
    let checks = report["data"]["report"]["checks"].as_array().unwrap();
    assert!(checks.iter().any(|check| check["check"] == "supervisor"
        && check["status"] == "unavailable"));
    assert!(
        checks
            .iter()
            .any(|check| check["check"] == "database_migrations"
                && check["status"] == "ok")
    );
    assert_eq!(fs::read(&stale_index).unwrap(), stale_contents);
    assert!(
        std::os::unix::net::UnixStream::connect(
            fixture
                .runtime
                .join("coterie")
                .join(format!("{RUN_ID}.sock"))
        )
        .is_err()
    );

    let restart = run(fixture.connect_command());
    assert!(restart.status.success(), "restart failed: {restart:?}");
    assert_eq!(
        fs::read(&stale_index).expect("the index should be republished"),
        stale_contents,
        "recovery should preserve the indexed run and project IDs"
    );
    fixture.run_json(&["task", "create", "After recovery", "--json"]);
    wait_until("reconnected follower", || {
        fs::read_to_string(&follow_path)
            .unwrap()
            .contains("After recovery")
    });

    let mut shutdown = fixture.command();
    shutdown.arg("__supervisor-shutdown");
    assert!(run(shutdown).status.success());
    wait_until("restarted supervisor retirement", || !stale_index.exists());
    wait_until("reconnected follower retirement", || {
        follower.try_wait().unwrap().is_some()
    });
    let output = follower.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let sequences = fs::read_to_string(follow_path)
        .unwrap()
        .lines()
        .flat_map(|line| {
            let page: Value = serde_json::from_str(line).unwrap();
            page["data"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|event| event["sequence"].as_u64().unwrap())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(sequences.windows(2).all(|pair| pair[1] == pair[0] + 1));
}

#[test]
fn indexed_recovery_does_not_recreate_a_missing_database() {
    let fixture = TestEnvironment::new();
    let connected = run(fixture.connect_command());
    assert!(connected.status.success(), "{connected:?}");
    let index = fixture.only_index_entry();
    let encoded = fs::read(&index).unwrap();
    let entry: Value = serde_json::from_slice(&encoded).unwrap();
    fixture.run_json(&["stop", "--json"]);
    fs::write(&index, encoded).unwrap();
    fs::set_permissions(&index, fs::Permissions::from_mode(0o600)).unwrap();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(entry["run_id"].as_str().unwrap())
        .join("state.sqlite3");
    let preserved = database.with_extension("preserved");
    fs::rename(&database, &preserved).unwrap();
    let result = run(fixture.connect_command());
    assert!(!result.status.success());
    assert!(!database.exists());
    assert!(preserved.exists());
    assert_eq!(fixture.index_entry_count(), 1);
    fs::rename(preserved, database).unwrap();
    let recovered = run(fixture.connect_command());
    assert!(recovered.status.success(), "{recovered:?}");
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn doctor_is_read_only_without_a_run_and_refuses_agent_context() {
    let fixture = TestEnvironment::new();
    let files = fixture.files();
    let report = fixture.run_json(&["doctor", "--json"]);
    assert!(report["data"]["report"]["run_id"].is_null());
    assert_eq!(fixture.files(), files);
    let mut command = fixture.command();
    command
        .args(["doctor", "--json"])
        .env("COTERIE_AGENT_ID", "incomplete");
    let output = run(command);
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    assert_eq!(fixture.files(), files);
}

#[test]
fn doctor_checks_live_state_and_reports_permissions_without_repair() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let report = fixture.run_json(&["doctor", "--json"]);
    let checks = report["data"]["report"]["checks"].as_array().unwrap();
    for name in [
        "supervisor",
        "database_migrations",
        "database_integrity",
        "foreign_keys",
        "operations",
        "assignments",
        "task_cycles",
        "provider",
        "project_lease",
    ] {
        assert!(
            checks
                .iter()
                .any(|check| check["check"] == name && check["status"] == "ok"),
            "missing check {name}: {checks:?}"
        );
    }
    let state = fixture.state.join("coterie");
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
    let report = fixture.run_json(&["doctor", "--json"]);
    assert!(
        report["data"]["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["check"] == "runtime_permissions"
                && check["status"] == "error")
    );
    assert_eq!(mode(&state), 0o755);
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn doctor_reports_corrupt_migrations_and_cycles_without_mutation() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let first = fixture.run_json(&["task", "create", "First", "--json"]);
    let second = fixture.run_json(&["task", "create", "Second", "--json"]);
    let run_id = fixture.run_json(&["status", "--json"])["data"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(&run_id)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    let first = first["data"]["task"]["id"].as_str().unwrap();
    let second = second["data"]["task"]["id"].as_str().unwrap();
    for (task, dependency) in [(first, second), (second, first)] {
        connection.execute("INSERT INTO task_dependencies (run_id, task_id, dependency_task_id, created_at) VALUES (?1, ?2, ?3, 1)", [&run_id, task, dependency]).unwrap();
    }
    let report = fixture.run_json(&["doctor", "--json"]);
    assert_eq!(
        report["data"]["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|check| check["check"] == "task_cycles"
                && check["status"] == "warning")
            .count(),
        2
    );
    connection.execute("INSERT INTO schema_migrations (version, name, source) VALUES (999, 'future', 'future')", []).unwrap();
    let before = query_rows(
        &connection,
        "SELECT version, name, source FROM schema_migrations ORDER BY version",
        3,
    );
    let report = fixture.run_json(&["doctor", "--json"]);
    assert!(
        report["data"]["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["check"] == "database_migrations"
                && check["status"] == "error")
    );
    assert_eq!(
        before,
        query_rows(
            &connection,
            "SELECT version, name, source FROM schema_migrations ORDER BY version",
            3
        )
    );
    fixture.run_json(&["stop", "--json"]);
}

#[test]
fn doctor_preserves_work_with_ambiguous_ownership_and_unreadable_transcripts() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let task =
        fixture.run_json(&["task", "create", "Preserved work", "--json"]);
    let worker = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        task["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    let report = fixture.run_json(&["doctor", "--json"]);
    assert!(
        report["data"]["report"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["check"] == "workspace"
                && check["status"] == "ok")
    );
    let run_id = fixture.run_json(&["status", "--json"])["data"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let root = fixture.state.join("coterie/runs").join(run_id);
    let connection =
        rusqlite::Connection::open(root.join("state.sqlite3")).unwrap();
    let workspace: Vec<u8> = connection
        .query_row(
            "SELECT path FROM workspaces WHERE assignment_id = ?1",
            [worker["data"]["assignment_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    let workspace = Path::new(std::ffi::OsStr::from_bytes(&workspace));
    let repository = Repository::open(workspace).unwrap();
    let head = repository.head().unwrap().target().unwrap();
    repository.set_head_detached(head).unwrap();
    fs::write(workspace.join("precious.txt"), "recoverable work").unwrap();
    let transcript = root.join("transcripts").join(format!(
        "{}.jsonl",
        worker["data"]["session_id"].as_str().unwrap()
    ));
    wait_until("transcript", || transcript.exists());
    let preserved = transcript.with_extension("preserved");
    fs::rename(&transcript, &preserved).unwrap();
    std::os::unix::fs::symlink(&preserved, &transcript).unwrap();
    let report = fixture.run_json(&["doctor", "--json"]);
    let checks = report["data"]["report"]["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|check| check["check"] == "workspace"
                && check["status"] != "ok")
    );
    assert!(
        checks.iter().any(|check| check["check"] == "transcript"
            && check["status"] == "error")
    );
    assert!(
        fs::symlink_metadata(&transcript)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read_to_string(workspace.join("precious.txt")).unwrap(),
        "recoverable work"
    );
    fs::remove_file(&transcript).unwrap();
    fs::rename(preserved, transcript).unwrap();
    fixture.run_json(&["stop", "--json"]);
    assert!(workspace.join("precious.txt").exists());
}

#[test]
fn event_following_pages_resume_and_drain_after_socket_retirement() {
    let fixture = TestEnvironment::new();
    fixture.launch(&[]);
    let first = fixture.run_json(&["events", "--limit", "1", "--json"]);
    let cursor = first["data"]["next_cursor"].as_u64().unwrap();
    let path = fixture.root.join("follow.jsonl");
    let mut command = fixture.command();
    command
        .args([
            "events",
            "--follow",
            "--limit",
            "1",
            "--after",
            &cursor.to_string(),
            "--json",
        ])
        .stdout(fs::File::create(&path).unwrap())
        .stderr(Stdio::piped());
    let mut follower = command.spawn().unwrap();
    wait_until("first follower page", || {
        fs::metadata(&path).unwrap().len() > 0
    });
    fixture.run_json(&["task", "create", "A streamed task", "--json"]);
    let complete =
        fixture.run_json(&["events", "--after", &cursor.to_string(), "--json"]);
    let run_id = fixture.run_json(&["status", "--json"])["data"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.run_json(&["stop", "--json"]);
    wait_until("follower terminal page", || {
        follower.try_wait().unwrap().is_some()
    });
    let output = follower.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let pages = fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let events = pages
        .iter()
        .flat_map(|page| page["data"]["events"].as_array().unwrap())
        .collect::<Vec<_>>();
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0]["sequence"].as_u64().unwrap()
                < pair[1]["sequence"].as_u64().unwrap())
    );
    assert_eq!(events[0]["sequence"].as_u64().unwrap(), cursor + 1);
    assert!(
        events
            .iter()
            .any(|event| event["event_type"] == "run.stopped")
    );
    for event in complete["data"]["events"].as_array().unwrap() {
        assert!(events.contains(&event));
    }
    let connection = rusqlite::Connection::open_with_flags(
        fixture
            .state
            .join("coterie/runs")
            .join(run_id)
            .join("state.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let durable_sequences = connection
        .prepare(
            "SELECT sequence FROM events WHERE sequence > ?1 ORDER BY sequence",
        )
        .unwrap()
        .query_map([cursor as i64], |row| row.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event["sequence"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        durable_sequences
    );
}

#[test]
fn mutation_retries_survive_credential_rotation_and_removal() {
    let fixture = TestEnvironment::new();
    let secret = "retry-fixture-api-key";
    let start = |key: Option<&str>| {
        let mut command = fixture.command();
        command
            .args(["__supervisor", RUN_ID, PROJECT_ID])
            .arg(&fixture.project)
            .env_remove("OPENAI_API_KEY")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(key) = key {
            command.env("OPENAI_API_KEY", key);
        }
        let child = command.spawn().unwrap();
        wait_until("supervisor publication", || {
            let mut command = fixture.command();
            command.args(["status", "--json"]);
            run(command).status.success()
        });
        child
    };
    let mut supervisor = start(Some(secret));
    fixture.launch(&[]);
    let create_id = format!("co-{}", ulid::Ulid::generate());
    let send_id = format!("co-{}", ulid::Ulid::generate());
    let close_id = format!("co-{}", ulid::Ulid::generate());
    let create = [
        "task",
        "create",
        secret,
        "--description",
        secret,
        "--group",
        secret,
        "--operation-id",
        &create_id,
        "--json",
    ];
    let created = fixture.run_json(&create);
    let task_id = created["data"]["task"]["id"].as_str().unwrap();
    let send = ["send", "lead", secret, "--operation-id", &send_id, "--json"];
    let sent = fixture.run_json(&send);
    let close = [
        "task",
        "close",
        task_id,
        "--summary",
        secret,
        "--operation-id",
        &close_id,
        "--json",
    ];
    let close_request = || {
        let mut command = fixture.command();
        command.args(close);
        run(command)
    };
    let closed = close_request();
    assert_eq!(closed.status.code(), Some(5));
    let created = fixture.run_json(&create);
    for key in [Some("replacement-fixture-api-key"), None] {
        supervisor.kill().unwrap();
        supervisor.wait().unwrap();
        supervisor = start(key);
        assert_eq!(fixture.run_json(&create), created);
        assert_eq!(fixture.run_json(&send), sent);
        let replayed = close_request();
        assert_eq!(replayed.status.code(), closed.status.code());
        assert_eq!(replayed.stderr, closed.stderr);
        let mut conflict = fixture.command();
        conflict.args([
            "task",
            "create",
            "[REDACTED]",
            "--description",
            "[REDACTED]",
            "--group",
            "[REDACTED]",
            "--operation-id",
            &create_id,
            "--json",
        ]);
        let output = run(conflict);
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(error["error"]["code"], "conflict");
    }
    fixture.run_json(&["stop", "--json"]);
    supervisor.wait().unwrap();
    for entry in
        fs::read_dir(fixture.state.join("coterie/runs").join(RUN_ID)).unwrap()
    {
        let path = entry.unwrap().path();
        if path.is_file() {
            assert!(
                !fs::read(path)
                    .unwrap()
                    .windows(secret.len())
                    .any(|bytes| bytes == secret.as_bytes())
            );
        }
    }
}

#[test]
fn credentials_are_redacted_from_durable_requests_and_worker_output() {
    let fixture = TestEnvironment::new();
    let script = FAKE_CODEX.replace("  printf '{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\\n'", "  printf '{\"type\":\"thread.started\",\"token\":\"%s\",\"key\":\"%s\"}\\n' \"$COTERIE_TOKEN\" \"$OPENAI_API_KEY\"");
    assert_ne!(script, FAKE_CODEX);
    fs::write(fixture.root.join("bin/codex"), script).unwrap();
    let secret = "fixture-api-key-unique";
    let mut command = fixture.connect_command();
    command.env("OPENAI_API_KEY", secret);
    assert!(run(command).status.success());
    let token = format!("cot1_{}", "ab".repeat(32));
    let task = fixture.run_json(&[
        "task",
        "create",
        &format!("{secret} {token}"),
        "--json",
    ]);
    assert_eq!(task["data"]["task"]["title"], "[REDACTED] [REDACTED]");
    let worker = fixture.run_json(&[
        "spawn",
        "reviewer",
        "--task",
        task["data"]["task"]["id"].as_str().unwrap(),
        "--json",
    ]);
    let agent = worker["data"]["agent"]["id"].as_str().unwrap();
    let session = worker["data"]["session_id"].as_str().unwrap();
    wait_until("redacted worker output", || {
        fixture.run_json(&["logs", agent, "--json"])["data"]["transcript"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    });
    let all = fixture.run_json(&["logs", agent, "--json"]);
    let mut transcript = String::new();
    let mut cursor = 0;
    loop {
        let page = fixture.run_json(&[
            "logs",
            agent,
            "--session",
            session,
            "--after",
            &cursor.to_string(),
            "--limit",
            "7",
            "--json",
        ]);
        transcript.push_str(page["data"]["transcript"].as_str().unwrap());
        cursor = page["data"]["next_cursor"].as_u64().unwrap();
        if page["data"]["eof"] == true {
            break;
        }
    }
    assert_eq!(transcript, all["data"]["transcript"].as_str().unwrap());
    assert!(!transcript.contains(secret));
    assert!(!transcript.contains("cot1_"));
    let run_id = fixture.run_json(&["status", "--json"])["data"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let follow_path = fixture.root.join("logs-follow.jsonl");
    let mut command = fixture.command();
    command
        .args(["logs", agent, "--follow", "--limit", "7", "--json"])
        .stdout(fs::File::create(&follow_path).unwrap())
        .stderr(Stdio::piped());
    let mut follower = command.spawn().unwrap();
    wait_until("transcript follower", || {
        fs::metadata(&follow_path).unwrap().len() > 0
    });
    fixture.run_json(&["stop", "--json"]);
    wait_until("transcript follower completion", || {
        follower.try_wait().unwrap().is_some()
    });
    let output = follower.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let pages = fs::read_to_string(&follow_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(
        pages
            .iter()
            .all(|page| page["data"]["session_id"] == session)
    );
    let followed: String = pages
        .iter()
        .map(|page| page["data"]["transcript"].as_str().unwrap())
        .collect();
    assert_eq!(followed, transcript);
    assert_eq!(pages.last().unwrap()["data"]["terminal"], true);
    let database = fixture
        .state
        .join("coterie/runs")
        .join(run_id)
        .join("state.sqlite3");
    let bytes = fs::read(database).unwrap();
    assert!(
        !bytes
            .windows(secret.len())
            .any(|candidate| candidate == secret.as_bytes())
    );
    assert!(
        !bytes
            .windows(token.len())
            .any(|candidate| candidate == token.as_bytes())
    );
}

#[test]
fn restart_marks_a_vanished_codex_worker_lost() {
    let fixture = TestEnvironment::new();
    let log_path = fixture.root.join("reconciliation-supervisor.log");
    let log = fs::File::create(&log_path).expect("the log should be created");
    let mut command = fixture.command();
    command
        .arg("__supervisor")
        .arg(RUN_ID)
        .arg(PROJECT_ID)
        .arg(&fixture.project)
        .stdout(std::process::Stdio::null())
        .stderr(log);
    let mut crashed = command.spawn().expect("the supervisor should start");
    wait_until("supervisor publication", || {
        fixture.index_entry_count() == 1
    });

    fixture.launch(&["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FB8"]);
    let task = fixture.run_json(&[
        "task",
        "create",
        "Implement parser",
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBA",
        "--json",
    ]);
    let task_id = task["data"]["task"]["id"]
        .as_str()
        .expect("task creation should return an ID")
        .to_owned();
    let downstream = fixture.run_json(&[
        "task",
        "create",
        "Document parser",
        "--after",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBB",
        "--json",
    ]);
    let downstream_id = downstream["data"]["task"]["id"]
        .as_str()
        .expect("dependent task creation should return an ID")
        .to_owned();
    let spawn = fixture.run_json(&[
        "spawn",
        "worker",
        "--task",
        &task_id,
        "--operation-id",
        "co-01ARZ3NDEKTSV4RRFFQ69G5FBC",
        "--json",
    ]);
    let worker_session_id = spawn["data"]["session_id"]
        .as_str()
        .expect("spawn should return a session ID")
        .to_owned();
    let tasks_before =
        fixture.run_json(&["prime", "--json"])["data"]["tasks"].clone();
    assert!(
        tasks_before
            .as_array()
            .is_some_and(|tasks| tasks.len() == 2)
    );
    assert!(tasks_before.as_array().is_some_and(|tasks| {
        tasks.iter().any(|task| {
            task["id"] == downstream_id
                && task["unresolved_dependencies"][0] == task_id
        })
    }));
    let transcript_before =
        fixture.run_json(&["logs", "worker-1", "--json"])["data"]["transcript"]
            .clone();
    let database = fixture
        .state
        .join("coterie/runs")
        .join(RUN_ID)
        .join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database)
        .expect("the active run database should open");
    let session_id: String = connection
        .query_row(
            "SELECT id FROM sessions WHERE process_owner = 'foreground' \
             ORDER BY created_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("the foreground session should be durable");
    let worker_process_id: u32 = connection
        .query_row(
            "SELECT provider_session_id FROM sessions WHERE id = ?1",
            [&worker_session_id],
            |row| row.get::<_, String>(0),
        )
        .expect("the worker process identity should be durable")
        .strip_prefix("process:")
        .and_then(|process_id| process_id.parse().ok())
        .expect("the worker should have a process identity");
    let durable_before = durable_restart_snapshot(&connection);
    assert_eq!(durable_before.tasks.len(), 2);
    assert_eq!(durable_before.dependencies.len(), 1);
    assert_eq!(durable_before.transcript_references.len(), 2);
    assert_eq!(durable_before.operations.len(), 4);
    assert_eq!(durable_before.workspaces.len(), 1);
    drop(connection);

    crashed
        .kill()
        .expect("the owned fixture process should stop");
    crashed.wait().expect("the killed process should be reaped");
    wait_until("the orphaned Codex worker to exit", || {
        !Path::new("/proc")
            .join(worker_process_id.to_string())
            .exists()
    });
    let restart = run(fixture.connect_command());
    assert!(restart.status.success(), "restart failed: {restart:?}");

    let connection = rusqlite::Connection::open(&database)
        .expect("the restarted run database should open");
    assert_eq!(durable_restart_snapshot(&connection), durable_before);
    let lost_sessions: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sessions \
             WHERE id IN (?1, ?2) AND state = 'lost' \
               AND reconciliation_state = 'lost'",
            [&session_id, &worker_session_id],
            |row| row.get(0),
        )
        .expect("the reconciled sessions should remain durable");
    assert_eq!(lost_sessions, 1);
    let observed_workspaces: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM workspaces WHERE state = 'observed'",
            [],
            |row| row.get(0),
        )
        .expect("the reconciled workspace should remain durable");
    assert_eq!(observed_workspaces, 1);
    let lost_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events \
             WHERE event_type = 'session.reconciliation_changed' \
               AND subject = ?1 \
               AND json_extract(payload_json, '$.data.state') = 'lost'",
            [&worker_session_id],
            |row| row.get(0),
        )
        .expect("the reconciliation event should be queryable");
    assert_eq!(lost_events, 1);
    drop(connection);

    let tasks_after =
        fixture.run_json(&["prime", "--json"])["data"]["tasks"].clone();
    assert_eq!(tasks_after, tasks_before);
    let transcript_after =
        fixture.run_json(&["logs", "worker-1", "--json"])["data"]["transcript"]
            .clone();
    assert_eq!(transcript_after, transcript_before);

    let mut replay = fixture.command();
    replay.args(["--operation-id", "co-01ARZ3NDEKTSV4RRFFQ69G5FB8"]);
    let replay = run(replay);
    assert_eq!(replay.status.code(), Some(5));
    assert!(replay.stdout.is_empty());
    assert!(String::from_utf8_lossy(&replay.stderr).contains("operation"));

    let mut shutdown = fixture.command();
    shutdown.arg("__supervisor-shutdown");
    assert!(run(shutdown).status.success());
    wait_until("restarted supervisor retirement", || {
        fixture.index_entry_count() == 0
    });
}

#[derive(Debug, PartialEq)]
struct DurableRestartSnapshot {
    tasks: Vec<Vec<rusqlite::types::Value>>,
    dependencies: Vec<Vec<rusqlite::types::Value>>,
    transcript_references: Vec<Vec<rusqlite::types::Value>>,
    operations: Vec<Vec<rusqlite::types::Value>>,
    workspaces: Vec<Vec<rusqlite::types::Value>>,
}

fn durable_restart_snapshot(
    connection: &rusqlite::Connection,
) -> DurableRestartSnapshot {
    DurableRestartSnapshot {
        tasks: query_rows(
            connection,
            "SELECT id, run_id, project_id, group_id, title, description, \
                    status, result_json, created_at, updated_at \
             FROM tasks ORDER BY id",
            10,
        ),
        dependencies: query_rows(
            connection,
            "SELECT run_id, task_id, dependency_task_id, created_at \
             FROM task_dependencies ORDER BY task_id, dependency_task_id",
            4,
        ),
        transcript_references: query_rows(
            connection,
            "SELECT id, run_id, agent_id, generation, provider, \
                    provider_session_id, transcript_path, created_at \
             FROM sessions ORDER BY id",
            8,
        ),
        operations: query_rows(
            connection,
            "SELECT id, run_id, kind, actor_agent_id, status, request_json, \
                    result_json, attempt_count, reconciliation_state, \
                    reconciliation_attempt_count, reconciliation_error, \
                    reconciled_at, created_at, updated_at \
             FROM operations ORDER BY id",
            14,
        ),
        workspaces: query_rows(
            connection,
            "SELECT assignment_id, run_id, project_id, kind, path, \
                    base_commit, result_commit, target_commit, created_at \
             FROM workspaces ORDER BY assignment_id",
            9,
        ),
    }
}

fn query_rows(
    connection: &rusqlite::Connection,
    sql: &str,
    column_count: usize,
) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = connection
        .prepare(sql)
        .expect("the durable snapshot query should prepare");
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("the durable snapshot query should execute")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("the durable snapshot rows should decode")
}

fn run(mut command: Command) -> std::process::Output {
    command.output().expect("Coterie should execute")
}

fn wait_until(description: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "{description} timed out");
        thread::sleep(Duration::from_millis(20));
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .expect("metadata should be available")
        .permissions()
        .mode()
        & 0o777
}

fn commit_file(
    worktree: &Path,
    relative_path: &Path,
    contents: &str,
    message: &str,
) -> String {
    fs::write(worktree.join(relative_path), contents)
        .expect("the worker result should be written");
    let repository =
        Repository::open(worktree).expect("the worker repository should open");
    let mut index = repository.index().expect("the index should open");
    index
        .add_path(relative_path)
        .expect("the worker result should enter the index");
    index.write().expect("the index should be persisted");
    let tree_id = index.write_tree().expect("the tree should be written");
    let tree = repository
        .find_tree(tree_id)
        .expect("the worker result tree should resolve");
    let parent = repository
        .head()
        .and_then(|head| head.peel_to_commit())
        .expect("the worker base commit should resolve");
    let signature = Signature::now("Coterie Worker", "worker@example.invalid")
        .expect("the worker signature should be valid");
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &[&parent],
        )
        .expect("the worker result commit should be created")
        .to_string()
}

fn assert_repository_clean(project: &Path) {
    let repository =
        Repository::open(project).expect("the fixture repository should open");
    let mut options = StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repository
        .statuses(Some(&mut options))
        .expect("the fixture repository status should be readable");
    assert!(
        statuses.is_empty(),
        "the fixture repository should be clean, but had {} status entries",
        statuses.len()
    );
}

fn captured_environment(path: &Path) -> Vec<(String, String)> {
    fs::read(path)
        .expect("the foreground environment should be captured")
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .filter_map(|record| record.strip_prefix(b"env:"))
        .map(|record| {
            let record = std::str::from_utf8(record)
                .expect("the captured environment should be UTF-8");
            let (name, value) = record
                .split_once('=')
                .expect("the captured environment should contain assignments");
            (name.to_owned(), value.to_owned())
        })
        .collect()
}

fn environment_value(environment: &[(String, String)], name: &str) -> String {
    environment
        .iter()
        .find_map(|(candidate, value)| {
            (candidate == name).then(|| value.clone())
        })
        .unwrap_or_else(|| {
            panic!("the captured environment should contain {name}")
        })
}

struct TestEnvironment {
    root: PathBuf,
    runtime: PathBuf,
    state: PathBuf,
    project: PathBuf,
    path: std::ffi::OsString,
}

impl TestEnvironment {
    fn new() -> Self {
        Self::new_with_repository(true)
    }

    fn new_plain() -> Self {
        Self::new_with_repository(false)
    }

    fn new_with_repository(initialize_repository: bool) -> Self {
        let fixture_id =
            format!("{}-{}", std::process::id(), ulid::Ulid::generate());
        let root = std::env::temp_dir().join(format!("ct-{fixture_id}"));
        // The kernel bounds Unix socket paths, so keep this fixture independent
        // of an arbitrarily long `TMPDIR` used for its other files.
        let runtime =
            Path::new("/tmp").join(format!("ct-runtime-{fixture_id}"));
        let state = root.join("state");
        let project = root.join("project");
        let bin = root.join("bin");
        let socket = runtime.join("coterie").join(format!("{RUN_ID}.sock"));
        assert!(
            socket.as_os_str().as_bytes().len() <= 107,
            "the test runtime must support Coterie's Unix socket path: {socket:?}"
        );
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&runtime)
            .expect("the runtime directory should be created");
        fs::create_dir_all(&project)
            .expect("the project directory should be created");
        if initialize_repository {
            let repository = Repository::init(&project)
                .expect("the fixture repository should initialize");
            fs::write(project.join("README.md"), "fixture\n")
                .expect("the fixture file should be written");
            let mut index = repository.index().expect("the index should open");
            index
                .add_path(Path::new("README.md"))
                .expect("the fixture file should enter the index");
            index.write().expect("the index should be persisted");
            let tree_id =
                index.write_tree().expect("the tree should be written");
            let tree = repository
                .find_tree(tree_id)
                .expect("the fixture tree should resolve");
            let signature =
                Signature::now("Coterie Test", "test@example.invalid")
                    .expect("the fixture signature should be valid");
            repository
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    "initial",
                    &tree,
                    &[],
                )
                .expect("the initial fixture commit should be created");
            drop(tree);
            drop(index);
            drop(repository);
        }
        fs::create_dir(&bin).expect("the fixture bin directory should exist");
        let codex = bin.join("codex");
        fs::write(&codex, FAKE_CODEX)
            .expect("the fake Codex executable should be written");
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o700))
            .expect("the fake Codex executable should be private");
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_coterie"),
            bin.join("coterie"),
        )
        .expect("the scripted worker should find the Coterie binary");
        let path = std::env::join_paths(std::iter::once(bin).chain(
            std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ),
        ))
        .expect("the fixture PATH should be representable");
        Self {
            root,
            runtime,
            state,
            project,
            path,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_coterie"));
        command
            .current_dir(&self.project)
            .env("XDG_RUNTIME_DIR", &self.runtime)
            .env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("PATH", &self.path);
        command
    }

    fn agent_command(&self, environment: &[(String, String)]) -> Command {
        let mut command = self.command();
        command.envs(environment.iter().map(|(name, value)| (name, value)));
        command
    }

    fn launch(&self, arguments: &[&str]) -> std::process::Output {
        let mut command = self.command();
        command.args(arguments);
        let output = run(command);
        assert!(
            output.status.success(),
            "foreground launch {arguments:?} failed: {output:?}"
        );
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "the foreground wrapper should not write around the TUI: {output:?}"
        );
        output
    }

    fn connect_command(&self) -> Command {
        let mut command = self.command();
        command.arg("__supervisor-connect");
        command
    }

    fn run_json(&self, arguments: &[&str]) -> Value {
        let mut command = self.command();
        command.args(arguments);
        let output = run(command);
        assert!(
            output.status.success(),
            "command {arguments:?} failed: {output:?}"
        );
        assert!(
            output.stderr.is_empty(),
            "successful JSON should not write diagnostics: {output:?}"
        );
        serde_json::from_slice(&output.stdout)
            .expect("the command should return one JSON response")
    }

    fn run_agent_json(
        &self,
        arguments: &[&str],
        environment: &[(String, String)],
    ) -> Value {
        let mut command = self.agent_command(environment);
        command.args(arguments);
        let output = run(command);
        assert!(
            output.status.success(),
            "agent command {arguments:?} failed: {output:?}"
        );
        assert!(
            output.stderr.is_empty(),
            "successful agent JSON should not write diagnostics: {output:?}"
        );
        serde_json::from_slice(&output.stdout)
            .expect("the agent command should return one JSON response")
    }

    fn only_index_entry(&self) -> PathBuf {
        let entries = fs::read_dir(self.state.join("coterie/projects"))
            .expect("the project index should exist")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().is_some_and(|value| value == "json")
            })
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "exactly one project should be indexed");
        entries[0].clone()
    }

    fn index_entry_count(&self) -> usize {
        fs::read_dir(self.state.join("coterie/projects"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.extension().is_some_and(|value| value == "json")
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    fn files(&self) -> Vec<PathBuf> {
        let mut pending = vec![self.root.clone(), self.runtime.clone()];
        let mut files = Vec::new();
        while let Some(path) = pending.pop() {
            files.push(path.clone());
            if let Ok(entries) = fs::read_dir(path) {
                pending.extend(
                    entries.filter_map(Result::ok).map(|entry| entry.path()),
                );
            }
        }
        files.sort();
        files
    }
}

impl Drop for TestEnvironment {
    fn drop(&mut self) {
        for path in [&self.root, &self.runtime] {
            if path.exists() {
                fs::remove_dir_all(path)
                    .expect("the test environment should be removable");
            }
        }
    }
}
