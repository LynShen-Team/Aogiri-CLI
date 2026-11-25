//! A provider one session runs on instead of the configured one: the
//! daemon's `session_create` / `session_open` `provider` (feature
//! `session_provider`), such as the 赛博鸡蛋 campaign's endpoint and key for
//! one conversation. Only that engine's copy of the config changes:
//! config.json and every other session keep the account's provider.

use crate::config::Config;

/// `{id, name, base_url, protocol, api_key, models, model}`. `Debug` leaves
/// the key out.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionProvider {
    /// What the engine reports as its provider (`model_status`).
    pub id: String,
    /// Its name as the user knows it.
    pub name: String,
    pub base_url: String,
    /// `chat`, `responses` or `anthropic`.
    pub protocol: String,
    pub api_key: String,
    /// The models it serves; `model` is one of them.
    pub models: Vec<String>,
    pub model: String,
}

impl std::fmt::Debug for SessionProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionProvider")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("protocol", &self.protocol)
            .field("models", &self.models)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// Provider ids a session provider may not take: the engine treats these as
/// the LynShen or Monoize gateway (the account's token, its headers) or as a
/// provider it knows (that provider's URLs, logins and protocols).
pub fn reserved_id(id: &str) -> bool {
    let catalog = llm_provider_kit::omp::catalog();
    matches!(id, "lynshen" | "lynshen_gateway" | "monoize")
        || catalog.auth_provider(id).is_some()
        || catalog.default_base_url(id).is_some()
        || !catalog.models(id).is_empty()
        || crate::providers::template(id).is_some()
}

/// Points `config` (an engine's own copy) at `provider`: its URL, protocol
/// and models, the helpers (compaction, safety review, idle summary) on the
/// conversation's model, and nothing of the account's left that would send a
/// request elsewhere: the gateway's page fetches, its routing, its image or
/// subagent models.
pub(crate) fn apply(config: &mut Config, provider: &SessionProvider) {
    let models = provider
        .models
        .iter()
        .map(|name| {
            // What the account's catalog knows of the model (its window, its
            // efforts); the provider's tokens carry no price.
            let mut entry = config
                .lynshen_models
                .iter()
                .chain(&config.models)
                .find(|entry| entry.name == *name)
                .cloned()
                .unwrap_or_else(|| config.model_config(name));
            entry.hidden = false;
            entry.input_cost = 0.0;
            entry.cached_input_cost = 0.0;
            entry.output_cost = 0.0;
            entry.group_windows.clear();
            entry
        })
        .collect();
    config.provider = provider.id.clone();
    config.protocol = provider.protocol.clone();
    config.base_url = provider.base_url.clone();
    config.models = models;
    config.model = provider.model.clone();
    let efforts = config.current_model_config().reasoning_efforts;
    if !efforts.contains(&config.reasoning_effort) {
        config.reasoning_effort = crate::config::default_effort(&efforts)
            .unwrap_or("medium")
            .to_string();
    }
    config.compact_model.clear();
    config.safety_model.clear();
    config.title_model.clear();
    config.web_search_model.clear();
    config.web_fetch_engine = crate::web::DEFAULT_FETCH_ENGINE.to_string();
    fence(config, provider);
}

/// What an engine reads from config.json again every turn
/// (`Config::reload_live_settings`), kept to `provider`: subagents on its
/// models only, no image model it does not serve, no gateway routing.
pub(crate) fn fence(config: &mut Config, provider: &SessionProvider) {
    config
        .subagent_models
        .retain(|entry| provider.models.contains(&entry.name));
    if !provider.models.contains(&config.image_model) {
        config.image_model.clear();
    }
    config.lynshen_groups.clear();
    config.monoize_providers.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn egg() -> SessionProvider {
        SessionProvider {
            id: "cyber-egg".into(),
            name: "赛博鸡蛋".into(),
            base_url: "https://api.lynshen.org/new/v1".into(),
            protocol: "chat".into(),
            api_key: "sk-egg-secret".into(),
            models: vec!["egg-mini".into(), "egg-pro".into()],
            model: "egg-pro".into(),
        }
    }

    fn account() -> Config {
        let value = json!({
            "provider": "lynshen",
            "model": "gpt-5.5",
            "compact_model": "gpt-5.4-mini",
            "safety_model": "gpt-5.4-mini",
            "title_model": "gpt-5.4-mini",
            "image_model": "gpt-image-2",
            "web_fetch_engine": "jina",
            "web_search_model": "claude-haiku-4-5",
            "subagent_models": [
                { "name": "egg-mini", "description": "fast" },
                { "name": "gpt-5.4-mini", "description": "cheap" }
            ],
            "lynshen_models": [
                { "name": "gpt-5.5", "context_window": 400000 },
                { "name": "egg-pro", "context_window": 1000000, "input_cost": 3.0,
                  "reasoning_efforts": ["low", "high"] }
            ],
            "lynshen_groups": { "egg-pro": "g-1" },
        });
        Config::from_value(&value.to_string(), PathBuf::from("config.json")).unwrap()
    }

    #[test]
    fn the_engine_copy_runs_on_the_provider_with_nothing_of_the_accounts() {
        let mut config = account();
        apply(&mut config, &egg());
        assert_eq!(config.provider, "cyber-egg");
        assert_eq!(config.base_url, "https://api.lynshen.org/new/v1");
        assert_eq!(config.protocol, "chat");
        assert_eq!(config.model, "egg-pro");
        let names: Vec<_> = config.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["egg-mini", "egg-pro"]);
        // The account's catalog says what it knows of the model, not its price.
        let known = config.current_model_config();
        assert_eq!(known.context_window, 1_000_000);
        assert_eq!(known.input_cost, 0.0);
        assert!(known.reasoning_efforts.contains(&config.reasoning_effort));
        // Helpers follow the conversation's model on the same provider.
        assert_eq!(config.compact().0, "egg-pro");
        assert_eq!(config.safety().0, "egg-pro");
        assert_eq!(config.title().0, "egg-pro");
        assert!(config.image_model.is_empty());
        assert!(config.web_search_model.is_empty());
        assert_eq!(config.web_fetch_engine, crate::web::DEFAULT_FETCH_ENGINE);
        assert!(config.lynshen_groups.is_empty());
        let subagents: Vec<_> = config.subagent_models.iter().map(|m| &m.name).collect();
        assert_eq!(subagents, ["egg-mini"]);
    }

    #[test]
    fn the_key_is_never_printed() {
        let printed = format!("{:?}", egg());
        assert!(!printed.contains("sk-egg-secret"), "{printed}");
        assert!(printed.contains("cyber-egg"), "{printed}");
    }

    #[test]
    fn built_in_providers_are_reserved() {
        for id in ["lynshen", "monoize", "openai", "anthropic", "deepseek"] {
            assert!(reserved_id(id), "{id}");
        }
        assert!(!reserved_id("cyber-egg"));
    }
}
