use super::*;
use crate::markdown::{md_code, render_markdown};
use crate::tool_preview::{tool_view, ToolState};
use lynshen_agent_core::{ModelOptionView, SessionListItemView, TreeNodeView};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

fn markdown_text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn preview_text(lines: &[UiLine]) -> String {
    lines
        .iter()
        .map(UiLine::plain)
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_modifier(line: &Line<'static>, modifier: Modifier) -> bool {
    line.spans
        .iter()
        .any(|span| span.style.add_modifier.contains(modifier))
}

fn input_lines(text: &str) -> Vec<UiLine> {
    let mut input = crate::input::InputBuffer::default();
    input.push_text(text);
    input.render(true)
}

#[derive(Default)]
struct TestRuntime {
    submitted: Vec<String>,
    commands: Vec<String>,
    interrupts: usize,
    queued: Vec<String>,
}

impl TuiRuntime for TestRuntime {
    fn startup_events(&self) -> Vec<AgentEvent> {
        Vec::new()
    }

    fn model_status_event(&self) -> AgentEvent {
        AgentEvent::Status("ready".to_string())
    }

    fn submit_user_message(&mut self, message: String) -> Vec<AgentEvent> {
        self.submitted.push(message.clone());
        vec![AgentEvent::UserMessage(message)]
    }

    fn interrupt(&mut self) -> Vec<AgentEvent> {
        self.interrupts += 1;
        vec![AgentEvent::Status("interrupted".to_string())]
    }

    fn unqueue_last(&mut self) -> Vec<AgentEvent> {
        let Some(content) = self.queued.pop() else {
            return Vec::new();
        };
        vec![
            AgentEvent::PendingMessages(self.queued.clone()),
            AgentEvent::Unqueued {
                content,
                images: Vec::new(),
            },
        ]
    }

    fn handle_command(&mut self, input: &str) -> (bool, Vec<AgentEvent>) {
        self.commands.push(input.to_string());
        if let Some(mode) = input.strip_prefix("/permissions ") {
            return (
                false,
                vec![AgentEvent::ApprovalMode {
                    mode: mode.to_string(),
                }],
            );
        }
        (false, Vec::new())
    }

    fn poll_events(&mut self) -> Vec<AgentEvent> {
        Vec::new()
    }
}

#[test]
fn paste_normalizes_newlines_without_submitting() {
    let mut app = TuiApp::new(TestRuntime::default());

    app.handle_paste("hello\r\nworld");

    assert_eq!(app.input.text(), "hello\nworld");
    assert!(app.runtime.submitted.is_empty());
}

#[test]
fn modified_enter_inserts_newline_and_plain_enter_submits_once() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();

    app.handle_key_at(KeyCode::Char('a'), KeyModifiers::empty(), now);
    app.handle_key_at(
        KeyCode::Enter,
        KeyModifiers::SHIFT,
        now + PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(1),
    );
    app.handle_key_at(
        KeyCode::Char('b'),
        KeyModifiers::empty(),
        now + PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(2),
    );
    app.handle_key_at(
        KeyCode::Enter,
        KeyModifiers::empty(),
        now + PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(11),
    );

    assert_eq!(app.runtime.submitted, vec!["a\nb".to_string()]);
}

#[test]
fn ctrl_enter_inserts_newline() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();

    app.handle_key_at(KeyCode::Char('a'), KeyModifiers::empty(), now);
    app.handle_key_at(
        KeyCode::Enter,
        KeyModifiers::CONTROL,
        now + Duration::from_millis(1),
    );
    app.handle_key_at(
        KeyCode::Char('b'),
        KeyModifiers::empty(),
        now + Duration::from_millis(2),
    );
    app.flush_paste_burst_if_due(now + PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(3));

    assert_eq!(app.input.text(), "a\nb");
    assert!(app.runtime.submitted.is_empty());
}

#[test]
fn paste_burst_keeps_multiline_text_in_input() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();

    for (index, ch) in "hello".chars().enumerate() {
        app.handle_key_at(
            KeyCode::Char(ch),
            KeyModifiers::empty(),
            now + Duration::from_millis(index as u64),
        );
    }
    app.handle_key_at(
        KeyCode::Enter,
        KeyModifiers::empty(),
        now + Duration::from_millis(5),
    );
    for (index, ch) in "world".chars().enumerate() {
        app.handle_key_at(
            KeyCode::Char(ch),
            KeyModifiers::empty(),
            now + Duration::from_millis(6 + index as u64),
        );
    }

    assert!(app.runtime.submitted.is_empty());
    app.flush_paste_burst_if_due(now + PASTE_BURST_IDLE_TIMEOUT + Duration::from_millis(20));

    assert_eq!(app.input.text(), "hello\nworld");
    assert!(app.runtime.submitted.is_empty());
}

#[test]
fn paste_burst_large_text_uses_placeholder() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();
    let pasted = "x".repeat(PASTE_PLACEHOLDER_CHARS + 1);

    for (index, ch) in pasted.chars().enumerate() {
        app.handle_key_at(
            KeyCode::Char(ch),
            KeyModifiers::empty(),
            now + Duration::from_millis(index as u64),
        );
    }
    app.flush_paste_burst_if_due(
        now + Duration::from_millis(pasted.len() as u64) + PASTE_BURST_IDLE_TIMEOUT,
    );

    assert_eq!(
        app.input.display_text(),
        format!("[已粘贴 {} 字]", PASTE_PLACEHOLDER_CHARS + 1)
    );
    assert_eq!(app.input.text(), pasted);
}

#[test]
fn paste_burst_render_tick_skips_until_pending_char_flushes() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();
    let mut frames = FrameScheduler {
        next_frame_at: None,
    };

    app.handle_key_at(KeyCode::Char('a'), KeyModifiers::empty(), now);

    assert_eq!(app.input.text(), "");
    assert!(app.paste_burst.is_active());
    assert!(app.handle_paste_burst_render_tick(now, &mut frames));
    assert_eq!(app.input.text(), "");
    assert_eq!(frames.next_frame_at, Some(now + paste_burst_render_delay()));

    assert!(app.handle_paste_burst_render_tick(now + paste_burst_render_delay(), &mut frames));
    assert_eq!(app.input.text(), "a");
    assert_eq!(frames.next_frame_at, Some(now + paste_burst_render_delay()));
    assert!(!app.handle_paste_burst_render_tick(now + paste_burst_render_delay(), &mut frames));
}

#[test]
fn single_typed_char_flushes_after_burst_window() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();

    app.handle_key_at(KeyCode::Char('a'), KeyModifiers::empty(), now);
    assert_eq!(app.input.text(), "");

    app.flush_paste_burst_if_due(now + PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(1));

    assert_eq!(app.input.text(), "a");
}

#[test]
fn esc_interrupts_active_turn_and_clears_draft_when_idle() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();

    app.input.push_char('x');
    app.apply_events(vec![AgentEvent::Connecting]);

    app.handle_key_at(KeyCode::Esc, KeyModifiers::empty(), now);
    assert_eq!(app.runtime.interrupts, 1);
    // The draft survives an interrupt; the turn is over.
    assert_eq!(app.input.text(), "x");
    assert!(!app.state.activity.is_active());

    app.handle_key_at(KeyCode::Esc, KeyModifiers::empty(), now);
    assert_eq!(app.runtime.interrupts, 1);
    assert_eq!(app.input.text(), "");
}

#[test]
fn up_on_an_empty_input_takes_back_the_last_queued_message() {
    let mut app = TuiApp::new(TestRuntime {
        queued: vec!["first".to_string(), "second".to_string()],
        ..TestRuntime::default()
    });
    let now = Instant::now();
    app.apply_events(vec![AgentEvent::PendingMessages(vec![
        "first".to_string(),
        "second".to_string(),
    ])]);

    // With a draft, Up moves in the draft; the queue is left alone.
    app.input.push_char('x');
    app.handle_key_at(KeyCode::Up, KeyModifiers::empty(), now);
    assert_eq!(app.runtime.queued.len(), 2);

    app.input.clear();
    app.handle_key_at(KeyCode::Up, KeyModifiers::empty(), now);
    assert_eq!(app.input.text(), "second");
    assert_eq!(app.state.pending_messages, ["first"]);
}

#[test]
fn input_renders_multiple_lines() {
    let document = UiBuilder::new()
        .input(&input_lines("one\ntwo"), None)
        .finish();

    assert_eq!(document.controls.len(), 2);
    assert_eq!(document.controls[0].kind, UiKind::Input);
    assert_eq!(document.controls[0].plain(), "  › one");
    assert_eq!(document.controls[1].kind, UiKind::Input);
    assert_eq!(document.controls[1].plain(), "    two");
}

#[test]
fn single_line_input_renders_one_prompt_row() {
    let document = UiBuilder::new()
        .input(&input_lines("hello"), Some("unused"))
        .finish();

    assert_eq!(document.controls.len(), 1);
    assert_eq!(document.controls[0].kind, UiKind::Input);
    assert_eq!(document.controls[0].plain(), "  › hello");
}

#[test]
fn empty_input_shows_the_placeholder_after_the_caret() {
    let document = UiBuilder::new()
        .input(&input_lines(""), Some("描述任务"))
        .finish();

    assert_eq!(document.controls[0].plain(), "  › 描述任务");
    assert_eq!(document.controls[0].cursor, Some(4));
    assert!(document.controls[0]
        .line
        .spans
        .iter()
        .any(|span| span.content == "描述任务" && span.style == crate::theme::faint()));
}

