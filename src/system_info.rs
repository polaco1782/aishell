#[cfg(any(target_os = "linux", test))]
use std::fs::File;
#[cfg(any(target_os = "linux", test))]
use std::io::Read;
#[cfg(any(target_os = "linux", test))]
use std::path::Path;

use serde::Serialize;

#[cfg(any(target_os = "linux", test))]
const MAX_OS_RELEASE_BYTES: u64 = 32 * 1024;
#[cfg(any(target_os = "linux", test))]
const MAX_IDENTIFIER_BYTES: usize = 64;
#[cfg(target_os = "linux")]
const OS_RELEASE_PATHS: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

#[derive(Debug, Serialize)]
pub(crate) struct SystemInfo {
    os: &'static str,
    architecture: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    distribution: Option<DistributionInfo>,
}

#[derive(Debug, Default, Eq, PartialEq, Serialize)]
struct DistributionInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_codename: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    id_like: Vec<String>,
}

impl SystemInfo {
    pub(crate) fn detect() -> Self {
        Self {
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            distribution: detect_distribution(),
        }
    }

    pub(crate) fn model_context(&self) -> String {
        serde_json::to_string(self).expect("system information is always valid JSON")
    }
}

#[cfg(target_os = "linux")]
fn detect_distribution() -> Option<DistributionInfo> {
    OS_RELEASE_PATHS
        .into_iter()
        .map(Path::new)
        .find_map(|path| read_bounded(path).and_then(|contents| parse_os_release(&contents)))
}

#[cfg(not(target_os = "linux"))]
fn detect_distribution() -> Option<DistributionInfo> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn read_bounded(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }

    let mut bytes = Vec::new();
    file.take(MAX_OS_RELEASE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_OS_RELEASE_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(any(target_os = "linux", test))]
fn parse_os_release(contents: &str) -> Option<DistributionInfo> {
    let mut distribution = DistributionInfo::default();

    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let Some(value) = parse_value(raw_value.trim()) else {
            continue;
        };

        match key {
            "ID" => distribution.id = valid_identifier(&value),
            "VERSION_ID" => distribution.version_id = valid_identifier(&value),
            "VERSION_CODENAME" => distribution.version_codename = valid_identifier(&value),
            "ID_LIKE" => {
                distribution.id_like = value
                    .split_ascii_whitespace()
                    .filter_map(valid_identifier)
                    .collect();
            }
            _ => {}
        }
    }

    (distribution != DistributionInfo::default()).then_some(distribution)
}

#[cfg(any(target_os = "linux", test))]
fn parse_value(raw_value: &str) -> Option<String> {
    let value = match raw_value.as_bytes() {
        [b'"', .., b'"'] | [b'\'', .., b'\''] if raw_value.len() >= 2 => {
            &raw_value[1..raw_value.len() - 1]
        }
        [b'"' | b'\'', ..] => return None,
        _ => raw_value,
    };

    let mut decoded = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            decoded.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character.is_control() {
            return None;
        } else {
            decoded.push(character);
        }
    }
    if escaped {
        return None;
    }
    Some(decoded)
}

#[cfg(any(target_os = "linux", test))]
fn valid_identifier(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    .then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        DistributionInfo, MAX_OS_RELEASE_BYTES, SystemInfo, parse_os_release, parse_value,
        read_bounded,
    };

    #[test]
    fn parses_only_package_relevant_os_release_fields() {
        let distribution = parse_os_release(
            r#"
                PRETTY_NAME="Ubuntu 24.04.3 LTS"
                NAME="Ubuntu"
                ID=ubuntu
                ID_LIKE="debian"
                VERSION_ID="24.04"
                VERSION_CODENAME=noble
                HOME_URL="https://ubuntu.com/"
            "#,
        )
        .unwrap();

        assert_eq!(
            distribution,
            DistributionInfo {
                id: Some("ubuntu".into()),
                version_id: Some("24.04".into()),
                version_codename: Some("noble".into()),
                id_like: vec!["debian".into()],
            }
        );
    }

    #[test]
    fn rejects_unsafe_identifiers_without_losing_valid_fields() {
        let distribution = parse_os_release(
            "ID=not a distro\nVERSION_ID=40\nVERSION_CODENAME='blue fin'\nID_LIKE=\"fedora rhel;dnf\"",
        )
        .unwrap();

        assert_eq!(distribution.id, None);
        assert_eq!(distribution.version_id.as_deref(), Some("40"));
        assert_eq!(distribution.version_codename, None);
        assert_eq!(distribution.id_like, vec!["fedora"]);
    }

    #[test]
    fn decodes_quoted_os_release_values_without_shell_evaluation() {
        assert_eq!(parse_value(r#""one\ two""#).as_deref(), Some("one two"));
        assert_eq!(parse_value(r#"'one two'"#).as_deref(), Some("one two"));
        assert_eq!(parse_value(r#""unterminated"#), None);
        assert_eq!(parse_value("trailing\\"), None);
    }

    #[test]
    fn ignores_oversized_os_release_files() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("os-release");
        fs::write(&path, vec![b'a'; MAX_OS_RELEASE_BYTES as usize + 1]).unwrap();

        assert_eq!(read_bounded(&path), None);
    }

    #[test]
    fn serializes_system_information_as_structured_context() {
        let info = SystemInfo {
            os: "linux",
            architecture: "x86_64",
            distribution: Some(DistributionInfo {
                id: Some("ubuntu".into()),
                version_id: Some("24.04".into()),
                version_codename: Some("noble".into()),
                id_like: vec!["debian".into()],
            }),
        };

        assert_eq!(
            info.model_context(),
            r#"{"os":"linux","architecture":"x86_64","distribution":{"id":"ubuntu","version_id":"24.04","version_codename":"noble","id_like":["debian"]}}"#
        );
    }
}
