//! A session on a provider of its own (`AgentCore::set_session_provider`,
//! the daemon's `session_provider`): its requests go to that provider's URL
//! with that key, the provider's refusals reach the client as it sent them,
//! and nothing of it reaches config.json or another session. A `/model` it
//! does not serve takes the session back to the configured provider.
//!
//! The configured provider is the fake model of `support/fake_model.rs`;
//! the session's own is a local stand-in for the campaign endpoint.

#[path = "support/fake_model.rs"]
mod fake_model;

use fake_model::{requests, setup, temp_dir};
use lynshen_agent_core::{AgentCore, AgentEvent, SessionProvider};
use serde_json::{json, Value};
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

/// What the campaign endpoint refuses a leaked key with, sent as is.
const REFUSAL: &str =
    r#"{"error":{"code":"cyber_egg_key_leaked","message":"这个 Key 已停用","type":"cyber_egg"}}"#;
const KEY: &str = "sk-egg-test-key";

/// One request the campaign endpoint got: its path, its Authorization
/// header and its body.
type Seen = Arc<Mutex<Vec<(String, String, Value)>>>;

/// A local stand-in for the campaign endpoint (Chat Completions): a user
/// message `REFUSE` is answered 403 with `REFUSAL`, anything else with
/// "egg reply". Returns its base URL (`…/new/v1`).
fn start_campaign(seen: Seen) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let seen = Arc::clone(&seen);
            thread::spawn(move || {
                // The request line and the Authorization header, then the
                // body the shared reader takes from the rest.
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let mut head = String::new();
                let mut authorization = String::new();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("authorization") {
                            authorization = value.trim().to_string();
                        }
                    }
                    head.push_str(&header);
                    if header.trim_end().is_empty() {
                        break;
                    }
                }
                let body = read_body(&head, &mut reader);
                let last = body["messages"]
                    .as_array()
                    .and_then(|messages| messages.last())
                    .map(|message| message["content"].to_string())
                    .unwrap_or_default();
                seen.lock().unwrap().push((path, authorization, body));
                let _ = if last.contains("REFUSE") {
                    write!(
                        stream,
                        "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{REFUSAL}",
                        REFUSAL.len()
                    )
                } else {
                    let chunks = [
                        json!({ "choices": [{ "index": 0, "delta": { "content": "egg reply" }, "finish_reason": null }] }),
                        json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] }),
                    ];
                    let sse: String = chunks
                        .iter()
                        .map(|chunk| format!("data: {chunk}\n\n"))
                        .chain(["data: [DONE]\n\n".to_string()])
                        .collect();
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{sse}"
                    )
                };
            });
        }
    });
    format!("http://{address}/new/v1")
}

/// The body after headers already read (`head`): its Content-Length bytes.
fn read_body(head: &str, reader: &mut impl std::io::Read) -> Value {
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap_or(Value::Null)
}

fn pump(core: &mut AgentCore) -> Vec<AgentEvent> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        let batch = core.poll_events();
        let done = batch
            .iter()
            .any(|event| matches!(event, AgentEvent::Status(status) if status == "ready"));
        seen.extend(batch);
        if done {
            return seen;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out; events: {seen:#?}");
}

fn turn(core: &mut AgentCore, message: &str) -> Vec<AgentEvent> {
    core.submit_user_message(message.to_string());
    pump(core)
}

fn errors(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Error(message) => Some(message.clone()),
            _ => None,
        })
        .collect()
}

fn reply(events: &[AgentEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::AssistantDelta(delta) => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

fn config() -> Value {
    let path = PathBuf::from(env::var_os("HOME").unwrap()).join(".lynshen/config.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn assert_config_untouched() {
    let config = config();
    assert_eq!(config["provider"], "fake", "{config}");
    assert_eq!(config["model"], "fake-model", "{config}");
    let text = config.to_string();
    assert!(!text.contains("egg"), "{text}");
}

fn egg(base_url: &str) -> SessionProvider {
    SessionProvider {
        id: "cyber-egg".to_string(),
        name: "赛博鸡蛋".to_string(),
        base_url: base_url.to_string(),
        protocol: "chat".to_string(),
        api_key: KEY.to_string(),
        models: vec!["egg-mini".to_string(), "egg-pro".to_string()],
        model: "egg-pro".to_string(),
    }
}

#[test]
fn a_session_on_its_own_provider_sends_there_alone_and_leaves_the_config_alone() {
    let _guard = setup();
    let seen: Seen = Arc::default();
    let campaign = start_campaign(Arc::clone(&seen));
    let cwd = temp_dir("session-provider");
    let mut on_egg = AgentCore::open(cwd.clone()).unwrap();
    on_egg.set_session_provider(egg(&campaign));
    let mut on_account = AgentCore::open(cwd).unwrap();

    // Its turn goes to the provider's URL, with its key, on its model.
    let events = turn(&mut on_egg, "hello egg");
    assert_eq!(reply(&events), "egg reply", "{events:#?}");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let (path, authorization, body) = &seen[0];
        assert_eq!(path, "/new/v1/chat/completions");
        assert_eq!(authorization, &format!("Bearer {KEY}"));
        assert_eq!(body["model"], "egg-pro");
    }
    assert!(matches!(
        on_egg.model_status_event(),
        AgentEvent::ModelStatus { provider, model, .. } if provider == "cyber-egg" && model == "egg-pro"
    ));

    // Another session keeps the configured provider.
    let before = requests().len();
    let events = turn(&mut on_account, "hello account");
    assert!(errors(&events).is_empty(), "{events:#?}");
    let sent = requests()[before..].to_vec();
    assert!(!sent.is_empty());
    assert!(
        sent.iter().all(|body| body["model"] == "fake-model"),
        "{sent:#?}"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_config_untouched();

    // The provider's refusal reaches the client as it was sent.
    let events = turn(&mut on_egg, "REFUSE");
    let failed = errors(&events);
    assert!(
        failed.iter().any(|error| error.contains(REFUSAL)),
        "{failed:#?}"
    );

    // Its own models: this conversation only. A sign-in is the account's.
    on_egg.handle_command("/model egg-mini");
    assert_eq!(on_egg.session_provider().unwrap().model, "egg-mini");
    let (_, events) = on_egg.handle_command("/login");
    assert_eq!(errors(&events).len(), 1, "{events:#?}");
    assert_config_untouched();

    // A model it does not serve: back on the configured provider.
    on_egg.handle_command("/model fake-model");
    assert!(on_egg.session_provider().is_none());
    let before = requests().len();
    let events = turn(&mut on_egg, "back on the account");
    assert!(errors(&events).is_empty(), "{events:#?}");
    assert!(requests()[before..]
        .iter()
        .any(|body| body["model"] == "fake-model"));
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_config_untouched();
}
