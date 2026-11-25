//! Sample frames for design previews: each scene is drawn by the real
//! builder and renderer into a test buffer and returned as JSON cells
//! (symbol, colours, modifiers). `examples/tui_preview.rs` prints them; a
//! script turns them into images. Built only with `--features preview`.

use std::time::{Duration, Instant};

use lynshen_agent_core::{AccountSummary, AgentEvent, ModelOptionView, SessionListItemView};
use ratatui::{
    backend::TestBackend,
    style::{Color, Modifier},
    Terminal,
};
use serde_json::{json, Value};

use crate::input::InputBuffer;
use crate::state::TuiState;
use crate::terminal_renderer::draw_frame;
use crate::theme::{self, ColorDepth, Theme};
use crate::{AccountState, ActivityKind, CommandCandidate};

/// Renders every scene in the dark, light and colourless themes.
pub fn render_all() -> Value {
    let themes = [
        (
            "dark",
            Theme::new(ColorDepth::TrueColor, Some((22, 21, 19))),
        ),
        (
            "light",
            Theme::new(ColorDepth::TrueColor, Some((251, 250, 247))),
        ),
        ("mono", Theme::new(ColorDepth::None, None)),
    ];
    let mut frames = Vec::new();
    for (theme_name, theme) in themes {
        theme::with_theme(theme, || {
            for (name, width, height, build) in scenes() {
                let (state, input) = build();
                frames.push(render(name, theme_name, width, height, state, input));
            }
        });
    }
    Value::Array(frames)
}

