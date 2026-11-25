#[cfg(feature = "bench")]
pub mod bench_support;
mod fuzzy;
mod git_bar;
mod input;
mod lang;
mod local_shell;
mod markdown;
mod mention;
mod picker;
#[cfg(feature = "preview")]
pub mod preview_support;
mod state;
mod terminal_renderer;
#[cfg(test)]
mod tests;
mod theme;
mod tool_preview;
mod ui_builder;

use git_bar::GitStatusTracker;
use input::{paste_burst_render_delay, InputBuffer, PasteBurst, PasteCharDecision, PasteFlush};
use local_shell::{local_shell_command, LocalShellRunner};
use lynshen_agent_core::{AccountLookup, AccountSummary, AgentEvent, CommandView, TranscriptItem};
use picker::{PickerMode, PickerState, TreePromptAction};
use ratatui::crossterm::{
    cursor::{Hide, Show},
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use state::TuiState;
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};
use terminal_renderer::TerminalRenderer;
use terminal_renderer::TextSelection;
use ui_builder::UiBuilder;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(30);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Output that brings nothing new for this long reads as stalled.
const STALL_AFTER: Duration = Duration::from_secs(8);
const PASTE_PLACEHOLDER_CHARS: usize = 200;
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);
#[cfg(not(windows))]
const PASTE_BURST_IDLE_TIMEOUT: Duration = Duration::from_millis(8);
#[cfg(windows)]
const PASTE_BURST_IDLE_TIMEOUT: Duration = Duration::from_millis(60);
const ENABLE_BRACKETED_PASTE: &str = "\x1b[?2004h";
const DISABLE_BRACKETED_PASTE: &str = "\x1b[?2004l";
pub(crate) const CONTENT_LEFT_PADDING: usize = 2;
/// The composer's own text selection (not the mouse drag overlay).
pub(crate) const INPUT_SELECTION: Style = Style::new().add_modifier(Modifier::REVERSED);
/// A second ctrl+c within this window quits.
const QUIT_CONFIRM_WINDOW: Duration = Duration::from_secs(2);
/// The account (balance, plan) is looked up again at most this often.
const ACCOUNT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) enum ChatLine {
    Startup {
        version: String,
        cwd: String,
        model: String,
        context_window: u64,
    },
    User(String),
    /// A sent message whose model request has not connected yet; drawn dim
    /// until the connection is up, then turned into `User`.
    PendingUser(String),
    Assistant(String),
    Reasoning {
        text: String,
        collapsed: bool,
        /// How long the model thought, recorded when the block collapsed.
        duration_secs: Option<u64>,
    },
    Tool {
        call_id: Option<String>,
        name: String,
        output: String,
        running: bool,
        /// Clicked open: shows its full output.
        expanded: bool,
    },
    System(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiKind {
    User,
    Assistant,
    ToolHeader,
    Tool,
    System,
    Error,
    Status,
    BottomStatus,
    Selected,
    Input,
    DiffAdd,
    DiffRemove,
    /// A row of the command palette, file matches or the shortcut sheet
    /// under the composer.
    Candidate,
    CandidateSelected,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UiLine {
    pub(crate) kind: UiKind,
    /// Styled content; `kind` supplies the fallback style for unstyled spans.
    pub(crate) line: Line<'static>,
    /// When set, clicking this line toggles the chat item it points at
    /// (currently: thinking headers expand/collapse their reasoning block).
    pub(crate) click: Option<usize>,
    /// Hardware-caret column on this line (composer only); follows the line
    /// through wrapping so the renderer can place the terminal cursor.
    pub(crate) cursor: Option<usize>,
}

impl UiLine {
    pub(crate) fn new(kind: UiKind, line: impl Into<Line<'static>>) -> Self {
        Self {
            kind,
            line: line.into(),
            click: None,
            cursor: None,
        }
    }

    pub(crate) fn clickable(kind: UiKind, line: impl Into<Line<'static>>, index: usize) -> Self {
        Self {
            kind,
            line: line.into(),
            click: Some(index),
            cursor: None,
        }
    }

    /// Span contents concatenated — what a row shows, styles aside.
    pub(crate) fn plain(&self) -> String {
        self.line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// No visible text (empty or whitespace-only spans).
    pub(crate) fn is_blank(&self) -> bool {
        self.line
            .spans
            .iter()
            .all(|span| span.content.trim().is_empty())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UiDocument {
    history: Vec<UiLine>,
    rendered_history_lines: Option<Vec<UiLine>>,
    controls: Vec<UiLine>,
    pub(crate) reset_screen: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CursorTarget {
    pub(crate) row: usize,
    pub(crate) column: usize,
}

#[cfg(test)]
pub(crate) struct RenderedFrame {
    pub(crate) lines: Vec<String>,
    pub(crate) cursor: Option<CursorTarget>,
}

// Flat projection of a document into terminal lines plus cursor position. The live
// renderer composes regions directly with ratatui widgets; this remains as a compact
// model for asserting layout invariants in tests.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct ProjectedDocument {
    pub(crate) transcript_lines: Vec<String>,
    active_lines: Vec<String>,
    cursor: Option<CursorTarget>,
}

/// The LynShen account behind the provider, as far as it is known.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum AccountState {
    #[default]
    Unknown,
    NotLynShen,
    SignedOut,
    Ready(AccountSummary),
    Failed,
}

impl From<AccountLookup> for AccountState {
    fn from(lookup: AccountLookup) -> Self {
        match lookup {
            AccountLookup::NotLynShen => Self::NotLynShen,
            AccountLookup::SignedOut => Self::SignedOut,
            AccountLookup::Summary(summary) => Self::Ready(summary),
            AccountLookup::Failed(_) => Self::Failed,
        }
    }
}

impl AccountState {
    /// The balance for the status bar, e.g. `¥128.40`.
    pub(crate) fn balance(&self) -> Option<String> {
        match self {
            Self::Ready(summary) => summary
                .balance
                .as_deref()
                .map(|balance| ui_builder::format_balance(balance, summary.currency.as_deref())),
            _ => None,
        }
    }
}

pub(crate) struct BottomStatus<'a> {
    #[allow(dead_code)]
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) reasoning_effort: &'a str,
    pub(crate) approval_mode: &'a str,
    pub(crate) git: Option<&'a git_bar::GitStatus>,
    pub(crate) context_tokens: u64,
    pub(crate) context_window: u64,
    pub(crate) cost: f64,
    /// The account balance, already formatted.
    pub(crate) balance: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CommandCandidate {
    pub(crate) command: String,
    pub(crate) marker: Option<String>,
    /// The engine's description (English); built-ins show a Chinese one.
    pub(crate) description: String,
    /// Characters of `command` the typed query matched.
    pub(crate) highlight: Vec<usize>,
}

impl From<CommandView> for CommandCandidate {
    fn from(value: CommandView) -> Self {
        Self {
            command: value.command,
            marker: value.marker,
            description: value.description,
            highlight: Vec::new(),
        }
    }
}

fn default_commands() -> Vec<CommandCandidate> {
    [
        "/help", "/login", "/new", "/model", "/tree", "/trust", "/resume", "/context", "/doctor",
        "/skills", "/mcp", "/pin", "/goal", "/compact", "/quit",
    ]
    .iter()
    .map(|command| CommandCandidate {
        command: (*command).to_string(),
        ..CommandCandidate::default()
    })
    .collect()
}

/// The palette's description for a command: the built-ins in Chinese when
/// the UI is Chinese, else the engine's own text.
pub(crate) fn command_description(candidate: &CommandCandidate) -> String {
    if lang::is_zh() && candidate.marker.is_none() {
        let zh = match candidate.command.as_str() {
            "/help" => "查看所有命令",
            "/login" => "登录 LynShen 或其他服务商",
            "/login-paste" => "粘贴回调地址完成登录",
            "/usage" => "套餐、余额、用量和最近调用",
            "/new" => "开始新会话",
            "/model" => "切换模型和思考强度",
            "/effort" => "切换思考强度",
            "/subagents" => "子智能体可以用的模型",
            "/tree" => "查看对话分支",
            "/trust" => "信任这个项目的技能和钩子",
            "/resume" => "恢复之前的会话",
            "/rewind" => "回到更早的一轮，对话和文件一起",
            "/approve" => "回应批准请求，可以只批准部分改动",
            "/plan" => "查看等待批准的计划",
            "/permissions" => "查看或切换批准模式",
            "/sandbox" => "查看或切换命令沙箱",
            "/style" => "查看或切换回答风格",
            "/context" => "上下文占用",
            "/goal" => "设定会话目标",
            "/doctor" => "检查运行环境",
            "/skills" => "管理技能",
            "/pin" => "本会话固定使用某个技能",
            "/image" => "给下一条消息附图",
            "/compact" => "立即压缩上下文",
            "/quit" | "/exit" => "退出",
            "/config" => "查看当前配置",
            "/checkout" => "切换到对话的某个节点",
            "/fork" => "从某个节点分叉",
            "/delete" => "删除分支",
            "/mcp" => "管理 MCP 服务器",
            "/android" => "操作安卓手机",
            "/stats" => "上下文统计",
            _ => "",
        };
        if !zh.is_empty() {
            return zh.to_string();
        }
    }
    candidate.description.clone()
}

/// The palette's tag for a command's source.
pub(crate) fn marker_label(marker: &str) -> String {
    if !lang::is_zh() {
        return marker.to_string();
    }
    match marker {
        "SKILL" => "技能",
        "PROJ" => "项目",
        "CMD" => "自定义",
        other => other,
    }
    .to_string()
}

fn format_plan_summary(items: &[lynshen_agent_core::PlanItem]) -> String {
    if items.is_empty() {
        return "Plan cleared".to_string();
    }
    let mut lines = vec!["Plan".to_string()];
    for item in items {
        let mark = match item.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[~]",
            _ => "[ ]",
        };
        lines.push(format!("{mark} {}", item.step));
    }
    lines.join("\n")
}

fn format_goal_summary(goal: Option<lynshen_agent_core::GoalView>) -> String {
    let Some(goal) = goal else {
        return "Goal\nNo goal set.\nCommands: /goal <objective>".to_string();
    };
    let mut lines = vec![
        "Goal".to_string(),
        format!("Status: {}", goal.status.replace('_', " ")),
        format!("Objective: {}", goal.objective),
        format!(
            "Time used: {}",
            format_elapsed_seconds(goal.time_used_seconds)
        ),
        format!("Tokens used: {}", format_compact_number(goal.tokens_used)),
    ];
    if let Some(token_budget) = goal.token_budget {
        lines.push(format!(
            "Token budget: {}",
            format_compact_number(token_budget)
        ));
    }
    let commands = match goal.status.as_str() {
        "active" => "Commands: /goal pause, /goal complete, /goal blocked, /goal clear",
        "paused" | "blocked" | "usage_limited" => {
            "Commands: /goal resume, /goal complete, /goal clear"
        }
        _ => "Commands: /goal <objective>, /goal clear",
    };
    lines.push(String::new());
    lines.push(commands.to_string());
    lines.join("\n")
}

fn format_elapsed_seconds(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    let remaining_minutes = minutes % 60;
    if hours >= 24 {
        let days = hours / 24;
        let remaining_hours = hours % 24;
        return format!("{days}d {remaining_hours}h {remaining_minutes}m");
    }
    if remaining_minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {remaining_minutes}m")
    }
}

fn format_compact_number(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn format_context_window(value: u64) -> String {
    if value >= 1_000_000 && value.is_multiple_of(1_000_000) {
        format!("{}M", value / 1_000_000)
    } else if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{}K", value / 1_000)
    } else {
        value.to_string()
    }
}

fn compact_home_path(path: &str) -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let Some(home) = home else {
        return path.to_string();
    };
    let home = home.trim_end_matches(['\\', '/']);
    if path == home {
        "~".to_string()
    } else {
        path.strip_prefix(&format!("{home}\\"))
            .or_else(|| path.strip_prefix(&format!("{home}/")))
            .map(|rest| format!("~/{rest}").replace('\\', "/"))
            .unwrap_or_else(|| path.to_string())
    }
}

/// If a paste is a single existing image-file path (optionally quoted or a
/// `file://` URL, as terminals produce for drag-and-drop), returns the path.
pub(crate) fn pasted_image_path(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return None;
    }
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|rest| rest.strip_suffix('\''))
        })
        .unwrap_or(trimmed);
    let path = unquoted.strip_prefix("file://").unwrap_or(unquoted);
    let extension = std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase();
    if !matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
    ) {
        return None;
    }
    if !std::path::Path::new(path).is_file() {
        return None;
    }
    Some(path.to_string())
}