#[test]
fn cursor_row_is_relative_to_whole_frame() {
    let frame = RenderedFrame::build(
        &UiDocument {
            history: vec![
                UiLine::new(UiKind::User, "hello"),
                UiLine::new(UiKind::Assistant, "world"),
            ],
            rendered_history_lines: None,
            controls: vec![{
                let mut line = UiLine::new(UiKind::Input, "› prompt");
                line.cursor = Some(8);
                line
            }],
            reset_screen: false,
        },
        80,
    );

    let cursor = frame.cursor.expect("cursor marker should be found");
    assert_eq!(cursor.row, 3);
    assert_eq!(cursor.column, 8);
}

#[test]
fn command_palette_renders_below_input_with_descriptions() {
    let document = UiBuilder::new()
        .input(&input_lines("/"), None)
        .candidates(
            &[
                CommandCandidate {
                    command: "/help".to_string(),
                    highlight: vec![0],
                    ..CommandCandidate::default()
                },
                CommandCandidate {
                    command: "/review".to_string(),
                    marker: Some("SKILL".to_string()),
                    description: "Review the diff".to_string(),
                    highlight: vec![0],
                },
            ],
            1,
            80,
        )
        .finish();

    assert_eq!(document.controls[0].plain(), "  › /");
    assert_eq!(document.controls[0].cursor, Some(5));
    assert_eq!(document.controls[1].kind, UiKind::Candidate);
    assert!(document.controls[1].plain().starts_with("    /help"));
    assert!(document.controls[1].plain().contains("查看所有命令"));
    assert_eq!(document.controls[2].kind, UiKind::CandidateSelected);
    let selected = document.controls[2].plain();
    assert!(selected.starts_with("  › /review"), "{selected}");
    // Skills keep the engine's description and carry their tag.
    assert!(selected.contains("Review the diff"), "{selected}");
    assert!(selected.ends_with("技能"), "{selected}");
    // The keys follow the rows.
    assert!(document
        .controls
        .last()
        .unwrap()
        .plain()
        .contains("tab 补全"));
}

#[test]
fn slash_query_matches_commands_fuzzily_and_highlights() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.commands = ["/tree", "/resume", "/approve", "/rewind", "/permissions"]
        .iter()
        .map(|command| CommandCandidate {
            command: command.to_string(),
            ..CommandCandidate::default()
        })
        .collect();
    app.input.push_text("/re");
    let rows = app.state.completion_rows(&app.input);
    let names: Vec<&str> = rows.iter().map(|row| row.command.as_str()).collect();
    assert_eq!(names, vec!["/resume", "/rewind", "/tree", "/approve"]);
    assert_eq!(rows[0].highlight, vec![0, 1, 2]);

    // Enter completes the top match.
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), Instant::now());
    assert_eq!(app.input.text(), "/resume ");
}

#[test]
fn model_and_context_render_below_input_without_ready_status() {
    let document = UiBuilder::new()
        .input(&input_lines("hello"), None)
        .bottom_status(
            BottomStatus {
                provider: "openai",
                model: "gpt-5",
                reasoning_effort: "medium",
                approval_mode: "auto-edit",
                git: None,
                context_tokens: 12_345,
                context_window: 400_000,
                cost: 0.0,
                balance: Some("¥128.40"),
            },
            80,
        )
        .finish();

    assert_eq!(document.controls.len(), 2);
    assert_eq!(document.controls[0].plain(), "  › hello");
    assert_eq!(document.controls[0].cursor, Some(9));
    let status = document.controls[1].plain();
    assert!(status.starts_with("  gpt-5 中   自动改文件"), "{status}");
    assert!(status.ends_with("上下文 3% · ¥128.40  "), "{status}");
    assert!(!status.contains("ready"));
    assert_eq!(document.controls[1].line.width(), 80);
}

#[test]
fn input_line_pads_to_frame_width() {
    let document = UiBuilder::new().input(&input_lines("hi"), None).finish();

    let frame = RenderedFrame::build(&document, 40);
    let input_line = frame
        .lines
        .iter()
        .find(|line| line.contains("› hi"))
        .expect("input line should render");

    // Projected to the frame, the composer row fills the terminal width.
    assert_eq!(UnicodeWidthStr::width(input_line.as_str()), 40);
}

#[test]
fn native_cursor_tracks_the_prompt_row() {
    let document = UiBuilder::new().input(&input_lines("hello"), None).finish();

    let frame = RenderedFrame::build(&document, 40);
    let cursor = frame.cursor.expect("cursor marker should be found");

    assert_eq!(frame.lines.len(), 1);
    assert!(frame.lines[cursor.row].starts_with("  › hello"));
    assert_eq!(cursor.row, 0);
    assert_eq!(cursor.column, 4 + "hello".len());
}

#[test]
fn progress_renders_above_input() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![AgentEvent::Connecting]);

    let document = app.build_document(80, Instant::now());
    let progress_index = document
        .controls
        .iter()
        .position(|line| line.plain().contains("连接中"))
        .expect("progress line should render");
    let input_index = document
        .controls
        .iter()
        .position(|line| line.kind == UiKind::Input)
        .expect("input line should render");

    assert!(progress_index < input_index);
}

fn user_line_color(app: &mut TuiApp<TestRuntime>, text: &str) -> Option<Color> {
    let document = app.build_document(80, Instant::now());
    document
        .rendered_history_lines
        .expect("state renders history")
        .iter()
        .filter(|line| line.kind == UiKind::User)
        .flat_map(|line| line.line.spans.iter())
        .find(|span| span.content.contains(text))
        .map(|span| span.style.fg)
        .expect("user message renders")
}

#[test]
fn sent_message_is_dim_until_the_request_connects() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::UserMessage("hello".to_string()),
        AgentEvent::Connecting,
    ]);
    assert_eq!(user_line_color(&mut app, "hello"), crate::theme::dim().fg);
    // The dim message stands in for the connecting indicator.
    let document = app.build_document(80, Instant::now());
    assert!(!document
        .controls
        .iter()
        .any(|line| line.plain().contains("连接中")));

    app.apply_events(vec![AgentEvent::ThinkingStart]);
    assert_eq!(user_line_color(&mut app, "hello"), None);
}

#[test]
fn a_failed_request_still_shows_the_sent_message_normally() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::UserMessage("hello".to_string()),
        AgentEvent::Connecting,
        AgentEvent::Error("connection refused".to_string()),
    ]);
    assert_eq!(user_line_color(&mut app, "hello"), None);
}

#[test]
fn colored_status_line_does_not_wrap_at_visible_width() {
    let document = UiBuilder::new()
        .input(&input_lines(""), None)
        .bottom_status(
            BottomStatus {
                provider: "lynshen",
                model: "claude-opus-4.7",
                reasoning_effort: "high",
                approval_mode: "manual",
                git: None,
                context_tokens: 1633,
                context_window: 400_000,
                cost: 0.0,
                balance: None,
            },
            64,
        )
        .finish();

    let frame = RenderedFrame::build(&document, 64);

    assert_eq!(frame.lines.len(), 2);
    assert!(frame.lines[1].contains("上下文 0%"), "{:?}", frame.lines[1]);
    assert_eq!(UnicodeWidthStr::width(frame.lines[1].as_str()), 64);
}

#[test]
fn narrow_status_line_drops_segments_in_steps() {
    let dirty = crate::git_bar::GitStatus {
        branch: "feat/tui".to_string(),
        dirty: true,
    };
    let status = |width| {
        UiBuilder::new()
            .bottom_status(
                BottomStatus {
                    provider: "lynshen",
                    model: "claude-opus-4.7",
                    reasoning_effort: "high",
                    approval_mode: "manual",
                    git: Some(&dirty),
                    context_tokens: 1633,
                    context_window: 400_000,
                    cost: 0.0,
                    balance: Some("¥128.40"),
                },
                width,
            )
            .finish()
            .controls
            .last()
            .unwrap()
            .plain()
    };
    let wide = status(100);
    assert!(
        wide.contains("feat/tui*") && wide.contains("手动批准"),
        "{wide}"
    );
    let narrow = status(44);
    assert!(!narrow.contains("feat/tui"), "{narrow}");
    assert!(narrow.contains("¥128.40"), "{narrow}");
    assert!(UnicodeWidthStr::width(narrow.as_str()) <= 44);
}

#[test]
fn startup_shows_the_mark_beside_the_session_lines() {
    let document = UiBuilder::new()
        .chat_with_width(
            &[ChatLine::Startup {
                version: "0.1.2".to_string(),
                cwd: "/srv/projects/lynshen".to_string(),
                model: "claude-opus-4-7".to_string(),
                context_window: 1_000_000,
            }],
            80,
        )
        .finish();
    let rows: Vec<String> = document.history.iter().map(UiLine::plain).collect();

    assert!(rows[0].starts_with("  ▄  ▐▌    LynShen  0.1.2"), "{rows:?}");
    assert!(
        rows[1].starts_with("▗▖▝▙ ▐▌    claude-opus-4-7 · 1M 上下文"),
        "{rows:?}"
    );
    assert!(
        rows[2].starts_with("▐▌ ▝▘▐▌    /srv/projects/lynshen"),
        "{rows:?}"
    );
    assert_eq!(rows[3].trim_end(), "▐▌   ▐▌");
    // The light is in the accent; the trunks keep the text colour.
    let light = document.history[0]
        .line
        .spans
        .iter()
        .find(|span| span.content == "▄")
        .unwrap();
    assert_eq!(light.style, crate::theme::accent());
    let name = document.history[0]
        .line
        .spans
        .iter()
        .find(|span| span.content == "LynShen")
        .unwrap();
    assert!(name.style.add_modifier.contains(Modifier::BOLD));
    assert!(
        rows[5].contains("/ 命令") && rows[5].contains("? 快捷键"),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|row| row.contains("llama")));
}

