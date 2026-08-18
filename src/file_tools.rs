use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::secure_fs::atomic_write_file;

pub const READ_FILE_TOOL: &str = "read_file";
pub const WRITE_FILE_TOOL: &str = "write_file";

const DEFAULT_READ_BYTES: usize = 16 * 1024;
const MAX_READ_BYTES: usize = 32 * 1024;
const MAX_WRITE_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 4096;

pub struct WorkspaceFiles {
    root: PathBuf,
}

impl WorkspaceFiles {
    pub fn new(working_directory: &Path) -> Result<Self> {
        let root = working_directory.canonicalize().with_context(|| {
            format!(
                "could not resolve file tool root {}",
                working_directory.display()
            )
        })?;
        if root.parent().is_none() {
            bail!("file tools cannot use a filesystem root as their working directory");
        }
        if !root.is_dir() {
            bail!("file tool root {} is not a directory", root.display());
        }
        Ok(Self { root })
    }

    pub fn execute(&self, name: &str, arguments: &str) -> String {
        let result = match name {
            READ_FILE_TOOL => self.read(arguments),
            WRITE_FILE_TOOL => self.write(arguments),
            _ => Err(anyhow::anyhow!("unknown file tool {name:?}")),
        };

        match result {
            Ok(result) => result.to_string(),
            Err(error) => json!({"ok": false, "error": format!("{error:#}")}).to_string(),
        }
    }

    fn read(&self, arguments: &str) -> Result<Value> {
        let arguments: ReadFileArguments = parse_arguments(arguments, READ_FILE_TOOL)?;
        if arguments.max_bytes == 0 || arguments.max_bytes > MAX_READ_BYTES {
            bail!("max_bytes must be between 1 and {MAX_READ_BYTES}");
        }

        let (relative, path) = self.existing_file(&arguments.path, true)?;
        let mut file = File::open(&path)
            .with_context(|| format!("could not open {}", relative.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("could not inspect {}", relative.display()))?;
        if !metadata.is_file() {
            bail!("{} is not a regular file", relative.display());
        }
        if arguments.offset > metadata.len() {
            bail!(
                "offset {} is beyond the {} byte file {}",
                arguments.offset,
                metadata.len(),
                relative.display()
            );
        }

        file.seek(SeekFrom::Start(arguments.offset))
            .with_context(|| format!("could not seek {}", relative.display()))?;
        let mut bytes = Vec::with_capacity(arguments.max_bytes);
        file.take(arguments.max_bytes as u64)
            .read_to_end(&mut bytes)
            .with_context(|| format!("could not read {}", relative.display()))?;
        let content = utf8_prefix(&bytes)?;
        let next_offset = arguments.offset + content.len() as u64;

        Ok(json!({
            "ok": true,
            "path": relative.to_string_lossy(),
            "offset": arguments.offset,
            "next_offset": next_offset,
            "eof": next_offset == metadata.len(),
            "content": content,
        }))
    }

    fn write(&self, arguments: &str) -> Result<Value> {
        let arguments: WriteFileArguments = parse_arguments(arguments, WRITE_FILE_TOOL)?;
        if arguments.content.len() > MAX_WRITE_BYTES {
            bail!("content exceeds the {MAX_WRITE_BYTES} byte write limit");
        }

        let relative = validate_relative_path(&arguments.path)?;
        let requested = self.root.join(&relative);
        reject_symlink_components(&self.root, &relative, true)?;

        let (target, permissions) = match fs::symlink_metadata(&requested) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!("refusing to write through the symbolic link {}", relative.display());
                }
                let target = requested
                    .canonicalize()
                    .with_context(|| format!("could not resolve {}", relative.display()))?;
                self.require_within_root(&target, &relative)?;
                if !metadata.is_file() {
                    bail!("{} is not a regular file", relative.display());
                }
                (target, Some(metadata.permissions()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = requested
                    .parent()
                    .context("the file path has no parent directory")?
                    .canonicalize()
                    .with_context(|| {
                        format!("parent directory for {} does not exist", relative.display())
                    })?;
                self.require_within_root(&parent, &relative)?;
                let file_name = requested
                    .file_name()
                    .context("the file path has no file name")?;
                (parent.join(file_name), None)
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not inspect {}", relative.display()));
            }
        };

        atomic_write_file(
            &target,
            arguments.content.as_bytes(),
            "workspace file",
            permissions,
        )?;
        Ok(json!({
            "ok": true,
            "path": relative.to_string_lossy(),
            "bytes_written": arguments.content.len(),
        }))
    }

    fn existing_file(&self, input: &str, follow_final_link: bool) -> Result<(PathBuf, PathBuf)> {
        let relative = validate_relative_path(input)?;
        reject_symlink_components(&self.root, &relative, follow_final_link)?;
        let path = self
            .root
            .join(&relative)
            .canonicalize()
            .with_context(|| format!("could not resolve {}", relative.display()))?;
        self.require_within_root(&path, &relative)?;
        Ok((relative, path))
    }