/// Lines moved per mouse-wheel notch; PageUp/PageDown use a full page.
const MOUSE_SCROLL_LINES: usize = 3;

/// Writes text to the system clipboard via OSC52 — works over SSH and inside
/// tmux (where supported), unlike a platform clipboard helper.
fn copy_to_clipboard(text: &str) {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = io::stdout();
    let _ = write!(stdout, "\x1b]52;c;{encoded}\x07");
    let _ = stdout.flush();
}

/// Number of transcript lines a PageUp/PageDown moves the viewport, one screen minus a
/// little overlap so context carries across the jump. Falls back if the size query fails.
fn scroll_page_size() -> usize {
    terminal::size()
        .map(|(_, height)| (height.saturating_sub(2)).max(1) as usize)
        .unwrap_or(10)
}

pub trait TuiRuntime {
    fn startup_events(&self) -> Vec<AgentEvent>;
    fn model_status_event(&self) -> AgentEvent;
    fn submit_user_message(&mut self, message: String) -> Vec<AgentEvent>;
    fn interrupt(&mut self) -> Vec<AgentEvent>;
    /// Takes the last queued message back (`unqueue` index -1).
    fn unqueue_last(&mut self) -> Vec<AgentEvent>;
    fn handle_command(&mut self, input: &str) -> (bool, Vec<AgentEvent>);
    fn poll_events(&mut self) -> Vec<AgentEvent>;
}