#[test]
fn startup_drops_the_mark_on_narrow_terminals() {
    let document = UiBuilder::new()
        .chat_with_width(
            &[ChatLine::Startup {
                version: "0.1.2".to_string(),
                cwd: "/srv/x".to_string(),
                model: "gpt-5.5".to_string(),
                context_window: 400_000,
            }],
            40,
        )
        .finish();
    let rows: Vec<String> = document.history.iter().map(UiLine::plain).collect();
    assert_eq!(rows[0], "LynShen  0.1.2");
    assert!(rows.iter().all(|row| !row.contains('▐')));
}

#[test]
fn startup_names_the_account_and_plan() {
    let welcome = crate::ui_builder::WelcomeInfo {
        provider: "lynshen".to_string(),
        model: "gpt-5.5".to_string(),
        effort: "high".to_string(),
        has_efforts: true,
        context_window: 400_000,
        account: AccountState::Ready(lynshen_agent_core::AccountSummary {
            email: "dev@example.com".to_string(),
            balance: Some("128.4".to_string()),
            currency: Some("CNY".to_string()),
            plan: Some("Pro".to_string()),
        }),
    };
    let document = UiBuilder::new()
        .welcome(welcome)
        .chat_with_width(
            &[ChatLine::Startup {
                version: "0.1.2".to_string(),
                cwd: "/srv/x".to_string(),
                model: "gpt-5.5".to_string(),
                context_window: 400_000,
            }],
            80,
        )
        .finish();
    let rows: Vec<String> = document.history.iter().map(UiLine::plain).collect();
    assert!(rows[1].ends_with("dev@example.com · Pro"), "{rows:?}");
    assert!(
        rows[2].contains("gpt-5.5 · 思考 高 · 400K 上下文"),
        "{rows:?}"
    );
    assert_eq!(
        AccountState::Ready(lynshen_agent_core::AccountSummary {
            email: String::new(),
            balance: Some("128.4".to_string()),
            currency: Some("CNY".to_string()),
            plan: None,
        })
        .balance()
        .as_deref(),
        Some("¥128.40")
    );
}

#[test]
fn startup_text_column_lines_up_when_projected() {
    let document = UiBuilder::new()
        .chat_with_width(
            &[ChatLine::Startup {
                version: "0.1.2".to_string(),
                cwd: "~/dev/projects/lynshen/LynShen-CLI".to_string(),
                model: "gpt-5.4".to_string(),
                context_window: 1_100_000,
            }],
            78,
        )
        .finish();

    let frame = RenderedFrame::build(&document, 80);
    let column = |line: &str, needle: &str| {
        let at = line.find(needle).expect(needle);
        UnicodeWidthStr::width(&line[..at])
    };
    assert_eq!(column(&frame.lines[0], "LynShen"), 13);
    assert_eq!(column(&frame.lines[1], "gpt-5.4"), 13);
    assert_eq!(column(&frame.lines[2], "~/dev"), 13);
}

#[test]
fn progress_shows_phase_elapsed_and_tokens() {
    let now = Instant::now();
    let mut activity = ActivityState::idle();
    activity.kind = ActivityKind::Connecting;
    activity.turn_started_at = Some(now - Duration::from_secs(75));
    activity.phase_started_at = Some(now - Duration::from_secs(2));

    let progress = activity.progress(now, 0).unwrap();
    assert_eq!(progress.phase, "连接中");
    assert_eq!(progress.detail, "1 分 15 秒");
    assert!(!progress.stalled);

    activity.kind = ActivityKind::Thinking;
    let progress = activity.progress(now, 2_400).unwrap();
    assert_eq!(progress.phase, "思考中");
    assert_eq!(progress.detail, "1 分 15 秒 · ↓ 2k tokens");
}

#[test]
fn connecting_event_then_thinking_event_switch_states() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![AgentEvent::Connecting]);
    assert!(matches!(app.state.activity.kind, ActivityKind::Connecting));
    app.apply_events(vec![AgentEvent::ThinkingStart]);
    assert!(matches!(app.state.activity.kind, ActivityKind::Thinking));
}

fn reasoning_entry(app: &TuiApp<TestRuntime>) -> Option<(String, bool)> {
    app.state.chat.iter().find_map(|line| match line {
        ChatLine::Reasoning {
            text, collapsed, ..
        } => Some((text.clone(), *collapsed)),
        _ => None,
    })
}

#[test]
fn reasoning_streams_into_transcript_then_collapses() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::Connecting,
        AgentEvent::ThinkingStart,
        AgentEvent::ReasoningDelta("Let me think".to_string()),
        AgentEvent::ReasoningDelta(" about it.".to_string()),
    ]);
    // Reasoning is a transcript message, streaming, not collapsed yet.
    assert_eq!(
        reasoning_entry(&app),
        Some(("Let me think about it.".to_string(), false))
    );
    assert!(matches!(app.state.activity.kind, ActivityKind::Thinking));

    app.apply_events(vec![AgentEvent::AssistantDelta("Answer".to_string())]);
    // Kept as a message, now collapsed.
    assert_eq!(
        reasoning_entry(&app),
        Some(("Let me think about it.".to_string(), true))
    );
    assert!(matches!(app.state.activity.kind, ActivityKind::Output));
}

#[test]
fn thinking_header_click_toggles_reasoning_block() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::ThinkingStart,
        AgentEvent::ReasoningDelta("deep thoughts".to_string()),
        AgentEvent::AssistantDelta("answer".to_string()),
    ]);
    let index = app
        .state
        .chat
        .iter()
        .position(|line| matches!(line, ChatLine::Reasoning { .. }))
        .expect("reasoning block");
    // Collapsed by the reply, with its thinking duration recorded.
    assert!(matches!(
        app.state.chat.get(index),
        Some(ChatLine::Reasoning {
            collapsed: true,
            duration_secs: Some(_),
            ..
        })
    ));
    app.state.toggle_item(index);
    assert!(matches!(
        app.state.chat.get(index),
        Some(ChatLine::Reasoning {
            collapsed: false,
            ..
        })
    ));
    app.state.toggle_item(index);
    assert!(matches!(
        app.state.chat.get(index),
        Some(ChatLine::Reasoning {
            collapsed: true,
            ..
        })
    ));
}

#[test]
fn reasoning_tokens_show_in_thinking_progress_not_transcript() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::ThinkingStart,
        AgentEvent::ReasoningDelta("thinking".to_string()),
        AgentEvent::Usage {
            input_tokens: 5,
            cached_input_tokens: 0,
            output_tokens: 2,
            reasoning_tokens: 88,
        },
    ]);
    assert_eq!(app.state.thinking_tokens, 88);
    let document = app.build_document(80, Instant::now());
    // Token count is in the progress line, not the transcript.
    assert!(document
        .controls
        .iter()
        .any(|line| line.plain().contains("思考中") && line.plain().contains("↓ 88 tokens")));
    assert!(!document
        .history
        .iter()
        .any(|line| line.plain().contains("88 tokens")));
}

#[test]
fn markdown_heading_renders_bold() {
    let lines = render_markdown("## Section title", usize::MAX);
    assert_eq!(markdown_text(&lines), vec!["Section title"]);
    assert!(lines[0]
        .spans
        .iter()
        .all(|span| span.style.add_modifier.contains(Modifier::BOLD)));
}

#[test]
fn markdown_bold_and_italic_render_inline() {
    let lines = render_markdown("a **bold** and *em* word", usize::MAX);
    assert_eq!(markdown_text(&lines), vec!["a bold and em word"]);
    let styled = |needle: &str| {
        lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == needle)
            .map(|span| span.style)
    };
    assert_eq!(styled("bold").unwrap().add_modifier, Modifier::BOLD);
    assert_eq!(styled("em").unwrap().add_modifier, Modifier::ITALIC);
}

#[test]
fn markdown_inline_code_recolors_and_restores_base() {
    let lines = render_markdown("run `a*b*c` now", usize::MAX);
    // Inline code is one span in the code tone.
    assert_eq!(markdown_text(&lines), vec!["run a*b*c now"]);
    let chip = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "a*b*c")
        .expect("code span");
    assert_eq!(chip.style.fg, md_code().fg);
}

#[test]
fn markdown_fenced_code_block_renders_verbatim_with_gutter() {
    let md = "before\n```rust\nlet x = **2**;\nfoo();\n```\nafter";
    let lines = render_markdown(md, usize::MAX);
    assert_eq!(
        markdown_text(&lines),
        vec!["before", "│ let x = **2**;", "│ foo();", "after"]
    );
    // A faint rule on the left; the code keeps the text colour and its
    // emphasis markers stay literal.
    assert_eq!(lines[1].spans[0].style, crate::theme::faint());
    assert_eq!(lines[1].spans[1].style.fg, None);
}

#[test]
fn markdown_unbalanced_markers_stay_literal() {
    let lines = render_markdown("2 * 3 = 6", usize::MAX);
    assert_eq!(markdown_text(&lines), vec!["2 * 3 = 6"]);
}

