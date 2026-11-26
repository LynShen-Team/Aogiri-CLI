use super::*;
use crate::git_bar::GitStatus;
use crate::lang::t;
use crate::local_shell::LocalShellResult;
use crate::picker::ApprovalView;
use crate::ui_builder::WelcomeInfo;

/// Sent messages kept for ↑/↓ recall.
const SENT_HISTORY_LIMIT: usize = 200;

pub(super) struct TuiState {
    pub(super) chat: Vec<ChatLine>,
    pub(super) history_revision: u64,
    pub(super) rendered_history_cache: RenderedHistoryCache,
    /// Index of the assistant message currently being streamed into `chat`.
    pub(super) assistant_index: Option<usize>,
    pub(super) reasoning_index: Option<usize>,
    /// When the active reasoning block started streaming; collapsed blocks keep
    /// the elapsed time on their header.
    pub(super) reasoning_started: Option<Instant>,
    pub(super) status: String,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) reasoning_effort: String,
    pub(super) context_window: u64,
    pub(super) max_output_tokens: u64,
    pub(super) reasoning_efforts: Vec<String>,
    /// Current approval mode name, tracked from `AgentEvent::ApprovalMode` so
    /// shift+tab can cycle it.
    pub(super) approval_mode: String,
    pub(super) current_context_tokens: u64,
    pub(super) current_cost: f64,
    pub(super) activity: ActivityState,
    pub(super) commands: Vec<CommandCandidate>,
    pub(super) completion_index: usize,
    pub(super) picker_view: Option<PickerState>,
    /// Approval requests waiting behind the currently shown picker; with
    /// subagents several can be pending at once.
    pub(super) queued_approvals: Vec<ApprovalView>,
    /// A proposed plan (id, title) waiting behind the shown picker.
    pub(super) queued_plan: Option<(String, String)>,
    pub(super) pending_messages: Vec<String>,
    pub(super) reset_screen: bool,
    /// Latest git branch/dirty reading for the bottom bar (None outside a repo).
    pub(super) git_status: Option<GitStatus>,
    /// Lazily-built project file list backing the `@` mention picker.
    pub(super) file_index: Option<Vec<String>>,
    cwd: std::path::PathBuf,
    /// The LynShen account behind the provider (welcome block, balance).
    pub(super) account: AccountState,
    /// Ctrl+O: every tool block shows its full output.
    pub(super) expand_tools: bool,
    /// The shortcut sheet `?` opened under the composer.
    pub(super) show_help: bool,
    /// Messages sent from this composer, oldest first, for ↑/↓.
    sent_history: Vec<String>,
    /// Position while browsing `sent_history`, and the draft it replaced.
    history_index: Option<usize>,
    history_draft: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct RenderedHistoryCache {
    revision: u64,
    width: usize,
    lines: Vec<UiLine>,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            chat: Vec::new(),
            history_revision: 0,
            rendered_history_cache: RenderedHistoryCache::default(),
            assistant_index: None,
            reasoning_index: None,
            reasoning_started: None,
            status: "ready".to_string(),
            provider: "unknown".to_string(),
            model: "unknown".to_string(),
            reasoning_effort: "medium".to_string(),
            context_window: 128_000,
            max_output_tokens: 128_000,
            reasoning_efforts: vec!["medium".to_string()],
            approval_mode: "manual".to_string(),
            current_context_tokens: 0,
            current_cost: 0.0,
            activity: ActivityState::idle(),
            commands: default_commands(),
            completion_index: 0,
            picker_view: None,
            queued_approvals: Vec::new(),
            queued_plan: None,
            pending_messages: Vec::new(),
            reset_screen: false,
            git_status: None,
            file_index: None,
            cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            account: AccountState::Unknown,
            expand_tools: false,
            show_help: false,
            sent_history: Vec::new(),
            history_index: None,
            history_draft: String::new(),
        }
    }
}