pub struct TuiApp<R> {
    runtime: R,
    input: InputBuffer,
    paste_burst: PasteBurst,
    state: TuiState,
    local_shell: LocalShellRunner,
    /// Lines the transcript viewport is lifted above the live tail (PageUp/PageDown).
    /// Zero follows live output; the renderer clamps it to the available range.
    scroll_offset: usize,
    /// Active drag selection over the whole screen, in cell coordinates.
    /// Releasing the drag copies the text via OSC52.
    mouse_selection: Option<TextSelection>,
    /// The first ctrl+c on an idle, empty composer: a second one within
    /// `QUIT_CONFIRM_WINDOW` quits.
    quit_armed_at: Option<Instant>,
}

#[derive(Debug, Clone)]
enum ActivityKind {
    Idle,
    Connecting,
    Compacting,
    Thinking,
    Reconnecting { attempt: usize },
    Output,
    Tool,
}

#[derive(Debug, Clone)]
struct ActivityState {
    kind: ActivityKind,
    turn_started_at: Option<Instant>,
    phase_started_at: Option<Instant>,
    last_delta_at: Option<Instant>,
    estimated_output_tokens: u64,
    animation_tick: usize,
}

struct TerminalGuard;

#[derive(Debug, Clone)]
struct FrameScheduler {
    next_frame_at: Option<Instant>,
}

impl FrameScheduler {
    fn new(now: Instant) -> Self {
        Self {
            next_frame_at: Some(now),
        }
    }

    fn request_now(&mut self, now: Instant) {
        self.request_at(now);
    }

    fn request_in(&mut self, now: Instant, delay: Duration) {
        self.request_at(now + delay);
    }

    fn request_at(&mut self, when: Instant) {
        self.next_frame_at = Some(self.next_frame_at.map_or(when, |current| current.min(when)));
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.next_frame_at.is_some_and(|when| now >= when) {
            self.next_frame_at = None;
            return true;
        }
        false
    }

    fn poll_timeout(&self, now: Instant, fallback: Duration) -> Duration {
        let Some(when) = self.next_frame_at else {
            return fallback;
        };
        fallback.min(when.saturating_duration_since(now))
    }
}

impl<R: TuiRuntime> TuiApp<R> {
    pub fn new(runtime: R) -> Self {
        let mut app = Self {
            runtime,
            input: InputBuffer::default(),
            paste_burst: PasteBurst::default(),
            state: TuiState::default(),
            local_shell: LocalShellRunner::default(),
            scroll_offset: 0,
            mouse_selection: None,
            quit_armed_at: None,
        };
        let events = app.runtime.startup_events();
        app.apply_events(events);
        app
    }

    /// Runs a slash command before the first frame (`/resume <id>` for a
    /// conversation the TUI opens on).
    pub fn with_command(mut self, input: &str) -> Self {
        let (_, events) = self.runtime.handle_command(input);
        self.apply_events(events);
        self
    }

