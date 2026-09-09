use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .join(format!("coterie-config-cli-{}", ulid::Ulid::generate()));
        fs::create_dir_all(root.join("project")).unwrap();
        Self(root)
    }

    fn write(&self, path: &str, text: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_coterie"));
        command.current_dir(self.0.join("project")).env_clear();
        command.env("HOME", self.0.join("home"));
        command.env("XDG_CONFIG_HOME", self.0.join("config"));
        command.env("XDG_STATE_HOME", self.0.join("state"));
        command.env("XDG_RUNTIME_DIR", self.0.join("runtime"));
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn success(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    value["data"].clone()
}

fn configuration_error(output: &Output) -> Value {
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(value["error"]["code"], "invalid_configuration");
    value["error"].clone()
}

#[test]
fn workspace_policy_requires_an_enforceable_launch() {
    for global in [
        include_str!("../examples/config/global.toml").replacen(
            "workspace = \"project\"",
            "workspace = \"worktree\"",
            1,
        ),
        include_str!("../examples/config/global.toml")
            .replace("workspace = \"worktree\"", "workspace = \"read-only\""),
    ] {
        let fixture = Fixture::new();
        fixture.write("config/coterie/config.toml", &global);
        let error =
            configuration_error(&fixture.run(&["config", "check", "--json"]));
        assert!(error["message"].as_str().unwrap().contains("workspace"));
    }
}

#[test]
fn inspection_and_locks_include_bounded_operator_overrides() {
    let fixture = Fixture::new();
    fixture.write("project/coterie.toml", "[roles.worker]\nmax_instances = 1");
    let flags = [
        "--role",
        "worker.max_instances=2",
        "--role",
        "worker.enabled=false",
        "--max-agents-per-run",
        "7",
    ];
    let shown = success(
        &fixture.run(
            &[
                flags.as_slice(),
                &["config", "show", "--provenance", "--json"],
            ]
            .concat(),
        ),
    );
    assert_eq!(shown["effective"]["roles"]["worker"]["max_instances"], 2);
    assert_eq!(shown["effective"]["roles"]["worker"]["enabled"], false);
    assert_eq!(
        shown["provenance"]["roles.worker.max_instances"]["source"]["layer"],
        "operator"
    );
    success(
        &fixture
            .run(&[flags.as_slice(), &["config", "lock", "--json"]].concat()),
    );
    assert_eq!(
        success(
            &fixture.run(
                &[flags.as_slice(), &["config", "check", "--json"]].concat()
            )
        )["lock"],
        "verified"
    );
    configuration_error(&fixture.run(&["config", "check", "--json"]));
    configuration_error(&fixture.run(&[
        "--role",
        "worker.max_instances=4",
        "config",
        "check",
        "--json",
    ]));
}

#[test]
fn inspection_works_without_a_run_or_provider_and_creates_no_files() {
    let fixture = Fixture::new();
    let checked = success(&fixture.run(&["config", "check", "--json"]));
    assert_eq!(checked["lock"], "absent");
    assert_eq!(checked["archetype"], "builtin:standard@1");
    let shown = success(&fixture.run(&[
        "config",
        "show",
        "--effective",
        "--provenance",
        "--json",
    ]));
    assert_eq!(shown["effective"]["roles"]["worker"]["max_instances"], 3);
    assert_eq!(
        shown["provenance"]["roles.worker.enabled"]["source"]["layer"],
        "compiled"
    );
    assert_eq!(shown["fingerprint"], checked["fingerprint"]);
    assert!(!fixture.0.join("state").exists());
    assert!(!fixture.0.join("runtime").exists());
    assert_eq!(fs::read_dir(fixture.0.join("project")).unwrap().count(), 0);
    let human = fixture.run(&["config", "check"]);
    assert!(human.status.success());
    assert!(
        String::from_utf8_lossy(&human.stdout)
            .contains("Configuration is valid")
    );
}

#[test]
fn locks_are_explicit_portable_and_verified_with_actionable_errors() {
    let first = Fixture::new();
    let second = Fixture::new();
    for (fixture, command) in
        [(&first, "/host/one/codex"), (&second, "/host/two/codex")]
    {
        fixture.write(
            "config/coterie/config.toml",
            "includes = ['provider.toml']",
        );
        fixture.write("config/coterie/provider.toml", &format!("[providers.codex]\ncommand = ['{command}', '--key', 'command-secret']"));
        fixture
            .write("project/coterie.toml", "[roles.worker]\nmax_instances = 2");
    }
    success(&first.run(&["config", "lock", "--json"]));
    let bytes = fs::read(first.0.join("project/coterie.lock")).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    for excluded in [
        "/host/",
        "command-secret",
        "provider.toml",
        &first.0.to_string_lossy(),
    ] {
        assert!(!text.contains(excluded), "{text}");
    }
    fs::write(second.0.join("project/coterie.lock"), &bytes).unwrap();
    assert_eq!(
        success(&second.run(&["config", "check", "--json"]))["lock"],
        "verified"
    );
    success(&second.run(&["config", "lock", "--json"]));
    assert_eq!(
        fs::read(second.0.join("project/coterie.lock")).unwrap(),
        bytes
    );
    second.write("project/coterie.toml", "[roles.worker]\nmax_instances = 1");
    for arguments in [
        vec!["config", "check", "--json"],
        vec!["config", "show", "--effective", "--json"],
    ] {
        let error = configuration_error(&second.run(&arguments));
        let message = error["message"].as_str().unwrap();
        assert!(message.contains("fingerprint"), "{message}");
        assert!(message.contains("config lock"), "{message}");
    }
    assert_eq!(
        fs::read(second.0.join("project/coterie.lock")).unwrap(),
        bytes
    );
    success(&second.run(&["config", "lock", "--json"]));
    assert_eq!(
        success(&second.run(&["config", "check", "--json"]))["lock"],
        "verified"
    );
}

#[test]
fn schema_generation_does_not_load_invalid_configuration() {
    let fixture = Fixture::new();
    fixture.write("project/coterie.toml", "invalid = true");
    for target in ["global", "project", "lock", "effective"] {
        let schema = success(
            &fixture.run(&["config", "schema", "--target", target, "--json"]),
        );
        assert!(schema["$schema"].is_string());
        let expected = fs::read_to_string(format!(
            "{}/schemas/config-{target}-v1.schema.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        assert_eq!(schema, serde_json::from_str::<Value>(&expected).unwrap());
    }
    let raw = fixture.run(&["config", "schema"]);
    assert!(raw.status.success());
    let schema: Value = serde_json::from_slice(&raw.stdout).unwrap();
    assert_eq!(schema["title"], "ProjectConfig");
    configuration_error(&fixture.run(&["config", "check", "--json"]));
}

#[test]
fn inspection_reports_include_and_project_origins_and_redacts_credentials() {
    let fixture = Fixture::new();
    fixture.write("config/coterie/config.toml", "includes = ['provider.toml']");
    fixture.write(
        "config/coterie/provider.toml",
        "[providers.codex]\ncommand = ['codex', 'secret\"with-escapes']",
    );
    fixture.write("project/coterie.toml", "[roles.worker]\nmax_instances = 2");
    let output = fixture
        .command()
        .args(["config", "show", "--provenance", "--json"])
        .env("OPENAI_API_KEY", "secret\"with-escapes")
        .output()
        .unwrap();
    let data = success(&output);
    assert_eq!(
        data["effective"]["providers"]["codex"]["command"][1],
        "[REDACTED]"
    );
    assert_eq!(
        data["provenance"]["providers.codex.command"]["source"]["file"],
        fixture
            .0
            .join("config/coterie/provider.toml")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        data["provenance"]["roles.worker.max_instances"]["source"]["layer"],
        "project"
    );
    let plain = success(&fixture.run(&["config", "show", "--json"]));
    assert!(plain.get("provenance").is_none());
    let human = fixture
        .command()
        .args(["config", "show", "--provenance"])
        .env("OPENAI_API_KEY", "secret\"with-escapes")
        .output()
        .unwrap();
    assert!(human.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&human.stdout).unwrap(),
        data
    );
}

#[test]
fn malformed_locks_fail_without_echoing_contents_or_rewriting_files() {
    let fixture = Fixture::new();
    success(&fixture.run(&["config", "lock", "--json"]));
    let path = fixture.0.join("project/coterie.lock");
    let valid: Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut variants = vec!["malformed secret-text".into()];
    for (field, replacement) in [
        ("schema_version", Value::from(2)),
        ("fingerprint", Value::from("secret-text")),
        ("coterie_version", Value::from("secret-text")),
        ("extra", Value::from("secret-text")),
    ] {
        let mut value = valid.clone();
        value[field] = replacement;
        variants.push(value.to_string());
    }
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("providers");
    variants.push(missing.to_string());
    for contents in variants {
        fs::write(&path, &contents).unwrap();
        let output = fixture.run(&["config", "check", "--json"]);
        configuration_error(&output);
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("secret-text")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), contents);
    }
    success(&fixture.run(&["config", "lock", "--json"]));
    success(&fixture.run(&["config", "check", "--json"]));
}

#[test]
fn git_project_discovery_keeps_configuration_at_the_root() {
    let fixture = Fixture::new();
    git2::Repository::init(fixture.0.join("project")).unwrap();
    fixture.write("project/coterie.toml", "[roles.worker]\nmax_instances = 1");
    fixture.write("project/nested/coterie.toml", "unknown = true");
    let data = success(
        &fixture
            .command()
            .current_dir(fixture.0.join("project/nested"))
            .args(["config", "show", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(data["effective"]["roles"]["worker"]["max_instances"], 1);
    success(
        &fixture
            .command()
            .current_dir(fixture.0.join("project/nested"))
            .args(["config", "lock", "--json"])
            .output()
            .unwrap(),
    );
    assert!(fixture.0.join("project/coterie.lock").exists());
    assert!(!fixture.0.join("project/nested/coterie.lock").exists());
}