impl TuiState {
    pub(super) fn build_document(
        &mut self,
        input: &InputBuffer,
        width: usize,
        now: Instant,
        notice: Option<&str>,
    ) -> UiDocument {
        // Reserve one column for the transcript scrollbar so wrapped history lines do not
        // collide with it.
        let content_width = padded_content_width(width).saturating_sub(1).max(1);
        let control_width = width.max(1);
        let completion_rows = self.completion_rows(input);
        // Emit the caret whenever the composer takes keys: the hardware
        // cursor anchors the IME compose window, and the composer stays
        // editable while a turn streams. An open picker takes the keys.
        let input_display = input.render(self.picker_view.is_none());
        let rendered_history_lines = self.rendered_history_lines(content_width);
        // A confirmation (approval, trust, plan) takes the composer's place.
        let confirming = self.picker_view.as_ref().is_some_and(|picker| {
            matches!(
                picker.mode,
                PickerMode::Approval | PickerMode::Trust | PickerMode::Plan
            )
        });
        let placeholder = if self.picker_view.is_some() {
            None
        } else if self.activity.is_active() {
            Some(t(
                "继续输入会排队，esc 中断当前任务",
                "type to queue a message, esc to interrupt",
            ))
        } else {
            Some(t(
                "描述任务，或输入 / 查看命令",
                "describe a task, or type / for commands",
            ))
        };
        let balance = self.account.balance();
        // The working indicator's glyph follows what the turn does; it
        // breathes while a confirmation waits on the user.
        self.activity.refresh_motion(now, confirming);
        let mut builder = UiBuilder::new()
            .rendered_history_lines(rendered_history_lines)
            .picker(self.picker_view.as_ref(), control_width)
            .pending_messages(&self.pending_messages, control_width)
            .progress(
                &self.activity,
                self.awaiting_connection(),
                now,
                control_width,
            )
            .notice(notice);
        if !confirming {
            builder = builder.input(&input_display, placeholder);
        }
        let below = if self.show_help && self.picker_view.is_none() {
            builder = builder.help(control_width);
            true
        } else if !completion_rows.is_empty() && !confirming {
            builder = builder.candidates(&completion_rows, self.completion_index, control_width);
            true
        } else {
            false
        };
        if !below {
            builder = builder.bottom_status(
                BottomStatus {
                    provider: &self.provider,
                    model: &self.model,
                    reasoning_effort: &self.reasoning_effort,
                    approval_mode: &self.approval_mode,
                    git: self.git_status.as_ref(),
                    context_tokens: self.current_context_tokens,
                    context_window: self.context_window,
                    cost: self.current_cost,
                    balance: balance.as_deref(),
                },
                control_width,
            );
        }
        builder.reset_screen(self.reset_screen).finish()
    }

    /// What the welcome block shows beyond the startup event.
    pub(super) fn welcome_info(&self) -> WelcomeInfo {
        WelcomeInfo {
            provider: self.provider.clone(),
            model: self.model.clone(),
            effort: self.reasoning_effort.clone(),
            has_efforts: !self.reasoning_efforts.is_empty(),
            context_window: self.context_window,
            account: self.account.clone(),
        }
    }

    pub(super) fn rendered_history_lines(&mut self, width: usize) -> Vec<UiLine> {
        if self.rendered_history_cache.revision != self.history_revision
            || self.rendered_history_cache.width != width
        {
            let history = UiBuilder::new()
                .welcome(self.welcome_info())
                .expand_tools(self.expand_tools)
                .chat_with_width(&self.chat, width)
                .into_history();
            self.rendered_history_cache = RenderedHistoryCache {
                revision: self.history_revision,
                width,
                lines: wrap_lines(&history, width),
            };
        }
        self.rendered_history_cache.lines.clone()
    }

    /// Stores the account; true when it changed what is shown.
    pub(super) fn set_account(&mut self, account: AccountState) -> bool {
        if self.account == account {
            return false;
        }
        // A failed refresh keeps what was shown.
        if account == AccountState::Failed && matches!(self.account, AccountState::Ready(_)) {
            return false;
        }
        self.account = account;
        self.mark_history_dirty();
        true
    }

    /// A click on a thinking header or a tool header opens or closes it.
    pub(super) fn toggle_item(&mut self, index: usize) {
        match self.chat.get_mut(index) {
            Some(ChatLine::Reasoning { collapsed, .. }) => *collapsed = !*collapsed,
            Some(ChatLine::Tool { expanded, .. }) => *expanded = !*expanded,
            _ => return,
        }
        self.mark_history_dirty();
    }

    /// Ctrl+O: all tool output open, or all back to folded.
    pub(super) fn toggle_expand_tools(&mut self) {
        self.expand_tools = !self.expand_tools;
        for line in &mut self.chat {
            if let ChatLine::Tool { expanded, .. } = line {
                *expanded = false;
            }
        }
        self.mark_history_dirty();
    }

