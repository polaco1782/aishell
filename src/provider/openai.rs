use super::{CheckStyle, ProviderSpec, RequestStyle};

pub(super) const SPEC: ProviderSpec = ProviderSpec {
    id: "openai",
    setup_choices: &["2", "openai"],
    display_name: "OpenAI",
    default_model: Some("gpt-5.6-luna"),
    default_base_url: "https://api.openai.com/v1",
    requires_api_key: true,
    request_style: RequestStyle::Official,
    check_style: CheckStyle::Model,
};