#[test]
fn markdown_table_renders_aligned_box() {
    let table = "| Name | Qty |\n|:-----|----:|\n| apple | 3 |\n| fig | 22 |";
    let lines = render_markdown(table, usize::MAX);
    let plain = markdown_text(&lines);

    assert_eq!(
        plain,
        vec![
            "┌───────┬─────┐".to_string(),
            "│ Name  │ Qty │".to_string(), // left-aligned header
            "├───────┼─────┤".to_string(),
            "│ apple │   3 │".to_string(), // Qty right-aligned
            "│ fig   │  22 │".to_string(),
            "└───────┴─────┘".to_string(),
        ]
    );
    // Header cells are bold.
    let header = lines[1]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "Name")
        .expect("header cell");
    assert!(header.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn markdown_table_is_capped_to_width() {
    let table = "| A | B |\n|---|---|\n| xxxxxxxxxx | yyyyyyyyyy |";
    let lines = render_markdown(table, 20);
    for line in &lines {
        assert!(line.width() <= 20, "line exceeds width: {}", line);
    }
}

#[test]
fn assistant_message_is_rendered_as_markdown() {
    let document = UiBuilder::new()
        .chat(&[ChatLine::Assistant("# Hi **there**".to_string())])
        .finish();
    assert!(document
        .history
        .iter()
        .any(|line| has_modifier(&line.line, Modifier::BOLD) && line.plain().contains("there")));
}

#[test]
fn thinking_tokens_reset_after_reply_completes() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::ThinkingStart,
        AgentEvent::ReasoningDelta("thinking".to_string()),
        AgentEvent::AssistantDelta("answer".to_string()),
        AgentEvent::Usage {
            input_tokens: 1,
            cached_input_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: 42,
        },
    ]);
    assert_eq!(app.state.thinking_tokens, 42);

    app.apply_events(vec![AgentEvent::Status("ready".to_string())]);
    // Progress token state is cleared; the reasoning message itself stays in chat.
    assert_eq!(app.state.thinking_tokens, 0);
    let document = app.build_document(80, Instant::now());
    assert!(!document
        .controls
        .iter()
        .any(|line| line.plain().contains("42 tokens")));
    assert!(reasoning_entry(&app).is_some());
}

#[test]
fn collapsed_reasoning_message_keeps_only_duration_header() {
    let document = UiBuilder::new()
        .chat(&[ChatLine::Reasoning {
            text: "l1\nl2\nl3\nl4\nl5".to_string(),
            collapsed: true,
            duration_secs: Some(2),
        }])
        .finish();
    let visible: Vec<&UiLine> = document
        .history
        .iter()
        .filter(|line| !line.is_blank())
        .collect();
    assert_eq!(visible.len(), 1);
    assert!(visible[0].plain().contains("∴ 思考了 2 秒"));
    // The header is the click target that expands the block again.
    assert_eq!(visible[0].click, Some(0));
}

#[test]
fn expanded_reasoning_message_renders_body_lines() {
    let document = UiBuilder::new()
        .chat(&[ChatLine::Reasoning {
            text: "l1\nl2".to_string(),
            collapsed: false,
            duration_secs: Some(90),
        }])
        .finish();
    let visible: Vec<String> = document
        .history
        .iter()
        .filter(|line| !line.is_blank())
        .map(|line| line.plain().trim().to_string())
        .collect();
    assert_eq!(visible, vec!["∴ 思考了 1 分 30 秒", "l1", "l2"]);
}

#[test]
fn compaction_events_set_progress_notices_and_context_meter() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![AgentEvent::ContextUsage {
        tokens: 900_000,
        tokenizer: "gpt-5".to_string(),
        cost: 0.0,
        breakdown: None,
    }]);
    assert_eq!(app.state.current_context_tokens, 900_000);

    app.apply_events(vec![AgentEvent::CompactionStart]);
    assert!(matches!(app.state.activity.kind, ActivityKind::Compacting));
    assert!(app
        .state
        .chat
        .iter()
        .any(|line| matches!(line, ChatLine::System(text) if text.contains("正在压缩"))));

    app.apply_events(vec![AgentEvent::CompactionProgress { output_tokens: 42 }]);
    assert_eq!(app.state.activity.estimated_output_tokens, 42);

    app.apply_events(vec![
        AgentEvent::CompactionEnd {
            summary: "summary".to_string(),
        },
        AgentEvent::ContextUsage {
            tokens: 25_000,
            tokenizer: "gpt-5".to_string(),
            cost: 0.0,
            breakdown: None,
        },
    ]);
    assert!(app
        .state
        .chat
        .iter()
        .any(|line| matches!(line, ChatLine::System(text) if text.contains("已压缩"))));
    assert_eq!(app.state.current_context_tokens, 25_000);
}

#[test]
fn output_progress_reports_a_stall() {
    let now = Instant::now();
    let mut activity = ActivityState::idle();
    activity.kind = ActivityKind::Output;
    activity.turn_started_at = Some(now - Duration::from_secs(20));
    activity.phase_started_at = Some(now - Duration::from_secs(20));
    activity.estimated_output_tokens = 12;

    activity.last_delta_at = Some(now);
    let fresh = activity.progress(now, 0).unwrap();
    assert_eq!(fresh.phase, "输出中");
    assert!(!fresh.stalled);

    activity.last_delta_at = Some(now - Duration::from_secs(9));
    let stalled = activity.progress(now, 0).unwrap();
    assert_eq!(stalled.phase, "等待响应 9 秒");
    assert!(stalled.stalled);
}

#[test]
fn spinner_frames_are_single_cells() {
    for tick in 0..24 {
        assert_eq!(UnicodeWidthStr::width(spinner_char(tick)), 1);
    }
    crate::theme::with_theme(
        crate::theme::Theme {
            rich_glyphs: false,
            ..crate::theme::Theme::new(crate::theme::ColorDepth::Ansi16, None)
        },
        || {
            for tick in 0..8 {
                assert!(spinner_char(tick).is_ascii());
            }
        },
    );
}

#[test]
fn long_generic_output_folds_until_expanded() {
    let output = (0..20)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tool = ChatLine::Tool {
        call_id: None,
        name: "custom".to_string(),
        output,
        running: false,
        expanded: false,
    };
    let folded = preview_text(
        &UiBuilder::new()
            .chat(std::slice::from_ref(&tool))
            .into_history(),
    );
    assert!(folded.contains("custom  line 0"), "{folded}");
    assert!(folded.contains("line 6"));
    assert!(!folded.contains("line 19"));
    assert!(folded.contains("⋯ 还有 13 行 · ctrl+o 展开"), "{folded}");

    let open = preview_text(
        &UiBuilder::new()
            .expand_tools(true)
            .chat(&[tool])
            .into_history(),
    );
    assert!(open.contains("line 19"));
    assert!(!open.contains("ctrl+o"));
}

#[test]
fn shell_output_keeps_its_tail_when_folded() {
    let stdout = (0..10)
        .map(|index| format!("step {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let output =
        serde_json::json!({"command": "make", "exit_code": 2, "stdout": stdout}).to_string();
    let history = UiBuilder::new()
        .chat(&[ChatLine::Tool {
            call_id: None,
            name: "bash".to_string(),
            output,
            running: false,
            expanded: false,
        }])
        .into_history();
    let text = preview_text(&history);
    assert!(text.starts_with("● 运行  make  退出码 2"), "{text}");
    assert!(text.contains("│ ⋯ 前 6 行已折叠 · ctrl+o 展开"), "{text}");
    assert!(text.contains("└ step 9"), "{text}");
    assert!(!text.contains("step 5"));
    // A failed call's dot is in the error colour, and the header is the
    // click target.
    assert_eq!(history[0].line.spans[0].style, crate::theme::err());
    assert_eq!(history[0].click, Some(0));
}

#[test]
fn ctrl_o_expands_every_tool_block() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![AgentEvent::ToolOutput {
        call_id: "c1".to_string(),
        name: "custom".to_string(),
        output: (0..30).map(|i| format!("row {i}\n")).collect(),
        is_error: false,
    }]);
    let history =
        |app: &mut TuiApp<TestRuntime>| preview_text(&app.state.rendered_history_lines(80));
    assert!(!history(&mut app).contains("row 29"));
    app.handle_key_at(KeyCode::Char('o'), KeyModifiers::CONTROL, Instant::now());
    assert!(history(&mut app).contains("row 29"));
    app.handle_key_at(KeyCode::Char('o'), KeyModifiers::CONTROL, Instant::now());
    assert!(!history(&mut app).contains("row 29"));
}

#[test]
fn tool_view_prefers_diff_field() {
    let output = serde_json::json!({
        "stdout": "raw",
        "diff": "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n"
    })
    .to_string();

    let view = tool_view("str_replace", &output, false);
    let body = preview_text(&view.body);

    assert_eq!(view.arg, "a");
    assert!(body.contains("    1 - old"), "{body}");
    assert!(body.contains("    1 + new"), "{body}");
    assert!(!body.contains("diff --git a/a b/a"));
    assert!(!body.contains("raw"));
}

#[test]
fn tool_view_appends_rejected_hunk_count_after_partial_approval() {
    let output = serde_json::json!({
        "diff": "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n",
        "applied_hunks": ["f0h2"],
        "rejected_hunks": ["f0h1", "f0h3"],
    })
    .to_string();

    let body = preview_text(&tool_view("str_replace", &output, false).body);

    // The diff shows only the applied hunks, plus a rejected-count line.
    assert!(body.contains("    1 + new"));
    assert!(body.contains("你拒绝了 2 处改动（没有应用）"));

    // A whole-call approval (nothing rejected) adds no such line.
    let full = serde_json::json!({
        "diff": "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n",
        "applied_hunks": ["f0h1"],
        "rejected_hunks": [],
    })
    .to_string();
    assert!(!preview_text(&tool_view("str_replace", &full, false).body).contains("拒绝"));
}

