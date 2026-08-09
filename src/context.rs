use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};

use crate::paths;
use crate::secure_fs::{create_private_file, verify_private_file};

const DATABASE_FILE_NAME: &str = "history.sqlite3";
const DATABASE_APPLICATION_ID: i64 = 0x4149_5348;
const DATABASE_SCHEMA_VERSION: i64 = 2;
const MAX_SESSION_ID_BYTES: usize = 128;
const MAX_STORED_REQUEST_BYTES: usize = 16 * 1024;
const TURNS_TABLE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS turns (
    id INTEGER PRIMARY KEY,
    scope TEXT NOT NULL REFERENCES contexts(scope) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    working_directory TEXT NOT NULL,
    request TEXT NOT NULL,
    response_kind TEXT NOT NULL CHECK(response_kind IN ('command', 'clarification', 'answer')),
    response TEXT NOT NULL
)";
const TURNS_INDEX_SCHEMA: &str = "CREATE INDEX IF NOT EXISTS turns_scope_id ON turns(scope, id)";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContextResponse {
    Command(String),
    Clarification(String),
    Answer(String),
}

impl ContextResponse {
    pub fn model_line(&self) -> String {
        match self {
            Self::Command(command) => format!("COMMAND: {command}"),
            Self::Clarification(question) => format!("QUESTION: {question}"),
            Self::Answer(answer) => format!("ANSWER: {answer}"),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Command(_) => "command",
            Self::Clarification(_) => "clarification",
            Self::Answer(_) => "answer",
        }
    }

    fn content(&self) -> &str {
        match self {
            Self::Command(content) | Self::Clarification(content) | Self::Answer(content) => {
                content
            }
        }
    }

