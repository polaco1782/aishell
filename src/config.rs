use std::env;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::secure_fs::{atomic_write_private, verify_private_file};
use crate::ui;

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_MODEL: &str = "openrouter/auto";
const DEFAULT_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 256;
const DEFAULT_CONTEXT_MAX_TURNS: usize = 6;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub openrouter: OpenRouterConfig,
    #[serde(default)]
    pub generation: GenerationConfig,
    #[serde(default)]
    pub context: ContextConfig,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterConfig {
    pub api_key: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationConfig {
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    #[serde(default = "default_context_enabled")]
    pub enabled: bool,
    #[serde(default = "default_context_max_turns")]
    pub max_turns: usize,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        }
    }
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_turns: DEFAULT_CONTEXT_MAX_TURNS,
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let base = match env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
            Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path),
            _ => {
                let home = env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .context("HOME is not set, so the configuration path cannot be determined")?;
                PathBuf::from(home).join(".config")
            }
        };

        Ok(base.join("aishell").join("config.toml"))
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        verify_private_file(path, "configuration")?;
        let contents = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read {}; run `ai setup` to create it",
                path.display()
            )
        })?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("invalid configuration in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let key = self.openrouter.api_key.trim();
        if key.is_empty() {
            bail!("openrouter.api_key cannot be empty");
        }
        if key.chars().any(char::is_control) {
            bail!("openrouter.api_key cannot contain control characters");
        }

        let model = self.openrouter.model.trim();
        if model.is_empty() || model.chars().any(char::is_whitespace) {
            bail!("openrouter.model must be a nonempty OpenRouter model slug");
        }

        let url = reqwest::Url::parse(self.openrouter.base_url.trim())
            .context("openrouter.base_url is not a valid URL")?;
        if url.scheme() != "https" {
            bail!("openrouter.base_url must use HTTPS");
        }
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!(
                "openrouter.base_url must be an HTTPS origin and path without credentials or query parameters"
            );
        }

        if !(1..=120).contains(&self.generation.timeout_seconds) {
            bail!("generation.timeout_seconds must be between 1 and 120");
        }
        if !(16..=2048).contains(&self.generation.max_output_tokens) {
            bail!("generation.max_output_tokens must be between 16 and 2048");
        }
        if !(1..=20).contains(&self.context.max_turns) {
            bail!("context.max_turns must be between 1 and 20");
        }

        Ok(())
    }

    pub fn redacted_toml(&self) -> String {
        format!(
            "[openrouter]\napi_key = \"<redacted>\"\nmodel = {:?}\nbase_url = {:?}\n\n[generation]\ntimeout_seconds = {}\nmax_output_tokens = {}\n\n[context]\nenabled = {}\nmax_turns = {}\n",
            self.openrouter.model,
            self.openrouter.base_url,
            self.generation.timeout_seconds,
            self.generation.max_output_tokens,
            self.context.enabled,
            self.context.max_turns
        )
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let contents = toml::to_string_pretty(self).context("could not serialize configuration")?;
        atomic_write_private(path, contents.as_bytes(), "configuration")
    }
}

pub fn interactive_setup() -> Result<PathBuf> {
    if !std::io::stdin().is_terminal() {
        bail!("`ai setup` must be run from an interactive terminal");
    }

    let path = Config::path()?;
    let existing = if path.exists() {
        Some(Config::load_from(&path)?)
    } else {
        None
    };

    let key_prompt = if existing.is_some() {
        "🔑 OpenRouter API key (Enter keeps the current key) › "
    } else {
        "🔑 OpenRouter API key › "
    };
    let entered_key =
        rpassword::prompt_password(key_prompt).context("could not read the API key")?;
    let api_key = match (entered_key.trim(), existing.as_ref()) {
        ("", Some(config)) => config.openrouter.api_key.clone(),
        ("", None) => bail!("an OpenRouter API key is required"),
        (value, _) => value.to_owned(),
    };

    let default_model = existing
        .as_ref()
        .map_or(DEFAULT_MODEL, |config| config.openrouter.model.as_str());
    print!("{} OpenRouter model [{default_model}] › ", ui::AI);
    std::io::stdout()
        .flush()
        .context("could not display setup prompt")?;
    let mut entered_model = String::new();
    std::io::stdin()
        .read_line(&mut entered_model)
        .context("could not read the model")?;
    let model = match entered_model.trim() {
        "" => default_model.to_owned(),
        value => value.to_owned(),
    };

    let generation = existing
        .as_ref()
        .map(|config| config.generation.clone())
        .unwrap_or_default();
    let context = existing
        .as_ref()
        .map(|config| config.context.clone())
        .unwrap_or_default();
    let config = Config {
        openrouter: OpenRouterConfig {
            api_key,
            model,
            base_url: existing.as_ref().map_or_else(
                || DEFAULT_BASE_URL.to_owned(),
                |config| config.openrouter.base_url.clone(),
            ),
        },
        generation,
        context,
    };
    config.save_to(&path)?;
    Ok(path)
}

fn default_model() -> String {
    DEFAULT_MODEL.to_owned()
}

fn default_base_url() -> String {
    DEFAULT_BASE_URL.to_owned()
}

const fn default_timeout_seconds() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

const fn default_max_output_tokens() -> u32 {
    DEFAULT_MAX_OUTPUT_TOKENS
}

const fn default_context_enabled() -> bool {
    true
}

const fn default_context_max_turns() -> usize {
    DEFAULT_CONTEXT_MAX_TURNS
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{Config, ContextConfig, GenerationConfig, OpenRouterConfig};

    fn config() -> Config {
        Config {
            openrouter: OpenRouterConfig {
                api_key: "secret-value".into(),
                model: "openrouter/auto".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
            },
            generation: GenerationConfig::default(),
            context: ContextConfig::default(),
        }
    }

    #[test]
    fn saves_and_loads_a_private_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("aishell/config.toml");
        config().save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.openrouter.api_key, "secret-value");
        assert_eq!(loaded.openrouter.model, "openrouter/auto");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
            assert_eq!(
                fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn redaction_never_displays_the_api_key() {
        let shown = config().redacted_toml();
        assert!(!shown.contains("secret-value"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn older_configuration_gets_default_context_settings() {
        let parsed: Config = toml::from_str(
            r#"[openrouter]
api_key = "secret-value"
model = "openrouter/auto"
base_url = "https://openrouter.ai/api/v1"
"#,
        )
        .unwrap();

        assert!(parsed.context.enabled);
        assert_eq!(parsed.context.max_turns, 6);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_group_readable_config() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().unwrap();
        let path = directory.path().join("aishell/config.toml");
        config().save_to(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        let error = Config::load_from(&path).err().unwrap().to_string();
        assert!(error.contains("insecure permissions"));
    }
}
