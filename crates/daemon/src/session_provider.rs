//! A LynShen session on a provider of its own (feature `session_provider`):
//! `session_create` / `session_open` `options.provider`, the 赛博鸡蛋
//! campaign's endpoint and key for one conversation, while every other
//! session and config.json keep the account's provider.
//!
//! `{id, name, base_url, protocol, api_key, models: [{name}], model}`, with
//! `protocol` one of `chat`, `responses`, `anthropic`. The base URL must be
//! https on the host of the account's gateway (config.json's
//! `lynshen_api_url`), so nothing that reaches the daemon can send a
//! conversation, or its key, anywhere else.
//!
//! The session keeps it: a reopen that names no provider (a phone, a
//! restart, an idle close) runs on it again, one that names another replaces
//! it, and `"provider": null` takes the session back to the account's. The
//! session record holds everything but the key (`session_provider`); the key
//! is in auth.json's `tool_providers` block as `session:<id>`, encrypted
//! when `encrypt_secrets` is on (docs/secrets.md), and never logged. A
//! session whose key is gone does not open rather than run on the account.

use lynshen_agent_core::SessionProvider;
use serde_json::{json, Value};

/// What a client asked for (`options.provider`).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Request {
    /// Not named: a new session runs on the account's provider, a reopened
    /// one on the provider it kept.
    #[default]
    Keep,
    /// `null`: back on the account's provider.
    Clear,
    Set(SessionProvider),
    /// Not a provider: refused with this.
    Invalid(String),
}

impl Request {
    pub fn from_options(options: &Value) -> Self {
        match options.get("provider") {
            None => Self::Keep,
            Some(Value::Null) => Self::Clear,
            Some(value) if value.is_object() => match parse(value) {
                Ok(provider) => Self::Set(provider),
                Err(error) => Self::Invalid(error),
            },
            Some(_) => Self::Invalid("options.provider must be an object or null".to_string()),
        }
    }
}

const PROTOCOLS: [&str; 3] = ["chat", "responses", "anthropic"];

/// A provider as a client sends it, its shape checked (the host is checked
/// against the gateway by `check_base_url`). `models` may also be plain names.
pub fn parse(value: &Value) -> Result<SessionProvider, String> {
    let text = |key: &str| value[key].as_str().map(str::trim).unwrap_or_default();
    let id = text("id");
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("provider.id must be 1-64 letters, digits, - or _".to_string());
    }
    if lynshen_agent_core::session_provider::reserved_id(id) {
        return Err(format!("provider.id {id} names a built-in provider"));
    }
    let protocol = text("protocol");
    if !PROTOCOLS.contains(&protocol) {
        return Err(format!(
            "provider.protocol must be one of {}",
            PROTOCOLS.join(", ")
        ));
    }
    let api_key = text("api_key");
    if api_key.is_empty() || api_key.len() > 1024 || api_key.chars().any(char::is_whitespace) {
        return Err("provider.api_key is missing or not a key".to_string());
    }
    let mut models: Vec<String> = Vec::new();
    for entry in value["models"].as_array().into_iter().flatten() {
        let name = entry["name"].as_str().or(entry.as_str()).map(str::trim);
        match name {
            Some(name) if !name.is_empty() && !name.chars().any(char::is_whitespace) => {
                if !models.iter().any(|known| known == name) {
                    models.push(name.to_string());
                }
            }
            _ => return Err("provider.models must be models with a name".to_string()),
        }
    }
    if models.is_empty() {
        return Err("provider.models is empty".to_string());
    }
    let model = text("model");
    if !models.iter().any(|known| known == model) {
        return Err("provider.model must be one of provider.models".to_string());
    }
    let name = match text("name") {
        "" => id,
        name => name,
    };
    Ok(SessionProvider {
        id: id.to_string(),
        name: name.to_string(),
        base_url: text("base_url").trim_end_matches('/').to_string(),
        protocol: protocol.to_string(),
        api_key: api_key.to_string(),
        models,
        model: model.to_string(),
    })
}

