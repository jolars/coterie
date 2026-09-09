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
                "private path {} must be owned by the current user, have mode {}, and be a {}",
                path.display(),
                if directory { "0700" } else { "0600" },
                if directory {
                    "directory"
                } else {
                    "regular file with one link"
                }
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

/// Checks SQLite's adjacent files before its connection can follow their paths.
pub(crate) fn database(path: &Path, create: bool) -> io::Result<File> {
    let file = open(path, create, create)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut adjacent = path.as_os_str().to_os_string();
        adjacent.push(suffix);
        let adjacent = Path::new(&adjacent);
        match fs::symlink_metadata(adjacent) {
            Ok(_) => {
                open(adjacent, false, false)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(file)
}

pub(crate) fn check_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let metadata = fs::symlink_metadata(path)?;
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
    use std::os::unix::fs::symlink;

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
        assert_eq!(file.metadata().unwrap().mode() & 0o777, 0o644);
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        let link = root.join("link");
        symlink(&path, &link).unwrap();
        assert!(open(&link, true, false).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert!(open(&path, true, false).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
