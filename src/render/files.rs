use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Replace `path` only after all new contents have been written to a private,
/// same-directory temporary file. Existing permissions are retained.
pub fn write_atomically(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let destination = resolved_destination(path)?;
    let directory = destination
        .parent()
        .filter(|directory| !directory.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("'{}' has no file name", destination.display()),
        )
    })?;
    let permissions = fs::metadata(&destination)
        .ok()
        .map(|metadata| metadata.permissions());

    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".gph-{}-{sequence}.tmp", std::process::id()));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        let result = (|| {
            file.write_all(contents.as_ref())?;
            file.flush()?;
            drop(file);
            if let Some(permissions) = permissions {
                fs::set_permissions(&temporary, permissions)?;
            }
            fs::rename(&temporary, &destination)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("cannot allocate a temporary file for '{}'", path.display()),
    ))
}

fn resolved_destination(path: &Path) -> io::Result<PathBuf> {
    let mut destination = path.to_owned();
    for followed in 0..=40 {
        match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() && followed == 40 => {
                return Err(io::Error::other(format!(
                    "too many symbolic links resolving '{}'",
                    path.display()
                )));
            }
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&destination)?;
                destination = if target.is_absolute() {
                    target
                } else {
                    destination
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."))
                        .join(target)
                };
            }
            Ok(_) => return Ok(destination),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(destination),
            Err(error) => return Err(error),
        }
    }
    unreachable!("the symlink limit branch returns on the final iteration")
}

#[cfg(test)]
mod tests {
    use super::write_atomically;

    fn temporary_path(name: &str) -> std::path::PathBuf {
        let sequence = super::TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("gph-{name}-{}-{sequence}", std::process::id()))
    }

    #[test]
    fn replaces_contents_without_leaving_a_temporary_file() {
        let path = temporary_path("atomic");
        std::fs::write(&path, "before").unwrap();
        write_atomically(&path, "after").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn writes_atomically_to_a_255_byte_basename_when_supported() {
        let root = temporary_path("long-name");
        std::fs::create_dir(&root).unwrap();
        let path = root.join("x".repeat(255));

        let Ok(()) = std::fs::write(&path, "before") else {
            std::fs::remove_dir_all(root).unwrap();
            return;
        };

        write_atomically(&path, "after").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    fn symlink_chain(root: &std::path::Path, links: usize) -> std::path::PathBuf {
        use std::os::unix::fs::symlink;

        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("target"), "before").unwrap();
        for index in (0..links).rev() {
            let target = if index + 1 == links {
                "target".to_string()
            } else {
                format!("link{}", index + 1)
            };
            symlink(target, root.join(format!("link{index}"))).unwrap();
        }
        root.join("link0")
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_a_relative_symlink_without_replacing_it() {
        use std::os::unix::fs::symlink;

        let root = temporary_path("symlink");
        let links = root.join("links");
        let targets = root.join("targets");
        std::fs::create_dir_all(&links).unwrap();
        std::fs::create_dir_all(&targets).unwrap();
        let target = targets.join("output.svg");
        let link = links.join("output.svg");
        std::fs::write(&target, "before").unwrap();
        symlink("../targets/output.svg", &link).unwrap();

        write_atomically(&link, "after").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            std::path::Path::new("../targets/output.svg")
        );
        assert_eq!(std::fs::read_to_string(target).unwrap(), "after");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn follows_exactly_forty_symbolic_links() {
        let root = temporary_path("forty-links");
        let link = symlink_chain(&root, 40);

        write_atomically(&link, "after").unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("target")).unwrap(),
            "after"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_more_than_forty_symbolic_links() {
        let root = temporary_path("forty-one-links");
        let link = symlink_chain(&root, 41);

        let error = write_atomically(&link, "after").unwrap_err();

        assert!(error.to_string().contains("too many symbolic links"));
        assert_eq!(
            std::fs::read_to_string(root.join("target")).unwrap(),
            "before"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