#[test]
fn large_diff_counts_both_sides_and_shows_all_when_expanded() {
    let removals = (0..30)
        .map(|index| format!("-old line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let diff = format!(
            "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1,30 +1,2 @@\n{removals}\n+new important line\n+another important line\n"
        );
    let output = serde_json::json!({ "stdout": "raw", "diff": diff }).to_string();

    let view = tool_view("edit", &output, false);
    let extra: String = view
        .extra
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(view.arg, "README.md");
    assert_eq!(extra, "+2 −30");
    let body = preview_text(&view.body);
    assert!(body.contains("    1 - old line 0"));
    assert!(body.contains("   30 - old line 29"));
    assert!(body.contains("    1 + new important line"));
    assert!(body.contains("    2 + another important line"));
    assert!(!body.contains("--- a/README.md"));

    let tool = ChatLine::Tool {
        call_id: None,
        name: "edit".to_string(),
        output,
        running: false,
        expanded: true,
    };
    let open = preview_text(&UiBuilder::new().chat(&[tool]).into_history());
    assert!(open.contains("another important line"));
}

#[test]
fn tool_view_projects_read_and_ls() {
    let read = serde_json::json!({
        "path": "C:\\repo\\src\\main.rs",
        "offset": 5,
        "lines_read": 12,
        "content": "hidden"
    })
    .to_string();
    let ls = serde_json::json!({ "path": "C:\\repo\\src", "entries": ["main.rs"] }).to_string();

    let read_view = tool_view("read", &read, false);
    let extra: String = read_view
        .extra
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(read_view.verb, "读取");
    assert_eq!(read_view.arg, "C:\\repo\\src\\main.rs");
    assert_eq!(extra, "5–16 行");
    assert!(read_view.body.is_empty());
    let ls_view = tool_view("ls", &ls, false);
    assert_eq!(ls_view.verb, "列出");
    assert_eq!(ls_view.arg, "C:\\repo\\src");
}

#[test]
fn generate_image_view_lists_saved_paths() {
    let output = serde_json::json!({
        "paths": ["images/fox.png", "images/fox-2.png"],
        "model": "gpt-image-2",
        "revised_prompt": "a fox"
    })
    .to_string();
    let view = tool_view("generate_image", &output, false);
    assert_eq!(view.verb, "生成图片");
    assert_eq!(view.arg, "images/fox.png");
    assert_eq!(preview_text(&view.body), "images/fox-2.png");
}

#[test]
fn tool_error_json_shows_message_not_raw_json() {
    let view = tool_view(
        "write_stdin",
        r#"{"error":"shell session not found","session_id":0}"#,
        false,
    );
    let extra: String = view
        .extra
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(view.state, ToolState::Failed);
    assert_eq!(extra, "shell session not found");
    // The header names the real tool instead of a generic "Tool".
    assert_eq!(view.verb, "输入");
    // MCP tools show server/tool rather than the mcp__ prefix form.
    assert_eq!(
        tool_view("mcp__fs__read_file", "", false).verb,
        "fs/read_file"
    );
}

#[test]
fn running_session_shows_still_running_not_exit_code() {
    let output = serde_json::json!({
        "command": "cargo test",
        "session_id": 3,
        "running": true,
        "stdout": "compiling…",
        "stderr": ""
    })
    .to_string();

    let view = tool_view("write_stdin", &output, false);
    let extra: String = view
        .extra
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(view.arg, "cargo test");
    assert_eq!(extra, "后台运行中");
    assert_ne!(view.state, ToolState::Failed);
}

#[test]
fn tool_view_projects_bash_latest_logs() {
    let output = serde_json::json!({
        "command": "cargo test",
        "exit_code": 0,
        "stdout": "one\ntwo\nthree\n",
        "stderr": "",
        "timed_out": false
    })
    .to_string();

    let view = tool_view("bash", &output, false);
    assert_eq!(view.arg, "cargo test");
    assert!(view.extra.is_empty());
    assert_eq!(preview_text(&view.body), "one\ntwo\nthree");
}

#[test]
fn chat_history_separates_turns_with_blank_lines() {
    let document = UiBuilder::new()
        .chat(&[
            ChatLine::User("first".to_string()),
            ChatLine::Assistant("second".to_string()),
        ])
        .finish();

    let kinds: Vec<UiKind> = document.history.iter().map(|line| line.kind).collect();
    let blank = document
        .history
        .iter()
        .position(|line| line.is_blank())
        .expect("a blank line separates the two turns");
    assert_eq!(kinds[..blank], [UiKind::User]);
    assert_eq!(kinds[blank + 1..], [UiKind::Assistant]);
    // No leading blank before the first block.
    assert!(!document.history[0].is_blank());
}

#[test]
fn tool_block_colors_diff_lines() {
    let document = UiBuilder::new()
        .chat(&[ChatLine::Tool {
            call_id: None,
            name: "edit".to_string(),
            output: serde_json::json!({
                "diff": "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n"
            })
            .to_string(),
            running: false,
            expanded: false,
        }])
        .finish();

    let removed = document
        .history
        .iter()
        .find(|line| line.kind == UiKind::DiffRemove)
        .expect("removed line");
    assert!(removed.plain().contains("1 - old"));
    assert!(document
        .history
        .iter()
        .any(|line| line.kind == UiKind::DiffAdd && line.plain().contains("1 + new")));
    // The line number stays unfilled; the sign and text carry the fill.
    let fill = crate::theme::surface(crate::theme::Surface::DelLine);
    assert!(fill.is_some());
    assert!(removed
        .line
        .spans
        .iter()
        .any(|span| span.content.contains("1 ") && span.style.bg.is_none()));
    assert!(removed.line.spans.iter().any(|span| span.style.bg == fill));
}

#[test]
fn projection_indents_history_and_keeps_controls_flush() {
    let document = UiBuilder::new()
        .chat(&[
            ChatLine::Assistant("answer".to_string()),
            ChatLine::Tool {
                call_id: None,
                name: "edit".to_string(),
                output: serde_json::json!({
                    "diff": "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n"
                })
                .to_string(),
                running: false,
                expanded: false,
            },
        ])
        .input(&input_lines("hello"), None)
        .bottom_status(
            BottomStatus {
                provider: "lynshen",
                model: "gpt-5",
                reasoning_effort: "medium",
                approval_mode: "manual",
                git: None,
                context_tokens: 10,
                context_window: 100,
                cost: 0.0,
                balance: None,
            },
            40,
        )
        .finish();

    let frame = RenderedFrame::build(&document, 40);
    let plain = frame.lines.clone();

    assert!(plain.iter().any(|line| line.starts_with("  answer")));
    assert!(plain.iter().any(|line| line.is_empty()));
    assert!(
        plain
            .iter()
            .any(|line| line.starts_with("  ● 编辑  a  +1 −1")),
        "{plain:?}"
    );
    assert!(plain.iter().any(|line| line.contains("1 - old")));
    assert!(plain.iter().any(|line| line.starts_with("  › hello")));
    assert!(plain.iter().any(|line| line.contains("上下文 10%")));
}

#[test]
fn projection_keeps_full_history_for_in_app_scroll() {
    let document = UiBuilder::new().finish_with_history_and_input(20);

    let frame = RenderedFrame::build(&document, 80);
    let output = frame.lines.join("\n");

    assert!(output.contains("line 0"));
    assert!(output.contains("line 19"));
    assert_eq!(frame.lines.len(), 22);
}

#[test]
fn assistant_delta_streams_into_transcript() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::Connecting,
        AgentEvent::AssistantDelta("streaming".to_string()),
    ]);
    // The in-flight reply is already a transcript message, not a live layer.
    assert!(app
        .state
        .chat
        .iter()
        .any(|line| matches!(line, ChatLine::Assistant(text) if text == "streaming")));
    let document = app.build_document(80, Instant::now());
    let history = document
        .rendered_history_lines
        .expect("history lines should be projected");
    assert!(history
        .iter()
        .any(|line| line.plain().contains("streaming")));
    // Nothing leaks into the control region above the composer.
    assert!(!document
        .controls
        .iter()
        .any(|line| line.plain().contains("streaming")));
}

#[test]
fn approval_wait_does_not_extend_thinking_duration() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![
        AgentEvent::ThinkingStart,
        AgentEvent::ReasoningDelta("plan".to_string()),
        AgentEvent::ApprovalRequest {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            summary: "run it".to_string(),
            subagent_id: None,
            hunks: None,
            info: Default::default(),
        },
    ]);
    let index = app
        .state
        .chat
        .iter()
        .position(|line| matches!(line, ChatLine::Reasoning { .. }))
        .expect("reasoning block");
    // The reasoning block collapses when the approval prompt appears, so the
    // user's deliberation time is not counted as thinking.
    assert!(matches!(
        app.state.chat.get(index),
        Some(ChatLine::Reasoning {
            collapsed: true,
            duration_secs: Some(_),
            ..
        })
    ));
    let waited = app
        .state
        .chat
        .iter()
        .find_map(|line| match line {
            ChatLine::Reasoning { duration_secs, .. } => *duration_secs,
            _ => None,
        })
        .unwrap();
    assert!(waited < 5, "approval wait leaked into thinking time");
}

#[test]
fn cursor_row_accounts_for_full_history() {
    let document = UiBuilder::new().finish_with_history_and_input(20);

    let frame = RenderedFrame::build(&document, 80);
    let cursor = frame.cursor.expect("cursor marker should be found");

    assert_eq!(frame.lines.len(), 22);
    assert_eq!(cursor.row, 21);
    assert_eq!(cursor.column, 4);
}

#[test]
fn checkout_tree_enter_maps_to_checkout_command() {
    let mut tree = PickerState::checkout(vec![TreeNodeView {
        id: "e3".to_string(),
        parent_id: None,
        label: "selected prompt".to_string(),
        active: true,
    }]);

    assert_eq!(tree.selected_command().as_deref(), Some("/checkout e3"));

    tree.begin_tree_prompt(TreePromptAction::Fork);
    for ch in "feature".chars() {
        tree.push_prompt_char(ch);
    }
    assert_eq!(tree.take_prompt_command().as_deref(), Some("/fork feature"));
}

