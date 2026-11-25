use std::collections::HashSet;

use lynshen_agent_core::{LoginProviderView, ModelOptionView, SessionListItemView, TreeNodeView};

#[derive(Debug, Clone)]
pub(crate) struct PickerState {
    pub(crate) rows: Vec<PickerRow>,
    pub(crate) selected: usize,
    pub(crate) mode: PickerMode,
    tree: Option<TreeRows>,
    pub(crate) efforts: Vec<String>,
    pub(crate) selected_effort: usize,
    /// Confirmation-style pickers (approval, trust) show a `● title` header
    /// plus an optional `⎿ context` line above the option rows.
    pub(crate) title: Option<String>,
    pub(crate) context: Option<String>,
    pub(crate) prompt: Option<TreePrompt>,
    /// Login-mode rows that need a pasted API key instead of a browser flow.
    key_rows: HashSet<String>,
    /// Typed filter of a list picker (model, session, rewind, login).
    pub(crate) filter: String,
    /// The unfiltered rows of a list picker once a filter was typed.
    all_rows: Vec<PickerRow>,
    /// A dim line under a confirmation's context (where, why).
    pub(crate) detail: Option<String>,
}

/// An approval request as the picker shows it.
#[derive(Debug, Clone, Default)]
pub(crate) struct ApprovalView {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) summary: String,
    pub(crate) command: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) reason: Option<String>,
    /// The command prefix "allow for this session" remembers.
    pub(crate) prefix: Option<String>,
    pub(crate) subagent: Option<String>,
    pub(crate) hunks: usize,
}