    pub fn run(mut self) -> io::Result<()> {
        let _guard = TerminalGuard::enter()?;
        // In raw mode and before any input is read: the background query
        // reads the terminal's reply from stdin.
        theme::install(theme::detect());
        let mut renderer = TerminalRenderer::new()?;
        let mut account = AccountFetcher::start();
        let git_tracker = GitStatusTracker::start(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        );
        let now = Instant::now();
        let mut frames = FrameScheduler::new(now);
        let mut next_progress_at = now + PROGRESS_INTERVAL;
        let mut turn_was_active = false;

        loop {
            let now = Instant::now();
            let events = self.runtime.poll_events();
            if self.apply_events(events) {
                frames.request_now(now);
            }
            let status_event = self.runtime.model_status_event();
            if self.apply_events(vec![status_event]) {
                frames.request_now(now);
            }
            if let Some(status) = git_tracker.poll() {
                if self.state.git_status != status {
                    self.state.git_status = status;
                    frames.request_now(now);
                }
            }
            for result in self.local_shell.poll() {
                self.state.finish_local_shell(result);
                frames.request_now(now);
            }
            if let Some(lookup) = account.poll() {
                if self.state.set_account(lookup.into()) {
                    frames.request_now(now);
                }
            }
            // The balance moves with each turn: look it up again once a turn
            // ends (at most every ACCOUNT_REFRESH_INTERVAL).
            let active = self.state.activity.is_active();
            if turn_was_active && !active {
                account.mark_turn();
            }
            turn_was_active = active;
            if !active {
                account.refresh_if_stale(now);
            }

            if self.state.activity.is_active() && now >= next_progress_at {
                self.state.activity.advance_animation();
                next_progress_at = now + PROGRESS_INTERVAL;
                frames.request_now(now);
            }

            if frames.take_due(now) {
                if self.handle_paste_burst_render_tick(now, &mut frames) {
                    continue;
                }
                let (width, _) = terminal::size()?;
                let width = width.max(1) as usize;
                let document = self.build_document(width, now);
                renderer.render(&document, &mut self.scroll_offset, self.mouse_selection)?;
                if document.reset_screen {
                    self.state.reset_screen = false;
                }
                continue;
            }

            let poll_timeout = {
                let mut timeout = frames.poll_timeout(now, EVENT_POLL_INTERVAL);
                if self.state.activity.is_active() {
                    timeout = timeout.min(next_progress_at.saturating_duration_since(now));
                }
                timeout
            };
            if event::poll(poll_timeout)? {
                loop {
                    let event_now = Instant::now();
                    match event::read()? {
                        Event::Key(key) if key.kind == KeyEventKind::Press => {
                            let quit = self.handle_key_at(key.code, key.modifiers, event_now);
                            frames.request_now(event_now);
                            if self.quit_armed_at.is_some() {
                                // Clears the "press again" notice when the window ends.
                                frames.request_in(
                                    event_now,
                                    QUIT_CONFIRM_WINDOW + Duration::from_millis(20),
                                );
                            }

                            if self.paste_burst.is_active() {
                                frames.request_in(event_now, paste_burst_render_delay());
                            }
                            if quit {
                                return Ok(());
                            }
                        }
                        Event::Resize(_, _) => {
                            frames.request_now(event_now);
                        }
                        Event::Paste(text) => {
                            self.handle_paste(&text);
                            frames.request_now(event_now);
                        }
                        Event::Mouse(mouse) => match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                self.scroll_offset =
                                    self.scroll_offset.saturating_add(MOUSE_SCROLL_LINES);
                                self.mouse_selection = None;
                                frames.request_now(event_now);
                            }
                            MouseEventKind::ScrollDown => {
                                self.scroll_offset =
                                    self.scroll_offset.saturating_sub(MOUSE_SCROLL_LINES);
                                self.mouse_selection = None;
                                frames.request_now(event_now);
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                self.mouse_selection =
                                    Some(TextSelection::new((mouse.row, mouse.column)));
                                frames.request_now(event_now);
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                if let Some(selection) = &mut self.mouse_selection {
                                    selection.cursor = (mouse.row, mouse.column);
                                    frames.request_now(event_now);
                                }
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                match self.mouse_selection.take() {
                                    // A press-release without a drag on a
                                    // thinking header toggles the block.
                                    Some(sel) if sel.is_empty() => {
                                        if let Some(index) = renderer.clickable_at(sel.anchor.0) {
                                            self.state.toggle_item(index);
                                            frames.request_now(event_now);
                                        }
                                    }
                                    Some(sel) => {
                                        let text = renderer.selected_text(sel);
                                        if !text.is_empty() {
                                            copy_to_clipboard(&text);
                                        }
                                    }
                                    None => {}
                                }
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
        }
    }

    fn handle_key_at(&mut self, code: KeyCode, modifiers: KeyModifiers, now: Instant) -> bool {
        self.flush_paste_burst_if_due(now);
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        if !(ctrl && code == KeyCode::Char('c')) {
            self.quit_armed_at = None;
        }

        if self.state.picker_view.is_some() {
            self.flush_paste_burst_before_non_plain_input();
            return self.handle_picker_key(code, modifiers);
        }

        // The shortcut sheet closes on any key; esc and `?` only close it.
        if self.state.show_help {
            self.state.show_help = false;
            if matches!(code, KeyCode::Esc | KeyCode::Char('?')) {
                return false;
            }
        }

        // Scrolling the transcript is a transient browse mode: any other interaction
        // snaps the viewport back to the live tail so new output and typing stay visible.
        if !matches!(code, KeyCode::PageUp | KeyCode::PageDown) {
            self.scroll_offset = 0;
        }
        if !matches!(code, KeyCode::Up | KeyCode::Down) {
            self.state.end_history_browse();
        }

        match code {
            KeyCode::Char('q') if ctrl => true,
            KeyCode::Char('c') if ctrl => self.handle_ctrl_c(now),
            KeyCode::Char('d') if ctrl => {
                self.flush_paste_burst_before_non_plain_input();
                self.input.text().is_empty() && !self.state.activity.is_active()
            }
            KeyCode::Char('o') if ctrl => {
                self.flush_paste_burst_before_non_plain_input();
                self.state.toggle_expand_tools();
                false
            }
            KeyCode::Char('j') if ctrl => {
                self.flush_paste_burst_before_non_plain_input();
                self.input.push_char('\n');
                self.state.completion_index = 0;
                false
            }
            KeyCode::Char('t') if ctrl => {
                self.flush_paste_burst_before_non_plain_input();
                self.cycle_reasoning_effort();
                false
            }
            // `?` on an empty composer opens the shortcut sheet instead of
            // typing (while a paste streams in, it is just text).
            KeyCode::Char('?')
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.input.text().is_empty()
                    && !self.paste_burst.is_active() =>
            {
                self.state.show_help = true;
                false
            }
            KeyCode::Char(ch)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if ch.is_ascii() {
                    if self.input.has_selection() {
                        self.input.delete_selection();
                        self.clamp_completion_index();
                    }
                    match self.paste_burst.on_plain_ascii_char(ch, now) {
                        PasteCharDecision::RetainFirstChar => {}
                        PasteCharDecision::BeginBufferFromPending
                        | PasteCharDecision::BufferAppend => {
                            self.paste_burst.append_char(ch, now);
                        }
                    }
                } else {
                    self.flush_paste_burst_before_non_plain_input();
                    self.input.push_char(ch);
                    self.clamp_completion_index();
                }
                false
            }
            KeyCode::Backspace => {
                self.flush_paste_burst_before_non_plain_input();
                self.input.pop();
                self.clamp_completion_index();
                false
            }
            KeyCode::Up => {
                self.flush_paste_burst_before_non_plain_input();
                let count = self.completion_rows_len();
                let shift = modifiers.contains(KeyModifiers::SHIFT);
                if count > 0 {
                    self.state.completion_index = (self.state.completion_index + count - 1) % count;
                } else if self.input.text().is_empty() && !self.state.pending_messages.is_empty() {
                    // The last queued message comes back to edit (`unqueued`).
                    let events = self.runtime.unqueue_last();
                    self.apply_events(events);
                } else if shift || !self.input.cursor_on_first_line() {
                    self.input.move_up(shift);
                } else {
                    // On the first line: the previous sent message.
                    self.state.history_previous(&mut self.input);
                }
                false
            }
            KeyCode::Down => {
                self.flush_paste_burst_before_non_plain_input();
                let count = self.completion_rows_len();
                let shift = modifiers.contains(KeyModifiers::SHIFT);
                if count > 0 {
                    self.state.completion_index = (self.state.completion_index + 1) % count;
                } else if shift || !self.input.cursor_on_last_line() {
                    self.input.move_down(shift);
                } else {
                    self.state.history_next(&mut self.input);
                }
                false
            }
            KeyCode::Left => {
                self.flush_paste_burst_before_non_plain_input();
                let extend = modifiers.contains(KeyModifiers::SHIFT);
                if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    self.input.move_word_left(extend);
                } else {
                    self.input.move_left(extend);
                }
                false
            }
            KeyCode::Right => {
                self.flush_paste_burst_before_non_plain_input();
                let extend = modifiers.contains(KeyModifiers::SHIFT);
                if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    self.input.move_word_right(extend);
                } else {
                    self.input.move_right(extend);
                }
                false
            }
            KeyCode::Home => {
                self.flush_paste_burst_before_non_plain_input();
                let extend = modifiers.contains(KeyModifiers::SHIFT);
                if modifiers.contains(KeyModifiers::CONTROL) {
                    self.input.move_document_start(extend);
                } else {
                    self.input.move_home(extend);
                }
                false
            }
            KeyCode::End => {
                self.flush_paste_burst_before_non_plain_input();
                let extend = modifiers.contains(KeyModifiers::SHIFT);
                if modifiers.contains(KeyModifiers::CONTROL) {
                    self.input.move_document_end(extend);
                } else {
                    self.input.move_end(extend);
                }
                false
            }
            KeyCode::Delete => {
                self.flush_paste_burst_before_non_plain_input();
                self.input.delete_forward();
                self.clamp_completion_index();
                false
            }
            KeyCode::PageUp => {
                self.flush_paste_burst_before_non_plain_input();
                self.scroll_offset = self.scroll_offset.saturating_add(scroll_page_size());
                false
            }
            KeyCode::PageDown => {
                self.flush_paste_burst_before_non_plain_input();
                self.scroll_offset = self.scroll_offset.saturating_sub(scroll_page_size());
                false
            }
            KeyCode::Tab => {
                self.flush_paste_burst_before_non_plain_input();
                if self.command_completion_active() {
                    self.complete_selected_command();
                } else {
                    self.complete_selected_mention();
                }
                false
            }
            KeyCode::BackTab => {
                self.flush_paste_burst_before_non_plain_input();
                self.cycle_approval_mode();
                false
            }
            KeyCode::Esc => {
                self.flush_paste_burst_before_non_plain_input();
                // While a turn runs, Esc interrupts it; queued pending messages
                // still start on the next turn (engine queue semantics). When
                // idle, Esc clears the draft instead.
                if self.state.activity.is_active() {
                    let events = self.runtime.interrupt();
                    self.apply_events(events);
                    return false;
                }
                self.input.clear();
                self.state.completion_index = 0;
                false
            }
            KeyCode::Enter => {
                if modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) {
                    self.flush_paste_burst_before_non_plain_input();
                    self.input.push_char('\n');
                    self.state.completion_index = 0;
                    return false;
                }
                if self.paste_burst.append_newline_if_active(now) {
                    self.state.completion_index = 0;
                    return false;
                }
                if self
                    .paste_burst
                    .newline_should_insert_instead_of_submit(now)
                {
                    self.input.push_char('\n');
                    self.state.completion_index = 0;
                    return false;
                }
                self.flush_paste_burst_before_non_plain_input();
                if self.should_complete_on_enter() {
                    self.complete_selected_command();
                    return false;
                }
                if self.complete_selected_mention() {
                    return false;
                }

                let submitted = self.input.text().trim().to_string();
                self.input.clear();
                self.state.completion_index = 0;
                if submitted.is_empty() {
                    return false;
                }

                self.state.remember_sent(&submitted);

                // `!command` is a local shell escape: run it on this machine,
                // never send it to the model.
                if let Some(command) = local_shell_command(&submitted) {
                    let call_id = self.local_shell.spawn(command);
                    self.state.begin_local_shell(&call_id, command);
                    return false;
                }

                if submitted.starts_with('/') {
                    let (quit, events) = self.runtime.handle_command(&submitted);
                    self.apply_events(events);
                    return quit;
                }

                let events = self.runtime.submit_user_message(submitted);
                self.apply_events(events);
                false
            }
            _ => {
                self.flush_paste_burst_before_non_plain_input();
                false
            }
        }
    }

    fn handle_paste(&mut self, text: &str) {
        self.paste_burst.clear_after_explicit_paste();
        // While a picker prompt is open the paste targets it — landing in the
        // composer instead leaves the prompt empty and Enter submits nothing.
        if let Some(picker) = self
            .state
            .picker_view
            .as_mut()
            .filter(|picker| picker.prompt.is_some())
        {
            picker.push_prompt_str(text);
            return;
        }
        if let Some(picker) = self
            .state
            .picker_view
            .as_mut()
            .filter(|picker| picker.filterable())
        {
            picker.push_filter_str(text);
            return;
        }
        // A pasted (or drag-and-dropped) image file path is attached directly
        // instead of inserted as text; terminals deliver file drops as paths.
        if let Some(path) = pasted_image_path(text) {
            let (_, events) = self.runtime.handle_command(&format!("/image {path}"));
            self.apply_events(events);
            return;
        }
        self.input.push_paste(text);
        self.clamp_completion_index();
    }

    fn flush_paste_burst_if_due(&mut self, now: Instant) -> bool {
        match self.paste_burst.flush_if_due(now) {
            PasteFlush::Paste(text) => {
                self.handle_paste(&text);
                true
            }
            PasteFlush::Typed(ch) => {
                if let Some(picker) = self
                    .state
                    .picker_view
                    .as_mut()
                    .filter(|picker| picker.prompt.is_some())
                {
                    picker.push_prompt_char(ch);
                } else {
                    self.input.push_char(ch);
                }
                self.clamp_completion_index();
                true
            }
            PasteFlush::None => false,
        }
    }

    fn handle_paste_burst_render_tick(
        &mut self,
        now: Instant,
        frames: &mut FrameScheduler,
    ) -> bool {
        if self.flush_paste_burst_if_due(now) {
            frames.request_now(now);
            return true;
        }
        if self.paste_burst.is_active() {
            frames.request_in(now, paste_burst_render_delay());
            return true;
        }
        false
    }

    fn flush_paste_burst_before_non_plain_input(&mut self) {
        if let Some(text) = self.paste_burst.flush_before_non_plain_input() {
            self.handle_paste(&text);
        }
        self.paste_burst.clear_after_non_char();
    }

    fn handle_picker_prompt_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match code {
            KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.close_picker(),
            KeyCode::Esc => {
                match self.state.picker_view.as_mut() {
                    // The paste prompt is the whole picker — esc closes it;
                    // `/login-paste` still works afterwards.
                    Some(picker) if picker.mode == PickerMode::LoginPaste => {
                        self.state.picker_view = None;
                    }
                    Some(picker) => picker.cancel_prompt(),
                    None => {}
                }
                false
            }
            KeyCode::Backspace => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.pop_prompt_char();
                }
                false
            }
            KeyCode::Enter => {
                let closes = self.state.picker_view.as_ref().is_some_and(|picker| {
                    matches!(
                        picker.mode,
                        PickerMode::Login | PickerMode::LoginPaste | PickerMode::Plan
                    )
                });
                let Some(command) = self
                    .state
                    .picker_view
                    .as_mut()
                    .and_then(PickerState::take_prompt_command)
                else {
                    return false;
                };
                // The key/paste prompt ends the flow — no picker to return to.
                if closes {
                    self.state.picker_view = None;
                }
                let (_, events) = self.runtime.handle_command(&command);
                self.apply_events(events);
                false
            }
            KeyCode::Char(ch)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.push_prompt_char(ch);
                }
                false
            }
            _ => false,
        }
    }

    fn handle_picker_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self
            .state
            .picker_view
            .as_ref()
            .is_some_and(|picker| picker.prompt.is_some())
        {
            return self.handle_picker_prompt_key(code, modifiers);
        }

        let filterable = self
            .state
            .picker_view
            .as_ref()
            .is_some_and(PickerState::filterable);
        let confirm = self.state.picker_view.as_ref().is_some_and(|picker| {
            matches!(
                picker.mode,
                PickerMode::Approval | PickerMode::Trust | PickerMode::Plan
            )
        });
        let plain = !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match code {
            KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => true,
            // Esc (or ctrl+c) closes the picker; an approval closed this way
            // is denied, as its hint says.
            // Esc first clears a typed filter, then closes.
            KeyCode::Esc
                if self
                    .state
                    .picker_view
                    .as_ref()
                    .is_some_and(|picker| !picker.filter.is_empty()) =>
            {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.clear_filter();
                }
                false
            }
            KeyCode::Esc => self.close_picker(),
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.close_picker(),
            KeyCode::Char(ch @ '1'..='9') if confirm && plain => {
                let number = ch.to_digit(10).unwrap_or(0) as usize;
                let selected = self
                    .state
                    .picker_view
                    .as_mut()
                    .is_some_and(|picker| picker.select_number(number));
                if selected {
                    return self.handle_picker_key(KeyCode::Enter, KeyModifiers::empty());
                }
                false
            }
            KeyCode::Backspace if filterable => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.pop_filter_char();
                }
                false
            }
            KeyCode::Char(ch) if filterable && plain => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.push_filter_char(ch);
                }
                false
            }
            KeyCode::Left | KeyCode::Right
                if self
                    .state
                    .picker_view
                    .as_ref()
                    .is_some_and(|picker| picker.mode == PickerMode::Model) =>
            {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    if code == KeyCode::Left {
                        picker.cycle_effort_back();
                    } else {
                        picker.cycle_effort();
                    }
                }
                false
            }
            KeyCode::Up => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.move_previous();
                }
                false
            }
            KeyCode::Down => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.move_next();
                }
                false
            }
            KeyCode::PageUp => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.page_up();
                }
                false
            }
            KeyCode::PageDown => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.page_down();
                }
                false
            }
            KeyCode::Left => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.move_parent();
                }
                false
            }
            KeyCode::Right => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.move_first_child();
                }
                false
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.cycle_effort();
                }
                false
            }
            KeyCode::Char('f') | KeyCode::Char('n')
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.begin_tree_prompt(TreePromptAction::Fork);
                }
                false
            }
            KeyCode::Delete => {
                if let Some(picker) = self.state.picker_view.as_mut() {
                    picker.begin_tree_prompt(TreePromptAction::Delete);
                }
                false
            }
            KeyCode::Enter => {
                if self
                    .state
                    .picker_view
                    .as_ref()
                    .is_some_and(PickerState::selected_wants_key)
                {
                    if let Some(picker) = self.state.picker_view.as_mut() {
                        picker.begin_key_prompt();
                    }
                    return false;
                }
                if self
                    .state
                    .picker_view
                    .as_ref()
                    .is_some_and(PickerState::selected_wants_feedback)
                {
                    if let Some(picker) = self.state.picker_view.as_mut() {
                        picker.begin_feedback_prompt();
                    }
                    return false;
                }
                let Some(command) = self
                    .state
                    .picker_view
                    .as_ref()
                    .and_then(PickerState::selected_command)
                else {
                    self.state.picker_view = None;
                    self.state.show_next_queued_approval();
                    return false;
                };
                self.state.picker_view = None;
                let (_, events) = self.runtime.handle_command(&command);
                self.apply_events(events);
                self.state.show_next_queued_approval();
                false
            }
            _ => false,
        }
    }

    /// Closes the open picker; an approval closed this way is answered
    /// "deny".
    fn close_picker(&mut self) -> bool {
        let deny = self
            .state
            .picker_view
            .as_ref()
            .and_then(PickerState::deny_command);
        self.state.picker_view = None;
        if let Some(command) = deny {
            let (_, events) = self.runtime.handle_command(&command);
            self.apply_events(events);
        }
        self.state.show_next_queued_approval();
        false
    }

    /// ctrl+c: interrupts a running turn, else clears the draft, else arms
    /// quitting; a second press within `QUIT_CONFIRM_WINDOW` quits.
    fn handle_ctrl_c(&mut self, now: Instant) -> bool {
        self.flush_paste_burst_before_non_plain_input();
        if self.state.activity.is_active() {
            self.quit_armed_at = None;
            let events = self.runtime.interrupt();
            self.apply_events(events);
            return false;
        }
        if !self.input.text().is_empty() {
            self.quit_armed_at = None;
            self.input.clear();
            self.state.completion_index = 0;
            return false;
        }
        if self
            .quit_armed_at
            .is_some_and(|at| now.saturating_duration_since(at) <= QUIT_CONFIRM_WINDOW)
        {
            return true;
        }
        self.quit_armed_at = Some(now);
        false
    }

    /// The notice above the composer while a ctrl+c waits for its second
    /// press.
    fn quit_notice(&self, now: Instant) -> Option<&'static str> {
        self.quit_armed_at
            .filter(|at| now.saturating_duration_since(*at) <= QUIT_CONFIRM_WINDOW)
            .map(|_| lang::t("再按一次 ctrl+c 退出", "press ctrl+c again to quit"))
    }

    fn build_document(&mut self, width: usize, now: Instant) -> UiDocument {
        let notice = self.quit_notice(now);
        self.state.build_document(&self.input, width, now, notice)
    }

    fn clamp_completion_index(&mut self) {
        self.state.clamp_completion_index(&self.input);
    }

    fn command_completion_active(&self) -> bool {
        self.state.command_completion_active(&self.input)
    }

    fn should_complete_on_enter(&self) -> bool {
        self.state.should_complete_on_enter(&self.input)
    }

    fn complete_selected_command(&mut self) {
        self.state.complete_selected_command(&mut self.input);
    }

    fn completion_rows_len(&mut self) -> usize {
        self.state.completion_rows(&self.input).len()
    }

    /// Replaces the `@token` before the cursor with the selected file mention.
    /// Returns true when a completion was applied.
    fn complete_selected_mention(&mut self) -> bool {
        let matches = self.state.mention_matches(&self.input);
        let Some(path) = matches.get(self.state.completion_index) else {
            return false;
        };
        let tail = self.input.tail_chars_before_cursor();
        let Some((token_chars, query)) = mention::mention_token(&tail) else {
            return false;
        };
        // The query already is the selected path: nothing to complete, let
        // Enter submit instead of demanding a second keypress.
        if *path == query {
            return false;
        }
        let path = path.clone();
        for _ in 0..token_chars {
            self.input.pop();
        }
        self.input.push_text(&format!("@{path} "));
        self.state.completion_index = 0;
        true
    }

    fn apply_events(&mut self, events: Vec<AgentEvent>) -> bool {
        self.state.apply_events(events, &mut self.input)
    }

    fn cycle_reasoning_effort(&mut self) {
        if self.state.model == "unknown" {
            return;
        }
        let next =
            next_reasoning_effort(&self.state.reasoning_efforts, &self.state.reasoning_effort);
        let (_, events) = self
            .runtime
            .handle_command(&format!("/model {} {next}", self.state.model));
        self.apply_events(events);
    }

    fn cycle_approval_mode(&mut self) {
        const ORDER: [&str; 5] = ["manual", "plan", "auto-edit", "auto", "full-access"];
        let next = ORDER
            .iter()
            .position(|mode| *mode == self.state.approval_mode)
            .map(|index| ORDER[(index + 1) % ORDER.len()])
            .unwrap_or("manual");
        let (_, events) = self.runtime.handle_command(&format!("/permissions {next}"));
        self.apply_events(events);
    }
}

