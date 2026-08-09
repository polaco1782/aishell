use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

pub fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || unsupported_reparse_point(&metadata)
                || !metadata.is_dir()
            {
                bail!("{} is not a private directory", path.display());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .with_context(|| format!("could not create {}", path.display()))?;
        }
        Err(error) => {
            return Err(error).with_context(|| format!("could not inspect {}", path.display()));
        }
    }

    set_directory_permissions(path)
}

pub fn verify_private_file(path: &Path, description: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {description} at {}", path.display()))?;
    if metadata.file_type().is_symlink() || unsupported_reparse_point(&metadata) {
        bail!(
            "refusing to read {description} through a link or reparse point at {}",
            path.display()
        );
    }
    if !metadata.is_file() {
        bail!("{description} {} is not a regular file", path.display());
    }

    verify_file_permissions(path, &metadata, description)
}

pub fn create_private_file(path: &Path, description: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("the private file path has no parent directory")?;
    ensure_private_directory(parent)?;

    match fs::symlink_metadata(path) {
        Ok(_) => verify_private_file(path, description),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            open_private_file(path, description).map(drop)
        }
        Err(error) => Err(error)
            .with_context(|| format!("could not inspect {description} at {}", path.display())),
    }
}

pub fn atomic_write_private(path: &Path, contents: &[u8], description: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("the private file path has no parent directory")?;
    ensure_private_directory(parent)?;

    match fs::symlink_metadata(path) {
        Ok(_) => verify_private_file(path, description)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not inspect {description} at {}", path.display()));
        }
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("private-file");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary_path = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));

    let write_result = (|| -> Result<()> {
        let mut temporary = open_private_file(&temporary_path, "temporary file")?;
        temporary
            .write_all(contents)
            .context("could not write temporary file")?;
        temporary
            .sync_all()
            .context("could not flush temporary file")?;
        replace_file(&temporary_path, path)
            .with_context(|| format!("could not replace {description} at {}", path.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    write_result
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    fn wide_path(path: &Path) -> std::io::Result<Vec<u16>> {
        let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path contains a null character",
            ));
        }
        wide.push(0);
        Ok(wide)
    }

    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    // Both paths are resolved and owned by this function for the duration of the call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("could not flush {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn unsupported_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn unsupported_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn verify_file_permissions(path: &Path, metadata: &fs::Metadata, description: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "{description} {} has insecure permissions {mode:03o}; run `chmod 600 {}`",
            path.display(),
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_file_permissions(
    _path: &Path,
    _metadata: &fs::Metadata,
    _description: &str,
) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not secure {}", path.display()))
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn open_private_file(path: &Path, description: &str) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("could not create {description} at {}", path.display()))
}

#[cfg(not(unix))]
fn open_private_file(path: &Path, description: &str) -> Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("could not create {description} at {}", path.display()))
}