type Scene = (&'static str, u16, u16, fn() -> (TuiState, InputBuffer));

fn scenes() -> Vec<Scene> {
    vec![
        ("welcome", 100, 22, welcome),
        ("working", 100, 36, working),
        ("approval", 100, 26, approval),
        ("palette", 100, 24, palette),
        ("model", 100, 26, model_picker),
        ("resume", 100, 24, resume_picker),
        ("help", 100, 26, help),
        ("narrow", 58, 16, welcome),
    ]
}

fn render(
    name: &str,
    theme_name: &str,
    width: u16,
    height: u16,
    mut state: TuiState,
    input: InputBuffer,
) -> Value {
    let now = Instant::now();
    let document = state.build_document(&input, width as usize, now, None);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    let mut cursor = None;
    terminal
        .draw(|frame| {
            let area = frame.area();
            let mut scroll = 0usize;
            cursor = draw_frame(frame.buffer_mut(), area, &document, &mut scroll);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let rows: Vec<Value> = (0..height)
        .map(|y| {
            Value::Array(
                (0..width)
                    .map(|x| {
                        let cell = &buffer[(x, y)];
                        json!([
                            if cell.skip { "" } else { cell.symbol() },
                            hex(cell.fg),
                            hex(cell.bg),
                            cell.modifier.contains(Modifier::BOLD),
                            cell.modifier.contains(Modifier::DIM),
                            cell.modifier.contains(Modifier::ITALIC),
                            cell.modifier.contains(Modifier::REVERSED),
                        ])
                    })
                    .collect(),
            )
        })
        .collect();
    json!({
        "name": name,
        "theme": theme_name,
        "width": width,
        "height": height,
        "cursor": cursor.map(|(x, y)| json!([y, x])),
        "rows": rows,
    })
}

fn hex(color: Color) -> Value {
    match color {
        Color::Rgb(r, g, b) => json!(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Reset => Value::Null,
        other => json!(format!("{other:?}")),
    }
}

fn base_state() -> TuiState {
    let mut state = TuiState::default();
    let mut input = InputBuffer::default();
    state.apply_events(
        vec![
            AgentEvent::Startup {
                version: "0.8.11".to_string(),
                session_id: "s1".to_string(),
                profile_dir: "~/.lynshen".to_string(),
                config_path: "~/.lynshen/config.json".to_string(),
                cwd: "~/Code/LynShen-CLI".to_string(),
                model: "gpt-5.5".to_string(),
                context_window: 400_000,
            },
            AgentEvent::ModelStatus {
                provider: "lynshen".to_string(),
                model: "gpt-5.5".to_string(),
                model_label: None,
                reasoning_effort: "high".to_string(),
                context_window: 400_000,
                context_limit: 400_000,
                max_output_tokens: 128_000,
                input_modalities: Vec::new(),
                reasoning_efforts: vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "max".to_string(),
                ],
                state: "ready".to_string(),
            },
        ],
        &mut input,
    );
    state.set_account(AccountState::Ready(AccountSummary {
        email: "dev@lynshen.org".to_string(),
        balance: Some("128.40".to_string()),
        currency: Some("CNY".to_string()),
        plan: Some("Pro 套餐".to_string()),
    }));
    state.git_status = Some(crate::git_bar::GitStatus {
        branch: "main".to_string(),
        dirty: true,
    });
    state
}

fn welcome() -> (TuiState, InputBuffer) {
    (base_state(), InputBuffer::default())
}

fn tool(state: &mut TuiState, id: &str, name: &str, output: Value, running: bool) {
    state.upsert_tool(
        id.to_string(),
        name.to_string(),
        output.to_string(),
        running,
    );
}

fn working() -> (TuiState, InputBuffer) {
    let mut state = base_state();
    state.chat.clear();
    let mut input = InputBuffer::default();
    state.apply_events(
        vec![AgentEvent::UserMessage(
            "修复 cargo test 里失败的那个测试，并说明原因".to_string(),
        )],
        &mut input,
    );
    state.confirm_pending_user();
    state.chat.push(crate::ChatLine::Reasoning {
        text: String::new(),
        collapsed: true,
        duration_secs: Some(6),
    });
    tool(
        &mut state,
        "t1",
        "read",
        json!({"path": "crates/tui/src/lib.rs", "offset": 1440, "lines_read": 91}),
        false,
    );
    let failing: String = (1..=14)
        .map(|i| format!("   Compiling crate-{i} v0.1.0\n"))
        .chain([
            "test wrap_keeps_wide_chars ... FAILED\n".to_string(),
            "thread 'wrap_keeps_wide_chars' panicked at crates/tui/src/lib.rs:1471\n".to_string(),
            "test result: FAILED. 3 passed; 1 failed\n".to_string(),
        ])
        .collect();
    tool(
        &mut state,
        "t2",
        "bash",
        json!({"command": "cargo test -p lynshen-tui wrap", "exit_code": 101, "stdout": failing}),
        false,
    );
    state.chat.push(crate::ChatLine::Assistant(
        "**失败原因：**`wrap_line` 按字符个数算宽度，中文这类双宽字符被算成 1 列，换行时超出一列，断言就失败了。改成按显示宽度计算：".to_string(),
    ));
    tool(
        &mut state,
        "t3",
        "edit",
        json!({"diff": "diff --git a/crates/tui/src/lib.rs b/crates/tui/src/lib.rs\n@@ -1468,4 +1468,4 @@\n     for ch in segment.chars() {\n-        let w = 1;\n+        let w = ch.width().unwrap_or(0);\n         flush(&mut current, output);\n"}),
        false,
    );
    tool(
        &mut state,
        "t4",
        "bash",
        json!({"command": "cargo test -p lynshen-tui wrap", "running": true, "session_id": 2,
               "stdout": "running 4 tests\ntest wrap_keeps_wide_chars ... ok\n"}),
        true,
    );
    state.activity.kind = ActivityKind::Tool;
    state.activity.turn_started_at = Some(Instant::now() - Duration::from_secs(14));
    state.activity.phase_started_at = Some(Instant::now() - Duration::from_secs(2));
    state.activity.estimated_output_tokens = 2_100;
    state.current_context_tokens = 72_000;
    (state, input)
}

fn approval() -> (TuiState, InputBuffer) {
    let mut state = base_state();
    state.chat.clear();
    let mut input = InputBuffer::default();
    state.apply_events(
        vec![AgentEvent::UserMessage(
            "把 target 里的增量缓存清掉，再完整编译一次".to_string(),
        )],
        &mut input,
    );
    state.confirm_pending_user();
    tool(
        &mut state,
        "t1",
        "bash",
        json!({"command": "du -sh target/debug/incremental", "exit_code": 0, "stdout": "4.1G\ttarget/debug/incremental\n"}),
        false,
    );
    let info = lynshen_agent_core::approval::ApprovalInfo {
        command: Some("rm -rf target/debug/incremental".to_string()),
        cwd: std::path::PathBuf::from("~/Code/LynShen-CLI"),
        justification: Some("清掉 4.1G 增量缓存".to_string()),
        prefix: Some("rm -rf".to_string()),
        ..Default::default()
    };
    state.apply_events(
        vec![AgentEvent::ApprovalRequest {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            summary: "rm -rf target/debug/incremental".to_string(),
            subagent_id: None,
            hunks: None,
            info,
        }],
        &mut input,
    );
    (state, input)
}

fn palette() -> (TuiState, InputBuffer) {
    let mut state = base_state();
    state.commands = [
        ("/help", None),
        ("/resume", None),
        ("/rewind", None),
        ("/review", Some("SKILL")),
        ("/tree", None),
        ("/approve", None),
        ("/permissions", None),
        ("/model", None),
    ]
    .iter()
    .map(|(command, marker)| CommandCandidate {
        command: command.to_string(),
        marker: marker.map(str::to_string),
        description: if marker.is_some() {
            "审查当前改动".to_string()
        } else {
            String::new()
        },
        highlight: Vec::new(),
    })
    .collect();
    let mut input = InputBuffer::default();
    input.push_text("/re");
    (state, input)
}

fn model_picker() -> (TuiState, InputBuffer) {
    let mut state = base_state();
    let mut input = InputBuffer::default();
    let model = |id: &str, context: u64, active: bool| ModelOptionView {
        model: id.to_string(),
        label: None,
        active,
        context_window: context,
        max_output_tokens: 64_000,
        reasoning_efforts: vec![
            "low".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "max".to_string(),
        ],
        input_modalities: Vec::new(),
    };
    state.apply_events(
        vec![AgentEvent::ModelView {
            models: vec![
                model("gpt-5.5", 400_000, true),
                model("claude-opus-4.5", 200_000, false),
                model("claude-sonnet-4.5", 1_000_000, false),
                model("claude-haiku-4.5", 200_000, false),
                model("kimi-k3", 256_000, false),
                model("deepseek-v4", 128_000, false),
            ],
            active_effort: "high".to_string(),
        }],
        &mut input,
    );
    if let Some(picker) = state.picker_view.as_mut() {
        picker.push_filter_str("cla");
    }
    (state, input)
}

fn resume_picker() -> (TuiState, InputBuffer) {
    let mut state = base_state();
    let mut input = InputBuffer::default();
    let session = |id: &str, label: &str, detail: &str| SessionListItemView {
        id: id.to_string(),
        label: label.to_string(),
        detail: detail.to_string(),
        active: false,
    };
    state.apply_events(
        vec![AgentEvent::ResumeView(vec![
            session("s1", "修复 wrap_line 宽字符测试", "2 分钟前 · 14 轮"),
            session("s2", "给 /usage 加上套餐重置时间", "今天 10:42 · 31 轮"),
            session("s3", "daemon 断线后自动重连", "昨天 21:40 · 86 轮"),
            session("s4", "ACP 会话恢复时保留工具调用", "11月22日 · 9 轮"),
            session("s5", "npm 包装时带上 Windows 版本", "11月20日 · 22 轮"),
        ])],
        &mut input,
    );
    (state, input)
}

fn help() -> (TuiState, InputBuffer) {
    let mut state = base_state();
    state.show_help = true;
    (state, InputBuffer::default())
}
