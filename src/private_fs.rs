//! Ownership, type, and permission checks for Coterie's private files.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::unix::fs::{
    DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt,
};
use std::path::Path;

use nix::fcntl::OFlag;
use nix::unistd::geteuid;

fn validate(
    metadata: &Metadata,
    path: &Path,
    directory: bool,
) -> io::Result<()> {
    // Retirement can unlink a file after open but before metadata inspection.
    // Classify that pinned inode directly, without racing another path lookup.
    if !directory && metadata.is_file() && metadata.nlink() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("private file {} has been unlinked", path.display()),
        ));
    }
    if metadata.uid() != geteuid().as_raw()
        || (if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        })
        || metadata.mode() & 0o7777 != if directory { 0o700 } else { 0o600 }
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "private path {} must be owned by the current user, have mode {}, and be a {} (observed uid {}, mode {:04o}, links {})",
                path.display(),
                if directory { "0700" } else { "0600" },
                if directory {
                    "directory"
                } else {
                    "regular file with one link"
                },
                metadata.uid(),
                metadata.mode() & 0o7777,
                metadata.nlink()
            ),
        ));
    }
    Ok(())
}

pub(crate) fn check_directory(path: &Path) -> io::Result<()> {
    validate(&fs::symlink_metadata(path)?, path, true)
}

pub(crate) fn directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            crate::fault::point("private.directory.before");
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
            crate::fault::point("private.directory.after");
        }
        Err(error) => return Err(error),
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_DIRECTORY).bits())
        .open(path)?;
    if file.metadata()?.uid() != geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private directory belongs to another user",
        ));
    }
    crate::fault::point("private.permissions.before");
    file.set_permissions(fs::Permissions::from_mode(0o700))?;
    crate::fault::point("private.permissions.after");
    validate(&file.metadata()?, path, true)
}

pub(crate) fn open(path: &Path, write: bool, create: bool) -> io::Result<File> {
    if create {
        crate::fault::point("private.file.before");
    }
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .create(create)
        .mode(0o600)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits())
        .open(path)?;
    if create {
        crate::fault::point("private.file.after");
    }
    validate(&file.metadata()?, path, false)?;
    Ok(file)
}

/// Inspects a file without releasing any POSIX record locks when it closes.
pub(crate) fn inspect(path: &Path) -> io::Result<File> {
    // Closing an ordinary descriptor would release SQLite's locks on the same
    // inode, even when SQLite owns a different descriptor in this process.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_PATH | OFlag::O_NOFOLLOW).bits())
        .open(path)?;
    validate(&file.metadata()?, path, false)?;
    Ok(file)
}

/// Checks SQLite's adjacent files before its connection can follow their paths.
pub(crate) fn database(path: &Path, create: bool) -> io::Result<File> {
    if create {
        crate::fault::point("private.file.before");
        // Only a newly created file may use an ordinary descriptor. Existing
        // databases can already have live SQLite connections in this process.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits())
            .open(path)
        {
            Ok(file) => drop(file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        crate::fault::point("private.file.after");
    }
    let file = inspect(path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut adjacent = path.as_os_str().to_os_string();
        adjacent.push(suffix);
        let adjacent = Path::new(&adjacent);
        match inspect(adjacent) {
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(file)
}

pub(crate) fn check_socket(path: &Path) -> io::Result<()> {
    validate_socket(&fs::symlink_metadata(path)?, path)
}

fn validate_socket(metadata: &Metadata, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    // Recovery can unlink the socket between path lookup and metadata capture.
    // Treat the vanished inode as missing so clients can retry recovery.
    if metadata.file_type().is_socket() && metadata.nlink() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("socket {} has been unlinked", path.display()),
        ));
    }
    if !metadata.file_type().is_socket()
        || metadata.uid() != geteuid().as_raw()
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket {} must be owned by the current user and have mode 0600 and one link",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Pins the socket's filesystem inode while its listener can be replaced or closed.
pub(crate) fn socket(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_PATH | OFlag::O_NOFOLLOW).bits())
        .open(path)?;
    check_socket(path)?;
    same_file(&file, path)?;
    Ok(file)
}