    fn from_parts(kind: &str, content: String) -> Result<Self> {
        match kind {
            "command" => Ok(Self::Command(content)),
            "clarification" => Ok(Self::Clarification(content)),
            "answer" => Ok(Self::Answer(content)),
            _ => bail!("context database contains an unknown response kind {kind:?}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextTurn {
    pub created_at: i64,
    pub working_directory: String,
    pub request: String,
    pub response: ContextResponse,
}

pub struct ContextStore {
    connection: Connection,
    path: PathBuf,
    scope: String,
    scope_description: String,
    working_directory: String,
    max_turns: usize,
}

impl ContextStore {
    pub fn open(max_turns: usize) -> Result<Self> {
        let working_directory =
            env::current_dir().context("could not determine current directory")?;
        let scope_directory = find_scope_directory(&working_directory);
        let session = env::var("AISHELL_SESSION_ID")
            .ok()
            .filter(|value| valid_session_id(value));
        let (scope, scope_description) = match session {
            Some(session) => (
                format!("session:{session}:{}", scope_directory.display()),
                format!("shell session in {}", scope_directory.display()),
            ),
            None => (
                format!("directory:{}", scope_directory.display()),
                format!("directory {}", scope_directory.display()),
            ),
        };

        let path = Self::path()?;
        create_private_file(&path, "context database")?;
        verify_private_file(&path, "context database")?;

        let connection = Connection::open(&path)
            .with_context(|| format!("could not open context database at {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(2))?;
        initialize_database(&connection)?;

        Ok(Self {
            connection,
            path,
            scope,
            scope_description,
            working_directory: working_directory.to_string_lossy().into_owned(),
            max_turns,
        })
    }

    pub fn path() -> Result<PathBuf> {
        paths::context_database_file(DATABASE_FILE_NAME)
    }

    pub fn working_directory(&self) -> &str {
        &self.working_directory
    }

    pub fn scope_description(&self) -> &str {
        &self.scope_description
    }

    pub fn load(&self) -> Result<Vec<ContextTurn>> {
        let limit = i64::try_from(self.max_turns).context("context turn limit is too large")?;
        let mut statement = self.connection.prepare(
            "SELECT created_at, working_directory, request, response_kind, response
             FROM (
                 SELECT id, created_at, working_directory, request, response_kind, response
                 FROM turns
                 WHERE scope = ?1
                 ORDER BY id DESC
                 LIMIT ?2
             )
             ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![self.scope, limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        rows.map(|row| {
            let (created_at, working_directory, request, kind, response) = row?;
            Ok(ContextTurn {
                created_at,
                working_directory,
                request,
                response: ContextResponse::from_parts(&kind, response)?,
            })
        })
        .collect()
    }

    pub fn append(&mut self, request: &str, response: &ContextResponse) -> Result<()> {
        if request.len() > MAX_STORED_REQUEST_BYTES {
            bail!(
                "request is too large to save in context (maximum {MAX_STORED_REQUEST_BYTES} bytes)"
            );
        }

        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO contexts(scope, description, created_at, updated_at)
             VALUES (?1, ?2, unixepoch(), unixepoch())
             ON CONFLICT(scope) DO UPDATE SET
                 description = excluded.description,
                 updated_at = excluded.updated_at",
            params![self.scope, self.scope_description],
        )?;
        transaction.execute(
            "INSERT INTO turns(
                 scope, created_at, working_directory, request, response_kind, response
             ) VALUES (?1, unixepoch(), ?2, ?3, ?4, ?5)",
            params![
                self.scope,
                self.working_directory,
                request,
                response.kind(),
                response.content()
            ],
        )?;
        let keep = i64::try_from(self.max_turns).context("context turn limit is too large")?;
        transaction.execute(
            "DELETE FROM turns
             WHERE scope = ?1
               AND id NOT IN (
                   SELECT id FROM turns WHERE scope = ?1 ORDER BY id DESC LIMIT ?2
               )",
            params![self.scope, keep],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn clear(&mut self) -> Result<bool> {
        let transaction = self.connection.transaction()?;
        let removed =
            transaction.execute("DELETE FROM contexts WHERE scope = ?1", [&self.scope])?;
        transaction.commit()?;
        Ok(removed != 0)
    }

    pub fn path_ref(&self) -> &Path {
        &self.path
    }
}

fn initialize_database(connection: &Connection) -> Result<()> {
    let existing_application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let existing_schema_version: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let existing_tables: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
        [],
        |row| row.get(0),
    )?;
    let is_empty =
        existing_tables == 0 && existing_application_id == 0 && existing_schema_version == 0;
    let is_supported = existing_tables != 0
        && existing_application_id == DATABASE_APPLICATION_ID
        && existing_schema_version == DATABASE_SCHEMA_VERSION;
    if !is_empty && !is_supported {
        bail!("unsupported context database format");
    }

    connection.execute_batch(&format!(
        "PRAGMA foreign_keys = ON;
         PRAGMA secure_delete = ON;
         CREATE TABLE IF NOT EXISTS contexts (
             scope TEXT PRIMARY KEY,
             description TEXT NOT NULL,
             created_at INTEGER NOT NULL,
             updated_at INTEGER NOT NULL
         );
         {TURNS_TABLE_SCHEMA};
         {TURNS_INDEX_SCHEMA};
         PRAGMA application_id = {DATABASE_APPLICATION_ID};
         PRAGMA user_version = {DATABASE_SCHEMA_VERSION};"
    ))?;

    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if application_id != DATABASE_APPLICATION_ID || schema_version != DATABASE_SCHEMA_VERSION {
        bail!("unsupported context database format");
    }
    Ok(())
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn find_scope_directory(working_directory: &Path) -> PathBuf {
    let canonical = working_directory
        .canonicalize()
        .unwrap_or_else(|_| working_directory.to_path_buf());
    canonical
        .ancestors()
        .find(|directory| directory.join(".git").exists())
        .map_or(canonical.clone(), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        ContextResponse, ContextStore, DATABASE_APPLICATION_ID, DATABASE_SCHEMA_VERSION,
        initialize_database, valid_session_id,
    };
    use crate::secure_fs::create_private_file;

    #[test]
    fn session_ids_cannot_escape_the_database_scope() {
        assert!(valid_session_id("bash-123-456"));
        assert!(valid_session_id("powershell-123-acde"));
        assert!(!valid_session_id("../../another-context"));
        assert!(!valid_session_id("contains spaces"));
    }

    #[test]
    fn response_round_trips_its_database_representation() {
        for response in [
            ContextResponse::Command("truncate -s 50M disk.img".into()),
            ContextResponse::Clarification("Which disk?".into()),
            ContextResponse::Answer("I can help with shell commands.".into()),
        ] {
            assert_eq!(
                ContextResponse::from_parts(response.kind(), response.content().into()).unwrap(),
                response
            );
        }
    }

    #[test]
    fn initializes_a_versioned_database() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        initialize_database(&connection).unwrap();

        let application_id: i64 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .unwrap();
        let schema_version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(application_id, DATABASE_APPLICATION_ID);
        assert_eq!(schema_version, DATABASE_SCHEMA_VERSION);
        assert!(fs::metadata(path).unwrap().is_file());
    }

    #[test]
    fn rejects_an_obsolete_database_version() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        initialize_database(&connection).unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();

        assert!(initialize_database(&connection).is_err());
    }

    #[test]
    fn stores_only_the_bounded_context_and_clears_it() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state/aishell/history.sqlite3");
        create_private_file(&path, "context database").unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        initialize_database(&connection).unwrap();
        let mut store = ContextStore {
            connection,
            path: path.clone(),
            scope: "test-scope".into(),
            scope_description: "test context".into(),
            working_directory: "/tmp/images".into(),
            max_turns: 2,
        };

        store
            .append("first", &ContextResponse::Command("echo first".into()))
            .unwrap();
        store
            .append("second", &ContextResponse::Command("echo second".into()))
            .unwrap();
        store
            .append("third", &ContextResponse::Answer("Third answer.".into()))
            .unwrap();

        let turns = store.load().unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].request, "second");
        assert_eq!(turns[1].request, "third");
        assert!(store.clear().unwrap());
        assert!(store.load().unwrap().is_empty());

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        }
    }
}