    pub(super) fn remember_sent(&mut self, text: &str) {
        self.history_index = None;
        self.history_draft.clear();
        if text.trim().is_empty() || self.sent_history.last().is_some_and(|last| last == text) {
            return;
        }
        self.sent_history.push(text.to_string());
        if self.sent_history.len() > SENT_HISTORY_LIMIT {
            self.sent_history.remove(0);
        }
    }

    /// ↑ on the composer's first line: the previous sent message (the draft
    /// is kept to come back to).
    pub(super) fn history_previous(&mut self, input: &mut InputBuffer) {
        if self.sent_history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.history_draft = input.text();
                self.sent_history.len() - 1
            }
            Some(0) => return,
            Some(index) => index - 1,
        };
        self.history_index = Some(index);
        input.clear();
        input.push_text(&self.sent_history[index]);
        self.completion_index = 0;
    }

    /// ↓ on the last line: the next sent message, then the draft again.
    pub(super) fn history_next(&mut self, input: &mut InputBuffer) {
        let Some(index) = self.history_index else {
            return;
        };
        input.clear();
        if index + 1 < self.sent_history.len() {
            self.history_index = Some(index + 1);
            input.push_text(&self.sent_history[index + 1]);
        } else {
            self.history_index = None;
            input.push_text(&std::mem::take(&mut self.history_draft));
        }
        self.completion_index = 0;
    }

    /// Any edit ends browsing: the recalled text becomes the draft.
    pub(super) fn end_history_browse(&mut self) {
        self.history_index = None;
    }

    pub(super) fn command_completion_active(&self, input: &InputBuffer) -> bool {
        let text = input.text();
        !text.contains('\n') && text.starts_with('/') && !self.command_matches(input).is_empty()
    }

    pub(super) fn should_complete_on_enter(&self, input: &InputBuffer) -> bool {
        self.command_completion_active(input)
    }

    pub(super) fn command_matches(&self, input: &InputBuffer) -> Vec<CommandCandidate> {
        let input = input.text();
        if !input.starts_with('/') || input.contains('\n') || input.contains(' ') {
            return Vec::new();
        }
        if self
            .commands
            .iter()
            .any(|candidate| candidate.command == input)
        {
            return Vec::new();
        }
        let query = &input[1..];
        crate::fuzzy::rank(&self.commands, query, |candidate| {
            candidate
                .command
                .strip_prefix('/')
                .unwrap_or(&candidate.command)
        })
        .into_iter()
        .map(|(candidate, found)| CommandCandidate {
            highlight: std::iter::once(0)
                .chain(found.indices.iter().map(|index| index + 1))
                .collect(),
            ..candidate.clone()
        })
        .collect()
    }

    pub(super) fn clamp_completion_index(&mut self, input: &InputBuffer) {
        let count = self.completion_rows(input).len();
        if count == 0 {
            self.completion_index = 0;
        } else if self.completion_index >= count {
            self.completion_index = count - 1;
        }
    }

    pub(super) fn complete_selected_command(&mut self, input: &mut InputBuffer) {
        let matches = self.command_matches(input);
        if let Some(command) = matches.get(self.completion_index) {
            input.clear();
            input.push_text(&command.command);
            input.push_char(' ');
            self.completion_index = 0;
        }
    }

    /// Rows for the completion list under the input: slash-command matches
    /// when typing a command, `@` file mentions otherwise.
    pub(super) fn completion_rows(&mut self, input: &InputBuffer) -> Vec<CommandCandidate> {
        let commands = self.command_matches(input);
        if !commands.is_empty() {
            return commands;
        }
        let query = crate::mention::mention_token(&input.tail_chars_before_cursor())
            .map(|(_, query)| query)
            .unwrap_or_default();
        self.mention_matches(input)
            .into_iter()
            .map(|path| {
                let highlight = crate::fuzzy::fuzzy_match(&path, &query)
                    .map(|found| found.indices.iter().map(|index| index + 1).collect())
                    .unwrap_or_default();
                CommandCandidate {
                    command: format!("@{path}"),
                    highlight,
                    ..CommandCandidate::default()
                }
            })
            .collect()
    }

    /// Fuzzy file matches for an `@token` ending at the cursor. Builds the
    /// project file index on first use.
    pub(super) fn mention_matches(&mut self, input: &InputBuffer) -> Vec<String> {
        let tail = input.tail_chars_before_cursor();
        let Some((_, query)) = crate::mention::mention_token(&tail) else {
            return Vec::new();
        };
        let files = self
            .file_index
            .get_or_insert_with(|| crate::mention::list_project_files(&self.cwd));
        crate::mention::fuzzy_filter(files, &query, crate::mention::MAX_MENTION_MATCHES)
    }

    pub(super) fn begin_local_shell(&mut self, call_id: &str, command: &str) {
        self.chat.push(ChatLine::User(format!("!{command}")));
        self.upsert_tool(
            call_id.to_string(),
            format!("! {command}"),
            String::new(),
            true,
        );
    }

    pub(super) fn finish_local_shell(&mut self, result: LocalShellResult) {
        if let Some(ChatLine::Tool {
            output, running, ..
        }) = self.chat.iter_mut().find(|line| {
            matches!(
                line,
                ChatLine::Tool {
                    call_id: Some(existing),
                    ..
                } if existing == &result.call_id
            )
        }) {
            *output = result.output;
            *running = false;
            self.mark_history_dirty();
        }
    }
}