impl ActivityState {
    fn idle() -> Self {
        Self {
            kind: ActivityKind::Idle,
            turn_started_at: None,
            phase_started_at: None,
            last_delta_at: None,
            estimated_output_tokens: 0,
            animation_tick: 0,
        }
    }

    fn start_connecting(&mut self) {
        let now = Instant::now();
        if self.turn_started_at.is_none() {
            self.turn_started_at = Some(now);
            self.estimated_output_tokens = 0;
            self.last_delta_at = None;
        }
        self.phase_started_at = Some(now);
        self.kind = ActivityKind::Connecting;
    }

    fn start_thinking(&mut self) {
        let now = Instant::now();
        if self.turn_started_at.is_none() {
            self.turn_started_at = Some(now);
        }
        if !matches!(self.kind, ActivityKind::Thinking) {
            self.phase_started_at = Some(now);
        }
        self.kind = ActivityKind::Thinking;
    }

    fn start_compacting(&mut self) {
        let now = Instant::now();
        if self.turn_started_at.is_none() {
            self.turn_started_at = Some(now);
            self.estimated_output_tokens = 0;
            self.last_delta_at = None;
        }
        if !matches!(self.kind, ActivityKind::Compacting) {
            self.phase_started_at = Some(now);
        }
        self.kind = ActivityKind::Compacting;
    }

