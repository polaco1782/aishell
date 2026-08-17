use std::io::{IsTerminal, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::provider::Provider;
use crate::secure_fs::{atomic_write_private, verify_private_file};
use crate::ui;

const DEFAULT_TIMEOUT_SECONDS: u64 = 20;
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 256;
const DEFAULT_CONTEXT_MAX_TURNS: usize = 6;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub provider: ProviderConfig,
    #[serde(default)]
    pub generation: GenerationConfig,
    #[serde(default)]
    pub context: ContextConfig,
    pub safety: SafetyConfig,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    #[serde(rename = "type")]
    pub kind: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub model: String,
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafetyConfig {
    pub risk_warning: bool,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self { risk_warning: true }
    }
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
        paths::config_file()
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let contents = Self::read_from(path)?;
        Self::parse_from(&contents, path)
    }

    fn read_from(path: &Path) -> Result<String> {
        verify_private_file(path, "configuration")?;
        std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read {}; run `ai setup` to create it",
                path.display()
            )
        })
    }

    fn parse_from(contents: &str, path: &Path) -> Result<Self> {
        let config: Self = toml::from_str(contents)
            .with_context(|| format!("invalid configuration in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let key = self.provider.api_key.as_deref().map(str::trim);
        if key.is_some_and(str::is_empty) {
            bail!("provider.api_key cannot be empty");
        }
        if self.provider.kind.requires_api_key() && key.is_none() {
            bail!("provider.api_key is required for {}", self.provider.kind);
        }
        if key.is_some_and(|key| key.chars().any(char::is_control)) {
            bail!("provider.api_key cannot contain control characters");
        }

        let model = self.provider.model.trim();
        if model.is_empty() || model.chars().any(char::is_whitespace) {
            bail!(
                "provider.model must be a nonempty {} model identifier",
                self.provider.kind
            );
        }

        let url = reqwest::Url::parse(self.provider.base_url.trim())
            .context("provider.base_url is not a valid URL")?;
        if url.scheme() != "https"
            && !(url.scheme() == "http" && url.host_str().is_some_and(is_loopback_host))
        {
            bail!("provider.base_url must use HTTPS, except for loopback HTTP servers");
        }
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            bail!(
                "provider.base_url must be an HTTP(S) origin and path without credentials or query parameters"
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
        let api_key = self
            .provider
            .api_key
            .as_ref()
            .map(|_| "api_key = \"<redacted>\"\n")
            .unwrap_or_default();
        format!(
            "[provider]\ntype = {:?}\n{api_key}model = {:?}\nbase_url = {:?}\n\n[generation]\ntimeout_seconds = {}\nmax_output_tokens = {}\n\n[context]\nenabled = {}\nmax_turns = {}\n\n[safety]\nrisk_warning = {}\n",
            self.provider.kind.as_str(),
            self.provider.model,
            self.provider.base_url,
            self.generation.timeout_seconds,
            self.generation.max_output_tokens,
            self.context.enabled,
            self.context.max_turns,
            self.safety.risk_warning
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

    #[cfg(windows)]
    crate::windows_console::restore_line_input()
        .context("could not restore the Windows console input mode")?;

    let path = Config::path()?;
    let existing = if path.exists() {
        let contents = Config::read_from(&path)?;
        match Config::parse_from(&contents, &path) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!(
                    "{} Existing configuration is invalid and will be replaced if setup completes · {error:#}",
                    ui::WARNING
                );
                None
            }
        }
    } else {
        None
    };

    let default_provider = existing
        .as_ref()
        .map_or(Provider::OpenRouter, |config| config.provider.kind);
    println!("{} AI provider", ui::AI);
    println!("  1) OpenRouter");
    println!("  2) OpenAI");
    println!("  3) llama.cpp");
    println!("  4) vLLM");
    let selected_provider = prompt_value(
        &format!("Select provider [{}] › ", default_provider.as_str()),
        Some(default_provider.as_str()),
    )?;
    let provider = Provider::from_str(&selected_provider)?;
    let existing_provider = existing
        .as_ref()
        .filter(|config| config.provider.kind == provider);

    let key_prompt = match (existing_provider, provider.requires_api_key()) {
        (Some(_), true) => format!(
            "🔑 {} API key (Enter keeps the current key) › ",
            provider.display_name()
        ),
        (None, true) => format!("🔑 {} API key › ", provider.display_name()),
        (Some(_), false) => format!(
            "🔑 {} API key (optional; Enter keeps it, - clears it) › ",
            provider.display_name()
        ),
        (None, false) => format!("🔑 {} API key (optional) › ", provider.display_name()),
    };
    let password_config = rpassword::ConfigBuilder::new()
        .password_feedback_mask('*')
        // rpassword's default CONOUT$ prompt writer emits raw UTF-8 bytes,
        // which legacy Windows consoles decode with their OEM code page.
        // Rust's stdout writes Unicode correctly to a console and UTF-8 when
        // redirected, matching the other setup prompts.
        .output_writer(std::io::stdout())
        .build();
    #[cfg(windows)]
    let _console_mode_guard = crate::windows_console::ConsoleModeGuard::install()
        .context("could not protect the Windows console input mode")?;
    let entered_key = rpassword::prompt_password_with_config(key_prompt, password_config)
        .context("could not read the API key")?;
    let api_key = match (entered_key.trim(), existing_provider) {
        ("-", _) if !provider.requires_api_key() => None,
        ("", Some(config)) => config.provider.api_key.clone(),
        ("", None) if provider.requires_api_key() => {
            bail!("a {} API key is required", provider.display_name())
        }
        ("", None) => None,
        (value, _) => Some(value.to_owned()),
    };

    let default_model = existing_provider
        .map(|config| config.provider.model.as_str())
        .or_else(|| provider.default_model());
    let model_prompt = default_model.map_or_else(
        || format!("{} {} served model › ", ui::AI, provider.display_name()),
        |model| format!("{} {} model [{model}] › ", ui::AI, provider.display_name()),
    );
    let model = prompt_value(&model_prompt, default_model)?;
    if model.is_empty() {
        bail!("a {} model identifier is required", provider.display_name());
    }

    let default_base_url = existing_provider.map_or(provider.default_base_url(), |config| {
        config.provider.base_url.as_str()
    });
    let base_url = prompt_value(
        &format!(
            "{} API base URL [{default_base_url}] › ",
            provider.display_name()
        ),
        Some(default_base_url),
    )?;

    let generation = existing
        .as_ref()
        .map(|config| config.generation.clone())
        .unwrap_or_default();
    let context = existing
        .as_ref()
        .map(|config| config.context.clone())
        .unwrap_or_default();
    let safety = existing
        .as_ref()
        .map(|config| config.safety.clone())
        .unwrap_or_default();
    let config = Config {
        provider: ProviderConfig {
            kind: provider,
            api_key,
            model,
            base_url,
        },
        generation,
        context,
        safety,
    };
    config.save_to(&path)?;
    Ok(path)
}

fn prompt_value(prompt: &str, default: Option<&str>) -> Result<String> {
    print!("{prompt}");
    std::io::stdout()
        .flush()
        .context("could not display setup prompt")?;
    let mut entered = String::new();
    std::io::stdin()
        .read_line(&mut entered)
        .context("could not read setup input")?;
    Ok(match entered.trim() {
        "" => default.unwrap_or_default().to_owned(),
        value => value.to_owned(),
    })
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
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
    #[cfg(unix)]
    use std::fs;

    use tempfile::tempdir;

    use std::str::FromStr;

    use super::{Config, ContextConfig, GenerationConfig, Provider, ProviderConfig, SafetyConfig};

    fn config() -> Config {
        Config {
            provider: ProviderConfig {
                kind: Provider::OpenRouter,
                api_key: Some("secret-value".into()),
                model: "openrouter/auto".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
            },
            generation: GenerationConfig::default(),
            context: ContextConfig::default(),
            safety: SafetyConfig::default(),
        }
    }

    #[test]
    fn saves_and_loads_a_private_config() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("aishell/config.toml");
        config().save_to(&path).unwrap();

        let mut updated = config();
        updated.provider.model = "openrouter/updated".into();
        updated.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.provider.kind, Provider::OpenRouter);
        assert_eq!(loaded.provider.api_key.as_deref(), Some("secret-value"));
        assert_eq!(loaded.provider.model, "openrouter/updated");

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
    fn safety_warning_can_be_disabled() {
        let mut config = config();
        config.safety.risk_warning = false;

        let serialized = toml::to_string(&config).unwrap();
        let parsed: Config = toml::from_str(&serialized).unwrap();
        assert!(!parsed.safety.risk_warning);
        assert!(
            config
                .redacted_toml()
                .contains("[safety]\nrisk_warning = false")
        );
    }

    #[test]
    fn provider_configuration_gets_default_generation_and_context_settings() {
        let parsed: Config = toml::from_str(
            r#"[provider]
type = "openai"
api_key = "secret-value"
model = "gpt-5.6-luna"
base_url = "https://api.openai.com/v1"

[safety]
risk_warning = true
"#,
        )
        .unwrap();

        assert!(parsed.context.enabled);
        assert_eq!(parsed.context.max_turns, 6);
    }

    #[test]
    fn requires_the_current_safety_configuration() {
        let parsed = toml::from_str::<Config>(
            r#"[provider]
type = "openai"
api_key = "secret-value"
model = "gpt-5.6-luna"
base_url = "https://api.openai.com/v1"
"#,
        );

        assert!(parsed.is_err());
    }

    #[test]
    fn rejects_the_obsolete_openrouter_only_schema() {
        let parsed = toml::from_str::<Config>(
            r#"[openrouter]
api_key = "secret-value"
model = "openrouter/auto"
base_url = "https://openrouter.ai/api/v1"
"#,
        );

        assert!(parsed.is_err());
    }

    #[test]
    fn parses_interactive_provider_choices() {
        assert_eq!(Provider::from_str("1").unwrap(), Provider::OpenRouter);
        assert_eq!(
            Provider::from_str("openrouter").unwrap(),
            Provider::OpenRouter
        );
        assert_eq!(Provider::from_str("2").unwrap(), Provider::OpenAi);
        assert_eq!(Provider::from_str("OpenAI").unwrap(), Provider::OpenAi);
        assert_eq!(Provider::from_str("3").unwrap(), Provider::LlamaCpp);
        assert_eq!(Provider::from_str("llama.cpp").unwrap(), Provider::LlamaCpp);
        assert_eq!(Provider::from_str("4").unwrap(), Provider::Vllm);
        assert!(Provider::from_str("other").is_err());
    }

    #[test]
    fn redacted_openai_configuration_uses_the_selected_provider() {
        let mut config = config();
        config.provider = ProviderConfig {
            kind: Provider::OpenAi,
            api_key: Some("openai-secret".into()),
            model: "gpt-5.6-luna".into(),
            base_url: "https://api.openai.com/v1".into(),
        };

        let shown = config.redacted_toml();
        assert!(shown.contains("type = \"openai\""));
        assert!(shown.contains("model = \"gpt-5.6-luna\""));
        assert!(!shown.contains("openai-secret"));
    }

    #[test]
    fn accepts_keyless_loopback_servers() {
        for (kind, base_url) in [
            (Provider::LlamaCpp, "http://127.0.0.1:8080/v1"),
            (Provider::Vllm, "http://[::1]:8000/v1"),
        ] {
            let mut config = config();
            config.provider = ProviderConfig {
                kind,
                api_key: None,
                model: "local-model".into(),
                base_url: base_url.into(),
            };
            config.validate().unwrap();
        }
    }

    #[test]
    fn rejects_unencrypted_remote_servers() {
        let mut config = config();
        config.provider = ProviderConfig {
            kind: Provider::Vllm,
            api_key: None,
            model: "local-model".into(),
            base_url: "http://inference.example/v1".into(),
        };

        assert!(config.validate().is_err());
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