#[test]
fn checkout_tree_fork_and_delete_are_interactive_commands() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();
    app.state.picker_view = Some(PickerState::checkout(vec![TreeNodeView {
        id: "e3".to_string(),
        parent_id: None,
        label: "selected prompt".to_string(),
        active: true,
    }]));

    app.handle_key_at(KeyCode::Char('f'), KeyModifiers::empty(), now);
    for (index, ch) in "feature".chars().enumerate() {
        app.handle_key_at(
            KeyCode::Char(ch),
            KeyModifiers::empty(),
            now + Duration::from_millis(index as u64 + 1),
        );
    }
    app.handle_key_at(
        KeyCode::Enter,
        KeyModifiers::empty(),
        now + Duration::from_millis(20),
    );

    app.handle_key_at(
        KeyCode::Delete,
        KeyModifiers::empty(),
        now + Duration::from_millis(30),
    );
    for (index, ch) in "feature".chars().enumerate() {
        app.handle_key_at(
            KeyCode::Char(ch),
            KeyModifiers::empty(),
            now + Duration::from_millis(index as u64 + 31),
        );
    }
    app.handle_key_at(
        KeyCode::Enter,
        KeyModifiers::empty(),
        now + Duration::from_millis(50),
    );

    assert_eq!(
        app.runtime.commands,
        vec!["/fork feature".to_string(), "/delete feature".to_string()]
    );
}

#[test]
fn checkout_tree_expands_sparse_history_fully() {
    // A mostly-linear history has little branching, so it expands all the way.
    let tree = PickerState::checkout(vec![
        TreeNodeView {
            id: "e1".to_string(),
            parent_id: None,
            label: "first".to_string(),
            active: false,
        },
        TreeNodeView {
            id: "e2".to_string(),
            parent_id: Some("e1".to_string()),
            label: "second".to_string(),
            active: false,
        },
        TreeNodeView {
            id: "e3".to_string(),
            parent_id: Some("e2".to_string()),
            label: "third".to_string(),
            active: false,
        },
    ]);

    assert_eq!(
        tree.rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["e1", "e2", "e3"]
    );
    assert!(tree.rows[0].prefix.contains("──"));
}

fn wide_tree_nodes() -> Vec<TreeNodeView> {
    let mut nodes = vec![TreeNodeView {
        id: "e1".to_string(),
        parent_id: None,
        label: "root".to_string(),
        active: false,
    }];
    for index in 2..=20 {
        nodes.push(TreeNodeView {
            id: format!("e{index}"),
            parent_id: Some("e1".to_string()),
            label: format!("child {index}"),
            active: false,
        });
    }
    nodes.push(TreeNodeView {
        id: "c1".to_string(),
        parent_id: Some("e2".to_string()),
        label: "grandchild".to_string(),
        active: false,
    });
    nodes
}

#[test]
fn checkout_tree_limits_expansion_when_branching_is_wide() {
    // Wide branching fills the row budget early, so deeper levels stay collapsed.
    let tree = PickerState::checkout(wide_tree_nodes());

    assert!(!tree.rows.iter().any(|row| row.id == "c1"));
    assert_eq!(tree.rows.len(), 20); // root + 19 children, grandchild hidden
}

#[test]
fn fill_input_event_populates_input_box() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.input.push_text("stale");
    app.apply_events(vec![AgentEvent::FillInput("resend this".to_string())]);
    assert_eq!(app.input.text(), "resend this");
}

#[test]
fn checkout_tree_marks_rows_with_children_as_folds() {
    // Wide tree: the root is expanded (▾); a child with hidden descendants
    // stays collapsed (▸).
    let tree = PickerState::checkout(wide_tree_nodes());
    let document = UiBuilder::new().picker(Some(&tree), 80).finish();

    assert!(document
        .controls
        .iter()
        .any(|line| line.plain().contains("▾") && line.plain().contains("root")));
    assert!(document
        .controls
        .iter()
        .any(|line| line.plain().contains("▸") && line.plain().contains("child 2")));
}

#[test]
fn checkout_tree_marks_head_and_active_path() {
    // Linear history e1 -> e2 -> e3, with e3 as the current HEAD.
    let nodes = vec![
        TreeNodeView {
            id: "e1".to_string(),
            parent_id: None,
            label: "first".to_string(),
            active: false,
        },
        TreeNodeView {
            id: "e2".to_string(),
            parent_id: Some("e1".to_string()),
            label: "second".to_string(),
            active: false,
        },
        TreeNodeView {
            id: "e3".to_string(),
            parent_id: Some("e2".to_string()),
            label: "third".to_string(),
            active: true,
        },
    ];
    let tree = PickerState::checkout(nodes);
    let document = UiBuilder::new().picker(Some(&tree), 80).finish();
    let controls = &document.controls;

    // The HEAD node is annotated as the current position, and selected.
    assert!(controls.iter().any(|line| line.kind == UiKind::Selected
        && line.plain().contains("third")
        && line.plain().contains("当前")));
    // Every node on the path to the HEAD is bulleted (root e1 included).
    assert!(controls
        .iter()
        .any(|line| line.plain().contains('\u{2022}') && line.plain().contains("first")));
}

#[test]
fn resume_picker_enter_maps_to_resume_command_without_delete() {
    let tree = PickerState::resume(vec![SessionListItemView {
        id: "s123".to_string(),
        label: "Fix resume list".to_string(),
        detail: "working · summarize current task".to_string(),
        active: false,
    }]);

    assert_eq!(tree.selected_command().as_deref(), Some("/resume s123"));
}

#[test]
fn model_picker_enter_includes_selected_effort() {
    let mut picker = PickerState::model(
        vec![
            ModelOptionView {
                model: "gpt-5.2".to_string(),
                label: None,
                active: false,
                context_window: 400_000,
                max_output_tokens: 128_000,
                reasoning_efforts: vec!["none".to_string(), "low".to_string()],
                input_modalities: Vec::new(),
            },
            ModelOptionView {
                model: "gpt-5.3-codex".to_string(),
                label: None,
                active: true,
                context_window: 400_000,
                max_output_tokens: 128_000,
                reasoning_efforts: vec![
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                    "xhigh".to_string(),
                ],
                input_modalities: Vec::new(),
            },
        ],
        "low".to_string(),
    );

    assert_eq!(
        picker.selected_command().as_deref(),
        Some("/model gpt-5.3-codex low")
    );

    picker.cycle_effort();

    assert_eq!(
        picker.selected_command().as_deref(),
        Some("/model gpt-5.3-codex medium")
    );
}

#[test]
fn model_picker_renders_effort_row() {
    let picker = PickerState::model(
        vec![ModelOptionView {
            model: "gpt-5.2".to_string(),
            label: None,
            active: true,
            context_window: 400_000,
            max_output_tokens: 128_000,
            reasoning_efforts: vec!["none".to_string(), "low".to_string()],
            input_modalities: Vec::new(),
        }],
        "none".to_string(),
    );
    let document = UiBuilder::new().picker(Some(&picker), 80).finish();
    let controls = document
        .controls
        .iter()
        .map(UiLine::plain)
        .collect::<Vec<_>>();

    assert!(controls.iter().any(|text| text.starts_with("  切换模型")));
    assert!(controls.iter().any(|text| text.contains("思考强度")
        && text.contains("‹ 不思考 ›")
        && text.contains("低")));
    assert!(controls.iter().any(|text| text.contains("gpt-5.2")
        && text.contains("400K 上下文")
        && text.contains("当前")));
}

#[test]
fn typing_filters_a_list_picker_and_enter_takes_the_top_match() {
    let models = ["gpt-5.5", "claude-opus-4.5", "claude-sonnet-4.5", "kimi-k3"]
        .iter()
        .map(|model| ModelOptionView {
            model: model.to_string(),
            label: None,
            active: *model == "gpt-5.5",
            context_window: 200_000,
            max_output_tokens: 32_000,
            reasoning_efforts: vec!["high".to_string()],
            input_modalities: Vec::new(),
        })
        .collect();
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.picker_view = Some(PickerState::model(models, "high".to_string()));
    let now = Instant::now();
    for ch in "sonn".chars() {
        app.handle_key_at(KeyCode::Char(ch), KeyModifiers::empty(), now);
    }
    let picker = app.state.picker_view.as_ref().unwrap();
    assert_eq!(picker.filter, "sonn");
    assert_eq!(picker.rows.len(), 1);
    let document = UiBuilder::new().picker(Some(picker), 80).finish();
    assert!(document
        .controls
        .iter()
        .any(|line| line.plain().contains("1 / 4")));

    // Esc clears the filter first; the picker stays open.
    app.handle_key_at(KeyCode::Esc, KeyModifiers::empty(), now);
    assert_eq!(app.state.picker_view.as_ref().unwrap().rows.len(), 4);
    for ch in "sonn".chars() {
        app.handle_key_at(KeyCode::Char(ch), KeyModifiers::empty(), now);
    }
    app.handle_key_at(KeyCode::Backspace, KeyModifiers::empty(), now);
    assert_eq!(app.state.picker_view.as_ref().unwrap().filter, "son");
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), now);
    assert_eq!(
        app.runtime.commands.last().map(String::as_str),
        Some("/model claude-sonnet-4.5 high")
    );
}

