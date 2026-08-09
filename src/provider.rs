use std::io::Read;
use std::str::FromStr;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cli::Shell;
use crate::config::Config;
use crate::context::ContextTurn;
use crate::system_info::SystemInfo;

mod llama_cpp;
mod openai;
mod openrouter;
mod vllm;

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 8192;
const MAX_CLARIFICATION_BYTES: usize = 1024;
const MAX_ANSWER_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    OpenRouter,
    OpenAi,
    LlamaCpp,
    Vllm,
}

#[derive(Clone, Copy)]
pub(super) enum RequestStyle {
    Router,
    Official,
    Compatible,
}

#[derive(Clone, Copy)]
pub(super) enum CheckStyle {
    Key,
    Model,
    Catalog,
}

pub(super) struct ProviderSpec {
    id: &'static str,
    setup_choices: &'static [&'static str],
    display_name: &'static str,
    default_model: Option<&'static str>,
    default_base_url: &'static str,
    requires_api_key: bool,
    request_style: RequestStyle,
    check_style: CheckStyle,
}

impl Provider {
    const ALL: [Self; 4] = [Self::OpenRouter, Self::OpenAi, Self::LlamaCpp, Self::Vllm];

    const fn spec(self) -> &'static ProviderSpec {
        match self {
            Self::OpenRouter => &openrouter::SPEC,
            Self::OpenAi => &openai::SPEC,
            Self::LlamaCpp => &llama_cpp::SPEC,
            Self::Vllm => &vllm::SPEC,
        }
    }

    pub const fn as_str(self) -> &'static str {
        self.spec().id
    }

    pub const fn display_name(self) -> &'static str {
        self.spec().display_name
    }

    pub(crate) const fn default_model(self) -> Option<&'static str> {
        self.spec().default_model
    }

    pub(crate) const fn default_base_url(self) -> &'static str {
        self.spec().default_base_url
    }

    pub const fn requires_api_key(self) -> bool {
        self.spec().requires_api_key
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.display_name())
    }
}

impl FromStr for Provider {
    type Err = ProviderParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|provider| provider.spec().setup_choices.contains(&normalized.as_str()))
            .ok_or_else(|| ProviderParseError(value.to_owned()))
    }
}

#[derive(Debug, Error)]
#[error("unsupported provider {0:?}; expected OpenRouter, OpenAI, llama.cpp, or vLLM")]
pub struct ProviderParseError(String);

#[derive(Debug, Eq, PartialEq)]
pub enum GeneratedOutput {
    Command(String),
    Clarification(String),
    Answer(String),
}

pub struct AiClient {
    client: Client,
    provider: Provider,
    api_key: Option<String>,
    model: String,
    base_url: String,
    max_output_tokens: u32,
}

impl AiClient {
    pub fn new(config: &Config) -> Result<Self, ProviderError> {
        let provider = config.provider.kind;
        let client = Client::builder()
            .timeout(Duration::from_secs(config.generation.timeout_seconds))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("aishell/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|source| ProviderError::Transport { provider, source })?;

        Ok(Self {
            client,
            provider,
            api_key: config
                .provider
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_owned),
            model: config.provider.model.trim().to_owned(),
            base_url: config
                .provider
                .base_url
                .trim()
                .trim_end_matches('/')
                .to_owned(),
            max_output_tokens: config.generation.max_output_tokens,
        })
    }

    pub fn generate(
        &self,
        request: &str,
        shell: Shell,
        history: &[ContextTurn],
        working_directory: &str,
        system_info: &SystemInfo,
    ) -> Result<GeneratedOutput, ProviderError> {
        let body = ChatRequest::new(
            self.provider,
            &self.model,
            chat_messages(shell, history, working_directory, system_info, request),
            self.max_output_tokens,
        );
        let response = self
            .authenticated(
                self.client
                    .post(self.endpoint("chat/completions"))
                    .json(&body),
            )
            .send()
            .map_err(|source| ProviderError::Transport {
                provider: self.provider,
                source,
            })?;
        let body = read_response(response, self.provider)?;
        parse_chat_response(&body)
    }