/// Requires the pathname to still identify the inspected, open inode.
pub(crate) fn same_file(file: &File, path: &Path) -> io::Result<()> {
    let expected = file.metadata()?;
    let actual = fs::symlink_metadata(path)?;
    if expected.dev() != actual.dev() || expected.ino() != actual.ino() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "path {} no longer identifies the owned file",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::fcntl::{FcntlArg, fcntl};
    use nix::libc;
    use std::os::unix::fs::symlink;

    #[test]
    fn an_unlinked_private_file_is_missing_instead_of_insecure() {
        let root = std::env::temp_dir()
            .join(format!("coterie-unlinked-{}", crate::id::RunId::generate()));
        directory(&root).unwrap();
        let path = root.join("index.json");
        let file = open(&path, true, true).unwrap();
        fs::remove_file(&path).unwrap();

        // An index reader can pin its inode just before retirement unlinks it.
        let error = validate(&file.metadata().unwrap(), &path, false)
            .expect_err("an unlinked inode must not be accepted");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        drop(file);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unlinked_socket_is_missing_instead_of_insecure() {
        let root = std::env::temp_dir()
            .join(format!("coterie-unlinked-{}", crate::id::RunId::generate()));
        directory(&root).unwrap();
        let path = root.join("supervisor.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let inode = socket(&path).unwrap();
        fs::remove_file(&path).unwrap();

        // A path lookup can resolve the inode before recovery unlinks it and
        // return its metadata afterward. Pin it to reproduce that observation.
        let metadata = inode.metadata().unwrap();
        assert_eq!(metadata.nlink(), 0);
        let error = validate_socket(&metadata, &path)
            .expect_err("an unlinked socket must not be accepted");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        drop(inode);
        drop(listener);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn socket_validation_refuses_unsafe_paths_without_modification() {
        let root = std::env::temp_dir()
            .join(format!("coterie-socket-{}", crate::id::RunId::generate()));
        directory(&root).unwrap();
        let path = root.join("supervisor.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        for mode in [0o644, 0o666] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .unwrap();
            assert_eq!(
                check_socket(&path).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(
                fs::symlink_metadata(&path).unwrap().mode() & 0o7777,
                mode
            );
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        check_socket(&path).unwrap();

        let link = root.join("link");
        fs::hard_link(&path, &link).unwrap();
        assert_eq!(
            check_socket(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::symlink_metadata(&path).unwrap().nlink(), 2);
        fs::remove_file(&link).unwrap();
        symlink(&path, &link).unwrap();
        assert_eq!(
            check_socket(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::read_link(&link).unwrap(), path);

        let regular_path = root.join("regular");
        let regular = open(&regular_path, true, true).unwrap();
        for candidate in [&regular_path, &root] {
            assert_eq!(
                check_socket(candidate).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        fs::remove_file(&regular_path).unwrap();
        assert_eq!(
            validate_socket(&regular.metadata().unwrap(), &regular_path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        drop(regular);
        drop(listener);
        fs::remove_dir_all(root).unwrap();
    }

    fn assert_sqlite_lock(file: &File) {
        // OFD queries also report POSIX locks owned by this process, so the
        // regression needs neither a fork nor timing-dependent contention.
        let mut lock = libc::flock {
            l_type: libc::F_WRLCK as _,
            l_whence: libc::SEEK_SET as _,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };
        fcntl(file, FcntlArg::F_OFD_GETLK(&mut lock)).unwrap();
        assert_ne!(
            lock.l_type,
            libc::F_UNLCK as libc::c_short,
            "SQLite lost its lock"
        );
    }

    #[test]
    fn database_creation_guard_preserves_live_sqlite_locks() {
        let root = std::env::temp_dir()
            .join(format!("coterie-locks-{}", crate::id::RunId::generate()));
        directory(&root).unwrap();
        let path = root.join("state.sqlite3");
        let guard = database(&path, true).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "wal")
            .unwrap();
        connection
            .execute_batch("CREATE TABLE sample (value INTEGER)")
            .unwrap();
        let probe = File::open(&path).unwrap();
        assert_sqlite_lock(&probe);

        drop(guard);

        assert_sqlite_lock(&probe);
        drop(connection);
        drop(probe);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn database_validation_preserves_live_sqlite_locks() {
        for journal_mode in ["delete", "wal"] {
            let root = std::env::temp_dir().join(format!(
                "coterie-locks-{}",
                crate::id::RunId::generate()
            ));
            directory(&root).unwrap();
            let path = root.join("state.sqlite3");
            drop(database(&path, true).unwrap());
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .pragma_update(None, "journal_mode", journal_mode)
                .unwrap();
            connection.execute_batch("CREATE TABLE sample (value INTEGER); BEGIN IMMEDIATE; INSERT INTO sample VALUES (1)").unwrap();
            // Keep probe descriptors open until SQLite closes its connection.
            let mut probes = vec![File::open(&path).unwrap()];
            if journal_mode == "wal" {
                probes
                    .push(File::open(root.join("state.sqlite3-shm")).unwrap());
            }
            for probe in &probes {
                assert_sqlite_lock(probe);
            }

            for create in [false, true] {
                drop(database(&path, create).unwrap());
                for probe in &probes {
                    assert_sqlite_lock(probe);
                }
            }

            connection.execute_batch("ROLLBACK").unwrap();
            drop(connection);
            drop(probes);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn database_validation_refuses_unsafe_files_without_modification() {
        let root = std::env::temp_dir()
            .join(format!("coterie-private-{}", crate::id::RunId::generate()));
        directory(&root).unwrap();
        let path = root.join("state.sqlite3");
        drop(database(&path, true).unwrap());
        let target = root.join("target");
        fs::write(&target, b"preserve").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .unwrap();

        for suffix in ["", "-wal", "-shm", "-journal"] {
            let candidate = root.join(format!("state.sqlite3{suffix}"));
            drop(open(&candidate, true, true).unwrap());
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644))
                .unwrap();
            for create in [false, true] {
                assert!(database(&path, create).is_err());
            }
            assert_eq!(fs::metadata(&candidate).unwrap().mode() & 0o777, 0o644);
            fs::remove_file(&candidate).unwrap();

            for destination in [&target, &root.join("missing")] {
                symlink(destination, &candidate).unwrap();
                for create in [false, true] {
                    assert!(database(&path, create).is_err());
                }
                assert_eq!(fs::read_link(&candidate).unwrap(), *destination);
                fs::remove_file(&candidate).unwrap();
            }
            assert!(!root.join("missing").exists());

            fs::hard_link(&target, &candidate).unwrap();
            for create in [false, true] {
                assert!(database(&path, create).is_err());
            }
            assert_eq!(fs::metadata(&target).unwrap().nlink(), 2);
            fs::remove_file(&candidate).unwrap();
            drop(database(&path, true).unwrap());
        }

        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_symlinks_hardlinks_and_public_files_without_modification() {
        let root = std::env::temp_dir()
            .join(format!("coterie-private-{}", crate::id::RunId::generate()));
        directory(&root).unwrap();
        let path = root.join("state");
        let file = open(&path, true, true).unwrap();
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(open(&path, true, false).is_err());
        assert!(inspect(&path).is_err());
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o644);
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        let link = root.join("link");
        symlink(&path, &link).unwrap();
        assert!(open(&link, true, false).is_err());
        assert!(inspect(&link).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert!(open(&path, true, false).is_err());
        assert!(inspect(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
