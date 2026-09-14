use super::*;
use std::fs;
use std::process::{Child, Command, Stdio};

use nix::fcntl::OFlag;
use nix::pty::{grantpt, posix_openpt, ptsname_r, unlockpt};

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn process(input: &File) -> Process {
    Process(
        Command::new("sleep")
            .arg("60")
            .stdin(input.try_clone().unwrap())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn pty() -> (File, File) {
    let master = posix_openpt(OFlag::O_RDWR | OFlag::O_CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave = File::options()
        .read(true)
        .write(true)
        .open(ptsname_r(&master).unwrap())
        .unwrap();
    (std::os::fd::OwnedFd::from(master).into(), slave)
}

#[test]
fn closed_pty_is_distinct_from_an_open_terminal_with_no_viewer() {
    let (master, slave) = pty();
    let mut child = process(&slave);
    let identity = ForegroundIdentity::capture_child(
        child.0.id(),
        InheritedInput::from_file(&slave),
    )
    .unwrap();
    let attributes = nix::sys::termios::tcgetattr(&slave).unwrap();
    assert_eq!(identity.inspect(), TerminalObservation::LivePty);
    assert_eq!(identity.inspect(), TerminalObservation::LivePty);
    assert_eq!(nix::sys::termios::tcgetattr(&slave).unwrap(), attributes);
    assert!(child.0.try_wait().unwrap().is_none());
    drop(master);
    // Concurrent forks can briefly retain a CLOEXEC master until exec completes.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(5);
    while identity.inspect() != TerminalObservation::ClosedPty {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(child.0.try_wait().unwrap().is_none());
}

#[test]
fn mismatched_process_identity_never_establishes_terminal_health() {
    let (_master, slave) = pty();
    let child = process(&slave);
    let identity = ForegroundIdentity::capture_child(
        child.0.id(),
        InheritedInput::from_file(&slave),
    )
    .unwrap();
    // A reused PID may name a live PTY, but cannot match the old process birth.
    let mut reused = identity.clone();
    reused.start_ticks += 1;
    assert_eq!(reused.inspect(), TerminalObservation::IdentityMismatch);
    let mut rebooted = identity.clone();
    rebooted.boot_id = "other-boot".into();
    assert_eq!(rebooted.inspect(), TerminalObservation::IdentityMismatch);
    let mut other_user = identity.clone();
    other_user.user_id += 1;
    assert_eq!(other_user.inspect(), TerminalObservation::IdentityMismatch);
    let mut changed_input = identity.clone();
    changed_input.input = InheritedInput::Pty {
        device: 0,
        inode: 0,
        special_device: 0,
    };
    assert_eq!(changed_input.inspect(), TerminalObservation::InputChanged);
    assert_eq!(identity.inspect(), TerminalObservation::LivePty);
}

#[test]
fn missing_process_and_nonterminal_input_remain_distinct() {
    let input = File::open("/dev/null").unwrap();
    let mut child = process(&input);
    let identity = ForegroundIdentity::capture_child(
        child.0.id(),
        InheritedInput::from_file(&input),
    )
    .unwrap();
    assert_eq!(identity.inspect(), TerminalObservation::NonTerminal);
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(identity.inspect(), TerminalObservation::MissingProcess);
    assert!(
        ForegroundIdentity::capture_child(
            std::process::id(),
            InheritedInput::NonTerminal
        )
        .is_none()
    );
}

#[test]
fn terminal_inspection_never_consumes_pending_input() {
    use std::io::{Read, Write};
    let (mut master, mut slave) = pty();
    let child = process(&slave);
    let identity = ForegroundIdentity::capture_child(
        child.0.id(),
        InheritedInput::from_file(&slave),
    )
    .unwrap();
    master.write_all(b"pending terminal input\n").unwrap();
    assert_eq!(identity.inspect(), TerminalObservation::LivePty);
    let mut input = [0; 23];
    slave.read_exact(&mut input).unwrap();
    assert_eq!(&input, b"pending terminal input\n");
    assert!(fs::metadata(format!("/proc/{}", child.0.id())).is_ok());
}

#[test]
fn a_pinned_process_directory_cannot_follow_a_reused_pid() {
    let (_master, slave) = pty();
    let mut child = process(&slave);
    let identity = ForegroundIdentity::capture_child(
        child.0.id(),
        InheritedInput::from_file(&slave),
    )
    .unwrap();
    let directory = process_directory(child.0.id()).unwrap();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(
        identity.verify(&directory).unwrap(),
        Some(TerminalObservation::MissingProcess)
    );
    assert!(openat(&directory, "fd/0", OFlag::O_PATH, Mode::empty()).is_err());
}

#[test]
fn process_stat_handles_unescaped_names_and_rejects_incomplete_evidence() {
    let fields = "S 123 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 456 0";
    let stat =
        parse_stat(&format!("321 (a ) (name\nwith spaces) {fields}")).unwrap();
    assert_eq!(stat.process_id, 321);
    assert_eq!(stat.parent_id, 123);
    assert_eq!(stat.start_ticks, 456);
    assert!(!stat.exited);
    for value in [
        "321 (name) S 123",
        "321 (name) ? 123",
        "garbage",
        "321 name S 123",
    ] {
        assert!(parse_stat(value).is_none(), "{value}");
    }
    assert!(
        parse_stat(&format!("321 (name) {}", fields.replacen('S', "Z", 1)))
            .unwrap()
            .exited
    );
}