    pub fn check(&self) -> Result<(), ProviderError> {
        let (endpoint, verify_model_list) = match self.provider.spec().check_style {
            CheckStyle::Key => (self.endpoint("key"), false),
            CheckStyle::Model => (self.model_endpoint(), false),
            CheckStyle::Catalog => (self.endpoint("models"), true),
        };
        let response = self
            .authenticated(self.client.get(endpoint))
            .send()
            .map_err(|source| ProviderError::Transport {
                provider: self.provider,
                source,
            })?;
        let body = read_response(response, self.provider)?;
        if verify_model_list {
            verify_model_available(&body, self.provider, &self.model)?;
        }
        Ok(())
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{path}", self.base_url)
    }

    fn model_endpoint(&self) -> String {
        let mut url = reqwest::Url::parse(&self.base_url).expect("validated provider base URL");
        url.path_segments_mut()
            .expect("provider base URL can contain path segments")
            .pop_if_empty()
            .push("models")
            .push(&self.model);
        url.into()
    }

    fn authenticated(&self, request: RequestBuilder) -> RequestBuilder {
        let request = if let Some(api_key) = &self.api_key {
            request.bearer_auth(api_key)
        } else {
            request
        };
        if matches!(self.provider.spec().request_style, RequestStyle::Router) {
            request.header("X-OpenRouter-Title", "aishell")
        } else {
            request
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_template_kwargs: Option<ChatTemplateConfig>,
}

impl<'a> ChatRequest<'a> {
    fn new(
        provider: Provider,
        model: &'a str,
        messages: Vec<Message>,
        max_output_tokens: u32,
    ) -> Self {
        let request_style = provider.spec().request_style;
        let is_openai = matches!(request_style, RequestStyle::Official);
        let openai_reasoning = is_openai && model.starts_with("gpt-5.6");
        let is_llama_cpp = provider == Provider::LlamaCpp;
        Self {
            model,
            messages,
            max_tokens: (!is_openai).then_some(max_output_tokens),
            max_completion_tokens: is_openai.then_some(max_output_tokens),
            stream: false,
            reasoning: matches!(request_style, RequestStyle::Router)
                .then_some(ReasoningConfig { enabled: false }),
            reasoning_effort: (openai_reasoning || is_llama_cpp).then_some("none"),
            reasoning_format: is_llama_cpp.then_some("deepseek"),
            chat_template_kwargs: is_llama_cpp.then_some(ChatTemplateConfig {
                enable_thinking: false,
            }),
        }
    }
}

#[derive(Serialize)]
struct ReasoningConfig {
    enabled: bool,
}

#[derive(Serialize)]
struct ChatTemplateConfig {
    enable_thinking: bool,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: ApiError,
}

#[derive(Deserialize)]
struct ApiError {
    message: String,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<Model>,
}

#[derive(Deserialize)]
struct Model {
    id: String,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("{provider} request failed: {source}")]
    Transport {
        provider: Provider,
        #[source]
        source: reqwest::Error,
    },
    #[error("could not read the {provider} response: {source}")]
    Io {
        provider: Provider,
        #[source]
        source: std::io::Error,
    },
    #[error("{provider} returned HTTP {status}: {message}")]
    Api {
        provider: Provider,
        status: StatusCode,
        message: String,
    },
    #[error("{provider} returned more than {MAX_RESPONSE_BYTES} bytes")]
    ResponseTooLarge { provider: Provider },
    #[error("{provider} returned a non-UTF-8 response")]
    InvalidUtf8 { provider: Provider },
    #[error("the configured model {model:?} is not served by {provider}")]
    ModelUnavailable { provider: Provider, model: String },
    #[error("the AI provider returned an invalid response: {0}")]
    InvalidResponse(String),
    #[error("the generated command is invalid: {0}")]
    InvalidCommand(String),
}

fn system_prompt(shell: Shell) -> String {
    format!(
        "You are an AI assistant embedded in an interactive shell. The user may request a shell \
operation or ask a general question, in any language. Output exactly one line using one of these \
forms: COMMAND: <one executable {} command line>, QUESTION: <one concise clarifying question>, \
or ANSWER: <one concise plain-text answer>. Use COMMAND whenever the user identifies a concrete \
shell operation, even if they do not know or name the appropriate utility. Use QUESTION only when \
the user appears to want a shell operation but essential details or the intended outcome are \
missing. Use ANSWER for general questions, explanations, capability questions, refusals, and any \
request that should not become an executable command. For 'what can you do?', explain briefly that \
you can answer questions and generate shell commands. Never use echo, printf, or another command \
to print a question, answer, refusal, or explanation. Do not use Markdown, a prompt marker, \
commentary, or explanation outside the prefixed line. Do not add sudo unless the user explicitly \
requests it. Quote concrete paths and values safely for {}. Do not invent placeholders when the \
user supplied concrete values. Previous request/response pairs may be included to resolve \
references and preserve concrete paths, names, and values. Their commands were only inserted into \
an editable shell buffer: they may have been changed or never executed, so never claim their \
effects occurred. Host system metadata in the current request is factual local context. For \
platform-dependent commands, use it to select commands and packages compatible with the reported \
operating system, distribution family, and version. Do not assume a different distribution or \
package manager unless the user requests one. {} A new command will also be shown for review and must \
not be described as already executed.",
        shell.as_str(),
        shell.as_str(),
        shell.generation_guidance()
    )
}

fn chat_messages(
    shell: Shell,
    history: &[ContextTurn],
    working_directory: &str,
    system_info: &SystemInfo,
    request: &str,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(2 + history.len() * 2);
    messages.push(Message {
        role: "system",
        content: system_prompt(shell),
    });
    for turn in history {
        messages.push(Message {
            role: "user",
            content: historical_user_prompt(turn),
        });
        messages.push(Message {
            role: "assistant",
            content: turn.response.model_line(),
        });
    }
    messages.push(Message {
        role: "user",
        content: user_prompt(working_directory, system_info, request),
    });
    messages
}

fn historical_user_prompt(turn: &ContextTurn) -> String {
    format!(
        "Working directory at the time: {}\nPrior request:\n{}",
        turn.working_directory, turn.request
    )
}

fn user_prompt(working_directory: &str, system_info: &SystemInfo, request: &str) -> String {
    format!(
        "Host system metadata: {}\nCurrent working directory: {}\nCurrent request:\n{}",
        system_info.model_context(),
        working_directory,
        request
    )
}

fn read_response(response: Response, provider: Provider) -> Result<String, ProviderError> {
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ProviderError::Io { provider, source })?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(ProviderError::ResponseTooLarge { provider });
    }
    let body = String::from_utf8(bytes).map_err(|_| ProviderError::InvalidUtf8 { provider })?;

    check_status(provider, status, &body)?;
    Ok(body)
}

