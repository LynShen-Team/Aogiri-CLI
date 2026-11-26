use std::time::Instant;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::lang::t;
use crate::markdown::render_markdown;
use crate::picker::{PickerMode, PickerState, TreePromptAction};
use crate::theme;
use crate::tool_preview::{tool_view, BodyMode, ToolState};
use crate::{
    compact_home_path, format_context_window, truncate_line_spans, truncate_line_to_width,
    truncate_to_width, AccountState, ActivityState, BottomStatus, ChatLine, CommandCandidate,
    UiDocument, UiKind, UiLine,
};

/// Picker rows rendered at once; longer lists window around the selection.
pub(crate) const PICKER_MAX_ROWS: usize = 12;
/// Command-palette rows rendered at once.
pub(crate) const PALETTE_MAX_ROWS: usize = 8;
/// Shell and generic tool output: lines kept when a block is collapsed.
pub(crate) const TOOL_TAIL_LINES: usize = 4;
/// Diff lines kept when an edit block is collapsed.
pub(crate) const DIFF_HEAD_LINES: usize = 12;
/// Lines an expanded block shows at most.
pub(crate) const TOOL_EXPANDED_LINES: usize = 400;
/// Below this width (or with plain glyphs) the welcome block drops the mark.
pub(crate) const LOGO_MIN_WIDTH: usize = 48;

/// The LynShen mark (04 林光) in quadrant blocks, four rows: two trunks in
/// the text colour and the light between them in the brand accent.
const LOGO: [&[(&str, bool)]; 4] = [
    &[("  ", false), ("▄", true), ("  ", false), ("▐▌", false)],
    &[("▗▖", false), ("▝▙", true), (" ", false), ("▐▌", false)],
    &[("▐▌", false), (" ", false), ("▝▘", true), ("▐▌", false)],
    &[("▐▌", false), ("   ", false), ("▐▌", false)],
];
const LOGO_WIDTH: usize = 7;

/// What the welcome block shows besides the startup event's own fields.
#[derive(Debug, Clone, Default)]
pub(crate) struct WelcomeInfo {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) has_efforts: bool,
    pub(crate) context_window: u64,
    pub(crate) account: AccountState,
}

fn format_thinking_duration(secs: u64) -> String {
    match (crate::lang::is_zh(), secs >= 60) {
        (true, true) => format!("{} 分 {} 秒", secs / 60, secs % 60),
        (true, false) => format!("{secs} 秒"),
        (false, true) => format!("{}m {}s", secs / 60, secs % 60),
        (false, false) => format!("{secs}s"),
    }
}

/// The reasoning effort as the UI names it.
pub(crate) fn effort_label(effort: &str) -> String {
    if !crate::lang::is_zh() {
        return effort.to_string();
    }
    match effort {
        "none" => "不思考",
        "minimal" => "最少",
        "low" => "低",
        "medium" => "中",
        "high" => "高",
        "xhigh" => "超高",
        "max" => "Max",
        "ultra" => "Ultra",
        other => other,
    }
    .to_string()
}

/// The approval mode as the UI names it, with its colour: muted for manual,
/// blue for plan, green when edits run unasked, attention once a classifier
/// decides, red for full access.
pub(crate) fn approval_mode_label(mode: &str) -> (&'static str, Style) {
    match mode {
        "plan" => (t("计划模式", "plan"), theme::info()),
        "auto-edit" => (t("自动改文件", "auto-edit"), theme::add()),
        "auto" => (t("自动批准", "auto"), theme::warn()),
        "full-access" => (t("完全放开", "full access"), theme::err()),
        _ => (t("手动批准", "manual"), theme::dim()),
    }
}

/// A balance as `¥128.40` / `$12.30` / `128.40 EUR`.
pub(crate) fn format_balance(balance: &str, currency: Option<&str>) -> String {
    let amount = balance
        .trim()
        .parse::<f64>()
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|_| balance.trim().to_string());
    match currency.map(str::trim) {
        Some("CNY") | Some("RMB") => format!("¥{amount}"),
        Some("USD") => format!("${amount}"),
        Some(code) if !code.is_empty() => format!("{amount} {code}"),
        _ => amount,
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

/// Left/right status layout; both sides keep their own span styles.
fn format_status_line(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    let width = width.max(1);
    let left_width = spans_width(&left);
    let right_width = spans_width(&right);
    if left_width + 1 + right_width <= width {
        let mut spans = left;
        spans.push(Span::raw(" ".repeat(width - left_width - right_width)));
        spans.extend(right);
        return Line::from(spans);
    }
    if right_width >= width {
        return truncate_line_to_width(&Line::from(right), width);
    }
    let left_budget = width - right_width - 1;
    let mut spans = truncate_line_to_width(&Line::from(left), left_budget).spans;
    let used = spans_width(&spans);
    spans.push(Span::raw(" ".repeat(width - used - right_width)));
    spans.extend(right);
    Line::from(spans)
}

/// A full-width rule with an optional label: `── 需要你批准 ────`.
fn labeled_rule(label: Option<&str>, style: Style, width: usize) -> Line<'static> {
    let Some(label) = label else {
        return Line::from(Span::styled("─".repeat(width.max(1)), style));
    };
    let lead = "── ";
    let used = UnicodeWidthStr::width(lead) + UnicodeWidthStr::width(label) + 1;
    Line::from(vec![
        Span::styled(lead, style),
        Span::styled(label.to_string(), style.add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" {}", "─".repeat(width.saturating_sub(used))),
            style,
        ),
    ])
}