#[test]
fn approval_picker_shows_header_context_and_options() {
    let picker = PickerState::approval(
        "call-1".to_string(),
        "bash".to_string(),
        "cargo test --workspace".to_string(),
    );
    let document = UiBuilder::new().picker(Some(&picker), 80).finish();
    let controls = document
        .controls
        .iter()
        .map(UiLine::plain)
        .collect::<Vec<_>>();

    assert!(controls[0].starts_with("── 需要你批准 ──"), "{controls:?}");
    assert_eq!(UnicodeWidthStr::width(controls[0].as_str()), 80);
    let title_row = controls
        .iter()
        .position(|text| text == "  运行命令")
        .expect("title row");
    assert_eq!(controls[title_row + 1], "    cargo test --workspace");
    assert!(controls.iter().any(|text| text == "  › 1  允许这一次"));
    assert!(controls
        .iter()
        .any(|text| text == "    2  本会话内都允许  bash"));
    assert!(controls.iter().any(|text| text == "    3  拒绝"));
    assert!(controls
        .iter()
        .any(|text| text.contains("1-3 直接选") && text.contains("esc 拒绝")));
    // The block closes with a rule in the attention colour.
    let last = document.controls.last().unwrap();
    assert!(last.plain().starts_with("────"));
    assert_eq!(last.line.spans[0].style, crate::theme::warn());

    // Long commands truncate to a single line with an ellipsis.
    let long = PickerState::approval("call-2".to_string(), "bash".to_string(), "x".repeat(200));
    let document = UiBuilder::new().picker(Some(&long), 60).finish();
    let context = document
        .controls
        .iter()
        .map(UiLine::plain)
        .find(|text| text.starts_with("    xxx"))
        .expect("context line");
    assert!(context.ends_with('…'));
    assert!(UnicodeWidthStr::width(context.as_str()) <= 60);
}

#[test]
fn approval_digits_pick_and_esc_denies() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();
    app.state.picker_view = Some(PickerState::approval(
        "c1".to_string(),
        "bash".to_string(),
        "rm -rf build".to_string(),
    ));
    app.handle_key_at(KeyCode::Char('2'), KeyModifiers::empty(), now);
    assert_eq!(
        app.runtime.commands.last().map(String::as_str),
        Some("/approve c1 allow always")
    );
    assert!(app.state.picker_view.is_none());

    app.state.picker_view = Some(PickerState::approval(
        "c2".to_string(),
        "bash".to_string(),
        "rm -rf dist".to_string(),
    ));
    app.handle_key_at(KeyCode::Esc, KeyModifiers::empty(), now);
    assert_eq!(
        app.runtime.commands.last().map(String::as_str),
        Some("/approve c2 deny")
    );
}

#[test]
fn approval_request_shows_where_and_why() {
    let mut app = TuiApp::new(TestRuntime::default());
    let info = lynshen_agent_core::approval::ApprovalInfo {
        command: Some("rm -rf target/debug/incremental".to_string()),
        cwd: std::path::PathBuf::from("/srv/repo"),
        justification: Some("free 4 GB".to_string()),
        prefix: Some("rm -rf".to_string()),
        ..Default::default()
    };
    app.apply_events(vec![AgentEvent::ApprovalRequest {
        call_id: "c1".to_string(),
        name: "bash".to_string(),
        summary: "rm".to_string(),
        subagent_id: None,
        hunks: None,
        info,
    }]);
    // The approval takes the composer's place.
    let document = app.build_document(80, Instant::now());
    let controls: Vec<String> = document.controls.iter().map(UiLine::plain).collect();
    assert!(!document
        .controls
        .iter()
        .any(|line| line.kind == UiKind::Input));
    assert!(controls
        .iter()
        .any(|text| text == "    rm -rf target/debug/incremental"));
    assert!(
        controls
            .iter()
            .any(|text| text == "    /srv/repo · 理由：free 4 GB"),
        "{controls:?}"
    );
    assert!(controls
        .iter()
        .any(|text| text.contains("本会话内都允许  rm -rf 开头的命令")));
}

#[test]
fn login_paste_picker_submits_paste_command() {
    let mut picker = PickerState::login_paste();
    assert!(picker.prompt.is_some());
    for ch in "zcode://cb?code=abc&state=s".chars() {
        picker.push_prompt_char(ch);
    }
    assert_eq!(
        picker.take_prompt_command().as_deref(),
        Some("/login-paste zcode://cb?code=abc&state=s")
    );
    assert_eq!(picker.take_prompt_command(), None);
}

#[test]
fn effort_cycle_wraps() {
    let efforts = vec!["none".to_string(), "low".to_string(), "medium".to_string()];
    assert_eq!(next_reasoning_effort(&efforts, "none"), "low");
    assert_eq!(next_reasoning_effort(&efforts, "low"), "medium");
    assert_eq!(next_reasoning_effort(&efforts, "medium"), "none");
    assert_eq!(next_reasoning_effort(&efforts, "unknown"), "none");
}

#[test]
fn approval_mode_colors_map_to_permission_level() {
    use crate::theme;
    use crate::ui_builder::approval_mode_label;
    assert_eq!(
        approval_mode_label("auto-edit"),
        ("自动改文件", theme::add())
    );
    assert_eq!(approval_mode_label("auto"), ("自动批准", theme::warn()));
    assert_eq!(
        approval_mode_label("full-access"),
        ("完全放开", theme::err())
    );
    assert_eq!(approval_mode_label("plan"), ("计划模式", theme::info()));
    assert_eq!(approval_mode_label("manual"), ("手动批准", theme::dim()));
}

#[test]
fn backtab_cycles_approval_mode() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();

    for expected in [
        "/permissions plan",
        "/permissions auto-edit",
        "/permissions auto",
        "/permissions full-access",
        "/permissions manual",
    ] {
        app.handle_key_at(KeyCode::BackTab, KeyModifiers::SHIFT, now);
        assert_eq!(
            app.runtime.commands.last().map(String::as_str),
            Some(expected)
        );
    }
    assert_eq!(app.state.approval_mode, "manual");
}

fn proposed_plan(id: &str, status: &str) -> AgentEvent {
    AgentEvent::ProposedPlan {
        id: id.to_string(),
        title: "Add login".to_string(),
        markdown: "1. Do it".to_string(),
        status: status.to_string(),
    }
}

#[test]
fn a_pending_plan_is_approved_from_the_picker() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![proposed_plan("call_1", "pending")]);
    assert_eq!(
        app.state.picker_view.as_ref().map(|picker| picker.mode),
        Some(PickerMode::Plan)
    );

    let now = Instant::now();
    app.handle_key_at(KeyCode::Down, KeyModifiers::empty(), now);
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), now);

    assert_eq!(
        app.runtime.commands.last().map(String::as_str),
        Some("/plan call_1 approve manual")
    );
    assert!(app.state.picker_view.is_none());
}

#[test]
fn revising_a_plan_takes_the_feedback_in_the_picker() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![proposed_plan("call_1", "pending")]);
    let now = Instant::now();
    for _ in 0..3 {
        app.handle_key_at(KeyCode::Down, KeyModifiers::empty(), now);
    }
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), now);
    assert!(app.runtime.commands.is_empty());
    for ch in "keep it small".chars() {
        app.handle_key_at(KeyCode::Char(ch), KeyModifiers::empty(), now);
    }
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), now);

    assert_eq!(
        app.runtime.commands,
        vec!["/plan call_1 revise keep it small".to_string()]
    );
    assert!(app.state.picker_view.is_none());
    assert!(app.runtime.submitted.is_empty());
}

#[test]
fn a_plan_waits_behind_an_open_picker_and_only_while_pending() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.picker_view = Some(PickerState::trust("/w".to_string(), None));
    app.apply_events(vec![proposed_plan("call_1", "pending")]);
    app.handle_key_at(KeyCode::Esc, KeyModifiers::empty(), Instant::now());
    assert_eq!(
        app.state.picker_view.as_ref().map(|picker| picker.mode),
        Some(PickerMode::Plan)
    );

    // Not pending (approved elsewhere, or replayed): no picker.
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![proposed_plan("call_2", "approved")]);
    assert!(app.state.picker_view.is_none());
}

#[test]
fn ctrl_t_cycles_reasoning_effort() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.model = "gpt-5.5".to_string();
    app.state.reasoning_efforts = vec!["low".to_string(), "medium".to_string(), "high".to_string()];
    app.state.reasoning_effort = "low".to_string();

    app.handle_key_at(KeyCode::Char('t'), KeyModifiers::CONTROL, Instant::now());

    assert_eq!(
        app.runtime.commands.last().map(String::as_str),
        Some("/model gpt-5.5 medium")
    );
}

#[test]
fn model_picker_tab_cycles_effort() {
    let models = vec![
        ModelOptionView {
            model: "gpt-5.5".to_string(),
            label: None,
            active: true,
            context_window: 200_000,
            max_output_tokens: 32_000,
            reasoning_efforts: vec!["low".to_string(), "medium".to_string()],
            input_modalities: Vec::new(),
        },
        ModelOptionView {
            model: "gpt-5.4-mini".to_string(),
            label: None,
            active: false,
            context_window: 200_000,
            max_output_tokens: 32_000,
            reasoning_efforts: vec!["none".to_string(), "low".to_string()],
            input_modalities: Vec::new(),
        },
    ];
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.picker_view = Some(PickerState::model(models, "low".to_string()));
    let now = Instant::now();

    app.handle_key_at(KeyCode::Tab, KeyModifiers::empty(), now);
    let picker = app.state.picker_view.as_ref().unwrap();
    assert_eq!(picker.efforts[picker.selected_effort], "medium");

    // Switching rows re-bases the effort list on the newly selected model:
    // "medium" is unsupported there, so it falls back to the first entry.
    app.handle_key_at(KeyCode::Down, KeyModifiers::empty(), now);
    let picker = app.state.picker_view.as_ref().unwrap();
    assert_eq!(picker.efforts[picker.selected_effort], "none");
}

