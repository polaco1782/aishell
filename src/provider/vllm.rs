use super::{CheckStyle, ProviderSpec, RequestStyle};

pub(super) const SPEC: ProviderSpec = ProviderSpec {
    id: "vllm",
    setup_choices: &["4", "vllm"],
    display_name: "vLLM",
    default_model: None,
    default_base_url: "http://127.0.0.1:8000/v1",
    requires_api_key: false,
    request_style: RequestStyle::Compatible,
    check_style: CheckStyle::Catalog,
};