impl TuiState {
    pub(super) fn apply_events(
        &mut self,
        events: Vec<AgentEvent>,
        input: &mut InputBuffer,
    ) -> bool {
        let mut changed = false;
        for event in events {
            changed |= match event {
                AgentEvent::Startup {
                    version,
                    session_id: _,
                    profile_dir: _,
                    config_path: _,
                    cwd,
                    model,
                    context_window,
                } => {
                    self.push_startup(version, cwd, model, context_window);
                    true
                }
                AgentEvent::ModelStatus {
                    provider,
                    model,
                    model_label: _,
                    reasoning_effort,
                    context_window,
                    context_limit: _,
                    max_output_tokens,
                    reasoning_efforts,
                    input_modalities: _,
                    state,
                } => {
                    let changed = self.provider != provider
                        || self.model != model
                        || self.reasoning_effort != reasoning_effort
                        || self.context_window != context_window
                        || self.max_output_tokens != max_output_tokens
                        || self.reasoning_efforts != reasoning_efforts;
                    self.provider = provider;
                    self.model = model;
                    self.reasoning_effort = reasoning_effort;
                    self.context_window = context_window;
                    self.max_output_tokens = max_output_tokens;
                    self.reasoning_efforts = reasoning_efforts;
                    if changed {
                        // The welcome block names the model and effort.
                        self.mark_history_dirty();
                    }
                    self.apply_status(state) || changed
                }
                AgentEvent::PendingMessages(messages) => {
                    let changed = self.pending_messages != messages;
                    self.pending_messages = messages;
                    changed
                }
                // A queued message taken back goes back to the input to edit.
                AgentEvent::Unqueued { content, .. } => {
                    input.clear();
                    input.push_text(&content);
                    self.completion_index = 0;
                    true
                }
                // The TUI shows its own turn timer and usage.
                AgentEvent::TurnStats(_) => false,
                AgentEvent::UserMessage(message) => {
                    self.chat.push(ChatLine::PendingUser(message));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::FillInput(content) => {
                    input.clear();
                    input.push_text(&content);
                    self.completion_index = 0;
                    true
                }
                AgentEvent::Connecting => {
                    self.begin_reasoning_turn_if_idle();
                    self.activity.start_connecting();
                    true
                }
                AgentEvent::CompactionStart => {
                    self.activity.start_compacting();
                    self.chat.push(ChatLine::System(
                        t(
                            "正在压缩之前的对话，腾出上下文…",
                            "Compacting earlier conversation to free up context…",
                        )
                        .to_string(),
                    ));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::CompactionProgress { .. } => {
                    self.activity.note_compaction_progress();
                    true
                }
                AgentEvent::CompactionEnd { .. } => {
                    self.chat.push(ChatLine::System(
                        t("上下文已压缩。", "Context compacted.").to_string(),
                    ));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::CompactionFailed(error) => {
                    self.chat.push(ChatLine::System(match crate::lang::lang() {
                        crate::lang::Lang::Zh => {
                            format!("压缩上下文失败（{error}），继续使用完整上下文。")
                        }
                        crate::lang::Lang::En => format!(
                            "Context compaction failed ({error}); continuing with full context."
                        ),
                    }));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::ContextUsage {
                    tokens,
                    cost,
                    breakdown,
                    ..
                } => {
                    // The whole request: the prompt and tools count too.
                    let tokens = breakdown.map_or(tokens, |b| {
                        b.system_prompt + b.skills + b.system_tools + b.mcp_tools + b.messages
                    });
                    let changed =
                        self.current_context_tokens != tokens || self.current_cost != cost;
                    self.current_context_tokens = tokens;
                    self.current_cost = cost;
                    changed
                }
                AgentEvent::ThinkingStart => {
                    self.confirm_pending_user();
                    self.begin_reasoning_turn_if_idle();
                    self.activity.start_thinking();
                    true
                }
                AgentEvent::ReasoningDelta(delta) => {
                    self.activity.start_thinking();
                    self.append_thinking_delta(&delta);
                    true
                }
                AgentEvent::Retrying { attempt, .. } => {
                    // The request is re-sent from scratch, so drop any partial
                    // streamed output to avoid duplicating it on the retry.
                    self.discard_partial_assistant();
                    self.discard_partial_reasoning();
                    self.activity.start_reconnecting(attempt);
                    true
                }
                AgentEvent::AssistantStart => {
                    self.collapse_live_thinking();
                    self.assistant_index = None;
                    true
                }
                AgentEvent::AssistantDelta(delta) => {
                    self.collapse_live_thinking();
                    self.activity.add_output_delta();
                    self.append_assistant_delta(&delta);
                    true
                }
                // Cut off for a steered message: what streamed stays shown,
                // marked, and the next response starts a block of its own.
                AgentEvent::ResponseInterrupted => {
                    self.collapse_live_thinking();
                    self.assistant_index = None;
                    self.chat.push(ChatLine::System(
                        t("回答被打断了。", "Response interrupted.").to_string(),
                    ));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::ToolStart { call_id, name } => {
                    self.collapse_live_thinking();
                    self.activity.start_tool(name.clone());
                    self.upsert_tool(call_id, name, String::new(), true);
                    true
                }
                AgentEvent::ToolUpdate {
                    call_id,
                    name,
                    output,
                } => {
                    self.collapse_live_thinking();
                    self.activity.start_tool(name.clone());
                    self.upsert_tool(call_id, name, output, true);
                    true
                }
                AgentEvent::ToolOutput {
                    call_id,
                    name,
                    output,
                    ..
                } => {
                    self.collapse_live_thinking();
                    self.activity.start_connecting();
                    self.upsert_tool(call_id, name, output, false);
                    true
                }
                AgentEvent::SubagentLifecycle {
                    path,
                    status,
                    message,
                    ..
                } => {
                    self.chat.push(ChatLine::System(format!(
                        "Agent {path}: {status} — {message}"
                    )));
                    self.mark_history_dirty();
                    true
                }
                // The working indicator shows no token counts.
                AgentEvent::Usage { .. } => false,
                AgentEvent::TreeView(nodes) => {
                    self.picker_view = Some(PickerState::checkout(nodes));
                    true
                }
                AgentEvent::ResumeView(sessions) => {
                    self.picker_view = Some(PickerState::resume(sessions));
                    true
                }
                AgentEvent::CheckpointView(items) => {
                    self.picker_view = Some(PickerState::checkpoint(items));
                    true
                }
                AgentEvent::ApprovalRequest {
                    call_id,
                    name,
                    summary,
                    subagent_id,
                    hunks,
                    info,
                } => {
                    // Stop the thinking clock here: time spent waiting on the
                    // user's decision is not the model's thinking time.
                    self.collapse_live_thinking();
                    // Hunk-by-hunk selection is a GUI feature; the TUI picker
                    // stays whole-call allow/deny but surfaces the hunk count
                    // (a partial decision can still be typed:
                    // /approve <call-id> allow --hunks f0h1,f0h2).
                    let view = ApprovalView {
                        call_id,
                        name,
                        summary,
                        command: info.command.clone(),
                        cwd: Some(info.cwd.display().to_string()),
                        reason: info.justification.clone(),
                        prefix: info.prefix.clone(),
                        subagent: subagent_id,
                        hunks: hunks.as_deref().map_or(0, <[_]>::len),
                    };
                    if self.picker_view.is_some() {
                        self.queued_approvals.push(view);
                    } else {
                        self.picker_view = Some(PickerState::approval_with(view));
                    }
                    true
                }
                AgentEvent::ApprovalMode { mode } => {
                    self.approval_mode = mode.clone();
                    let (label, _) = crate::ui_builder::approval_mode_label(&mode);
                    self.chat.push(ChatLine::System(format!(
                        "{}{label}",
                        t("批准模式：", "approval mode: ")
                    )));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::TrustPrompt { cwd, repo_root } => {
                    self.picker_view = Some(PickerState::trust(cwd, repo_root));
                    true
                }
                AgentEvent::ModelView {
                    models,
                    active_effort,
                } => {
                    self.picker_view = Some(PickerState::model(models, active_effort));
                    true
                }
                AgentEvent::LoginPicker(providers) => {
                    self.picker_view = Some(PickerState::login(providers));
                    true
                }
                AgentEvent::LoginPastePrompt { .. } => {
                    self.picker_view = Some(PickerState::login_paste());
                    true
                }
                AgentEvent::CommandList(commands) => {
                    self.commands = commands.into_iter().map(CommandCandidate::from).collect();
                    self.clamp_completion_index(input);
                    true
                }
                AgentEvent::Goal(goal) => {
                    self.chat.push(ChatLine::System(format_goal_summary(goal)));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::Plan(items) => {
                    self.chat
                        .push(ChatLine::System(format_plan_summary(&items)));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::ProposedPlan {
                    id,
                    title,
                    markdown,
                    status,
                } => {
                    self.chat.push(ChatLine::System(format!(
                        "Plan ({status}): {title}\n{markdown}"
                    )));
                    self.mark_history_dirty();
                    // A pending plan waits for the user: approve or revise it.
                    if status == "pending" {
                        self.collapse_live_thinking();
                        if self.picker_view.is_some() {
                            self.queued_plan = Some((id, title));
                        } else {
                            self.picker_view = Some(PickerState::plan(id, title));
                        }
                    } else if self
                        .queued_plan
                        .as_ref()
                        .is_some_and(|(queued, _)| *queued == id)
                    {
                        self.queued_plan = None;
                    }
                    true
                }
                // The TUI shows the plan once it is proposed, not while written.
                AgentEvent::PlanDraft { .. } => false,
                // Structured MCP state is for GUI front-ends; the TUI relies on
                // the accompanying Info lines (and /mcp) instead.
                AgentEvent::McpServers { .. } | AgentEvent::ContextBreakdown { .. } => false,
                // The agent trace is for GUI front-ends (the TUI has /subagents).
                AgentEvent::AgentRuns(_)
                | AgentEvent::TaskBoard(_)
                | AgentEvent::SubagentTranscript { .. }
                | AgentEvent::AgentMessage { .. }
                | AgentEvent::MergeResult { .. }
                | AgentEvent::TeamBudget { .. } => false,
                AgentEvent::Transcript(items) => {
                    self.replace_transcript(items);
                    true
                }
                AgentEvent::Info(message) => {
                    self.chat.push(ChatLine::System(message));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::Error(error) => {
                    self.confirm_pending_user();
                    self.collapse_live_thinking();
                    self.commit_live_assistant();
                    self.activity.finish();
                    self.chat.push(ChatLine::Error(error));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::Status(status) => self.apply_status(status),
                AgentEvent::ActionDeferred(action) => {
                    self.chat.push(ChatLine::System(format!(
                        "deferred {} for confirmation: {} ({})",
                        action.id, action.name, action.summary
                    )));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::ActionDecided { id, allow, .. } => {
                    self.chat.push(ChatLine::System(format!(
                        "deferred action {id} {}",
                        if allow { "approved" } else { "declined" }
                    )));
                    self.mark_history_dirty();
                    true
                }
                AgentEvent::Attended(_) | AgentEvent::Sandbox(_) | AgentEvent::ReplyStyle(_) => {
                    false
                }
                AgentEvent::Escalated { info, .. } => {
                    let what = match (&info.command, info.paths.first()) {
                        (Some(command), _) => command.clone(),
                        (None, Some(path)) => format!("write {}", path.display()),
                        (None, None) => info.kind.as_str().to_string(),
                    };
                    let line = match &info.justification {
                        Some(reason) => format!("outside the sandbox: {what} — {reason}"),
                        None => format!("outside the sandbox: {what}"),
                    };
                    self.chat.push(ChatLine::System(line));
                    self.mark_history_dirty();
                    true
                }
            };
        }
        changed
    }

    /// Surface the next queued approval request once no picker is showing.
    /// Surface the next queued approval request (then a queued plan) once no
    /// picker is showing.
    pub(super) fn show_next_queued_approval(&mut self) {
        if self.picker_view.is_some() {
            return;
        }
        if !self.queued_approvals.is_empty() {
            let view = self.queued_approvals.remove(0);
            self.picker_view = Some(PickerState::approval_with(view));
        } else if let Some((id, title)) = self.queued_plan.take() {
            self.picker_view = Some(PickerState::plan(id, title));
        }
    }

    /// The request connected (or the turn ended without connecting): show
    /// the sent message at full brightness.
    pub(super) fn confirm_pending_user(&mut self) -> bool {
        let mut changed = false;
        for line in &mut self.chat {
            if let ChatLine::PendingUser(text) = line {
                *line = ChatLine::User(std::mem::take(text));
                changed = true;
            }
        }
        if changed {
            self.mark_history_dirty();
        }
        changed
    }

    /// True while a sent message waits for its request to connect; the dim
    /// message itself shows that, so no separate connecting line is drawn.
    pub(super) fn awaiting_connection(&self) -> bool {
        self.chat
            .iter()
            .any(|line| matches!(line, ChatLine::PendingUser(_)))
    }

    pub(super) fn apply_status(&mut self, status: String) -> bool {
        let mut changed = self.status != status;
        if status == "ready" || status == "interrupted" {
            changed |= self.confirm_pending_user();
        }
        if status == "ready" || status == "interrupted" || status.starts_with("queued:") {
            // The reasoning message (collapsed) stays in the transcript, but the
            // above-input status indicator is reset once the reply is done.
            self.collapse_live_thinking();
            changed |= self.commit_live_assistant();
            self.reasoning_index = None;
        }
        if status == "ready" || status == "interrupted" {
            let was_active = self.activity.is_active();
            self.activity.finish();
            changed |= was_active;
        }
        self.status = status;
        changed
    }

    /// Stream the reply straight into its transcript message, exactly where it
    /// stays — there is no separate live layer to merge later.
    pub(super) fn append_assistant_delta(&mut self, delta: &str) {
        if let Some(index) = self.assistant_index {
            if let Some(ChatLine::Assistant(text)) = self.chat.get_mut(index) {
                text.push_str(delta);
                self.mark_history_dirty();
                return;
            }
        }
        self.chat.push(ChatLine::Assistant(delta.to_string()));
        self.assistant_index = Some(self.chat.len() - 1);
        self.mark_history_dirty();
    }

    /// Drop a partial assistant message before a retry re-streams it.
    pub(super) fn discard_partial_assistant(&mut self) {
        if let Some(index) = self.assistant_index.take() {
            if matches!(self.chat.get(index), Some(ChatLine::Assistant(_))) {
                if index + 1 == self.chat.len() {
                    self.chat.pop();
                } else if let Some(ChatLine::Assistant(text)) = self.chat.get_mut(index) {
                    text.clear();
                }
                self.mark_history_dirty();
            }
        }
    }

    /// Stream reasoning into a transcript message. Deltas keep appending to the
    /// active block even if the user collapsed it by hand; once reasoning ends
    /// (index cleared) a later delta starts a new block, e.g. after a tool call.
    pub(super) fn append_thinking_delta(&mut self, delta: &str) {
        if let Some(index) = self.reasoning_index {
            if let Some(ChatLine::Reasoning { text, .. }) = self.chat.get_mut(index) {
                text.push_str(delta);
                self.mark_history_dirty();
                return;
            }
        }
        self.chat.push(ChatLine::Reasoning {
            text: delta.to_string(),
            collapsed: false,
            duration_secs: None,
        });
        self.reasoning_index = Some(self.chat.len() - 1);
        self.reasoning_started = Some(Instant::now());
        self.mark_history_dirty();
    }

    pub(super) fn begin_reasoning_turn_if_idle(&mut self) {
        if self.status == "ready" || !self.activity.is_active() {
            self.reset_thinking();
        }
    }

    /// Forget the current reasoning message (next turn).
    pub(super) fn reset_thinking(&mut self) {
        self.reasoning_index = None;
        self.reasoning_started = None;
    }

    /// Reasoning finished: collapse its transcript message to the duration header.
    pub(super) fn collapse_live_thinking(&mut self) {
        if let Some(index) = self.reasoning_index.take() {
            let elapsed = self
                .reasoning_started
                .take()
                .map(|started| started.elapsed().as_secs());
            if let Some(ChatLine::Reasoning {
                collapsed,
                duration_secs,
                ..
            }) = self.chat.get_mut(index)
            {
                *collapsed = true;
                *duration_secs = elapsed;
                self.mark_history_dirty();
            }
        }
    }

    /// Drop a partial reasoning message before a retry re-streams it.
    pub(super) fn discard_partial_reasoning(&mut self) {
        if let Some(index) = self.reasoning_index.take() {
            if matches!(
                self.chat.get(index),
                Some(ChatLine::Reasoning {
                    collapsed: false,
                    ..
                })
            ) {
                if index + 1 == self.chat.len() {
                    self.chat.pop();
                    self.mark_history_dirty();
                } else if let Some(ChatLine::Reasoning { text, .. }) = self.chat.get_mut(index) {
                    text.clear();
                    self.mark_history_dirty();
                }
            }
        }
    }

    /// The reply already lives in `chat`; finishing it just releases the
    /// streaming index and drops a message that stayed empty.
    pub(super) fn commit_live_assistant(&mut self) -> bool {
        let Some(index) = self.assistant_index.take() else {
            return false;
        };
        if matches!(
            self.chat.get(index),
            Some(ChatLine::Assistant(text)) if text.trim().is_empty()
        ) {
            self.chat.remove(index);
        }
        self.mark_history_dirty();
        true
    }

    pub(super) fn upsert_tool(
        &mut self,
        call_id: String,
        name: String,
        output: String,
        running: bool,
    ) {
        if let Some(ChatLine::Tool {
            name: existing_name,
            output: existing_output,
            running: existing_running,
            ..
        }) = self.chat.iter_mut().find(|line| {
            matches!(
                line,
                ChatLine::Tool {
                    call_id: Some(existing),
                    ..
                } if existing == &call_id
            )
        }) {
            *existing_name = name;
            *existing_output = output;
            *existing_running = running;
            self.mark_history_dirty();
            return;
        }

        self.chat.push(ChatLine::Tool {
            call_id: Some(call_id),
            name,
            output,
            running,
            expanded: false,
        });
        self.mark_history_dirty();
    }

    pub(super) fn replace_transcript(&mut self, items: Vec<(TranscriptItem, u64)>) {
        self.commit_live_assistant();
        self.reset_thinking();
        self.chat = items
            .into_iter()
            .filter_map(|(item, _)| {
                Some(match item {
                    TranscriptItem::User(text) => ChatLine::User(text),
                    TranscriptItem::UserWithImages { content, images } => {
                        ChatLine::User(match images.len() {
                            1 => format!("{content}\n[image attached]"),
                            n => format!("{content}\n[{n} images attached]"),
                        })
                    }
                    TranscriptItem::Assistant(text) => ChatLine::Assistant(text),
                    TranscriptItem::Tool { name, output } => ChatLine::Tool {
                        call_id: None,
                        name,
                        output,
                        running: false,
                        expanded: false,
                    },
                    TranscriptItem::Branch(label) => ChatLine::System(label),
                    TranscriptItem::Compaction { .. } => {
                        ChatLine::System(t("上下文已压缩。", "Context compacted.").to_string())
                    }
                    TranscriptItem::Plan {
                        title,
                        content,
                        status,
                        ..
                    } => ChatLine::System(format!("Plan ({status}): {title}\n{content}")),
                    // The TUI shows no per-turn figures.
                    TranscriptItem::TurnStats(_) => return None,
                })
            })
            .collect();
        // The resumed conversation's own messages come back with ↑.
        self.sent_history = self
            .chat
            .iter()
            .filter_map(|line| match line {
                ChatLine::User(text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        self.history_index = None;
        self.reset_screen = true;
        self.mark_history_dirty();
    }

    pub(super) fn push_startup(
        &mut self,
        version: String,
        cwd: String,
        model: String,
        context_window: u64,
    ) {
        self.chat.push(ChatLine::Startup {
            version,
            cwd,
            model,
            context_window,
        });
        self.mark_history_dirty();
    }

    pub(super) fn mark_history_dirty(&mut self) {
        self.history_revision = self.history_revision.wrapping_add(1);
    }
}
