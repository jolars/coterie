use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::apply_codex_runtime_environment;

struct ShellFixture(PathBuf);

impl ShellFixture {
    fn new() -> Self {
        let root = env::temp_dir()
            .join(format!("coterie-shell-{}", crate::id::RunId::generate()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        fs::create_dir(root.join("toolchain")).unwrap();
        let tool = root.join("toolchain/coterie-toolchain-probe");
        fs::write(&tool, "#!/bin/sh\nprintf 'toolchain-ok\\n'\n").unwrap();
        fs::set_permissions(tool, fs::Permissions::from_mode(0o700)).unwrap();
        // A quoted variable must preserve path bytes instead of evaluating them.
        symlink(env::current_exe().unwrap(), root.join("cli ' $ ; path"))
            .unwrap();
        Self(root)
    }

    fn command(
        &self,
        shell: &Path,
        identity: bool,
        initialized: bool,
    ) -> Command {
        let mut command = Command::new(shell);
        command.env_clear().current_dir(&self.0);
        let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .unwrap()
            .unwrap();
        let mut inputs = vec![
            (OsString::from("HOME"), self.0.join("home").into_os_string()),
            (
                "PATH".into(),
                env::join_paths(
                    std::iter::once(self.0.join("toolchain")).chain(
                        env::split_paths(
                            &env::var_os("PATH").unwrap_or_default(),
                        ),
                    ),
                )
                .unwrap(),
            ),
            ("SHELL".into(), shell.as_os_str().to_owned()),
            ("BASH_ENV".into(), "/must-not-be-sourced".into()),
            ("PROJECT_SECRET".into(), "must-not-pass".into()),
        ];
        if identity {
            inputs.push(("USER".into(), user.name.clone().into()));
            inputs.push(("LOGNAME".into(), user.name.into()));
        }
        if initialized {
            inputs.push(("__ETC_PROFILE_DONE".into(), "1".into()));
            inputs.push(("__NIXOS_SET_ENVIRONMENT_DONE".into(), "1".into()));
        }
        apply_codex_runtime_environment(&mut command, inputs);
        command.env("COTERIE_BIN", self.0.join("cli ' $ ; path"));
        command
    }
}

impl Drop for ShellFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn shell_path(name: &str) -> PathBuf {
    env::split_paths(&env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| {
            panic!("{name} must be available for the shell contract test")
        })
}

fn successful(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn non_login_bash_preserves_toolchain_and_absolute_bootstrap() {
    let fixture = ShellFixture::new();
    let output = successful(fixture.command(&shell_path("bash"), true, true).args([
        "-c",
        "test -z \"${PROJECT_SECRET-}${BASH_ENV-}\" && coterie-toolchain-probe && \"$COTERIE_BIN\" --list",
    ]).output().unwrap());
    assert!(output.starts_with("toolchain-ok\n"));
    assert!(output.contains(
        "codex_worker_bootstrap_preserves_shell_identity_and_cli_location"
    ));
}

#[test]
#[ignore = "requires NixOS with bash, fish, and ripgrep in the operator profile"]
fn nixos_actual_login_and_non_login_shells() {
    assert!(
        Path::new("/etc/NIXOS").exists(),
        "run this contract on NixOS"
    );
    let fixture = ShellFixture::new();
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .unwrap()
        .unwrap()
        .name;
    let profile = format!("/etc/profiles/per-user/{user}/bin");

    for shell in ["bash", "fish"] {
        let shell = shell_path(shell);
        for flag in ["-c", "-lc"] {
            let output = successful(
                fixture
                    .command(&shell, true, true)
                    .args([flag, "\"$COTERIE_BIN\" --list"])
                    .output()
                    .unwrap(),
            );
            assert!(output.contains("codex_worker_bootstrap_preserves_shell_identity_and_cli_location"));
        }
        let output = successful(
            fixture
                .command(&shell, true, true)
                .args(["-c", "coterie-toolchain-probe"])
                .output()
                .unwrap(),
        );
        assert_eq!(output, "toolchain-ok\n");
    }

    let bash = shell_path("bash");
    let repaired = successful(
        fixture
            .command(&bash, true, false)
            .args(["-lc", "printf '%s\\n' \"$PATH\"; command -v rg"])
            .output()
            .unwrap(),
    );
    assert!(repaired.contains(&profile), "{repaired}");
    assert!(!repaired.contains(fixture.0.join("toolchain").to_str().unwrap()));
    let broken = successful(
        fixture
            .command(&bash, false, false)
            .args(["-lc", "printf '%s\\n' \"$PATH\""])
            .output()
            .unwrap(),
    );
    assert!(!broken.contains(&profile), "{broken}");
}