    fn set_compaction_output_tokens(&mut self, output_tokens: u64) {
        if !matches!(self.kind, ActivityKind::Compacting) {
            self.start_compacting();
        }
        self.estimated_output_tokens = output_tokens;
        self.last_delta_at = Some(Instant::now());
    }

    fn start_reconnecting(&mut self, attempt: usize) {
        let now = Instant::now();
        if self.turn_started_at.is_none() {
            self.turn_started_at = Some(now);
        }
        self.phase_started_at = Some(now);
        self.kind = ActivityKind::Reconnecting { attempt };
    }

    fn start_tool(&mut self, _name: String) {
        let now = Instant::now();
        if self.turn_started_at.is_none() {
            self.turn_started_at = Some(now);
        }
        self.phase_started_at = Some(now);
        self.kind = ActivityKind::Tool;
    }

    fn add_output_delta(&mut self, delta: &str) {
        let now = Instant::now();
        if !matches!(self.kind, ActivityKind::Output) {
            self.phase_started_at = Some(now);
        }
        self.last_delta_at = Some(now);
        self.estimated_output_tokens += estimate_tokens(delta);
        self.kind = ActivityKind::Output;
    }

    fn finish(&mut self) {
        self.kind = ActivityKind::Idle;
        self.turn_started_at = None;
        self.phase_started_at = None;
        self.last_delta_at = None;
        self.estimated_output_tokens = 0;
    }