fn check_status(provider: Provider, status: StatusCode, body: &str) -> Result<(), ProviderError> {
    if status.is_success() {
        return Ok(());
    }

    let message = serde_json::from_str::<ErrorResponse>(body)
        .map(|error| sanitize_error(&error.error.message))
        .unwrap_or_else(|_| "request failed without a readable error message".to_owned());
    Err(ProviderError::Api {
        provider,
        status,
        message,
    })
}

fn verify_model_available(
    body: &str,
    provider: Provider,
    model: &str,
) -> Result<(), ProviderError> {
    let response: ModelsResponse = serde_json::from_str(body)
        .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
    if response.data.iter().any(|available| available.id == model) {
        return Ok(());
    }
    Err(ProviderError::ModelUnavailable {
        provider,
        model: model.to_owned(),
    })
}

fn parse_chat_response(body: &str) -> Result<GeneratedOutput, ProviderError> {
    let response: ChatResponse = serde_json::from_str(body)
        .map_err(|error| ProviderError::InvalidResponse(error.to_string()))?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::InvalidResponse("no choice was returned".into()))?;
    let content = choice.message.content.ok_or_else(|| {
        let reason = match choice.finish_reason.as_deref() {
            Some("length") => "the output token limit was reached before text was returned",
            _ => "no text choice was returned",
        };
        ProviderError::InvalidResponse(reason.into())
    })?;
    parse_generated_output(&content)
}

