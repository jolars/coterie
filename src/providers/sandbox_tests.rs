//! Opt-in checks against the installed provider's actual Linux sandbox.

use std::env;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::ProcessProbeRunner;

const CHILD_TEST: &str = "providers::sandbox_tests::sandbox_access_child";
const RESULT_PREFIX: &str = "COTERIE_SANDBOX_OBSERVATION=";

struct SandboxFixture {
    root: PathBuf,
    _listeners: [UnixListener; 2],
    tcp_listener: TcpListener,
}

impl SandboxFixture {
    fn new() -> Self {
        // System temp directories are writable in Codex's workspace profile.
        // A separate runtime directory tests that the socket grants no writes.
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .expect(
                "the real sandbox test requires an absolute XDG_RUNTIME_DIR",
            );
        Self::under(&runtime)
    }

    fn under(parent: &Path) -> Self {
        let root = parent.join(format!("ct-{}", crate::id::RunId::generate()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        for name in ["project", "codex-home"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        fs::write(
            root.join("codex-home/config.toml"),
            "default_permissions = \":read-only\"\n\
             [features.network_proxy]\n\
             enabled = true\n\
             dangerously_allow_all_unix_sockets = true\n",
        )
        .unwrap();
        let listeners = ["supervisor.sock", "unrelated.sock"].map(|name| {
            let path = root.join(name);
            let listener = UnixListener::bind(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .unwrap();
            listener
        });
        Self {
            root,
            _listeners: listeners,
            tcp_listener: TcpListener::bind("127.0.0.1:0").unwrap(),
        }
    }

    fn workspace_command(&self) -> Command {
        let mut command = Command::new("codex");
        command
            .args(["sandbox", "--permission-profile", ":workspace", "--cd"])
            .arg(self.root.join("project"));
        self.append_child(&mut command);
        command
    }

    fn scoped_command(&self, parent: &str) -> Command {
        let mut command = Command::new("codex");
        let socket_key =
            serde_json::to_string(&self.root.join("supervisor.sock")).unwrap();
        command
            .args([
                "sandbox",
                "--permission-profile",
                "coterie-probe",
                "--config",
                "default_permissions=\"coterie-probe\"",
                "--config",
            ])
            .arg(format!("permissions.coterie-probe.extends=\"{parent}\""))
            .args([
                "--config",
                "permissions.coterie-probe.network.enabled=false",
                "--config",
                "permissions.coterie-probe.network.dangerously_allow_all_unix_sockets=false",
                "--config",
            ])
            .arg(format!(
                "permissions.coterie-probe.network.unix_sockets={{{socket_key}=\"allow\"}}"
            ))
            .arg("--cd")
            .arg(self.root.join("project"));
        self.append_child(&mut command);
        command
    }

    fn append_child(&self, command: &mut Command) {
        let child = self.child_command();
        command
            .arg(child.get_program())
            .args(child.get_args())
            .env_clear()
            .envs(
                child.get_envs().filter_map(|(name, value)| {
                    value.map(|value| (name, value))
                }),
            );
    }

    fn child_command(&self) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
            .env_clear();
        super::apply_codex_runtime_environment(&mut command, env::vars_os());
        command
            .env("CODEX_HOME", self.root.join("codex-home"))
            .env("COTERIE_SANDBOX_FIXTURE", &self.root)
            .env(
                "COTERIE_SANDBOX_TCP",
                self.tcp_listener.local_addr().unwrap().to_string(),
            );
        command
    }
}

impl Drop for SandboxFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
#[ignore = "helper launched only inside the opt-in Codex sandbox test"]
fn sandbox_access_child() {
    let root = PathBuf::from(
        env::var_os("COTERIE_SANDBOX_FIXTURE")
            .expect("only the sandbox test may launch this helper"),
    );
    let mut results = serde_json::Map::new();
    for name in ["supervisor.sock", "unrelated.sock"] {
        let result = UnixStream::connect(root.join(name));
        results.insert(
            name.to_owned(),
            serde_json::json!({
                "allowed": result.is_ok(),
                "errno": result.err().and_then(|error| error.raw_os_error()),
            }),
        );
    }
    let address = env::var("COTERIE_SANDBOX_TCP").unwrap().parse().unwrap();
    let result =
        TcpStream::connect_timeout(&address, Duration::from_millis(100));
    results.insert(
        "tcp".to_owned(),
        serde_json::json!({
            "allowed": result.is_ok(),
            "errno": result.err().and_then(|error| error.raw_os_error()),
        }),
    );
    for name in ["project/write-check", "outside-write-check"] {
        let result = fs::write(root.join(name), b"sandbox fixture\n");
        results.insert(
            name.to_owned(),
            serde_json::json!({
                "allowed": result.is_ok(),
                "errno": result.err().and_then(|error| error.raw_os_error()),
            }),
        );
    }
    println!("{RESULT_PREFIX}{}", serde_json::Value::Object(results));
}

fn observe(command: Command) -> serde_json::Value {
    let output = ProcessProbeRunner
        .run_command_bounded(command, Duration::from_secs(2))
        .expect("the observation must finish within the probe deadline");
    assert!(
        output.success,
        "sandbox helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(RESULT_PREFIX))
            .expect("the sandbox must actually execute the observation helper"),
    )
    .unwrap()
}

#[test]
fn sandbox_observation_control_reaches_live_endpoints_and_writes_files() {
    let fixture = SandboxFixture::under(Path::new("/tmp"));
    // Running the same helper as an ordinary process supplies the positive
    // control without executing an installed provider in ordinary CI.
    let observation = observe(fixture.child_command());
    for name in [
        "supervisor.sock",
        "unrelated.sock",
        "tcp",
        "project/write-check",
        "outside-write-check",
    ] {
        assert_eq!(observation[name]["allowed"], true, "{name}");
    }
}

#[test]
#[ignore = "requires explicit opt-in to run the installed Codex Linux sandbox"]
fn installed_codex_reproduces_workspace_socket_denial() {
    let fixture = SandboxFixture::new();
    for name in ["supervisor.sock", "unrelated.sock"] {
        UnixStream::connect(fixture.root.join(name))
            .expect("the operator must reach the same live socket");
    }
    TcpStream::connect(fixture.tcp_listener.local_addr().unwrap())
        .expect("the operator must reach the same TCP endpoint");
    let observation = observe(fixture.workspace_command());
    assert_eq!(observation["project/write-check"]["allowed"], true);
    assert_eq!(observation["outside-write-check"]["allowed"], false);
    assert_eq!(observation["supervisor.sock"]["allowed"], false);
    assert_eq!(observation["supervisor.sock"]["errno"], 1);
    assert_eq!(observation["unrelated.sock"]["allowed"], false);
    assert_eq!(observation["tcp"]["allowed"], false);
}

#[test]
#[ignore = "requires explicit opt-in; reproduces the rejected scoped grant on Codex 0.153.4 Linux"]
fn installed_codex_reproduces_scoped_socket_denial() {
    let observations = [":workspace", ":read-only"].map(|parent| {
        let fixture = SandboxFixture::new();
        (parent, observe(fixture.scoped_command(parent)))
    });
    for (parent, observation) in &observations {
        for denied in ["unrelated.sock", "tcp", "outside-write-check"] {
            assert_eq!(
                observation[denied]["allowed"], false,
                "{parent} must still deny {denied}: {observation}"
            );
        }
        assert_eq!(
            observation["project/write-check"]["allowed"],
            *parent == ":workspace",
            "the selected filesystem restriction must remain effective"
        );
    }
    assert!(
        observations.iter().all(|(_, observation)| {
            observation["supervisor.sock"]["allowed"] == false
        }),
        "Linux must reproduce the unsupported grant under both profiles: {observations:?}"
    );
}
