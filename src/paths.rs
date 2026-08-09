use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn config_file() -> Result<PathBuf> {
    Ok(platform_config_base()?.join("aishell").join("config.toml"))
}

pub fn context_database_file(file_name: &str) -> Result<PathBuf> {
    Ok(platform_state_base()?.join("aishell").join(file_name))
}

#[cfg(windows)]
fn platform_config_base() -> Result<PathBuf> {
    windows_local_data_base()
}

#[cfg(windows)]
fn platform_state_base() -> Result<PathBuf> {
    windows_local_data_base()
}

#[cfg(windows)]
fn windows_local_data_base() -> Result<PathBuf> {
    absolute_environment_path("LOCALAPPDATA").context(
        "LOCALAPPDATA is not set to an absolute path, so the private data path cannot be determined",
    )
}

#[cfg(not(windows))]
fn platform_config_base() -> Result<PathBuf> {
    unix_base("XDG_CONFIG_HOME", &[".config"], "configuration")
}

#[cfg(not(windows))]
fn platform_state_base() -> Result<PathBuf> {
    unix_base("XDG_STATE_HOME", &[".local", "state"], "state")
}

#[cfg(not(windows))]
fn unix_base(variable: &str, fallback: &[&str], description: &str) -> Result<PathBuf> {
    if let Some(path) = absolute_environment_path(variable) {
        return Ok(path);
    }

    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .context(format!(
            "HOME is not set, so the {description} path cannot be determined"
        ))?;
    Ok(fallback
        .iter()
        .fold(PathBuf::from(home), |path, component| path.join(component)))
}

fn absolute_environment_path(variable: &str) -> Option<PathBuf> {
    let value = env::var_os(variable).filter(|value| !value.is_empty())?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{config_file, context_database_file};

    #[test]
    fn private_paths_have_distinct_stable_names() {
        assert!(
            config_file()
                .unwrap()
                .ends_with(Path::new("aishell/config.toml"))
        );
        assert!(
            context_database_file("history.sqlite3")
                .unwrap()
                .ends_with(Path::new("aishell/history.sqlite3"))
        );
    }
}