#[derive(Debug, Clone)]
struct TreeRows {
    all_rows: Vec<PickerRow>,
    expanded: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PickerRow {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) depth: usize,
    pub(crate) prefix: String,
    pub(crate) label: String,
    pub(crate) active: bool,
    pub(crate) has_children: bool,
    pub(crate) detail: String,
    pub(crate) reasoning_efforts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerMode {
    Checkout,
    Resume,
    Rewind,
    Model,
    Trust,
    Approval,
    /// A proposed plan: approve it in a mode, or type a revision.
    Plan,
    Login,
    /// No rows: just the paste input for a manual-callback login.
    LoginPaste,
}

#[derive(Debug, Clone)]
pub(crate) struct TreePrompt {
    pub(crate) action: TreePromptAction,
    pub(crate) input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TreePromptAction {
    Fork,
    Delete,
    /// Paste an API key for the selected login-mode provider row.
    ApiKey,
    /// Paste the redirect URL or code for a manual-callback login.
    LoginPaste,
    /// What to change in the proposed plan.
    PlanRevise,
}

impl PickerState {
    pub(crate) fn checkout(nodes: Vec<TreeNodeView>) -> Self {
        let all_rows = build_tree_rows(&nodes);
        let expanded = adaptive_expanded_rows(&all_rows);
        let rows = visible_tree_rows(&all_rows, &expanded);
        let selected = rows.iter().position(|row| row.active).unwrap_or(0);
        Self {
            rows,
            selected,
            mode: PickerMode::Checkout,
            tree: Some(TreeRows { all_rows, expanded }),
            efforts: Vec::new(),
            selected_effort: 0,
            title: None,
            context: None,
            prompt: None,
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    pub(crate) fn resume(sessions: Vec<SessionListItemView>) -> Self {
        let rows = sessions
            .into_iter()
            .map(|session| PickerRow {
                id: session.id,
                parent_id: None,
                depth: 0,
                prefix: String::new(),
                label: session.label,
                active: session.active,
                has_children: false,
                detail: session.detail,
                reasoning_efforts: Vec::new(),
            })
            .collect::<Vec<_>>();
        let selected = rows.iter().position(|row| row.active).unwrap_or(0);
        Self {
            rows,
            selected,
            mode: PickerMode::Resume,
            tree: None,
            efforts: Vec::new(),
            selected_effort: 0,
            title: None,
            context: None,
            prompt: None,
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    pub(crate) fn checkpoint(items: Vec<SessionListItemView>) -> Self {
        let rows = items
            .into_iter()
            .map(|item| PickerRow {
                id: item.id,
                parent_id: None,
                depth: 0,
                prefix: String::new(),
                label: item.label,
                active: false,
                has_children: false,
                detail: item.detail,
                reasoning_efforts: Vec::new(),
            })
            .collect::<Vec<_>>();
        Self {
            rows,
            selected: 0,
            mode: PickerMode::Rewind,
            tree: None,
            efforts: Vec::new(),
            selected_effort: 0,
            title: None,
            context: None,
            prompt: None,
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    pub(crate) fn model(models: Vec<ModelOptionView>, active_effort: String) -> Self {
        let selected = models.iter().position(|row| row.active).unwrap_or(0);
        let efforts = models
            .get(selected)
            .map(|model| model.reasoning_efforts.clone())
            .unwrap_or_default();
        let selected_effort = efforts
            .iter()
            .position(|effort| effort == &active_effort)
            .unwrap_or(0);
        let rows = models
            .into_iter()
            .map(|model| PickerRow {
                id: model.model.clone(),
                parent_id: None,
                depth: 0,
                prefix: String::new(),
                label: model.label.unwrap_or(model.model),
                active: model.active,
                has_children: false,
                detail: format!(
                    "{} {}",
                    crate::format_context_window(model.context_window),
                    crate::lang::t("上下文", "context")
                ),
                reasoning_efforts: model.reasoning_efforts,
            })
            .collect::<Vec<_>>();
        Self {
            rows,
            selected,
            mode: PickerMode::Model,
            tree: None,
            efforts,
            selected_effort,
            title: None,
            context: None,
            prompt: None,
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    pub(crate) fn login(providers: Vec<LoginProviderView>) -> Self {
        let selected = providers.iter().position(|row| row.active).unwrap_or(0);
        let key_rows = providers
            .iter()
            .filter(|p| p.wants_key)
            .map(|p| p.id.clone())
            .collect();
        let rows = providers
            .into_iter()
            .map(|p| PickerRow {
                id: p.id,
                parent_id: None,
                depth: 0,
                prefix: String::new(),
                label: p.label,
                active: p.active,
                has_children: false,
                detail: p.detail,
                reasoning_efforts: Vec::new(),
            })
            .collect::<Vec<_>>();
        Self {
            rows,
            selected,
            mode: PickerMode::Login,
            tree: None,
            efforts: Vec::new(),
            selected_effort: 0,
            title: None,
            context: None,
            prompt: None,
            key_rows,
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    /// Manual-callback login waiting on a pasted redirect URL or code: no
    /// rows, the prompt is open on arrival.
    pub(crate) fn login_paste() -> Self {
        Self {
            rows: Vec::new(),
            selected: 0,
            mode: PickerMode::LoginPaste,
            tree: None,
            efforts: Vec::new(),
            selected_effort: 0,
            title: None,
            context: None,
            prompt: Some(TreePrompt {
                action: TreePromptAction::LoginPaste,
                input: String::new(),
            }),
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn approval(call_id: String, name: String, summary: String) -> Self {
        Self::approval_with(ApprovalView {
            call_id,
            name,
            summary,
            ..ApprovalView::default()
        })
    }

    pub(crate) fn approval_with(view: ApprovalView) -> Self {
        use crate::lang::t;
        let call_id = view.call_id.clone();
        let row = |suffix: &str, label: String, detail: String| PickerRow {
            id: format!("{call_id} {suffix}"),
            parent_id: None,
            depth: 0,
            prefix: String::new(),
            label,
            active: false,
            has_children: false,
            detail,
            reasoning_efforts: Vec::new(),
        };
        let scope = match &view.prefix {
            Some(prefix) => match crate::lang::lang() {
                crate::lang::Lang::Zh => format!("{prefix} 开头的命令"),
                crate::lang::Lang::En => format!("commands starting with {prefix}"),
            },
            None => view.name.clone(),
        };
        let rows = vec![
            row(
                "allow once",
                t("允许这一次", "Allow once").to_string(),
                String::new(),
            ),
            row(
                "allow always",
                t("本会话内都允许", "Allow for this session").to_string(),
                scope,
            ),
            row("deny", t("拒绝", "Deny").to_string(), String::new()),
        ];
        let title = match view.name.as_str() {
            "bash" | "exec_command" | "execute" | "shell_command" | "write_stdin" => {
                t("运行命令", "Run a command").to_string()
            }
            "edit" | "str_replace" | "hashline_edit" | "apply_patch" => {
                t("修改文件", "Edit a file").to_string()
            }
            "write" => t("写入文件", "Write a file").to_string(),
            "web_fetch" => t("访问网页", "Fetch a page").to_string(),
            name => match crate::lang::lang() {
                crate::lang::Lang::Zh => format!("使用工具 {name}"),
                crate::lang::Lang::En => format!("Use {name}"),
            },
        };
        let context = view
            .command
            .clone()
            .filter(|command| !command.trim().is_empty())
            .or_else(|| (!view.summary.is_empty()).then(|| view.summary.clone()));
        let mut detail = Vec::new();
        if let Some(cwd) = view.cwd.as_ref().filter(|cwd| !cwd.is_empty()) {
            detail.push(crate::compact_home_path(cwd));
        }
        if let Some(reason) = view.reason.as_ref().filter(|reason| !reason.is_empty()) {
            detail.push(format!("{}{reason}", t("理由：", "reason: ")));
        }
        if let Some(agent) = &view.subagent {
            detail.push(format!("{} {agent}", t("子智能体", "subagent")));
        }
        if view.hunks > 0 {
            detail.push(match crate::lang::lang() {
                crate::lang::Lang::Zh => format!("{} 处改动", view.hunks),
                crate::lang::Lang::En => format!("{} hunks", view.hunks),
            });
        }
        Self {
            rows,
            selected: 0,
            mode: PickerMode::Approval,
            tree: None,
            efforts: Vec::new(),
            selected_effort: 0,
            title: Some(title),
            context,
            prompt: None,
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: (!detail.is_empty()).then(|| detail.join(" · ")),
        }
    }

    /// Rows are `<plan-id> approve <mode>` and `<plan-id> revise`.
    pub(crate) fn plan(id: String, title: String) -> Self {
        let row = |suffix: &str, label: &str| PickerRow {
            id: format!("{id} {suffix}"),
            parent_id: None,
            depth: 0,
            prefix: String::new(),
            label: label.to_string(),
            active: false,
            has_children: false,
            detail: String::new(),
            reasoning_efforts: Vec::new(),
        };
        let rows = vec![
            row(
                "approve auto-edit",
                crate::lang::t(
                    "批准，允许它直接改文件",
                    "Approve, run with edits allowed (auto-edit)",
                ),
            ),
            row(
                "approve manual",
                crate::lang::t(
                    "批准，每次改动前问我",
                    "Approve, run asking before each change (manual)",
                ),
            ),
            row(
                "approve auto",
                crate::lang::t("批准，不再询问", "Approve, run without asking (auto)"),
            ),
            row(
                "revise",
                crate::lang::t(
                    "修改：告诉它要改什么",
                    "Revise: tell the agent what to change",
                ),
            ),
        ];
        Self {
            rows,
            selected: 0,
            mode: PickerMode::Plan,
            tree: None,
            efforts: Vec::new(),
            selected_effort: 0,
            title: Some(crate::lang::t("它提出了一个计划", "Proposed plan").to_string()),
            context: (!title.is_empty()).then_some(title),
            prompt: None,
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    pub(crate) fn trust(cwd: String, repo_root: Option<String>) -> Self {
        let row = |id: &str, label: &str, detail: String| PickerRow {
            id: id.to_string(),
            parent_id: None,
            depth: 0,
            prefix: String::new(),
            label: label.to_string(),
            active: false,
            has_children: false,
            detail,
            reasoning_efforts: Vec::new(),
        };
        use crate::lang::t;
        let mut rows = vec![row(
            "yes",
            t("信任这个目录", "Trust this folder"),
            String::new(),
        )];
        if let Some(root) = repo_root {
            rows.push(row(
                "repo",
                t("信任整个仓库", "Trust the whole repository"),
                crate::compact_home_path(&root),
            ));
        }
        rows.push(row(
            "no",
            t(
                "不信任（项目里的技能和钩子保持关闭）",
                "Do not trust (project skills & hooks stay disabled)",
            ),
            String::new(),
        ));
        Self {
            rows,
            selected: 0,
            mode: PickerMode::Trust,
            tree: None,
            efforts: Vec::new(),
            selected_effort: 0,
            title: Some(
                t(
                    "信任后会加载这个项目自带的技能和钩子",
                    "Trusting loads this project's skills and hooks",
                )
                .to_string(),
            ),
            context: Some(crate::compact_home_path(&cwd)),
            prompt: None,
            key_rows: HashSet::new(),
            filter: String::new(),
            all_rows: Vec::new(),
            detail: None,
        }
    }

    pub(crate) fn selected_id(&self) -> Option<String> {
        self.rows.get(self.selected).map(|row| row.id.clone())
    }

    pub(crate) fn selected_command(&self) -> Option<String> {
        let id = self.selected_id()?;
        match self.mode {
            PickerMode::Checkout => Some(format!("/checkout {id}")),
            PickerMode::Resume => Some(format!("/resume {id}")),
            PickerMode::Rewind => Some(format!("/rewind {id}")),
            PickerMode::Approval => Some(format!("/approve {id}")),
            PickerMode::Plan => Some(format!("/plan {id}")),
            PickerMode::Model => {
                let effort = self.efforts.get(self.selected_effort)?;
                Some(format!("/model {id} {effort}"))
            }
            PickerMode::Trust => Some(format!("/trust {id}")),
            PickerMode::Login => Some(format!("/login {id}")),
            PickerMode::LoginPaste => None,
        }
    }

    /// Login mode: the selected provider wants a pasted key, so Enter opens
    /// the inline prompt instead of running `/login <id>` directly.
    pub(crate) fn selected_wants_key(&self) -> bool {
        self.mode == PickerMode::Login
            && self
                .selected_id()
                .is_some_and(|id| self.key_rows.contains(&id))
    }

    pub(crate) fn begin_key_prompt(&mut self) {
        self.prompt = Some(TreePrompt {
            action: TreePromptAction::ApiKey,
            input: String::new(),
        });
    }

    /// Plan mode: the selected row is "revise", so Enter opens the feedback
    /// prompt instead of running a command.
    pub(crate) fn selected_wants_feedback(&self) -> bool {
        self.mode == PickerMode::Plan
            && self.selected_id().is_some_and(|id| id.ends_with(" revise"))
    }

    pub(crate) fn begin_feedback_prompt(&mut self) {
        self.prompt = Some(TreePrompt {
            action: TreePromptAction::PlanRevise,
            input: String::new(),
        });
    }

    /// Ids of the nodes on the path from the root to the current HEAD, used to
    /// highlight "where you are" in the tree. Empty outside checkout mode.
    pub(crate) fn active_path_ids(&self) -> HashSet<String> {
        let mut path = HashSet::new();
        let Some(tree) = self.tree.as_ref() else {
            return path;
        };
        let Some(active) = tree.all_rows.iter().find(|row| row.active) else {
            return path;
        };
        let mut current = Some(active.id.clone());
        while let Some(id) = current {
            path.insert(id.clone());
            current = tree
                .all_rows
                .iter()
                .find(|row| row.id == id)
                .and_then(|row| row.parent_id.clone());
        }
        path
    }

    pub(crate) fn is_expanded_tree_row(&self, id: &str) -> bool {
        self.tree
            .as_ref()
            .is_some_and(|tree| tree.expanded.contains(id))
    }

    pub(crate) fn begin_tree_prompt(&mut self, action: TreePromptAction) {
        if self.mode != PickerMode::Checkout {
            return;
        }
        self.prompt = Some(TreePrompt {
            action,
            input: String::new(),
        });
    }

    pub(crate) fn cancel_prompt(&mut self) {
        self.prompt = None;
    }

    pub(crate) fn push_prompt_char(&mut self, ch: char) {
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.input.push(ch);
        }
    }

    /// Bulk insert (bracketed paste); newlines can't be submitted anyway, so
    /// they're dropped rather than splitting the value.
    pub(crate) fn push_prompt_str(&mut self, text: &str) {
        if let Some(prompt) = self.prompt.as_mut() {
            prompt
                .input
                .extend(text.chars().filter(|ch| !matches!(ch, '\n' | '\r')));
        }
    }

    pub(crate) fn pop_prompt_char(&mut self) {
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.input.pop();
        }
    }

    pub(crate) fn take_prompt_command(&mut self) -> Option<String> {
        // Empty input keeps the prompt open — in LoginPaste mode the prompt
        // is the whole picker, so consuming it would leave a dead panel.
        if self.prompt.as_ref()?.input.trim().is_empty() {
            return None;
        }
        let prompt = self.prompt.take()?;
        let label = prompt.input.trim();
        match prompt.action {
            TreePromptAction::Fork => Some(format!("/fork {label}")),
            TreePromptAction::Delete => Some(format!("/delete {label}")),
            TreePromptAction::ApiKey => self.selected_id().map(|id| format!("/login {id} {label}")),
            TreePromptAction::LoginPaste => Some(format!("/login-paste {label}")),
            TreePromptAction::PlanRevise => {
                self.selected_id().map(|id| format!("/plan {id} {label}"))
            }
        }
    }

    /// List pickers take a typed filter; the tree keeps its letter keys and
    /// confirmations their digits.
    pub(crate) fn filterable(&self) -> bool {
        matches!(
            self.mode,
            PickerMode::Resume | PickerMode::Rewind | PickerMode::Model | PickerMode::Login
        )
    }

    /// Rows before the filter.
    pub(crate) fn total_rows(&self) -> usize {
        if self.all_rows.is_empty() {
            self.rows.len()
        } else {
            self.all_rows.len()
        }
    }

    /// Characters of `label` the filter matched, for highlighting.
    pub(crate) fn match_indices(&self, label: &str) -> Vec<usize> {
        if self.filter.is_empty() {
            return Vec::new();
        }
        crate::fuzzy::fuzzy_match(label, &self.filter)
            .map(|found| found.indices)
            .unwrap_or_default()
    }

    pub(crate) fn push_filter_char(&mut self, ch: char) {
        self.filter.push(ch);
        self.apply_filter();
    }

    pub(crate) fn push_filter_str(&mut self, text: &str) {
        self.filter
            .extend(text.chars().filter(|ch| !ch.is_control()));
        self.apply_filter();
    }

    pub(crate) fn clear_filter(&mut self) {
        if !self.filter.is_empty() {
            self.filter.clear();
            self.apply_filter();
        }
    }

    /// Returns false when the filter was already empty.
    pub(crate) fn pop_filter_char(&mut self) -> bool {
        if self.filter.pop().is_none() {
            return false;
        }
        self.apply_filter();
        true
    }

    fn apply_filter(&mut self) {
        if self.all_rows.is_empty() {
            self.all_rows = self.rows.clone();
        }
        let previous = self.selected_id();
        self.rows = crate::fuzzy::rank(&self.all_rows, &self.filter, |row| row.label.as_str())
            .into_iter()
            .map(|(row, _)| row.clone())
            .collect();
        self.selected = if self.filter.is_empty() {
            previous
                .and_then(|id| self.rows.iter().position(|row| row.id == id))
                .or_else(|| self.rows.iter().position(|row| row.active))
                .unwrap_or(0)
        } else {
            0
        };
        self.sync_selected_model_efforts();
    }

    /// Digit keys on confirmations: selects row `number` (1-based).
    pub(crate) fn select_number(&mut self, number: usize) -> bool {
        if number == 0 || number > self.rows.len() {
            return false;
        }
        self.selected = number - 1;
        true
    }

    /// The command that answers an approval with "deny" (esc).
    pub(crate) fn deny_command(&self) -> Option<String> {
        if self.mode != PickerMode::Approval {
            return None;
        }
        self.rows
            .iter()
            .find(|row| row.id.ends_with(" deny"))
            .map(|row| format!("/approve {}", row.id))
    }

    pub(crate) fn cycle_effort_back(&mut self) {
        if self.mode != PickerMode::Model || self.efforts.is_empty() {
            return;
        }
        self.selected_effort = (self.selected_effort + self.efforts.len() - 1) % self.efforts.len();
    }

    pub(crate) fn move_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.sync_selected_model_efforts();
    }

    pub(crate) fn move_next(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
        self.sync_selected_model_efforts();
    }

    pub(crate) fn page_up(&mut self) {
        self.selected = self
            .selected
            .saturating_sub(crate::ui_builder::PICKER_MAX_ROWS);
        self.sync_selected_model_efforts();
    }

    pub(crate) fn page_down(&mut self) {
        self.selected = (self.selected + crate::ui_builder::PICKER_MAX_ROWS)
            .min(self.rows.len().saturating_sub(1));
        self.sync_selected_model_efforts();
    }

    pub(crate) fn move_parent(&mut self) {
        if self.mode != PickerMode::Checkout {
            return;
        }
        let Some(selected_id) = self.selected_id() else {
            return;
        };
        let Some(tree) = self.tree.as_mut() else {
            return;
        };
        if tree.expanded.remove(&selected_id) {
            self.rebuild_visible_rows(Some(selected_id));
            return;
        }
        let Some(parent_id) = self
            .rows
            .get(self.selected)
            .and_then(|row| row.parent_id.as_ref())
        else {
            return;
        };
        if let Some(index) = self.rows.iter().position(|row| &row.id == parent_id) {
            self.selected = index;
        }
    }

    pub(crate) fn move_first_child(&mut self) {
        if self.mode != PickerMode::Checkout {
            return;
        }
        let Some(id) = self.rows.get(self.selected).map(|row| row.id.as_str()) else {
            return;
        };
        let Some(tree) = self.tree.as_ref() else {
            return;
        };
        if self.has_children(id) && !tree.expanded.contains(id) {
            let selected_id = id.to_string();
            if let Some(tree) = self.tree.as_mut() {
                tree.expanded.insert(selected_id.clone());
            }
            self.rebuild_visible_rows(Some(selected_id));
            return;
        }
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| row.parent_id.as_deref() == Some(id))
        {
            self.selected = index;
        }
    }

    fn has_children(&self, id: &str) -> bool {
        let Some(tree) = self.tree.as_ref() else {
            return false;
        };
        tree.all_rows
            .iter()
            .any(|row| row.parent_id.as_deref() == Some(id))
    }

    fn rebuild_visible_rows(&mut self, selected_id: Option<String>) {
        let Some(tree) = self.tree.as_ref() else {
            return;
        };
        self.rows = visible_tree_rows(&tree.all_rows, &tree.expanded);
        if let Some(selected_id) = selected_id {
            if let Some(index) = self.rows.iter().position(|row| row.id == selected_id) {
                self.selected = index;
                return;
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub(crate) fn cycle_effort(&mut self) {
        if self.mode != PickerMode::Model || self.efforts.is_empty() {
            return;
        }
        self.selected_effort = (self.selected_effort + 1) % self.efforts.len();
    }

    fn sync_selected_model_efforts(&mut self) {
        if self.mode != PickerMode::Model {
            return;
        }
        let Some(row) = self.rows.get(self.selected) else {
            return;
        };
        let current = self.efforts.get(self.selected_effort).cloned();
        self.efforts = row.reasoning_efforts.clone();
        self.selected_effort = current
            .and_then(|value| self.efforts.iter().position(|effort| effort == &value))
            .unwrap_or_else(|| {
                self.efforts
                    .iter()
                    .position(|effort| effort == "medium")
                    .unwrap_or(0)
            });
    }
}

fn build_tree_rows(nodes: &[TreeNodeView]) -> Vec<PickerRow> {
    let mut rows = Vec::new();
    push_tree_rows(None, 0, nodes, &mut rows);
    rows
}

/// Soft cap on rows revealed by default; deeper levels are revealed only while the
/// total visible-row count stays within it.
const TREE_VISIBLE_ROW_BUDGET: usize = 16;
/// Hard cap on how deep the default reveal goes, regardless of budget.
const TREE_MAX_REVEAL_DEPTH: usize = 6;

/// Picks how many levels to reveal by default. Sparse (mostly linear) histories
/// expand many levels; wide (heavily branched) ones stop earlier — chosen by
/// growing the revealed depth while the visible-row count stays within budget.
fn adaptive_expanded_rows(all_rows: &[PickerRow]) -> HashSet<String> {
    let max_depth = all_rows.iter().map(|row| row.depth).max().unwrap_or(0);
    // Always reveal two levels when the tree has any depth, then grow greedily.
    let mut reveal_depth = max_depth.min(1);
    let mut depth = 2;
    while depth <= max_depth.min(TREE_MAX_REVEAL_DEPTH) {
        let visible = all_rows.iter().filter(|row| row.depth <= depth).count();
        if visible <= TREE_VISIBLE_ROW_BUDGET {
            reveal_depth = depth;
            depth += 1;
        } else {
            break;
        }
    }
    all_rows
        .iter()
        .filter(|row| row.depth < reveal_depth)
        .map(|row| row.id.clone())
        .collect()
}

fn push_tree_rows(
    parent_id: Option<&str>,
    depth: usize,
    nodes: &[TreeNodeView],
    rows: &mut Vec<PickerRow>,
) {
    for node in nodes
        .iter()
        .filter(|node| node.parent_id.as_deref() == parent_id)
    {
        rows.push(PickerRow {
            id: node.id.clone(),
            parent_id: node.parent_id.clone(),
            depth,
            prefix: String::new(),
            label: node.label.clone(),
            active: node.active,
            has_children: nodes
                .iter()
                .any(|candidate| candidate.parent_id.as_deref() == Some(node.id.as_str())),
            detail: String::new(),
            reasoning_efforts: Vec::new(),
        });
        push_tree_rows(Some(node.id.as_str()), depth + 1, nodes, rows);
    }
}

fn visible_tree_rows(rows: &[PickerRow], expanded: &HashSet<String>) -> Vec<PickerRow> {
    let mut visible = Vec::new();
    push_visible_tree_rows(None, "", rows, expanded, &mut visible);
    visible
}

fn push_visible_tree_rows(
    parent_id: Option<&str>,
    ancestor_prefix: &str,
    rows: &[PickerRow],
    expanded: &HashSet<String>,
    visible: &mut Vec<PickerRow>,
) {
    let children = rows
        .iter()
        .filter(|row| row.parent_id.as_deref() == parent_id)
        .collect::<Vec<_>>();
    let child_count = children.len();
    for (index, row) in children.into_iter().enumerate() {
        let last = index + 1 == child_count;
        let connector = if last { "└── " } else { "├── " };
        let mut next = row.clone();
        next.prefix = format!("{ancestor_prefix}{connector}");
        visible.push(next);
        if expanded.contains(&row.id) {
            let branch = if last { "    " } else { "│   " };
            push_visible_tree_rows(
                Some(row.id.as_str()),
                &format!("{ancestor_prefix}{branch}"),
                rows,
                expanded,
                visible,
            );
        }
    }
}
