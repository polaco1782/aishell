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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileIoOperation {
    Read,
    Write,
    Modify,
}

impl FileIoOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Modify => "modify",
        }
    }

    pub fn from_str(value: &str) -> Result<Self> {
        match value {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "modify" => Ok(Self::Modify),
            _ => bail!("file I/O log contains an unknown operation {value:?}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIoEvent {
    pub operation: FileIoOperation,
    pub path: Option<String>,
    pub offset: Option<u64>,
    pub outcome: FileIoOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileIoOutcome {
    Succeeded { content: String },
    Failed { reason: String },
}

pub struct FileToolExecution {
    pub response: String,
    pub audit: Option<FileIoEvent>,
}

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

    pub fn execute(&self, name: &str, arguments: &str) -> FileToolExecution {
        let (operation, result) = match name {
            READ_FILE_TOOL => (Some(FileIoOperation::Read), self.read(arguments)),
            WRITE_FILE_TOOL => (Some(FileIoOperation::Write), self.write(arguments)),
            _ => (None, Err(anyhow::anyhow!("unknown file tool {name:?}"))),
        };

        match result {
            Ok((response, audit)) => FileToolExecution {
                response: response.to_string(),
                audit: Some(audit),
            },
            Err(error) => {
                let reason = format!("{error:#}");
                FileToolExecution {
                    response: json!({"ok": false, "error": reason}).to_string(),
                    audit: operation.map(|operation| failed_event(operation, arguments, reason)),
                }
            }
        }
    }

    fn read(&self, arguments: &str) -> Result<(Value, FileIoEvent)> {
        let arguments: ReadFileArguments = parse_arguments(arguments, READ_FILE_TOOL)?;
        if arguments.max_bytes == 0 || arguments.max_bytes > MAX_READ_BYTES {
            bail!("max_bytes must be between 1 and {MAX_READ_BYTES}");
        }

        let (relative, path) = self.existing_file(&arguments.path, true)?;
        let mut file =
            File::open(&path).with_context(|| format!("could not open {}", relative.display()))?;
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

        let path = relative.to_string_lossy().into_owned();
        Ok((
            json!({
                "ok": true,
                "path": path,
                "offset": arguments.offset,
                "next_offset": next_offset,
                "eof": next_offset == metadata.len(),
                "content": content,
            }),
            FileIoEvent {
                operation: FileIoOperation::Read,
                path: Some(path),
                offset: Some(arguments.offset),
                outcome: FileIoOutcome::Succeeded {
                    content: content.to_owned(),
                },
            },
        ))
    }

    fn write(&self, arguments: &str) -> Result<(Value, FileIoEvent)> {
        let arguments: WriteFileArguments = parse_arguments(arguments, WRITE_FILE_TOOL)?;
        if arguments.content.len() > MAX_WRITE_BYTES {
            bail!("content exceeds the {MAX_WRITE_BYTES} byte write limit");
        }

        let relative = validate_relative_path(&arguments.path)?;
        let requested = self.root.join(&relative);
        reject_symlink_components(&self.root, &relative, true)?;

        let (target, permissions, operation) = match fs::symlink_metadata(&requested) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    bail!(
                        "refusing to write through the symbolic link {}",
                        relative.display()
                    );
                }
                let target = requested
                    .canonicalize()
                    .with_context(|| format!("could not resolve {}", relative.display()))?;
                self.require_within_root(&target, &relative)?;
                if !metadata.is_file() {
                    bail!("{} is not a regular file", relative.display());
                }
                (
                    target,
                    Some(metadata.permissions()),
                    FileIoOperation::Modify,
                )
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
                (parent.join(file_name), None, FileIoOperation::Write)
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
        let path = relative.to_string_lossy().into_owned();
        Ok((
            json!({
                "ok": true,
                "path": path,
                "bytes_written": arguments.content.len(),
            }),
            FileIoEvent {
                operation,
                path: Some(path),
                offset: None,
                outcome: FileIoOutcome::Succeeded {
                    content: arguments.content,
                },
            },
        ))
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
        if !path.starts_with(&self.root) {
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

fn failed_event(operation: FileIoOperation, arguments: &str, reason: String) -> FileIoEvent {
    let arguments = serde_json::from_str::<Value>(arguments).ok();
    let path = arguments
        .as_ref()
        .and_then(|arguments| arguments.get("path"))
        .and_then(Value::as_str)
        .filter(|path| path.len() <= MAX_PATH_BYTES)
        .map(str::to_owned);
    let offset = if operation == FileIoOperation::Read {
        arguments
            .as_ref()
            .and_then(|arguments| arguments.get("offset"))
            .and_then(Value::as_u64)
    } else {
        None
    };

    FileIoEvent {
        operation,
        path,
        offset,
        outcome: FileIoOutcome::Failed { reason },
    }
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

    use super::{FileIoOperation, FileIoOutcome, FileToolExecution, WorkspaceFiles};

    fn result(execution: &FileToolExecution) -> Value {
        serde_json::from_str(&execution.response).unwrap()
    }

    #[test]
    fn reads_bounded_utf8_chunks() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("script.sh"), "echo café\necho done\n").unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        let first_execution = tools.execute("read_file", r#"{"path":"script.sh","max_bytes":9}"#);
        let first = result(&first_execution);
        assert_eq!(first["ok"], true);
        assert_eq!(first["content"], "echo caf");
        assert_eq!(first["next_offset"], 8);
        assert_eq!(first["eof"], false);
        assert_eq!(
            first_execution.audit.unwrap().operation,
            FileIoOperation::Read
        );

        let second_execution = tools.execute(
            "read_file",
            r#"{"path":"script.sh","offset":8,"max_bytes":32}"#,
        );
        let second = result(&second_execution);
        assert_eq!(second["content"], "é\necho done\n");
        assert_eq!(second["eof"], true);
        assert_eq!(second_execution.audit.unwrap().offset, Some(8));
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

        let execution = tools.execute(
            "write_file",
            r##"{"path":"script.sh","content":"#!/bin/sh\necho new\n"}"##,
        );
        let written = result(&execution);
        assert_eq!(written["ok"], true);
        assert_eq!(fs::read_to_string(&path).unwrap(), "#!/bin/sh\necho new\n");
        let audit = execution.audit.unwrap();
        assert_eq!(audit.operation, FileIoOperation::Modify);
        assert_eq!(audit.path.as_deref(), Some("script.sh"));
        assert_eq!(
            audit.outcome,
            FileIoOutcome::Succeeded {
                content: "#!/bin/sh\necho new\n".into()
            }
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[test]
    fn creates_files_at_the_workspace_root_and_in_existing_subdirectories() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("scripts")).unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        let root_creation = tools.execute(
            "write_file",
            r##"{"path":"run.sh","content":"#!/bin/sh\n"}"##,
        );
        assert_eq!(result(&root_creation)["ok"], true);
        assert_eq!(
            fs::read_to_string(directory.path().join("run.sh")).unwrap(),
            "#!/bin/sh\n"
        );

        let creation = tools.execute(
            "write_file",
            r#"{"path":"scripts/new.sh","content":"echo hello\n"}"#,
        );
        assert_eq!(result(&creation)["ok"], true);
        assert_eq!(creation.audit.unwrap().operation, FileIoOperation::Write);
        assert_eq!(
            fs::read_to_string(directory.path().join("scripts/new.sh")).unwrap(),
            "echo hello\n"
        );
        let denied = tools.execute(
            "write_file",
            r#"{"path":"missing/new.sh","content":"nope"}"#,
        );
        assert_eq!(result(&denied)["ok"], false);
        let audit = denied.audit.unwrap();
        assert_eq!(audit.path.as_deref(), Some("missing/new.sh"));
        assert!(matches!(audit.outcome, FileIoOutcome::Failed { .. }));
    }

    #[test]
    fn rejects_absolute_paths_and_parent_traversal() {
        let directory = tempdir().unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        for arguments in [
            r#"{"path":"../outside","content":"nope"}"#,
            r#"{"path":"/tmp/outside","content":"nope"}"#,
        ] {
            let execution = tools.execute("write_file", arguments);
            assert_eq!(result(&execution)["ok"], false);
            let audit = execution.audit.unwrap();
            assert_eq!(audit.operation, FileIoOperation::Write);
            let FileIoOutcome::Failed { reason } = audit.outcome else {
                panic!("denied write was logged as successful");
            };
            assert!(reason.contains("path must be relative") || reason.contains("parent"));
        }
    }

    #[test]
    fn logs_malformed_read_failures_without_inventing_a_path() {
        let directory = tempdir().unwrap();
        let tools = WorkspaceFiles::new(directory.path()).unwrap();

        let execution = tools.execute("read_file", r#"{"path":42}"#);
        assert_eq!(result(&execution)["ok"], false);
        let audit = execution.audit.unwrap();
        assert_eq!(audit.operation, FileIoOperation::Read);
        assert_eq!(audit.path, None);
        let FileIoOutcome::Failed { reason } = audit.outcome else {
            panic!("malformed read was logged as successful");
        };
        assert!(reason.contains("invalid read_file arguments"));
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
            result(&tools.execute("read_file", r#"{"path":"outside/secret"}"#))["ok"],
            false
        );
        assert_eq!(
            result(&tools.execute("write_file", r#"{"path":"outside/new","content":"nope"}"#,))["ok"],
            false
        );
    }

    #[test]
    fn rejects_filesystem_roots_as_the_tool_scope() {
        let root = std::path::Path::new(std::path::MAIN_SEPARATOR_STR);
        assert!(WorkspaceFiles::new(root).is_err());
    }
}