#[test]
fn bang_input_runs_locally_and_never_reaches_the_model() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.input.push_text("!echo tui-local-shell");
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), Instant::now());

    assert!(app.runtime.submitted.is_empty(), "must not go to the model");
    assert!(app.runtime.commands.is_empty());
    assert!(app.input.text().is_empty());
    assert!(app
        .state
        .chat
        .iter()
        .any(|line| matches!(line, ChatLine::User(text) if text == "!echo tui-local-shell")));
    assert!(app.state.chat.iter().any(|line| matches!(
        line,
        ChatLine::Tool { name, running: true, .. } if name == "! echo tui-local-shell"
    )));

    let mut finished = false;
    for _ in 0..300 {
        for result in app.local_shell.poll() {
            app.state.finish_local_shell(result);
        }
        if app.state.chat.iter().any(|line| {
            matches!(
                line,
                ChatLine::Tool { output, running: false, .. } if output.contains("tui-local-shell")
            )
        }) {
            finished = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(finished, "local shell output never landed in history");
}

#[test]
fn lone_bang_submits_as_a_regular_message() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.input.push_text("!");
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), Instant::now());
    assert_eq!(app.runtime.submitted, vec!["!".to_string()]);
}

#[test]
fn at_mention_tab_completes_top_ranked_file() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.file_index = Some(vec![
        "docs/main-notes.md".to_string(),
        "src/main.rs".to_string(),
    ]);
    app.input.push_text("look at @main");

    app.handle_key_at(KeyCode::Tab, KeyModifiers::empty(), Instant::now());

    assert_eq!(app.input.text(), "look at @src/main.rs ");
    assert!(app.runtime.submitted.is_empty());
}

#[test]
fn at_mention_enter_completes_instead_of_submitting() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.file_index = Some(vec!["src/main.rs".to_string()]);
    app.input.push_text("@ma");

    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), Instant::now());
    assert_eq!(app.input.text(), "@src/main.rs ");
    assert!(app.runtime.submitted.is_empty());

    // Second Enter submits the completed message.
    app.input.push_text("please review");
    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), Instant::now());
    assert_eq!(
        app.runtime.submitted,
        vec!["@src/main.rs please review".to_string()]
    );
}

#[test]
fn at_mention_arrow_keys_cycle_candidates() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.file_index = Some(vec![
        "src/main.rs".to_string(),
        "src/maintenance.rs".to_string(),
    ]);
    app.input.push_text("@main");

    app.handle_key_at(KeyCode::Down, KeyModifiers::empty(), Instant::now());
    app.handle_key_at(KeyCode::Tab, KeyModifiers::empty(), Instant::now());

    assert_eq!(app.input.text(), "@src/maintenance.rs ");
}

#[test]
fn exact_mention_path_submits_on_enter() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.state.file_index = Some(vec!["src/main.rs".to_string()]);
    app.input.push_text("@src/main.rs");

    app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), Instant::now());

    assert_eq!(app.runtime.submitted, vec!["@src/main.rs".to_string()]);
}

#[test]
fn pasted_image_path_is_attached_not_inserted() {
    let dir = std::env::temp_dir().join(format!(
        "lynshen-tui-image-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shot.png");
    std::fs::write(&path, b"fake png bytes").unwrap();

    let mut app = TuiApp::new(TestRuntime::default());
    app.handle_paste(&path.display().to_string());
    assert_eq!(
        app.runtime.commands,
        vec![format!("/image {}", path.display())]
    );
    assert!(app.input.text().is_empty());

    // Quoted drag-and-drop form also resolves.
    app.handle_paste(&format!("\"{}\"", path.display()));
    assert_eq!(app.runtime.commands.len(), 2);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pasted_non_image_text_stays_in_input() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.handle_paste("notes.txt and some text");
    assert_eq!(app.input.text(), "notes.txt and some text");
    assert!(app.runtime.commands.is_empty());
}

#[test]
fn bottom_status_shows_git_branch_with_dirty_marker() {
    let dirty = crate::git_bar::GitStatus {
        branch: "main".to_string(),
        dirty: true,
    };
    let document = UiBuilder::new()
        .bottom_status(
            BottomStatus {
                provider: "p",
                model: "m",
                reasoning_effort: "low",
                approval_mode: "manual",
                git: Some(&dirty),
                context_tokens: 1,
                context_window: 100,
                cost: 0.0,
                balance: None,
            },
            64,
        )
        .finish();
    let line = document.controls.last().unwrap().plain();
    assert!(line.starts_with("  m 低   手动批准"), "{line}");
    assert!(line.contains("main* · 上下文 1%"), "{line}");
}

#[test]
fn ctrl_c_interrupts_then_clears_then_quits_on_a_second_press() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();
    app.apply_events(vec![AgentEvent::Connecting]);
    assert!(!app.handle_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, now));
    assert_eq!(app.runtime.interrupts, 1);

    app.input.push_text("draft");
    assert!(!app.handle_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, now));
    assert!(app.input.text().is_empty());

    assert!(!app.handle_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, now));
    let document = app.build_document(80, now);
    assert!(document
        .controls
        .iter()
        .any(|line| line.plain().contains("再按一次 ctrl+c 退出")));
    assert!(app.handle_key_at(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        now + Duration::from_millis(500)
    ));

    // Too late: the second press only arms again.
    let mut app = TuiApp::new(TestRuntime::default());
    assert!(!app.handle_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, now));
    assert!(!app.handle_key_at(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        now + Duration::from_secs(3)
    ));
}

#[test]
fn ctrl_d_quits_only_on_an_empty_idle_composer() {
    let mut app = TuiApp::new(TestRuntime::default());
    app.input.push_text("x");
    assert!(!app.handle_key_at(KeyCode::Char('d'), KeyModifiers::CONTROL, Instant::now()));
    app.input.clear();
    assert!(app.handle_key_at(KeyCode::Char('d'), KeyModifiers::CONTROL, Instant::now()));
}

#[test]
fn question_mark_on_an_empty_composer_opens_the_shortcut_sheet() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();
    app.handle_key_at(KeyCode::Char('?'), KeyModifiers::empty(), now);
    assert!(app.state.show_help);
    assert!(app.input.text().is_empty());
    let document = app.build_document(100, now);
    let rows: Vec<String> = document.controls.iter().map(UiLine::plain).collect();
    assert!(rows.iter().any(|row| row.trim() == "快捷键"));
    assert!(rows
        .iter()
        .any(|row| row.contains("ctrl+o") && row.contains("展开或折叠工具输出")));
    // No status line while the sheet is open.
    assert!(!document
        .controls
        .iter()
        .any(|line| line.kind == UiKind::BottomStatus));

    // Esc closes it without clearing anything else; a typed key closes it
    // and types.
    app.handle_key_at(KeyCode::Esc, KeyModifiers::empty(), now);
    assert!(!app.state.show_help);
    app.handle_key_at(KeyCode::Char('?'), KeyModifiers::empty(), now);
    app.input.push_text("");
    app.handle_key_at(KeyCode::Char('?'), KeyModifiers::empty(), now);
    assert!(!app.state.show_help);

    // In a sentence `?` is just text.
    let mut app = TuiApp::new(TestRuntime::default());
    app.input.push_text("why");
    app.handle_key_at(
        KeyCode::Char('?'),
        KeyModifiers::empty(),
        now + Duration::from_secs(1),
    );
    app.flush_paste_burst_if_due(now + Duration::from_secs(2));
    assert_eq!(app.input.text(), "why?");
}

#[test]
fn up_and_down_walk_the_sent_messages_and_keep_the_draft() {
    let mut app = TuiApp::new(TestRuntime::default());
    let now = Instant::now();
    for message in ["first", "second"] {
        app.input.push_text(message);
        app.handle_key_at(KeyCode::Enter, KeyModifiers::empty(), now);
    }
    app.input.push_text("dra");
    app.handle_key_at(KeyCode::Up, KeyModifiers::empty(), now);
    assert_eq!(app.input.text(), "second");
    app.handle_key_at(KeyCode::Up, KeyModifiers::empty(), now);
    assert_eq!(app.input.text(), "first");
    app.handle_key_at(KeyCode::Up, KeyModifiers::empty(), now);
    assert_eq!(app.input.text(), "first");
    app.handle_key_at(KeyCode::Down, KeyModifiers::empty(), now);
    assert_eq!(app.input.text(), "second");
    app.handle_key_at(KeyCode::Down, KeyModifiers::empty(), now);
    assert_eq!(app.input.text(), "dra");
}

#[test]
fn content_anchored_layout_puts_the_composer_under_a_short_transcript() {
    use ratatui::{backend::TestBackend, Terminal};
    let mut app = TuiApp::new(TestRuntime::default());
    app.apply_events(vec![AgentEvent::Info("hello".to_string())]);
    let document = app.build_document(60, Instant::now());
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            let mut scroll = 0;
            crate::terminal_renderer::draw_frame(frame.buffer_mut(), area, &document, &mut scroll);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..20)
        .map(|y| (0..60).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect();
    assert!(rows[0].starts_with("  hello"), "{rows:#?}");
    assert!(rows[1].trim().is_empty());
    assert!(rows[2].starts_with("────"), "{rows:#?}");
    assert!(rows[3].starts_with("  › "), "{rows:#?}");
    assert!(rows[4].starts_with("────"));
    // A wide character's second cell reads as a space.
    assert!(rows[5].replace(' ', "").contains("手动批准"), "{rows:#?}");
    assert!(rows[19].trim().is_empty());
}

trait TestUiBuilderExt {
    fn finish_with_history_and_input(self, history_lines: usize) -> UiDocument;
}

impl TestUiBuilderExt for UiBuilder {
    fn finish_with_history_and_input(mut self, history_lines: usize) -> UiDocument {
        for index in 0..history_lines {
            self.history_line(UiKind::Assistant, format!("line {index}"));
        }
        self.input(&input_lines(""), None).finish()
    }
}
