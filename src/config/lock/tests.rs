use super::*;
use crate::config::resolution_tests::{CUSTOM, Fixture};

fn defaults() -> EffectiveConfig {
    Fixture::new().load().unwrap()
}

#[test]
fn fingerprint_covers_all_portable_policy_and_ignores_host_bindings() {
    let config = defaults();
    let expected = ConfigLock::for_config(&config);
    let mut host = config.clone();
    host.providers.get_mut("codex").unwrap().command =
        vec!["/another/host/codex".into(), "secret".into()];
    host.provenance.clear();
    assert_eq!(ConfigLock::for_config(&host), expected);

    let mut changes = Vec::new();
    let mut changed = config.clone();
    changed
        .archetype
        .roles
        .get_mut("lead")
        .unwrap()
        .instructions = Some("Different orchestration instructions.".into());
    changes.push(changed);
    let mut changed = config.clone();
    changed.archetype.roles.get_mut("worker").unwrap().provider =
        "other".into();
    changes.push(changed);
    let mut changed = config.clone();
    changed
        .archetype
        .roles
        .get_mut("worker")
        .unwrap()
        .capabilities
        .clear();
    changes.push(changed);
    let mut changed = config.clone();
    changed.archetype.roles.get_mut("worker").unwrap().workspace =
        crate::config::WorkspacePolicy::ReadOnly;
    changes.push(changed);
    let mut changed = config.clone();
    changed.roles.get_mut("worker").unwrap().enabled = false;
    changes.push(changed);
    let mut changed = config.clone();
    changed.roles.get_mut("worker").unwrap().max_instances = Some(1);
    changes.push(changed);
    let mut changed = config.clone();
    changed
        .roles
        .get_mut("worker")
        .unwrap()
        .permission_profile
        .filesystem = crate::config::FilesystemPolicy::ReadOnly;
    changes.push(changed);
    let mut changed = config.clone();
    changed.limits.max_concurrent_agents = 2;
    changes.push(changed);
    let mut changed = config.clone();
    changed.supervision.job_timeout_seconds = 120;
    changes.push(changed);
    for changed in changes {
        assert_ne!(
            ConfigLock::for_config(&changed).fingerprint,
            expected.fingerprint
        );
    }
}

fn golden_locks() -> [(PathBuf, String); 3] {
    let fixture = Fixture::new();
    fixture.write(
        "config.toml",
        "includes = ['team.toml']\n[limits]\nmax_agents_per_run = 12",
    );
    fixture.write("team.toml", CUSTOM);
    fixture.write("coterie.toml", "[roles.builder]\nmax_instances = 2");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let examples = crate::config::resolve(
        &toml::from_str(include_str!("../../../examples/config/global.toml"))
            .unwrap(),
        &toml::from_str(include_str!("../../../examples/config/project.toml"))
            .unwrap(),
        &crate::config::OperatorOverrides::default(),
    )
    .unwrap();
    [
        ("compiled", defaults()),
        ("layered", fixture.load().unwrap()),
        ("example", examples),
    ]
    .map(|(name, config)| {
        let path = if name == "example" {
            root.join("examples/config/coterie.lock")
        } else {
            root.join(format!("tests/golden/config-lock-{name}.json"))
        };
        (
            path,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&ConfigLock::for_config(&config))
                    .unwrap()
            ),
        )
    })
}

#[test]
fn source_layout_and_explicit_defaults_do_not_change_the_fingerprint() {
    let first = Fixture::new();
    first.write("config.toml", "includes = ['a.toml', 'b.toml']");
    first.write("a.toml", "[limits]\nmax_agents_per_run = 12");
    first.write("b.toml", "[limits]\nmax_concurrent_agents = 6");
    let second = Fixture::new();
    second.write("config.toml", "archetype = 'builtin:standard@1'\n[limits]\nmax_concurrent_agents = 6\nmax_agents_per_run = 12\n[providers.codex]\ncommand = ['/different/host/codex']");
    second.write("coterie.toml", "[roles.worker]\nmax_instances = 3");
    assert_ne!(
        first.load().unwrap().provenance,
        second.load().unwrap().provenance
    );
    assert_eq!(
        ConfigLock::for_config(&first.load().unwrap()),
        ConfigLock::for_config(&second.load().unwrap())
    );
}

#[test]
fn oversized_locks_are_refused_before_replacement() {
    let fixture = Fixture::new();
    let path = fixture.write("coterie.lock", &" ".repeat(MAX_LOCK_BYTES + 1));
    let mut lock = ConfigLock::for_config(&defaults());
    assert!(
        lock.verify(&path)
            .unwrap_err()
            .to_string()
            .contains("1 MiB")
    );
    lock.archetype = "x".repeat(MAX_LOCK_BYTES + 1);
    assert!(lock.write(&path).unwrap_err().to_string().contains("1 MiB"));
    assert_eq!(fs::read(path).unwrap(), vec![b' '; MAX_LOCK_BYTES + 1]);
}

#[test]
fn crash_lock_writer() {
    let Ok(path) = std::env::var("COTERIE_TEST_CONFIG_LOCK_PATH") else {
        return;
    };
    let phase = std::env::var("COTERIE_TEST_CONFIG_LOCK_PHASE").unwrap();
    let mut updated = ConfigLock::for_config(&defaults());
    updated.fingerprint = "0".repeat(64);
    updated
        .write_with(Path::new(&path), |boundary| {
            if boundary == phase {
                std::process::exit(86);
            }
            Ok(())
        })
        .unwrap();
    panic!("crash boundary was not reached");
}

