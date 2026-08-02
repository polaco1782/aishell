use std::io::Read;
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cli::Shell;
use crate::config::Config;
use crate::context::ContextTurn;

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 8192;
const MAX_CLARIFICATION_BYTES: usize = 1024;
const MAX_ANSWER_BYTES: usize = 4096;

#[derive(Debug, Eq, PartialEq)]
pub enum GeneratedOutput {
    Command(String),
    Clarification(String),
    Answer(String),
}

pub struct OpenRouterClient {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
    max_output_tokens: u32,
}

impl OpenRouterClient {
    pub fn new(config: &Config) -> Result<Self, OpenRouterError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.generation.timeout_seconds))
            // Refusing redirects prevents credentials from being forwarded to an unexpected host.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("aishell/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self {
            client,
            api_key: config.openrouter.api_key.trim().to_owned(),
            model: config.openrouter.model.trim().to_owned(),
            base_url: config
                .openrouter
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
    ) -> Result<GeneratedOutput, OpenRouterError> {
        let body = ChatRequest {
            model: &self.model,
            messages: chat_messages(shell, history, working_directory, request),
            max_tokens: self.max_output_tokens,
            stream: false,
            // Shell translation is a small, latency-sensitive task. Reasoning models can
            // otherwise consume the entire output budget before producing visible text.
            reasoning: ReasoningConfig { enabled: false },
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-OpenRouter-Title", "aishell")
            .json(&body)
            .send()?;
        let body = read_response(response)?;
        parse_chat_response(&body)
    }

    pub fn check_key(&self) -> Result<(), OpenRouterError> {
        let response = self
            .client
            .get(format!("{}/key", self.base_url))
            .bearer_auth(&self.api_key)
            .header("X-OpenRouter-Title", "aishell")
            .send()?;
        read_response(response)?;
        Ok(())
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message>,
    max_tokens: u32,
    stream: bool,
    reasoning: ReasoningConfig,
}

#[derive(Serialize)]
struct ReasoningConfig {
    enabled: bool,
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

#[derive(Debug, Error)]
pub enum OpenRouterError {
    #[error("OpenRouter request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("could not read the OpenRouter response: {0}")]
    Io(#[from] std::io::Error),
    #[error("OpenRouter returned HTTP {status}: {message}")]
    Api { status: StatusCode, message: String },
    #[error("OpenRouter returned more than {MAX_RESPONSE_BYTES} bytes")]
    ResponseTooLarge,
    #[error("OpenRouter returned a non-UTF-8 response")]
    InvalidUtf8,
    #[error("OpenRouter returned an invalid response: {0}")]
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
effects occurred. A new command will also be shown for review and must not be described as already \
executed.",
        shell.as_str(),
        shell.as_str()
    )
}

fn chat_messages(
    shell: Shell,
    history: &[ContextTurn],
    working_directory: &str,
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
        content: user_prompt(working_directory, request),
    });
    messages
}

fn historical_user_prompt(turn: &ContextTurn) -> String {
    format!(
        "Working directory at the time: {}\nPrior request:\n{}",
        turn.working_directory, turn.request
    )
}

fn user_prompt(working_directory: &str, request: &str) -> String {
    format!(
        "Operating system: {}\nArchitecture: {}\nCurrent working directory: \
{}\nCurrent request:\n{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        working_directory,
        request
    )
}

fn read_response(response: Response) -> Result<String, OpenRouterError> {
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(OpenRouterError::ResponseTooLarge);
    }
    let body = String::from_utf8(bytes).map_err(|_| OpenRouterError::InvalidUtf8)?;

    check_status(status, &body)?;

    Ok(body)
}

fn check_status(status: StatusCode, body: &str) -> Result<(), OpenRouterError> {
    if status.is_success() {
        return Ok(());
    }

    let message = serde_json::from_str::<ErrorResponse>(body)
        .map(|error| sanitize_error(&error.error.message))
        .unwrap_or_else(|_| "request failed without a readable error message".to_owned());
    Err(OpenRouterError::Api { status, message })
}