    fn require_within_root(&self, path: &Path, relative: &Path) -> Result<()> {
        if !path.starts_with(&self.root) || path == self.root {
            bail!(
                "{} resolves outside the current working directory",
                relative.display()
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadFileArguments {
    path: String,
    #[serde(default)]
    offset: u64,
    #[serde(default = "default_read_bytes")]
    max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteFileArguments {
    path: String,
    content: String,
}

fn parse_arguments<'a, T: Deserialize<'a>>(arguments: &'a str, tool: &str) -> Result<T> {
    serde_json::from_str(arguments).with_context(|| format!("invalid {tool} arguments"))
}

fn validate_relative_path(input: &str) -> Result<PathBuf> {
    if input.is_empty() {
        bail!("path cannot be empty");
    }
    if input.len() > MAX_PATH_BYTES {
        bail!("path exceeds {MAX_PATH_BYTES} bytes");
    }

    let path = Path::new(input);
    if path.is_absolute() {
        bail!("path must be relative to the current working directory");
    }

    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(component) => relative.push(component),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("path cannot contain parent or root components");
            }
        }
    }
    if relative.as_os_str().is_empty() {
        bail!("path must name a file below the current working directory");
    }
    Ok(relative)
}

fn reject_symlink_components(root: &Path, relative: &Path, follow_final_link: bool) -> Result<()> {
    let component_count = relative.components().count();
    let mut current = root.to_path_buf();
    for (index, component) in relative.components().enumerate() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    && (!follow_final_link || index + 1 != component_count) =>
            {
                bail!("path cannot traverse symbolic links");
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn utf8_prefix(bytes: &[u8]) -> Result<&str> {
    match std::str::from_utf8(bytes) {
        Ok(content) => Ok(content),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).context("file is not valid UTF-8")
        }
        Err(_) => bail!("file is not valid UTF-8"),
    }
}

const fn default_read_bytes() -> usize {
    DEFAULT_READ_BYTES
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::WorkspaceFiles;

    fn result(output: String) -> Value {
        serde_json::from_str(&output).unwrap()
    }

    #[test]
    fn reads_bounded_utf8_chunks() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("script.sh"), "echo café\necho done\n").unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        let first = result(tools.execute(
            "read_file",
            r#"{"path":"script.sh","max_bytes":9}"#,
        ));
        assert_eq!(first["ok"], true);
        assert_eq!(first["content"], "echo caf");
        assert_eq!(first["next_offset"], 8);
        assert_eq!(first["eof"], false);

        let second = result(tools.execute(
            "read_file",
            r#"{"path":"script.sh","offset":8,"max_bytes":32}"#,
        ));
        assert_eq!(second["content"], "é\necho done\n");
        assert_eq!(second["eof"], true);
    }

    #[test]
    fn writes_atomically_and_preserves_existing_permissions() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("script.sh");
        fs::write(&path, "old\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        let written = result(tools.execute(
            "write_file",
            r#"{"path":"script.sh","content":"\#!/bin/sh\necho new\n"}"#,
        ));
        assert_eq!(written["ok"], true);
        assert_eq!(fs::read_to_string(&path).unwrap(), "#!/bin/sh\necho new\n");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn creates_files_only_below_an_existing_workspace_directory() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("scripts")).unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        assert_eq!(
            result(tools.execute(
                "write_file",
                r#"{"path":"scripts/new.sh","content":"echo hello\n"}"#,
            ))["ok"],
            true
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("scripts/new.sh")).unwrap(),
            "echo hello\n"
        );
        assert_eq!(
            result(tools.execute(
                "write_file",
                r#"{"path":"missing/new.sh","content":"nope"}"#,
            ))["ok"],
            false
        );
    }

    #[test]
    fn rejects_absolute_paths_and_parent_traversal() {
        let directory = tempdir().unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        for arguments in [
            r#"{"path":"../outside","content":"nope"}"#,
            r#"{"path":"/tmp/outside","content":"nope"}"#,
        ] {
            assert_eq!(result(tools.execute("write_file", arguments))["ok"], false);
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_reads_and_writes_that_escape_the_workspace() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret"), "secret").unwrap();
        symlink(outside.path(), directory.path().join("outside")).unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        assert_eq!(
            result(tools.execute("read_file", r#"{"path":"outside/secret"}"#))["ok"],
            false
        );
        assert_eq!(
            result(tools.execute(
                "write_file",
                r#"{"path":"outside/new","content":"nope"}"#,
            ))["ok"],
            false
        );
    }

    #[test]
    fn rejects_filesystem_roots_as_the_tool_scope() {
        let root = std::path::Path::new(std::path::MAIN_SEPARATOR_STR);
        assert!(WorkspaceFiles::new(root).is_err());
    }
}
