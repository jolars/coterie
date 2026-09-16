//! Read-only Linux process and PTY observations, separate from process control.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use nix::sys::statfs::{DEVPTS_SUPER_MAGIC, PROC_SUPER_MAGIC, fstatfs};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ForegroundIdentity {
    pub(crate) process_id: u32,
    pub(crate) boot_id: String,
    pub(crate) start_ticks: u64,
    pub(crate) user_id: u32,
    pub(crate) input: InheritedInput,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum InheritedInput {
    Pty {
        device: u64,
        inode: u64,
        special_device: u64,
    },
    NonTerminal,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TerminalObservation {
    LivePty,
    ClosedPty,
    NonTerminal,
    MissingProcess,
    ExitedProcess,
    IdentityMismatch,
    InputChanged,
    Unavailable,
}

impl InheritedInput {
    pub(crate) fn from_stdin() -> Self {
        open(
            "/proc/self/fd/0",
            OFlag::O_PATH | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map(|fd| Self::from_file(&fd.into()))
        .unwrap_or(Self::Unavailable)
    }

    pub(crate) fn from_file(file: &File) -> Self {
        let Ok(metadata) = file.metadata() else {
            return Self::Unavailable;
        };
        if !metadata.file_type().is_char_device() {
            return Self::NonTerminal;
        }
        match fstatfs(file) {
            Ok(filesystem)
                if filesystem.filesystem_type() == DEVPTS_SUPER_MAGIC
                    && metadata.rdev() != nix::sys::stat::makedev(5, 2) =>
            {
                Self::Pty {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    special_device: metadata.rdev(),
                }
            }
            Ok(_) => Self::NonTerminal,
            Err(_) => Self::Unavailable,
        }
    }
}

impl ForegroundIdentity {
    /// MCP metadata is accepted only from the host bridge launched directly by
    /// this provider. Agent-selected RPC payloads do not establish provenance.
    pub(crate) fn owns_bridge(&self, process_id: u32) -> bool {
        let check = || -> io::Result<bool> {
            let provider = process_directory(self.process_id)?;
            if self.verify(&provider)?.is_some() {
                return Ok(false);
            }
            let bridge = process_directory(process_id)?;
            let stat = process_stat(&bridge)?;
            let executable: File = openat(
                &bridge,
                "exe",
                OFlag::O_PATH | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?
            .into();
            let executable = executable.metadata()?;
            let current = std::fs::metadata(std::env::current_exe()?)?;
            Ok(stat.parent_id == self.process_id
                && !stat.exited
                && bridge.metadata()?.uid() == self.user_id
                && executable.dev() == current.dev()
                && executable.ino() == current.ino()
                && self.verify(&provider)?.is_none())
        };
        check().unwrap_or(false)
    }

    /// The caller retains the unreaped child, so this PID cannot yet be reused.
    pub(crate) fn capture_child(
        process_id: u32,
        input: InheritedInput,
    ) -> Option<Self> {
        let directory = process_directory(process_id).ok()?;
        let stat = process_stat(&directory).ok()?;
        if stat.process_id != process_id || stat.parent_id != std::process::id()
        {
            return None;
        }
        Some(Self {
            process_id,
            boot_id: boot_id().ok()?,
            start_ticks: stat.start_ticks,
            user_id: directory.metadata().ok()?.uid(),
            input,
        })
    }

    pub(crate) fn inspect(&self) -> TerminalObservation {
        self.inspect_process()
            .unwrap_or(TerminalObservation::Unavailable)
    }

    fn inspect_process(&self) -> io::Result<TerminalObservation> {
        let directory = match process_directory(self.process_id) {
            Ok(directory) => directory,
            Err(error) if absent(&error) => {
                return Ok(TerminalObservation::MissingProcess);
            }
            Err(error) => return Err(error),
        };
        if let Some(observation) = self.verify(&directory)? {
            return Ok(observation);
        }
        let observation = match self.input {
            InheritedInput::NonTerminal => TerminalObservation::NonTerminal,
            InheritedInput::Unavailable => TerminalObservation::Unavailable,
            InheritedInput::Pty { .. } => {
                // O_PATH pins the inode without opening a device for I/O. Resolving
                // it relative to the procfs handle cannot select a replacement PID.
                let input: File = openat(
                    &directory,
                    "fd/0",
                    OFlag::O_PATH | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )?
                .into();
                if InheritedInput::from_file(&input) != self.input {
                    TerminalObservation::InputChanged
                } else {
                    match input.metadata()?.nlink() {
                        0 => TerminalObservation::ClosedPty,
                        1 => TerminalObservation::LivePty,
                        _ => TerminalObservation::Unavailable,
                    }
                }
            }
        };
        Ok(self.verify(&directory)?.unwrap_or(observation))
    }

    fn verify(
        &self,
        directory: &File,
    ) -> io::Result<Option<TerminalObservation>> {
        let stat = match process_stat(directory) {
            Ok(stat) => stat,
            Err(error) if absent(&error) => {
                return Ok(Some(TerminalObservation::MissingProcess));
            }
            Err(error) => return Err(error),
        };
        if stat.process_id != self.process_id
            || stat.start_ticks != self.start_ticks
            || directory.metadata()?.uid() != self.user_id
            || boot_id()? != self.boot_id
        {
            return Ok(Some(TerminalObservation::IdentityMismatch));
        }
        Ok(stat.exited.then_some(TerminalObservation::ExitedProcess))
    }
}

fn absent(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
        || error.raw_os_error() == Some(nix::libc::ESRCH)
}

fn process_directory(process_id: u32) -> io::Result<File> {
    let directory: File = open(
        format!("/proc/{process_id}").as_str(),
        OFlag::O_PATH
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?
    .into();
    if fstatfs(&directory)?.filesystem_type() != PROC_SUPER_MAGIC {
        return Err(io::Error::other("process information is not on procfs"));
    }
    Ok(directory)
}

fn boot_id() -> io::Result<String> {
    let value = read_bounded(File::open("/proc/sys/kernel/random/boot_id")?)?;
    let value = value.trim();
    if value.len() != 36
        || !value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(io::Error::other("invalid Linux boot identity"));
    }
    Ok(value.to_owned())
}

fn read_bounded(file: File) -> io::Result<String> {
    let mut value = String::new();
    file.take(8193).read_to_string(&mut value)?;
    if value.len() > 8192 {
        return Err(io::Error::other(
            "process information exceeds its size limit",
        ));
    }
    Ok(value)
}

struct ProcessStat {
    process_id: u32,
    parent_id: u32,
    start_ticks: u64,
    exited: bool,
}

fn process_stat(directory: &File) -> io::Result<ProcessStat> {
    let file = openat(
        directory,
        "stat",
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    parse_stat(&read_bounded(file.into())?)
        .ok_or_else(|| io::Error::other("invalid process stat record"))
}

fn parse_stat(value: &str) -> Option<ProcessStat> {
    // Linux encloses comm in parentheses but does not escape parentheses or
    // whitespace inside it. Numeric fields begin after the last closing one.
    let (process_id, rest) = value.split_once(" (")?;
    let (_, fields) = rest.rsplit_once(')')?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    let state = *fields.first()?;
    if !["R", "S", "D", "Z", "T", "t", "X", "x", "K", "W", "P", "I"]
        .contains(&state)
    {
        return None;
    }
    Some(ProcessStat {
        process_id: process_id.parse().ok()?,
        parent_id: fields.get(1)?.parse().ok()?,
        start_ticks: fields.get(19)?.parse().ok()?,
        exited: matches!(state, "Z" | "X" | "x"),
    })
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
