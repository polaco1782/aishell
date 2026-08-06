use super::{CheckStyle, ProviderSpec, RequestStyle};

pub(super) const SPEC: ProviderSpec = ProviderSpec {
    id: "llamacpp",
    setup_choices: &["3", "llamacpp", "llama.cpp", "llama-cpp"],
    display_name: "llama.cpp",
    default_model: None,
    default_base_url: "http://127.0.0.1:8080/v1",
    requires_api_key: false,
    request_style: RequestStyle::Compatible,
    check_style: CheckStyle::Catalog,
};