fn parse_generated_output(content: &str) -> Result<GeneratedOutput, ProviderError> {
    let output = content.trim();
    if let Some(parsed) = parse_typed_output(output) {
        return parsed;
    }

    const REASONING_TERMINATORS: [&str; 2] = ["<|end|>", "</think>"];
    if let Some((offset, terminator)) = REASONING_TERMINATORS
        .iter()
        .filter_map(|terminator| output.rfind(terminator).map(|offset| (offset, *terminator)))
        .max_by_key(|(offset, _)| *offset)
    {
        let final_output = output[offset + terminator.len()..].trim();
        if let Some(parsed) = parse_typed_output(final_output) {
            return parsed;
        }
    }

    Err(ProviderError::InvalidResponse(
        "the response was not marked as COMMAND, QUESTION, or ANSWER".into(),
    ))
}

fn parse_typed_output(output: &str) -> Option<Result<GeneratedOutput, ProviderError>> {
    if let Some(command) = output.strip_prefix("COMMAND:") {
        return Some(validate_command(command).map(GeneratedOutput::Command));
    }
    if let Some(question) = output.strip_prefix("QUESTION:") {
        return Some(validate_clarification(question).map(GeneratedOutput::Clarification));
    }
    if let Some(answer) = output.strip_prefix("ANSWER:") {
        return Some(validate_answer(answer).map(GeneratedOutput::Answer));
    }
    None
}

fn validate_command(content: &str) -> Result<String, ProviderError> {
    let command =
        validate_single_line(content, MAX_COMMAND_BYTES).map_err(ProviderError::InvalidCommand)?;
    if command.starts_with("```") || command.ends_with("```") {
        return Err(ProviderError::InvalidCommand(
            "the response contained a Markdown code fence".into(),
        ));
    }

    Ok(command.to_owned())
}

fn validate_clarification(content: &str) -> Result<String, ProviderError> {
    validate_informational_output(content, MAX_CLARIFICATION_BYTES, "clarification")
}

fn validate_answer(content: &str) -> Result<String, ProviderError> {
    validate_informational_output(content, MAX_ANSWER_BYTES, "answer")
}

fn validate_informational_output(
    content: &str,
    max_bytes: usize,
    kind: &str,
) -> Result<String, ProviderError> {
    validate_single_line(content, max_bytes)
        .map(str::to_owned)
        .map_err(|error| ProviderError::InvalidResponse(format!("invalid {kind}: {error}")))
}

fn validate_single_line(content: &str, max_bytes: usize) -> Result<&str, String> {
    let line = content.trim();
    if line.is_empty() {
        return Err("the response was empty".into());
    }
    if line.len() > max_bytes {
        return Err(format!("the response exceeded {max_bytes} bytes"));
    }
    if line.chars().any(char::is_control) {
        return Err("the response contained multiple lines or control characters".into());
    }
    Ok(line)
}