#[test]
fn crashes_during_creation_and_replacement_leave_complete_locks_and_retries_converge()
 {
    let original = ConfigLock::for_config(&defaults());
    let mut updated = original.clone();
    updated.fingerprint = "0".repeat(64);
    for existing in [false, true] {
        for phase in [
            "created",
            "written",
            "synced",
            "published",
            "directory-synced",
        ] {
            let fixture = Fixture::new();
            let path = fixture.0.join("coterie.lock");
            if existing {
                original.write(&path).unwrap();
            }
            let output =
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "config::lock::tests::crash_lock_writer",
                        "--nocapture",
                    ])
                    .env("COTERIE_TEST_CONFIG_LOCK_PATH", &path)
                    .env("COTERIE_TEST_CONFIG_LOCK_PHASE", phase)
                    .output()
                    .unwrap();
            assert_eq!(output.status.code(), Some(86), "{output:?}");
            if matches!(phase, "published" | "directory-synced") {
                assert!(matches!(
                    updated.verify(&path).unwrap(),
                    LockStatus::Verified
                ));
            } else {
                assert!(
                    matches!(
                        original.verify(&path).unwrap(),
                        LockStatus::Verified
                    ) == existing
                );
            }
            let preserved = fs::read_dir(&fixture.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|candidate| candidate != &path)
                .map(|path| {
                    let bytes = fs::read(&path).unwrap();
                    (path, bytes)
                })
                .collect::<Vec<_>>();
            updated.write(&path).unwrap();
            assert!(matches!(
                updated.verify(&path).unwrap(),
                LockStatus::Verified
            ));
            let bytes = fs::read(&path).unwrap();
            updated.write(&path).unwrap();
            assert_eq!(fs::read(&path).unwrap(), bytes);
            for (path, bytes) in preserved {
                assert_eq!(fs::read(path).unwrap(), bytes);
            }
        }
    }
}

#[test]
fn locks_and_fingerprints_match_golden_contracts() {
    for (path, actual) in golden_locks() {
        assert_eq!(actual, fs::read_to_string(path).unwrap());
    }
}

#[test]
#[ignore = "explicit developer command to regenerate lock golden files"]
fn regenerate_lock_goldens() {
    for (path, text) in golden_locks() {
        fs::write(path, text).unwrap();
    }
}

#[test]
fn version_requirements_are_verified_semantically_and_mismatches_are_complete()
{
    let expected = ConfigLock::for_config(&defaults());
    let mut locked = expected.clone();
    locked.coterie_version = ">=0.1.0, <0.2.0".into();
    let path = Path::new("coterie.lock");
    locked.compare(&expected, path, "0.1.9").unwrap();
    assert!(locked.compare(&expected, path, "0.2.0").is_err());
    assert!(locked.compare(&expected, path, "0.1.9-beta.1").is_err());
    locked.archetype = "builtin:other@1".into();
    locked.providers.clear();
    locked.fingerprint = "0".repeat(64);
    let error = locked
        .compare(&expected, path, "0.2.0")
        .unwrap_err()
        .to_string();
    assert_eq!(
        format!("{error}\n"),
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/golden/config-lock-mismatch.txt")
        )
        .unwrap()
    );
}

#[test]
fn failed_publication_preserves_a_complete_old_or_new_lock_and_retry_converges()
{
    let fixture = Fixture::new();
    let path = fixture.0.join("coterie.lock");
    let original = ConfigLock::for_config(&defaults());
    let mut updated = original.clone();
    updated.fingerprint = "0".repeat(64);
    for phase in [
        "created",
        "written",
        "synced",
        "published",
        "directory-synced",
    ] {
        original.write(&path).unwrap();
        assert!(
            updated
                .write_with(&path, |current| if current == phase {
                    Err(io::Error::other("injected failure"))
                } else {
                    Ok(())
                })
                .is_err()
        );
        let current: ConfigLock =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            current,
            if matches!(phase, "published" | "directory-synced") {
                updated.clone()
            } else {
                original.clone()
            }
        );
        updated.write(&path).unwrap();
        assert!(matches!(
            updated.verify(&path).unwrap(),
            LockStatus::Verified
        ));
        let bytes = fs::read(&path).unwrap();
        updated.write(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    }
}

#[test]
fn a_lock_changed_during_creation_is_preserved() {
    let fixture = Fixture::new();
    let path = fixture.0.join("coterie.lock");
    let lock = ConfigLock::for_config(&defaults());
    lock.write(&path).unwrap();
    let error = lock
        .write_with(&path, |phase| {
            if phase == "synced" {
                fs::write(&path, "a concurrent edit")?;
            }
            Ok(())
        })
        .unwrap_err();
    assert!(error.to_string().contains("changed during creation"));
    assert_eq!(fs::read_to_string(path).unwrap(), "a concurrent edit");
}

#[test]
fn lock_reads_and_writes_refuse_symlinks_hardlinks_and_nonregular_files() {
    let fixture = Fixture::new();
    let path = fixture.0.join("coterie.lock");
    let target = fixture.write("target", "retain this file");
    let lock = ConfigLock::for_config(&defaults());
    for dangling in [false, true] {
        std::os::unix::fs::symlink(
            if dangling {
                fixture.0.join("absent")
            } else {
                target.clone()
            },
            &path,
        )
        .unwrap();
        assert!(lock.verify(&path).is_err());
        assert!(lock.write(&path).is_err());
        fs::remove_file(&path).unwrap();
    }
    fs::hard_link(&target, &path).unwrap();
    assert!(lock.verify(&path).is_err());
    assert!(lock.write(&path).is_err());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(lock.verify(&path).is_err());
    assert!(lock.write(&path).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "retain this file");
}