/// Text with the characters at `indices` in the accent (bold), the rest in
/// `base`.
fn highlighted(text: &str, indices: &[usize], base: Style) -> Vec<Span<'static>> {
    if indices.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let hit = theme::accent().add_modifier(Modifier::BOLD);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_hit = false;
    for (index, ch) in text.chars().enumerate() {
        let is_hit = indices.contains(&index);
        if is_hit != run_hit && !run.is_empty() {
            let style = if run_hit { base.patch(hit) } else { base };
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        run_hit = is_hit;
        run.push(ch);
    }
    if !run.is_empty() {
        let style = if run_hit { base.patch(hit) } else { base };
        spans.push(Span::styled(run, style));
    }
    spans
}

/// Key hints `↑↓ 选择 · enter 确认`: keys in the text colour, words dim.
fn key_hints(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    for (index, (key, label)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", theme::faint()));
        }
        spans.push(Span::styled((*key).to_string(), theme::dim()));
        spans.push(Span::styled(format!(" {label}"), theme::dim()));
    }
    Line::from(spans)
}

pub(crate) struct UiBuilder {
    history: Vec<UiLine>,
    rendered_history_lines: Option<Vec<UiLine>>,
    controls: Vec<UiLine>,
    reset_screen: bool,
    welcome: Option<WelcomeInfo>,
    expand_tools: bool,
}

impl UiBuilder {
    pub(crate) fn new() -> Self {
        Self {
            history: Vec::new(),
            rendered_history_lines: None,
            controls: Vec::new(),
            reset_screen: false,
            welcome: None,
            expand_tools: false,
        }
    }

    pub(crate) fn rendered_history_lines(mut self, lines: Vec<UiLine>) -> Self {
        self.rendered_history_lines = Some(lines);
        self
    }

    pub(crate) fn welcome(mut self, welcome: WelcomeInfo) -> Self {
        self.welcome = Some(welcome);
        self
    }

    /// Ctrl+O: every tool block shows its full output.
    pub(crate) fn expand_tools(mut self, expand: bool) -> Self {
        self.expand_tools = expand;
        self
    }

    #[cfg(test)]
    pub(crate) fn chat(self, chat: &[ChatLine]) -> Self {
        self.chat_with_width(chat, usize::MAX)
    }

    pub(crate) fn chat_with_width(mut self, chat: &[ChatLine], width: usize) -> Self {
        for (item_index, item) in chat.iter().enumerate() {
            self.separate_block();
            match item {
                ChatLine::Startup {
                    version,
                    cwd,
                    model,
                    context_window,
                    ..
                } => self.push_welcome(version, cwd, model, *context_window, width),
                ChatLine::User(text) => self.push_user_message(text, Style::new()),
                ChatLine::PendingUser(text) => self.push_user_message(text, theme::dim()),
                ChatLine::Assistant(text) => {
                    for line in render_markdown(text, width) {
                        self.history_line(UiKind::Assistant, line);
                    }
                }
                ChatLine::Reasoning {
                    text,
                    collapsed,
                    duration_secs,
                } => {
                    // Clicking the header toggles the block; collapsed keeps
                    // only the header with its recorded duration.
                    let header = match duration_secs {
                        Some(secs) => format!(
                            "∴ {}{}",
                            t("思考了 ", "thought for "),
                            format_thinking_duration(*secs)
                        ),
                        None if *collapsed => format!("∴ {}", t("思考过程", "thought")),
                        None => format!("∴ {}", t("思考中", "thinking")),
                    };
                    self.history_clickable_line(
                        UiKind::Status,
                        Line::from(Span::styled(header, theme::dim())),
                        item_index,
                    );
                    if !*collapsed {
                        for line in render_markdown(text, width.saturating_sub(2)) {
                            let mut spans = vec![Span::raw("  ")];
                            spans.extend(line.spans);
                            self.history_line(UiKind::Status, Line::from(spans));
                        }
                    }
                }
                ChatLine::Tool {
                    name,
                    output,
                    running,
                    expanded,
                    ..
                } => self.push_tool_block(
                    name,
                    output,
                    *running,
                    *expanded || self.expand_tools,
                    item_index,
                    width,
                ),
                ChatLine::System(text) => self.push_history(UiKind::System, text),
                ChatLine::Error(text) => self.push_history(UiKind::Error, text),
            }
        }
        self
    }

    pub(crate) fn picker(mut self, picker: Option<&PickerState>, width: usize) -> Self {
        let Some(picker) = picker else {
            return self;
        };
        match picker.mode {
            PickerMode::Approval | PickerMode::Trust | PickerMode::Plan => {
                self.push_confirm_picker(picker, width)
            }
            _ => self.push_list_picker(picker, width),
        }
        self
    }