    fn is_active(&self) -> bool {
        !matches!(self.kind, ActivityKind::Idle)
    }

    fn advance_animation(&mut self) {
        self.animation_tick = self.animation_tick.wrapping_add(1);
    }

    fn progress(&self, now: Instant, thinking_tokens: u64) -> Option<ProgressState> {
        let turn_started_at = self.turn_started_at.or(self.phase_started_at)?;
        let phase_started_at = self.phase_started_at.unwrap_or(turn_started_at);
        let elapsed = now.saturating_duration_since(turn_started_at);
        let phase_elapsed = now.saturating_duration_since(phase_started_at);
        let mut stalled = false;
        let phase = match self.kind {
            ActivityKind::Idle => return None,
            ActivityKind::Connecting => lang::t("连接中", "Connecting").to_string(),
            ActivityKind::Thinking => lang::t("思考中", "Thinking").to_string(),
            ActivityKind::Compacting => lang::t("压缩上下文", "Compacting context").to_string(),
            ActivityKind::Reconnecting { attempt } => match lang::lang() {
                lang::Lang::Zh => format!("重连中（第 {attempt} 次）"),
                lang::Lang::En => format!("Reconnecting (attempt {attempt})"),
            },
            ActivityKind::Output => {
                // No new text for a while: the reply stalled upstream.
                let quiet_for = self
                    .last_delta_at
                    .map(|last| now.saturating_duration_since(last))
                    .unwrap_or(phase_elapsed);
                if quiet_for >= STALL_AFTER {
                    stalled = true;
                    match lang::lang() {
                        lang::Lang::Zh => format!("等待响应 {} 秒", quiet_for.as_secs()),
                        lang::Lang::En => format!("Waiting for the reply {}s", quiet_for.as_secs()),
                    }
                } else {
                    lang::t("输出中", "Writing").to_string()
                }
            }
            ActivityKind::Tool => lang::t("运行工具", "Running a tool").to_string(),
        };
        let mut detail = ui_builder::format_turn_elapsed(elapsed.as_secs());
        let tokens = self.estimated_output_tokens + thinking_tokens;
        if tokens > 0 {
            detail.push_str(&format!(" · ↓ {} tokens", format_token_count(tokens)));
        }
        Some(ProgressState {
            phase,
            detail,
            stalled,
        })
    }
}

/// What the progress row says: the phase, the elapsed time and tokens, and
/// whether the reply stalled (the spinner turns to the attention colour).
struct ProgressState {
    phase: String,
    detail: String,
    stalled: bool,
}

/// Looks the LynShen account up off the UI thread: at startup, and again
/// after a turn when the last lookup is older than the refresh interval.
struct AccountFetcher {
    rx: std::sync::mpsc::Receiver<AccountLookup>,
    tx: std::sync::mpsc::Sender<AccountLookup>,
    last_started: Instant,
    in_flight: bool,
    turn_since: bool,
}