fn sanitize_error(message: &str) -> String {
    let sanitized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    sanitized.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::json;

    use crate::cli::Shell;
    use crate::context::{ContextResponse, ContextTurn};
    use crate::system_info::SystemInfo;

    use super::{
        ChatRequest, GeneratedOutput, Message, Provider, ProviderError, chat_messages,
        check_status, parse_chat_response, sanitize_error, validate_command,
        verify_model_available,
    };

    fn request(provider: Provider, model: &str) -> serde_json::Value {
        serde_json::to_value(ChatRequest::new(
            provider,
            model,
            vec![Message {
                role: "user",
                content: "test".into(),
            }],
            256,
        ))
        .unwrap()
    }

    #[test]
    fn uses_provider_specific_chat_parameters() {
        let openrouter = request(Provider::OpenRouter, "openrouter/auto");
        assert_eq!(openrouter["max_tokens"], 256);
        assert_eq!(openrouter["reasoning"], json!({"enabled": false}));
        assert!(openrouter.get("max_completion_tokens").is_none());

        let openai = request(Provider::OpenAi, "gpt-5.6-luna");
        assert_eq!(openai["max_completion_tokens"], 256);
        assert_eq!(openai["reasoning_effort"], "none");
        assert!(openai.get("max_tokens").is_none());
        assert!(openai.get("reasoning").is_none());
        assert!(openai.get("reasoning_format").is_none());
        assert!(openai.get("chat_template_kwargs").is_none());

        let llama_cpp = request(Provider::LlamaCpp, "local-model");
        assert_eq!(llama_cpp["max_tokens"], 256);
        assert_eq!(llama_cpp["reasoning_effort"], "none");
        assert_eq!(llama_cpp["reasoning_format"], "deepseek");
        assert_eq!(
            llama_cpp["chat_template_kwargs"],
            json!({"enable_thinking": false})
        );
        assert!(llama_cpp.get("max_completion_tokens").is_none());
        assert!(llama_cpp.get("reasoning").is_none());

        let vllm = request(Provider::Vllm, "local-model");
        assert_eq!(vllm["max_tokens"], 256);
        assert!(vllm.get("max_completion_tokens").is_none());
        assert!(vllm.get("reasoning").is_none());
        assert!(vllm.get("reasoning_effort").is_none());
        assert!(vllm.get("reasoning_format").is_none());
        assert!(vllm.get("chat_template_kwargs").is_none());
    }

    #[test]
    fn omits_reasoning_effort_for_non_reasoning_openai_models() {
        let request = request(Provider::OpenAi, "gpt-4.1-mini");
        assert!(request.get("reasoning_effort").is_none());
    }

    #[test]
    fn sends_bounded_history_before_the_current_request() {
        let history = [ContextTurn {
            created_at: 1,
            working_directory: "/tmp/images".into(),
            request: "create a 50 meg file named disk.img".into(),
            response: ContextResponse::Command("truncate -s 50M disk.img".into()),
        }];
        let system_info = SystemInfo::detect();
        let messages = chat_messages(
            Shell::Bash,
            &history,
            "/tmp/images",
            &system_info,
            "create a filesystem on it",
        );

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, "user");
        assert!(messages[1].content.contains("create a 50 meg file"));
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[2].content, "COMMAND: truncate -s 50M disk.img");
        assert!(messages[3].content.contains("create a filesystem on it"));
        assert!(messages[3].content.contains(&system_info.model_context()));
        assert!(
            messages[0]
                .content
                .contains("may have been changed or never executed")
        );
        assert!(messages[0].content.contains("ANSWER:"));
        assert!(messages[0].content.contains("what can you do?"));
        assert!(messages[0].content.contains("distribution family"));
    }

    #[test]
    fn gives_windows_shells_distinct_syntax_contracts() {
        let system_info = SystemInfo::detect();
        let powershell = chat_messages(Shell::Pwsh, &[], "C:\\work", &system_info, "show files");
        assert!(
            powershell[0]
                .content
                .contains("executable powershell command line")
        );
        assert!(
            powershell[0]
                .content
                .contains("Windows PowerShell 5.1 and PowerShell 7")
        );

        let cmd = chat_messages(Shell::Cmd, &[], "C:\\work", &system_info, "show files");
        assert!(cmd[0].content.contains("executable cmd command line"));
        assert!(cmd[0].content.contains("Use cmd.exe built-ins"));
    }

    #[test]
    fn extracts_a_single_command() {
        let response = r#"{
            "choices": [{
                "message": {"content": "  COMMAND: qemu-system-x86_64 -drive file=xyz.vdi,format=vdi\n"},
                "finish_reason": "stop"
            }]
        }"#;
        assert_eq!(
            parse_chat_response(response).unwrap(),
            GeneratedOutput::Command("qemu-system-x86_64 -drive file=xyz.vdi,format=vdi".into())
        );
    }

    #[test]
    fn extracts_a_clarifying_question() {
        let response = r#"{
            "choices": [{
                "message": {"content": "QUESTION: What would you like to do with the archive?"},
                "finish_reason": "stop"
            }]
        }"#;
        assert_eq!(
            parse_chat_response(response).unwrap(),
            GeneratedOutput::Clarification("What would you like to do with the archive?".into())
        );
    }

    #[test]
    fn extracts_a_command_after_llama_cpp_reasoning() {
        let response = r#"{
            "choices": [{
                "message": {
                    "content": "<|channel|>analysis<|message|>The user wants a file listing.\n<|end|>COMMAND: ls -la\n\n"
                },
                "finish_reason": "stop"
            }]
        }"#;
        assert_eq!(
            parse_chat_response(response).unwrap(),
            GeneratedOutput::Command("ls -la".into())
        );
    }

    #[test]
    fn extracts_a_conversational_answer() {
        let response = r#"{
            "choices": [{
                "message": {"content": "ANSWER: I can answer questions and generate shell commands for you to review."},
                "finish_reason": "stop"
            }]
        }"#;
        assert_eq!(
            parse_chat_response(response).unwrap(),
            GeneratedOutput::Answer(
                "I can answer questions and generate shell commands for you to review.".into()
            )
        );
    }

    #[test]
    fn explains_an_output_limit_without_text() {
        let response = r#"{
            "choices": [{"message": {"content": null}, "finish_reason": "length"}]
        }"#;
        let error = parse_chat_response(response).unwrap_err();
        assert!(matches!(
            error,
            ProviderError::InvalidResponse(ref message)
                if message.contains("output token limit")
        ));
    }

    #[test]
    fn rejects_an_untyped_model_response() {
        let response = r#"{
            "choices": [{"message": {"content": "echo ambiguous"}, "finish_reason": "stop"}]
        }"#;
        assert!(parse_chat_response(response).is_err());
    }

    #[test]
    fn rejects_multiline_output() {
        let error = validate_command("echo first\necho second").unwrap_err();
        assert!(matches!(error, ProviderError::InvalidCommand(_)));
    }

    #[test]
    fn rejects_markdown_fences() {
        assert!(validate_command("```sh").is_err());
    }

    #[test]
    fn checks_local_model_catalogs() {
        let body = r#"{"object":"list","data":[{"id":"Qwen/Qwen3-8B"}]}"#;
        verify_model_available(body, Provider::Vllm, "Qwen/Qwen3-8B").unwrap();
        assert!(verify_model_available(body, Provider::LlamaCpp, "other").is_err());
    }

    #[test]
    fn sanitizes_remote_errors() {
        assert_eq!(
            sanitize_error("invalid\n  request\tvalue"),
            "invalid request value"
        );
    }

    #[test]
    fn preserves_the_http_status_and_safe_api_message() {
        let error = check_status(
            Provider::OpenAi,
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":401,"message":"invalid\ncredentials"}}"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ProviderError::Api {
                provider: Provider::OpenAi,
                status: StatusCode::UNAUTHORIZED,
                ref message,
            } if message == "invalid credentials"
        ));
    }
}