    /// Approval, trust and plan pickers: a block in the attention colour
    /// with what is asked, numbered options and the keys.
    fn push_confirm_picker(&mut self, picker: &PickerState, width: usize) {
        let rule = theme::warn();
        let label = match picker.mode {
            PickerMode::Approval => t("需要你批准", "Needs your approval"),
            PickerMode::Trust => t("信任这个项目吗", "Trust this project?"),
            _ => t("计划待批准", "Plan waiting for approval"),
        };
        self.control_line(UiKind::System, labeled_rule(Some(label), rule, width));
        self.control_line(UiKind::System, Line::default());
        if let Some(title) = &picker.title {
            self.control_line(
                UiKind::System,
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(title.clone(), Style::new().add_modifier(Modifier::BOLD)),
                ]),
            );
        }
        if let Some(context) = &picker.context {
            // The payload (a shell command or path) can be long; keep each
            // of its first lines to one row with an ellipsis.
            for line in context.lines().take(3) {
                let available = width.saturating_sub(5);
                let text = if UnicodeWidthStr::width(line) > available {
                    format!("{}…", truncate_to_width(line, available.saturating_sub(1)))
                } else {
                    line.to_string()
                };
                self.control_line(
                    UiKind::System,
                    Line::from(vec![Span::raw("    "), Span::styled(text, theme::code())]),
                );
            }
        }
        if let Some(detail) = picker.detail.as_ref().filter(|detail| !detail.is_empty()) {
            let text = truncate_to_width(detail, width.saturating_sub(5));
            self.control_line(
                UiKind::System,
                Line::from(vec![Span::raw("    "), Span::styled(text, theme::dim())]),
            );
        }
        self.control_line(UiKind::System, Line::default());
        if let Some(prompt) = picker.prompt.as_ref() {
            self.push_picker_prompt(prompt.action, &prompt.input);
        } else {
            for (index, row) in picker.rows.iter().enumerate() {
                let selected = index == picker.selected;
                let mut spans = vec![if selected {
                    Span::styled("  › ", theme::accent().add_modifier(Modifier::BOLD))
                } else {
                    Span::raw("    ")
                }];
                let number_style = if selected {
                    theme::accent()
                } else {
                    theme::dim()
                };
                spans.push(Span::styled(format!("{}  ", index + 1), number_style));
                let label_style = if selected {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                spans.push(Span::styled(row.label.clone(), label_style));
                if !row.detail.is_empty() {
                    spans.push(Span::styled(format!("  {}", row.detail), theme::dim()));
                }
                let line = truncate_line_spans(&Line::from(spans), width);
                self.control_line(UiKind::System, line);
            }
        }
        self.control_line(UiKind::System, Line::default());
        let count = picker.rows.len().clamp(1, 9).to_string();
        let digits = format!("1-{count}");
        let hints: Vec<(&str, &str)> = if picker.prompt.is_some() {
            vec![("enter", t("提交", "submit")), ("esc", t("返回", "back"))]
        } else {
            match picker.mode {
                PickerMode::Approval => vec![
                    ("↑↓", t("选择", "move")),
                    ("enter", t("确认", "select")),
                    (digits.as_str(), t("直接选", "pick")),
                    ("esc", t("拒绝", "deny")),
                ],
                PickerMode::Plan => vec![
                    ("↑↓", t("选择", "move")),
                    ("enter", t("确认", "select")),
                    (
                        "esc",
                        t("稍后决定（/plan 再打开）", "decide later (/plan reopens)"),
                    ),
                ],
                _ => vec![
                    ("↑↓", t("选择", "move")),
                    ("enter", t("确认", "select")),
                    (digits.as_str(), t("直接选", "pick")),
                ],
            }
        };
        self.control_line(UiKind::System, key_hints(&hints));
        self.control_line(UiKind::System, labeled_rule(None, rule, width));
    }

    fn push_picker_prompt(&mut self, action: TreePromptAction, input: &str) {
        let label = match action {
            TreePromptAction::Fork => t("新分支名", "fork branch"),
            TreePromptAction::Delete => t("输入要删除的分支", "delete branch"),
            TreePromptAction::ApiKey => t("粘贴 API Key", "paste api key"),
            TreePromptAction::LoginPaste => t("粘贴回调地址或授权码", "redirect url or code"),
            TreePromptAction::PlanRevise => t("要改什么", "what to change"),
        };
        // `▏` is the prompt's visible caret; the hardware cursor stays in
        // the composer.
        self.control_line(
            UiKind::System,
            Line::from(vec![
                Span::styled("  › ", theme::accent().add_modifier(Modifier::BOLD)),
                Span::styled(format!("{label}  "), theme::dim()),
                Span::raw(input.to_string()),
                Span::styled("▏", theme::accent()),
            ]),
        );
    }

    /// Model, session, rewind, login and branch pickers: a title with the
    /// filter, the rows, and the keys.
    fn push_list_picker(&mut self, picker: &PickerState, width: usize) {
        let title = match picker.mode {
            PickerMode::Checkout => t("对话分支", "Conversation tree"),
            PickerMode::Resume => t("恢复会话", "Resume a session"),
            PickerMode::Rewind => t("回到之前的一轮", "Rewind to a turn"),
            PickerMode::Model => t("切换模型", "Switch model"),
            PickerMode::Login => t("登录", "Sign in"),
            PickerMode::LoginPaste => t("完成登录", "Finish signing in"),
            _ => "",
        };
        let mut head = vec![
            Span::raw("  "),
            Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
        ];
        if picker.filterable() {
            head.push(Span::raw("   "));
            head.push(Span::styled("› ", theme::accent()));
            if picker.filter.is_empty() {
                head.push(Span::styled("▏", theme::accent()));
                head.push(Span::styled(
                    t("输入筛选", "type to filter"),
                    theme::faint(),
                ));
            } else {
                head.push(Span::raw(picker.filter.clone()));
                head.push(Span::styled("▏", theme::accent()));
            }
        }
        let count = if picker.filter.is_empty() {
            if picker.rows.len() > PICKER_MAX_ROWS {
                match crate::lang::lang() {
                    crate::lang::Lang::Zh => format!("共 {} 个", picker.rows.len()),
                    crate::lang::Lang::En => format!("{} total", picker.rows.len()),
                }
            } else {
                String::new()
            }
        } else {
            format!("{} / {}", picker.rows.len(), picker.total_rows())
        };
        let right = if count.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(count, theme::dim()), Span::raw("  ")]
        };
        self.control_line(UiKind::System, format_status_line(head, right, width));
        self.control_line(UiKind::System, Line::default());

        if let Some(prompt) = picker.prompt.as_ref() {
            self.push_picker_prompt(prompt.action, &prompt.input);
            self.control_line(UiKind::System, Line::default());
        }

        if picker.rows.is_empty() {
            if picker.mode != PickerMode::LoginPaste {
                let empty = if picker.filter.is_empty() {
                    t("（空）", "(empty)")
                } else {
                    t("没有匹配的", "no matches")
                };
                self.control_line(
                    UiKind::System,
                    Line::from(vec![Span::raw("    "), Span::styled(empty, theme::dim())]),
                );
            }
        } else {
            self.push_picker_rows(picker, width);
        }

        if picker.mode == PickerMode::Model && !picker.efforts.is_empty() {
            self.control_line(UiKind::System, Line::default());
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    t("思考强度", "Effort"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
            ];
            for (index, effort) in picker.efforts.iter().enumerate() {
                let label = effort_label(effort);
                if index == picker.selected_effort {
                    spans.push(Span::styled(" ‹ ", theme::accent()));
                    spans.push(Span::styled(
                        label,
                        Style::new().add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(" › ", theme::accent()));
                } else {
                    spans.push(Span::styled(format!("   {label}   "), theme::dim()));
                }
            }
            self.control_line(
                UiKind::System,
                truncate_line_spans(&Line::from(spans), width),
            );
        }

        self.control_line(UiKind::System, Line::default());
        let hints: Vec<(&str, &str)> = match picker.mode {
            PickerMode::Checkout => vec![
                ("↑↓", t("移动", "move")),
                ("←→", t("收起展开", "fold")),
                ("enter", t("切换", "checkout")),
                ("f", t("分叉", "fork")),
                ("delete", t("删除", "delete")),
                ("esc", t("关闭", "close")),
            ],
            PickerMode::Resume => vec![
                ("↑↓", t("选择", "move")),
                ("enter", t("恢复", "resume")),
                ("esc", t("关闭", "close")),
            ],
            PickerMode::Rewind => vec![
                ("↑↓", t("选择", "move")),
                ("enter", t("回到这一轮", "rewind")),
                ("esc", t("关闭", "close")),
            ],
            PickerMode::Model => vec![
                ("↑↓", t("选择", "move")),
                ("←→", t("思考强度", "effort")),
                ("enter", t("确认", "select")),
                ("esc", t("关闭", "close")),
            ],
            PickerMode::Login => vec![
                ("↑↓", t("选择", "move")),
                ("enter", t("登录", "sign in")),
                ("esc", t("关闭", "close")),
            ],
            _ => vec![("enter", t("提交", "submit")), ("esc", t("关闭", "close"))],
        };
        self.control_line(UiKind::System, key_hints(&hints));
        self.control_line(UiKind::System, Line::default());
    }

    fn push_picker_rows(&mut self, picker: &PickerState, width: usize) {
        let is_tree = picker.mode == PickerMode::Checkout;
        let active_path = if is_tree {
            picker.active_path_ids()
        } else {
            std::collections::HashSet::new()
        };
        // Long lists (the login provider picker has ~60 rows) would flood
        // the control area; render a window centred on the selection.
        let total = picker.rows.len();
        let (start, end) = if total <= PICKER_MAX_ROWS {
            (0, total)
        } else {
            let start = picker
                .selected
                .saturating_sub(PICKER_MAX_ROWS / 2)
                .min(total - PICKER_MAX_ROWS);
            (start, start + PICKER_MAX_ROWS)
        };
        if start > 0 {
            self.control_line(
                UiKind::System,
                Line::from(Span::styled(more_label("↑", start), theme::dim())),
            );
        }
        let label_width = picker.rows[start..end]
            .iter()
            .map(|row| UnicodeWidthStr::width(row.label.as_str()))
            .max()
            .unwrap_or(0)
            .min(48);
        for (offset, row) in picker.rows[start..end].iter().enumerate() {
            let index = start + offset;
            let selected = index == picker.selected;
            let mut spans = vec![if selected {
                Span::styled("  › ", theme::accent().add_modifier(Modifier::BOLD))
            } else {
                Span::raw("    ")
            }];
            let label_style = if selected {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            if is_tree {
                let expander = if row.has_children {
                    if picker.is_expanded_tree_row(&row.id) {
                        "▾ "
                    } else {
                        "▸ "
                    }
                } else {
                    "  "
                };
                let dot = if active_path.contains(&row.id) {
                    Span::styled("• ", theme::accent())
                } else {
                    Span::raw("  ")
                };
                spans.push(Span::styled(row.prefix.clone(), theme::faint()));
                spans.push(Span::styled(expander, theme::dim()));
                spans.push(dot);
                spans.push(Span::styled(row.label.clone(), label_style));
                if row.active {
                    spans.push(Span::styled(
                        format!("  {}", t("◀ 当前", "◀ current")),
                        theme::accent(),
                    ));
                }
            } else {
                let indices = picker.match_indices(&row.label);
                spans.push(Span::raw(row.prefix.clone()));
                spans.extend(highlighted(&row.label, &indices, label_style));
                let pad = label_width.saturating_sub(UnicodeWidthStr::width(row.label.as_str()));
                if !row.detail.is_empty() {
                    spans.push(Span::raw(" ".repeat(pad + 3)));
                    spans.push(Span::styled(row.detail.clone(), theme::dim()));
                }
                if row.active {
                    spans.push(Span::styled(
                        format!("   {}", t("当前", "current")),
                        theme::accent(),
                    ));
                }
            }
            let kind = if selected {
                UiKind::Selected
            } else {
                UiKind::System
            };
            self.control_line(kind, truncate_line_spans(&Line::from(spans), width));
        }
        if end < total {
            self.control_line(
                UiKind::System,
                Line::from(Span::styled(more_label("↓", total - end), theme::dim())),
            );
        }
    }

    pub(crate) fn pending_messages(mut self, pending_messages: &[String], width: usize) -> Self {
        if pending_messages.is_empty() {
            return self;
        }
        self.control_line(
            UiKind::System,
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    t("排队中", "Queued"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    t(
                        "  esc 中断当前任务后发出 · ↑ 取回最后一条",
                        "  esc interrupts the turn and sends them · ↑ edits the last",
                    ),
                    theme::dim(),
                ),
            ]),
        );
        for (index, message) in pending_messages.iter().enumerate() {
            let text = truncate_to_width(&message.replace('\n', " "), width.saturating_sub(8));
            self.control_line(
                UiKind::System,
                Line::from(vec![
                    Span::styled(format!("    {}  ", index + 1), theme::dim()),
                    Span::styled(text, theme::dim()),
                ]),
            );
        }
        self.control_line(UiKind::System, Line::default());
        self
    }

    /// The composer: its lines carry the `›` prompt; an empty composer shows
    /// `placeholder` dim. The renderer draws the rules above and below.
    pub(crate) fn input(mut self, input_lines: &[UiLine], placeholder: Option<&str>) -> Self {
        let empty = input_lines.iter().all(|line| line.is_blank()) && input_lines.len() <= 1;
        for (index, source) in input_lines.iter().enumerate() {
            let prefix = if index == 0 {
                Span::styled("  › ", theme::accent().add_modifier(Modifier::BOLD))
            } else {
                Span::raw("    ")
            };
            let mut spans = vec![prefix];
            spans.extend(source.line.spans.iter().cloned());
            if index == 0 && empty {
                if let Some(placeholder) = placeholder {
                    spans.push(Span::styled(placeholder.to_string(), theme::faint()));
                }
            }
            let mut line = UiLine::new(UiKind::Input, Line::from(spans));
            line.cursor = source.cursor.map(|column| column + 4);
            self.controls.push(line);
        }
        if input_lines.is_empty() {
            let mut spans = vec![Span::styled(
                "  › ",
                theme::accent().add_modifier(Modifier::BOLD),
            )];
            if let Some(placeholder) = placeholder {
                spans.push(Span::styled(placeholder.to_string(), theme::faint()));
            }
            self.controls
                .push(UiLine::new(UiKind::Input, Line::from(spans)));
        }
        self
    }

    /// The `/` command palette (or `@` file matches) under the composer.
    pub(crate) fn candidates(
        mut self,
        rows: &[CommandCandidate],
        selected: usize,
        width: usize,
    ) -> Self {
        if rows.is_empty() {
            return self;
        }
        let total = rows.len();
        let (start, end) = if total <= PALETTE_MAX_ROWS {
            (0, total)
        } else {
            let start = selected
                .saturating_sub(PALETTE_MAX_ROWS / 2)
                .min(total - PALETTE_MAX_ROWS);
            (start, start + PALETTE_MAX_ROWS)
        };
        let command_width = rows[start..end]
            .iter()
            .map(|row| UnicodeWidthStr::width(row.command.as_str()))
            .max()
            .unwrap_or(0)
            .clamp(8, 28);
        let is_commands = rows[0].command.starts_with('/');
        for (offset, row) in rows[start..end].iter().enumerate() {
            let index = start + offset;
            let is_selected = index == selected;
            let mut spans = vec![if is_selected {
                Span::styled("  › ", theme::accent().add_modifier(Modifier::BOLD))
            } else {
                Span::raw("    ")
            }];
            let base = if is_selected {
                Style::new().add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            spans.extend(highlighted(&row.command, &row.highlight, base));
            let pad = command_width.saturating_sub(UnicodeWidthStr::width(row.command.as_str()));
            let description = crate::command_description(row);
            if !description.is_empty() {
                spans.push(Span::raw(" ".repeat(pad + 3)));
                let style = if is_selected {
                    Style::new()
                } else {
                    theme::dim()
                };
                spans.push(Span::styled(description, style));
            }
            if let Some(marker) = &row.marker {
                spans.push(Span::styled(
                    format!("  {}", crate::marker_label(marker)),
                    theme::dim(),
                ));
            }
            let kind = if is_selected {
                UiKind::CandidateSelected
            } else {
                UiKind::Candidate
            };
            self.controls.push(UiLine::new(
                kind,
                truncate_line_spans(&Line::from(spans), width),
            ));
        }
        if is_commands {
            self.controls
                .push(UiLine::new(UiKind::Candidate, Line::default()));
            self.controls.push(UiLine::new(
                UiKind::Candidate,
                key_hints(&[
                    ("↑↓", t("选择", "move")),
                    ("tab", t("补全", "complete")),
                    ("enter", t("运行", "run")),
                    ("esc", t("关闭", "close")),
                ]),
            ));
        }
        self
    }

    /// The shortcut sheet `?` opens under the composer.
    pub(crate) fn help(mut self, width: usize) -> Self {
        let keys: [(&str, &str); 14] = [
            ("enter", t("发送", "send")),
            (
                "shift+enter",
                t("换行（也可 ctrl+j）", "newline (or ctrl+j)"),
            ),
            ("esc", t("中断任务 / 关闭面板", "interrupt / close")),
            (
                "ctrl+c",
                t("中断；空输入时按两次退出", "interrupt; twice to quit"),
            ),
            ("shift+tab", t("切换批准模式", "approval mode")),
            ("ctrl+t", t("切换思考强度", "reasoning effort")),
            ("ctrl+o", t("展开或折叠工具输出", "expand tool output")),
            ("↑ ↓", t("翻看发过的消息", "message history")),
            (
                "pgup pgdn",
                t("滚动对话，滚轮也行", "scroll (or the wheel)"),
            ),
            (t("拖动鼠标", "drag"), t("选中即复制", "select to copy")),
            ("/", t("命令", "commands")),
            ("@", t("引用文件", "mention a file")),
            ("!", t("直接运行本地命令", "run a local command")),
            ("ctrl+d", t("空输入时退出", "quit on empty input")),
        ];
        let line = |kind: UiKind, line: Line<'static>| UiLine::new(kind, line);
        self.controls.push(line(UiKind::Candidate, Line::default()));
        self.controls.push(line(
            UiKind::Candidate,
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    t("快捷键", "Shortcuts"),
                    Style::new().add_modifier(Modifier::BOLD),
                ),
            ]),
        ));
        self.controls.push(line(UiKind::Candidate, Line::default()));
        let cell = |key: &str, label: &str| -> Vec<Span<'static>> {
            let pad = 13usize.saturating_sub(UnicodeWidthStr::width(key));
            vec![
                Span::raw(key.to_string()),
                Span::raw(" ".repeat(pad)),
                Span::styled(label.to_string(), theme::dim()),
            ]
        };
        if width >= 84 {
            let half = keys.len().div_ceil(2);
            for row in 0..half {
                let (key, label) = keys[row];
                let mut spans = vec![Span::raw("    ")];
                let left = cell(key, label);
                let left_width = spans_width(&left);
                spans.extend(left);
                if let Some((key, label)) = keys.get(row + half) {
                    spans.push(Span::raw(" ".repeat(38usize.saturating_sub(left_width))));
                    spans.extend(cell(key, label));
                }
                self.controls.push(line(
                    UiKind::Candidate,
                    truncate_line_spans(&Line::from(spans), width),
                ));
            }
        } else {
            for (key, label) in keys {
                let mut spans = vec![Span::raw("    ")];
                spans.extend(cell(key, label));
                self.controls.push(line(
                    UiKind::Candidate,
                    truncate_line_spans(&Line::from(spans), width),
                ));
            }
        }
        self.controls.push(line(UiKind::Candidate, Line::default()));
        self.controls.push(line(
            UiKind::Candidate,
            Line::from(vec![
                Span::raw("  "),
                Span::styled(t("按任意键关闭", "any key closes"), theme::dim()),
            ]),
        ));
        self
    }

    /// The working indicator while a turn runs (`working_indicator`).
    pub(crate) fn progress(
        mut self,
        activity: &ActivityState,
        awaiting_connection: bool,
        now: Instant,
        width: usize,
    ) -> Self {
        if awaiting_connection && matches!(activity.kind, crate::ActivityKind::Connecting) {
            return self;
        }
        let Some(progress) = activity.progress(now) else {
            return self;
        };
        let row = crate::working_indicator::line(&theme::current(), &progress);
        self.control_line(UiKind::Status, truncate_line_spans(&row, width));
        self
    }

    /// A one-line notice above the composer (e.g. "press ctrl+c again").
    pub(crate) fn notice(mut self, notice: Option<&str>) -> Self {
        if let Some(notice) = notice {
            self.control_line(
                UiKind::Status,
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(notice.to_string(), theme::warn()),
                ]),
            );
        }
        self
    }

    pub(crate) fn bottom_status(mut self, status: BottomStatus<'_>, width: usize) -> Self {
        let percent = if status.context_window == 0 {
            0.0
        } else {
            (status.context_tokens as f64 / status.context_window as f64 * 100.0).min(100.0)
        };
        let (mode, mode_style) = approval_mode_label(status.approval_mode);
        let money = match status.balance {
            Some(balance) => balance.to_string(),
            None if status.cost > 0.0 => format!("${:.4}", status.cost),
            None => String::new(),
        };
        let branch = status.git.map(crate::git_bar::GitStatus::label);
        // Narrow terminals drop segments in steps: the branch, the context
        // label, the mode, then the money.
        for step in 0..5 {
            let mut left = vec![Span::raw("  "), Span::raw(status.model.to_string())];
            if !status.reasoning_effort.is_empty() {
                left.push(Span::styled(
                    format!(" {}", effort_label(status.reasoning_effort)),
                    theme::dim(),
                ));
            }
            if step < 3 {
                left.push(Span::raw("   "));
                left.push(Span::styled(mode, mode_style));
            }
            let mut right: Vec<Span<'static>> = Vec::new();
            let push = |text: String, right: &mut Vec<Span<'static>>| {
                if !right.is_empty() {
                    right.push(Span::styled(" · ", theme::faint()));
                }
                right.push(Span::styled(text, theme::dim()));
            };
            if step == 0 {
                if let Some(branch) = &branch {
                    push(branch.clone(), &mut right);
                }
            }
            if step < 4 {
                let context = if step < 2 {
                    format!("{} {percent:.0}%", t("上下文", "context"))
                } else {
                    format!("{percent:.0}%")
                };
                push(context, &mut right);
            }
            if !money.is_empty() {
                push(money.clone(), &mut right);
            }
            right.push(Span::raw("  "));
            if spans_width(&left) + 1 + spans_width(&right) <= width || step == 4 {
                let line = format_status_line(left, right, width);
                self.control_line(UiKind::BottomStatus, line);
                return self;
            }
        }
        self
    }

    pub(crate) fn reset_screen(mut self, reset_screen: bool) -> Self {
        self.reset_screen = reset_screen;
        self
    }

    pub(crate) fn finish(self) -> UiDocument {
        UiDocument {
            history: self.history,
            rendered_history_lines: self.rendered_history_lines,
            controls: self.controls,
            reset_screen: self.reset_screen,
        }
    }

    pub(crate) fn into_history(self) -> Vec<UiLine> {
        self.history
    }

    /// One blank line between top-level blocks (turns, tool calls, notices)
    /// gives the transcript its rhythm; leading blank is skipped.
    fn separate_block(&mut self) {
        if self.history.last().is_some_and(|line| !line.is_blank()) {
            self.history_line(UiKind::Status, Line::default());
        }
    }

    /// User turns: a `›` marker on a faintly filled row, so each turn reads
    /// apart from the output.
    fn push_user_message(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            self.history_line(UiKind::User, Line::from(Span::styled("›", theme::dim())));
            return;
        }
        for (index, line) in text.lines().enumerate() {
            let marker = if index == 0 { "› " } else { "  " };
            self.history_line(
                UiKind::User,
                Line::from(vec![
                    Span::styled(marker, theme::dim()),
                    Span::styled(line.to_string(), style),
                ]),
            );
        }
    }

    fn push_history(&mut self, kind: UiKind, text: &str) {
        if text.is_empty() {
            self.history_line(kind, Line::default());
            return;
        }
        for line in text.lines() {
            self.history_line(kind, line.to_string());
        }
    }

    /// The welcome block: the mark beside four lines (name and version,
    /// account, model, folder), then the entry hints. Narrow terminals and
    /// plain glyph sets get the lines without the mark.
    fn push_welcome(
        &mut self,
        version: &str,
        cwd: &str,
        model: &str,
        context_window: u64,
        width: usize,
    ) {
        let info = self.welcome.clone().unwrap_or_else(|| WelcomeInfo {
            model: model.to_string(),
            context_window,
            ..WelcomeInfo::default()
        });
        let model = if info.model.is_empty() || info.model == "unknown" {
            model.to_string()
        } else {
            info.model.clone()
        };
        let context_window = if info.context_window > 0 {
            info.context_window
        } else {
            context_window
        };
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut lines: Vec<Vec<Span<'static>>> = vec![vec![
            Span::styled("LynShen", bold),
            Span::styled(format!("  {version}"), theme::dim()),
        ]];
        match &info.account {
            AccountState::Ready(account) => {
                let mut spans = vec![Span::raw(account.email.clone())];
                let plan = account
                    .plan
                    .clone()
                    .unwrap_or_else(|| t("没有套餐", "no plan").to_string());
                spans.push(Span::styled(format!(" · {plan}"), theme::dim()));
                lines.push(spans);
            }
            AccountState::SignedOut => lines.push(vec![
                Span::styled(t("还没登录", "Not signed in"), theme::warn()),
                Span::styled(t(" · 输入 /login 登录", " · type /login"), theme::dim()),
            ]),
            AccountState::NotLynShen if !info.provider.is_empty() => lines.push(vec![
                Span::raw(info.provider.clone()),
                Span::styled(t(" · 自己的服务商", " · your provider"), theme::dim()),
            ]),
            _ => {}
        }
        let mut model_line = vec![Span::raw(model)];
        if info.has_efforts && !info.effort.is_empty() {
            model_line.push(Span::styled(
                format!(" · {}{}", t("思考 ", "effort "), effort_label(&info.effort)),
                theme::dim(),
            ));
        }
        model_line.push(Span::styled(
            format!(
                " · {} {}",
                format_context_window(context_window),
                t("上下文", "context")
            ),
            theme::dim(),
        ));
        lines.push(model_line);
        lines.push(vec![Span::raw(compact_home_path(cwd))]);

        let with_logo = theme::rich_glyphs() && width >= LOGO_MIN_WIDTH;
        let rows = if with_logo {
            lines.len().max(LOGO.len())
        } else {
            lines.len()
        };
        for row in 0..rows {
            let mut spans = Vec::new();
            if with_logo {
                let mut used = 0;
                if let Some(parts) = LOGO.get(row) {
                    for (text, light) in parts.iter() {
                        let style = if *light {
                            theme::accent()
                        } else {
                            Style::new()
                        };
                        used += UnicodeWidthStr::width(*text);
                        spans.push(Span::styled(*text, style));
                    }
                }
                spans.push(Span::raw(" ".repeat(LOGO_WIDTH - used + 4)));
            }
            if let Some(text) = lines.get(row) {
                spans.extend(text.iter().cloned());
            }
            let line = Line::from(spans);
            let line = if width == usize::MAX {
                line
            } else {
                truncate_line_spans(&line, width)
            };
            self.history_line(UiKind::Assistant, line);
        }
        self.history_line(UiKind::Assistant, Line::default());
        let hint = |key: &'static str, label: &'static str| {
            [
                Span::styled(key, theme::accent()),
                Span::styled(format!(" {label}   "), theme::dim()),
            ]
        };
        let mut spans = Vec::new();
        spans.extend(hint("/", t("命令", "commands")));
        spans.extend(hint("@", t("引用文件", "files")));
        spans.extend(hint("!", t("运行命令", "shell")));
        spans.extend(hint("?", t("快捷键", "shortcuts")));
        self.history_line(UiKind::Assistant, Line::from(spans));
    }

    /// A tool call: the status dot, the verb, the target and the result on
    /// one clickable row, then the output folded to its useful end.
    fn push_tool_block(
        &mut self,
        name: &str,
        output: &str,
        running: bool,
        expanded: bool,
        item_index: usize,
        width: usize,
    ) {
        let view = tool_view(name, output, running);
        let dot = match view.state {
            ToolState::Running => theme::accent(),
            ToolState::Done => theme::dim(),
            ToolState::Failed => theme::err(),
        };
        let mut head = vec![
            Span::styled("● ", dot),
            Span::styled(view.verb.clone(), Style::new().add_modifier(Modifier::BOLD)),
        ];
        let extra_width = spans_width(&view.extra) + 2;
        if !view.arg.is_empty() {
            let budget = width
                .saturating_sub(spans_width(&head) + 2 + extra_width)
                .max(8);
            head.push(Span::raw("  "));
            head.push(Span::styled(
                truncate_line_spans(&Line::from(view.arg.clone()), budget)
                    .spans
                    .into_iter()
                    .map(|span| span.content.to_string())
                    .collect::<String>(),
                theme::dim(),
            ));
        }
        if !view.extra.is_empty() {
            head.push(Span::raw("  "));
            head.extend(view.extra.iter().cloned());
        }
        let head = if width == usize::MAX {
            Line::from(head)
        } else {
            truncate_line_spans(&Line::from(head), width)
        };
        self.history_clickable_line(UiKind::ToolHeader, head, item_index);

        let body = view.body;
        if body.iter().all(UiLine::is_blank) {
            return;
        }
        let total = body.len();
        let (shown, hidden_before, hidden_after) = if expanded {
            let end = total.min(TOOL_EXPANDED_LINES);
            (&body[..end], 0, total - end)
        } else {
            match view.body_mode {
                BodyMode::Tail => {
                    let start = total.saturating_sub(TOOL_TAIL_LINES);
                    (&body[start..], start, 0)
                }
                BodyMode::Head(limit) => {
                    let end = total.min(limit);
                    (&body[..end], 0, total - end)
                }
            }
        };
        let gutter = view.gutter;
        let fold = |count: usize, before: bool| -> UiLine {
            let text = match (crate::lang::is_zh(), before) {
                (true, true) => format!("⋯ 前 {count} 行已折叠 · ctrl+o 展开"),
                (true, false) => format!("⋯ 还有 {count} 行 · ctrl+o 展开"),
                (false, true) => format!("⋯ {count} earlier lines · ctrl+o to expand"),
                (false, false) => format!("⋯ {count} more lines · ctrl+o to expand"),
            };
            UiLine::new(UiKind::Tool, Line::from(Span::styled(text, theme::faint())))
        };
        let mut rows: Vec<UiLine> = Vec::new();
        if hidden_before > 0 {
            rows.push(fold(hidden_before, true));
        }
        rows.extend(shown.iter().cloned());
        if hidden_after > 0 {
            rows.push(if expanded {
                let text = match crate::lang::is_zh() {
                    true => format!("⋯ 还有 {hidden_after} 行"),
                    false => format!("⋯ {hidden_after} more lines"),
                };
                UiLine::new(UiKind::Tool, Line::from(Span::styled(text, theme::faint())))
            } else {
                fold(hidden_after, false)
            });
        }
        let last = rows.len().saturating_sub(1);
        for (index, row) in rows.into_iter().enumerate() {
            let prefix = if gutter {
                Span::styled(if index == last { "  └ " } else { "  │ " }, theme::faint())
            } else {
                Span::raw("  ")
            };
            let mut spans = vec![prefix];
            spans.extend(row.line.spans);
            self.history.push(UiLine {
                kind: row.kind,
                line: Line::from(spans),
                click: None,
                cursor: None,
            });
        }
    }

    pub(crate) fn history_line(&mut self, kind: UiKind, line: impl Into<Line<'static>>) {
        self.history.push(UiLine::new(kind, line));
    }

    /// A history line carrying a click target: clicking its painted row
    /// toggles the chat item at `index`.
    fn history_clickable_line(
        &mut self,
        kind: UiKind,
        line: impl Into<Line<'static>>,
        index: usize,
    ) {
        self.history.push(UiLine::clickable(kind, line, index));
    }

    fn control_line(&mut self, kind: UiKind, line: impl Into<Line<'static>>) {
        self.controls.push(UiLine::new(kind, line));
    }
}

fn more_label(arrow: &str, count: usize) -> String {
    match crate::lang::lang() {
        crate::lang::Lang::Zh => format!("    {arrow} 还有 {count} 个"),
        crate::lang::Lang::En => format!("    {arrow} {count} more"),
    }
}