impl AccountFetcher {
    fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut fetcher = Self {
            rx,
            tx,
            last_started: Instant::now(),
            in_flight: false,
            turn_since: false,
        };
        fetcher.spawn(Instant::now());
        fetcher
    }

    fn spawn(&mut self, now: Instant) {
        let tx = self.tx.clone();
        self.in_flight = true;
        self.turn_since = false;
        self.last_started = now;
        std::thread::spawn(move || {
            let _ = tx.send(lynshen_agent_core::lynshen_account());
        });
    }

    fn poll(&mut self) -> Option<AccountLookup> {
        let lookup = self.rx.try_recv().ok()?;
        self.in_flight = false;
        Some(lookup)
    }

    fn mark_turn(&mut self) {
        self.turn_since = true;
    }

    fn refresh_if_stale(&mut self, now: Instant) {
        if self.turn_since
            && !self.in_flight
            && now.saturating_duration_since(self.last_started) >= ACCOUNT_REFRESH_INTERVAL
        {
            self.spawn(now);
        }
    }
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide, EnableMouseCapture)?;
        stdout.write_all(ENABLE_BRACKETED_PASTE.as_bytes())?;
        stdout.flush()?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = stdout.write_all(DISABLE_BRACKETED_PASTE.as_bytes());
        let _ = stdout.flush();
        let _ = execute!(stdout, Show, DisableMouseCapture, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
impl RenderedFrame {
    fn build(document: &UiDocument, width: u16) -> Self {
        ProjectedDocument::from_document(document, width).into_frame()
    }
}

pub(crate) fn wrap_lines(lines: &[UiLine], width: usize) -> Vec<UiLine> {
    let width = width.max(1);
    let mut wrapped = Vec::new();
    for line in lines {
        wrap_line(line, width, &mut wrapped);
    }
    wrapped
}

pub(crate) fn padded_content_width(width: usize) -> usize {
    width.saturating_sub(CONTENT_LEFT_PADDING).max(1)
}

/// Hard-wrap at the column limit — wide chars split at grapheme boundaries,
/// so a CJK cell never tears. The click target stays on the first segment;
/// the caret column lands on whichever segment contains it.
fn wrap_line(line: &UiLine, width: usize, output: &mut Vec<UiLine>) {
    if line.line.width() <= width {
        output.push(line.clone());
        return;
    }

    let before = output.len();
    let mut click = line.click;
    let mut cursor = line.cursor;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut segment_width = 0usize;
    let mut consumed = 0usize;

    for grapheme in line.line.styled_graphemes(Style::default()) {
        let grapheme_width = grapheme.symbol.width();
        if segment_width > 0 && segment_width + grapheme_width > width {
            push_wrapped_segment(
                output,
                line.kind,
                &mut spans,
                segment_width,
                &mut consumed,
                &mut click,
                &mut cursor,
            );
            segment_width = 0;
        }
        match spans.last_mut() {
            Some(span) if span.style == grapheme.style => {
                span.content.to_mut().push_str(grapheme.symbol);
            }
            _ => spans.push(Span::styled(grapheme.symbol.to_string(), grapheme.style)),
        }
        segment_width += grapheme_width;
    }
    if !spans.is_empty() {
        push_wrapped_segment(
            output,
            line.kind,
            &mut spans,
            segment_width,
            &mut consumed,
            &mut click,
            &mut cursor,
        );
    }
    // A line of only zero-width graphemes still occupies a screen row.
    if output.len() == before {
        output.push(UiLine::new(line.kind, Line::default()));
    }
}

#[allow(clippy::too_many_arguments)]
fn push_wrapped_segment(
    output: &mut Vec<UiLine>,
    kind: UiKind,
    spans: &mut Vec<Span<'static>>,
    segment_width: usize,
    consumed: &mut usize,
    click: &mut Option<usize>,
    cursor: &mut Option<usize>,
) {
    let caret = cursor
        .filter(|column| *column <= *consumed + segment_width)
        .map(|column| column - *consumed);
    if caret.is_some() {
        *cursor = None;
    }
    *consumed += segment_width;
    output.push(UiLine {
        kind,
        line: Line::from(std::mem::take(spans)),
        click: click.take(),
        cursor: caret,
    });
}

/// The caret rides on the line carrying `cursor` — a field, not a text marker.
pub(crate) fn extract_cursor(lines: &[UiLine]) -> Option<CursorTarget> {
    for (row, line) in lines.iter().enumerate().rev() {
        if let Some(column) = line.cursor {
            return Some(CursorTarget { row, column });
        }
    }
    None
}

/// One frame of the progress spinner: braille dots, or an ASCII line where
/// the terminal's font lacks braille.
pub(crate) fn spinner_char(tick: usize) -> &'static str {
    const BRAILLE: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const ASCII: &[&str] = &["-", "\\", "|", "/"];
    let frames = if theme::rich_glyphs() { BRAILLE } else { ASCII };
    frames[tick % frames.len()]
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output
}

/// Width-aware hard truncation of a styled line; span styles survive the cut.
pub(crate) fn truncate_line_to_width(line: &Line<'static>, max: usize) -> Line<'static> {
    if line.width() <= max {
        return line.clone();
    }
    let mut spans = Vec::new();
    let mut visible = 0usize;
    'outer: for span in &line.spans {
        let mut current = String::new();
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0);
            if visible + ch_width > max {
                break;
            }
            current.push(ch);
            visible += ch_width;
        }
        if !current.is_empty() {
            spans.push(Span::styled(current, span.style));
        }
        if visible >= max {
            break 'outer;
        }
    }
    Line::from(spans)
}

/// Same, but reserves a column for a trailing `…` when content was dropped.
pub(crate) fn truncate_line_spans(line: &Line<'static>, max: usize) -> Line<'static> {
    if max == 0 {
        return Line::default();
    }
    if line.width() <= max {
        return line.clone();
    }
    let mut truncated = truncate_line_to_width(line, max - 1);
    truncated.spans.push(Span::raw("…"));
    truncated
}

/// Fallback style for a line kind; spans keep their own style where set.
/// Text uses the terminal's own colour; fills only where the theme draws
/// them.
pub(crate) fn kind_style(kind: UiKind) -> Style {
    let fill = |surface| match theme::surface(surface) {
        Some(color) => Style::new().bg(color),
        None => Style::new(),
    };
    match kind {
        UiKind::User => fill(theme::Surface::User),
        UiKind::Selected | UiKind::CandidateSelected => fill(theme::Surface::Selected),
        UiKind::Assistant
        | UiKind::ToolHeader
        | UiKind::System
        | UiKind::BottomStatus
        | UiKind::Input
        | UiKind::Candidate => Style::new(),
        UiKind::Tool | UiKind::Status => theme::dim(),
        UiKind::Error => theme::err(),
        UiKind::DiffAdd => theme::add(),
        UiKind::DiffRemove => theme::del(),
    }
}

fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count() as u64;
    u64::max(1, chars.div_ceil(4))
}

fn format_token_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.2}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{}k", value / 1_000)
    } else {
        value.to_string()
    }
}

fn next_reasoning_effort(efforts: &[String], current: &str) -> String {
    if efforts.is_empty() {
        return current.to_string();
    }
    let index = efforts
        .iter()
        .position(|effort| effort == current)
        .map(|index| (index + 1) % efforts.len())
        .unwrap_or(0);
    efforts[index].clone()
}
