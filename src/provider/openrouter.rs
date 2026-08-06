use super::{CheckStyle, ProviderSpec, RequestStyle};

pub(super) const SPEC: ProviderSpec = ProviderSpec {
    id: "openrouter",
    setup_choices: &["1", "openrouter"],
    display_name: "OpenRouter",
    default_model: Some("openrouter/auto"),
    default_base_url: "https://openrouter.ai/api/v1",
    requires_api_key: true,
    request_style: RequestStyle::Router,
    check_style: CheckStyle::Key,
};