/// `base_url` may only be https on the host and port of the account's
/// gateway (`gateway`, its `lynshen_api_url`).
pub fn check_base_url(base_url: &str, gateway: &str) -> Result<(), String> {
    let wanted = https_authority(gateway);
    let refused = || {
        format!(
            "provider.base_url must be an https URL on the LynShen gateway ({})",
            wanted.as_deref().unwrap_or("none configured over https")
        )
    };
    let clean = !base_url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\');
    match https_authority(base_url) {
        Some(authority) if clean && Some(&authority) == wanted.as_ref() => Ok(()),
        _ => Err(refused()),
    }
}

/// The `host[:port]` of an https URL, lowercased, without the default port;
/// None for any other scheme, a user part, or a host that is not plain
/// letters, digits, dots and dashes.
fn https_authority(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].to_ascii_lowercase();
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority.as_str(), None),
    };
    let plain = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    if host.is_empty() || !host.chars().all(plain) {
        return None;
    }
    match port {
        None | Some("443") => Some(host.to_string()),
        Some(port) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            Some(format!("{host}:{port}"))
        }
        _ => None,
    }
}

/// The account's gateway, as config.json names it now.
pub fn gateway() -> String {
    lynshen_agent_core::lynshen_api_url().unwrap_or_else(|| "https://api.lynshen.org".to_string())
}

/// The provider a new session starts on: None for the account's.
pub fn for_new(
    request: &Request,
    lynshen: bool,
    agent: bool,
    gateway: &str,
) -> Result<Option<SessionProvider>, String> {
    match request {
        Request::Keep | Request::Clear => Ok(None),
        Request::Invalid(error) => Err(error.clone()),
        Request::Set(_) if !lynshen => Err("options.provider is for LynShen sessions".to_string()),
        Request::Set(_) if agent => {
            Err("an agent's sessions run on the account's provider".to_string())
        }
        Request::Set(provider) => {
            check_base_url(&provider.base_url, gateway)?;
            Ok(Some(provider.clone()))
        }
    }
}

/// The provider a session reopens on: the one the client names, else the
/// one it kept (`kept`, Err when its key is gone), checked against the
/// gateway again; None for the account's.
pub fn for_open(
    request: &Request,
    kept: Option<Result<SessionProvider, String>>,
    gateway: &str,
) -> Result<Option<SessionProvider>, String> {
    let provider = match request {
        Request::Invalid(error) => return Err(error.clone()),
        Request::Clear => return Ok(None),
        Request::Set(provider) => provider.clone(),
        Request::Keep => match kept {
            None => return Ok(None),
            Some(kept) => kept?,
        },
    };
    check_base_url(&provider.base_url, gateway)?;
    Ok(Some(provider))
}

/// The provider as the session record keeps it: everything but the key.
pub fn record(provider: &SessionProvider) -> Value {
    json!({
        "id": provider.id,
        "name": provider.name,
        "base_url": provider.base_url,
        "protocol": provider.protocol,
        "models": provider.models,
        "model": provider.model,
    })
}

/// The provider session `id` kept (`record`), with its key.
pub fn restore(id: &str, kept: &Value) -> Result<SessionProvider, String> {
    let key = keys::get(id).ok_or_else(|| {
        let name = kept["name"].as_str().unwrap_or("its provider");
        format!("this conversation runs on {name} and its key is gone: pick its model again")
    })?;
    let mut value = kept.clone();
    value["api_key"] = json!(key);
    parse(&value)
}

/// Saves (`Some`) or removes (`None`) the key session `id` runs with.
pub fn save_key(id: &str, key: Option<&str>) -> Result<(), String> {
    keys::set(id, key)
}

/// Where keys are kept: auth.json in the daemon, a map in unit tests (which
/// never read or write the user's auth.json).
mod keys {
    const TOOL: &str = "session";

    #[cfg(not(test))]
    pub fn get(id: &str) -> Option<String> {
        lynshen_agent_core::tool_provider_key(TOOL, id)
    }