fn parse_chat_response(body: &str) -> Result<GeneratedOutput, OpenRouterError> {
    let response: ChatResponse = serde_json::from_str(body)
        .map_err(|error| OpenRouterError::InvalidResponse(error.to_string()))?;
    let choice = response
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| OpenRouterError::InvalidResponse("no choice was returned".into()))?;
    let content = choice.message.content.ok_or_else(|| {
        let reason = match choice.finish_reason.as_deref() {
            Some("length") => "the output token limit was reached before text was returned",
            _ => "no text choice was returned",
        };
        OpenRouterError::InvalidResponse(reason.into())
    })?;
    parse_generated_output(&content)
}

fn parse_generated_output(content: &str) -> Result<GeneratedOutput, OpenRouterError> {
    let output = content.trim();
    if let Some(command) = output.strip_prefix("COMMAND:") {
        return validate_command(command).map(GeneratedOutput::Command);
    }
    if let Some(question) = output.strip_prefix("QUESTION:") {
        return validate_clarification(question).map(GeneratedOutput::Clarification);
    }
    if let Some(answer) = output.strip_prefix("ANSWER:") {
        return validate_answer(answer).map(GeneratedOutput::Answer);
    }

    Err(OpenRouterError::InvalidResponse(
        "the response was not marked as COMMAND, QUESTION, or ANSWER".into(),
    ))
}

fn validate_command(content: &str) -> Result<String, OpenRouterError> {
    let command = validate_single_line(content, MAX_COMMAND_BYTES)
        .map_err(OpenRouterError::InvalidCommand)?;
    if command.starts_with("```") || command.ends_with("```") {
        return Err(OpenRouterError::InvalidCommand(
            "the response contained a Markdown code fence".into(),
        ));
    }

    Ok(command.to_owned())
}

fn validate_clarification(content: &str) -> Result<String, OpenRouterError> {
    validate_informational_output(content, MAX_CLARIFICATION_BYTES, "clarification")
}

fn validate_answer(content: &str) -> Result<String, OpenRouterError> {
    validate_informational_output(content, MAX_ANSWER_BYTES, "answer")
}

fn validate_informational_output(
    content: &str,
    max_bytes: usize,
    kind: &str,
) -> Result<String, OpenRouterError> {
    validate_single_line(content, max_bytes)
        .map(str::to_owned)
        .map_err(|error| OpenRouterError::InvalidResponse(format!("invalid {kind}: {error}")))
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

    use crate::cli::Shell;
    use crate::context::{ContextResponse, ContextTurn};

    use super::{
        GeneratedOutput, OpenRouterError, chat_messages, check_status, parse_chat_response,
        sanitize_error, validate_command,
    };

    #[test]
    fn sends_bounded_history_before_the_current_request() {
        let history = [ContextTurn {
            created_at: 1,
            working_directory: "/tmp/images".into(),
            request: "create a 50 meg file named disk.img".into(),
            response: ContextResponse::Command("truncate -s 50M disk.img".into()),
        }];
        let messages = chat_messages(
            Shell::Bash,
            &history,
            "/tmp/images",
            "create a filesystem on it",
        );

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, "user");
        assert!(messages[1].content.contains("create a 50 meg file"));
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[2].content, "COMMAND: truncate -s 50M disk.img");
        assert!(messages[3].content.contains("create a filesystem on it"));
        assert!(
            messages[0]
                .content
                .contains("may have been changed or never executed")
        );
        assert!(messages[0].content.contains("ANSWER:"));
        assert!(messages[0].content.contains("what can you do?"));
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
                "message": {"content": "QUESTION: What would you like to do with tobacco?"},
                "finish_reason": "stop"
            }]
        }"#;
        assert_eq!(
            parse_chat_response(response).unwrap(),
            GeneratedOutput::Clarification("What would you like to do with tobacco?".into())
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
            OpenRouterError::InvalidResponse(ref message)
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
        assert!(matches!(error, OpenRouterError::InvalidCommand(_)));
    }

    #[test]
    fn rejects_markdown_fences() {
        assert!(validate_command("```sh").is_err());
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
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":401,"message":"invalid\ncredentials"}}"#,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            OpenRouterError::Api {
                status: StatusCode::UNAUTHORIZED,
                ref message,
            } if message == "invalid credentials"
        ));
    }
}