    #[cfg(not(test))]
    pub fn set(id: &str, key: Option<&str>) -> Result<(), String> {
        lynshen_agent_core::set_tool_provider_key(TOOL, id, key)
    }

    #[cfg(test)]
    fn map() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<String, String>> {
        static KEYS: std::sync::Mutex<std::collections::BTreeMap<String, String>> =
            std::sync::Mutex::new(std::collections::BTreeMap::new());
        KEYS.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[cfg(test)]
    pub fn get(id: &str) -> Option<String> {
        map().get(&format!("{TOOL}:{id}")).cloned()
    }

    #[cfg(test)]
    pub fn set(id: &str, key: Option<&str>) -> Result<(), String> {
        let name = format!("{TOOL}:{id}");
        match key {
            Some(key) => map().insert(name, key.to_string()),
            None => map().remove(&name),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GATEWAY: &str = "https://api.lynshen.org";

    fn egg(base_url: &str) -> Value {
        json!({
            "id": "cyber-egg",
            "name": "赛博鸡蛋",
            "base_url": base_url,
            "protocol": "chat",
            "api_key": "sk-egg-secret",
            "models": [{ "name": "egg-mini" }, { "name": "egg-pro" }],
            "model": "egg-pro",
        })
    }

    fn set(base_url: &str) -> Request {
        Request::from_options(&json!({ "provider": egg(base_url) }))
    }

    #[test]
    fn the_desktops_provider_is_read_as_sent() {
        let Request::Set(provider) = set("https://api.lynshen.org/new/v1/") else {
            panic!("not a provider");
        };
        assert_eq!(provider.id, "cyber-egg");
        assert_eq!(provider.name, "赛博鸡蛋");
        assert_eq!(provider.base_url, "https://api.lynshen.org/new/v1");
        assert_eq!(provider.protocol, "chat");
        assert_eq!(provider.api_key, "sk-egg-secret");
        assert_eq!(provider.models, ["egg-mini", "egg-pro"]);
        assert_eq!(provider.model, "egg-pro");
        assert_eq!(Request::from_options(&json!({})), Request::Keep);
        assert_eq!(
            Request::from_options(&json!({ "provider": null })),
            Request::Clear
        );
    }

    #[test]
    fn a_provider_that_is_not_one_is_refused() {
        let broken = |change: &dyn Fn(&mut Value)| {
            let mut value = egg("https://api.lynshen.org/new/v1");
            change(&mut value);
            matches!(
                Request::from_options(&json!({ "provider": value })),
                Request::Invalid(_)
            )
        };
        assert!(broken(&|v| v["api_key"] = json!("")));
        assert!(broken(&|v| v["api_key"] = json!("sk with space")));
        assert!(broken(&|v| v["model"] = json!("gpt-5.5")));
        assert!(broken(&|v| v["models"] = json!([])));
        assert!(broken(&|v| v["protocol"] = json!("codex")));
        assert!(broken(&|v| v["id"] = json!("lynshen")));
        assert!(broken(&|v| v["id"] = json!("openai")));
        assert!(broken(&|v| v["id"] = json!("../x")));
        assert!(matches!(
            Request::from_options(&json!({ "provider": "cyber-egg" })),
            Request::Invalid(_)
        ));
        // Its message never carries the key.
        let Request::Invalid(message) =
            Request::from_options(&json!({ "provider": { "api_key": "sk-egg-secret" } }))
        else {
            panic!("accepted");
        };
        assert!(!message.contains("sk-egg-secret"), "{message}");
    }

    #[test]
    fn a_base_url_off_the_gateways_https_host_is_refused() {
        for good in [
            "https://api.lynshen.org/new/v1",
            "https://API.lynshen.org/new/v1",
            "https://api.lynshen.org:443/new/v1",
            "https://api.lynshen.org",
        ] {
            assert_eq!(check_base_url(good, GATEWAY), Ok(()), "{good}");
            assert_eq!(check_base_url(good, "https://api.lynshen.org/"), Ok(()));
        }
        for bad in [
            "http://api.lynshen.org/new/v1",
            "https://evil.example/new/v1",
            "https://api.lynshen.org.evil.example/new/v1",
            "https://evil.example/api.lynshen.org",
            "https://user@api.lynshen.org/new/v1",
            "https://evil.example@api.lynshen.org/new/v1",
            "https://api.lynshen.org@evil.example/new/v1",
            "https://api.lynshen.org\\@evil.example/new/v1",
            "https://api.lynshen.org:8443/new/v1",
            "https://api.lynshen.org%2eevil.example/new/v1",
            "https://api.lynshen.org/new/v1 https://evil.example",
            "ftp://api.lynshen.org/new/v1",
            "//api.lynshen.org/new/v1",
            "api.lynshen.org/new/v1",
            "",
        ] {
            assert!(check_base_url(bad, GATEWAY).is_err(), "{bad}");
        }
        // A gateway on another host (a staging one): only that host.
        let staging = "https://staging.lynshen.test:8443";
        assert!(check_base_url("https://staging.lynshen.test:8443/new/v1", staging).is_ok());
        assert!(check_base_url("https://api.lynshen.org/new/v1", staging).is_err());
        // A gateway that is not https: nothing passes.
        assert!(check_base_url("https://localhost:8080/new/v1", "http://localhost:8080").is_err());
    }

    #[test]
    fn a_provider_starts_new_lynshen_sessions_only_and_on_the_gateway() {
        let request = set("https://api.lynshen.org/new/v1");
        let provider = for_new(&request, true, false, GATEWAY).unwrap().unwrap();
        assert_eq!(provider.id, "cyber-egg");
        assert!(for_new(&request, false, false, GATEWAY).is_err());
        assert!(for_new(&request, true, true, GATEWAY).is_err());
        assert!(for_new(&set("https://evil.example/new/v1"), true, false, GATEWAY).is_err());
        assert_eq!(for_new(&Request::Keep, true, false, GATEWAY), Ok(None));
        assert_eq!(for_new(&Request::Clear, true, false, GATEWAY), Ok(None));
    }

    #[test]
    fn a_reopen_keeps_the_provider_until_a_client_names_another_or_none() {
        let Request::Set(kept) = set("https://api.lynshen.org/new/v1") else {
            panic!("not a provider");
        };
        let kept_ok = || Some(Ok(kept.clone()));
        // Named by nobody: the one it kept, or the account's if none.
        assert_eq!(
            for_open(&Request::Keep, kept_ok(), GATEWAY),
            Ok(Some(kept.clone()))
        );
        assert_eq!(for_open(&Request::Keep, None, GATEWAY), Ok(None));
        // Its key gone: it does not open, rather than on the account.
        assert!(for_open(&Request::Keep, Some(Err("gone".into())), GATEWAY).is_err());
        // A gateway moved elsewhere since: refused too.
        assert!(for_open(&Request::Keep, kept_ok(), "https://other.example").is_err());
        // Another one replaces it; null drops it.
        let mut other = egg("https://api.lynshen.org/new/v1");
        other["model"] = json!("egg-mini");
        let other = Request::from_options(&json!({ "provider": other }));
        let opened = for_open(&other, kept_ok(), GATEWAY).unwrap().unwrap();
        assert_eq!(opened.model, "egg-mini");
        assert_eq!(for_open(&Request::Clear, kept_ok(), GATEWAY), Ok(None));
    }

    #[test]
    fn the_record_keeps_everything_but_the_key() {
        let Request::Set(provider) = set("https://api.lynshen.org/new/v1") else {
            panic!("not a provider");
        };
        let kept = record(&provider);
        assert!(!kept.to_string().contains("sk-egg-secret"), "{kept}");
        let id = "s-record-test";
        assert!(restore(id, &kept).is_err());
        save_key(id, Some(&provider.api_key)).unwrap();
        assert_eq!(restore(id, &kept), Ok(provider));
        save_key(id, None).unwrap();
        assert!(restore(id, &kept).is_err());
    }
}
