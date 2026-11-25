//! How a tool call reads in the transcript: a verb, the target, the result
//! on one line, and the output below it (folded by the builder).

use std::path::Path;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::lang::t;
use crate::theme::{self, Surface};
use crate::ui_builder::DIFF_HEAD_LINES;
use crate::{UiKind, UiLine};

/// Generic output lines kept when a block is collapsed.
const GENERIC_HEAD_LINES: usize = 6;
/// Characters kept per output line (a minified file would wrap for pages).
const MAX_LINE_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolState {
    Running,
    Done,
    Failed,
}

/// Which end of the output a collapsed block keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyMode {
    /// The last lines (command output: the result is at the end).
    Tail,
    /// The first `n` lines (diffs, listings).
    Head(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct ToolView {
    pub(crate) verb: String,
    /// The target: the command, the path, the query.
    pub(crate) arg: String,
    /// The result after the target: `退出码 101`, `+2 −2`, `90 行`.
    pub(crate) extra: Vec<Span<'static>>,
    pub(crate) state: ToolState,
    pub(crate) body: Vec<UiLine>,
    pub(crate) body_mode: BodyMode,
    /// Output lines hang off a `│` rule; diffs carry their own line numbers.
    pub(crate) gutter: bool,
}

pub(crate) fn tool_view(name: &str, output: &str, running: bool) -> ToolView {
    let mut view = ToolView {
        verb: tool_verb(name),
        arg: String::new(),
        extra: Vec::new(),
        state: if running {
            ToolState::Running
        } else {
            ToolState::Done
        },
        body: Vec::new(),
        body_mode: BodyMode::Head(GENERIC_HEAD_LINES),
        gutter: true,
    };
    let value = serde_json::from_str::<serde_json::Value>(output)
        .ok()
        .filter(serde_json::Value::is_object);

    // `!command` typed by the user: run here, not by the model.
    if let Some(command) = name.strip_prefix("! ") {
        view.arg = first_line(command);
        match &value {
            Some(value) => shell_view(&mut view, value, running),
            None => {
                view.body = text_lines(output);
                view.body_mode = BodyMode::Tail;
            }
        }
        return view;
    }
    if is_shell_tool(name) || name == "write_stdin" {
        match &value {
            Some(value) => shell_view(&mut view, value, running),
            None => {
                view.body = text_lines(output);
                view.body_mode = BodyMode::Tail;
            }
        }
        return view;
    }
    // Any tool can fail with {"error": "…"}; show the message, not raw JSON.
    if let Some(error) = value.as_ref().and_then(json_error) {
        view.state = ToolState::Failed;
        let mut lines = error.lines();
        view.extra = vec![Span::styled(
            lines.next().unwrap_or_default().to_string(),
            theme::err(),
        )];
        view.body = lines.map(|line| dim_line(line.to_string())).collect();
        return view;
    }
    if let Some(value) = &value {
        if projected_view(&mut view, name, value) {
            return view;
        }
    }
    if let Some(diff) = diff_from_tool_output(output) {
        diff_view(&mut view, &diff);
        view.body.extend(rejected_hunks_line(output));
        return view;
    }
    // Anything else: the first line as the target, the rest below it.
    let mut lines = text_lines(output);
    if !lines.is_empty() {
        view.arg = lines.remove(0).plain();
    }
    view.body = lines;
    view
}

/// The verb a tool call reads as.
fn tool_verb(name: &str) -> String {
    let verb = match name {
        "bash" | "exec_command" | "execute" | "shell_command" => t("运行", "Ran"),
        "edit" | "str_replace" | "hashline_edit" | "apply_patch" => t("编辑", "Edited"),
        "write" => t("写入", "Wrote"),
        "read" => t("读取", "Read"),
        "ls" => t("列出", "Listed"),
        "rg" | "ripgrep" | "search" => t("搜索", "Searched"),
        "web_search" => t("搜索网页", "Searched the web"),
        "write_stdin" => t("输入", "Sent"),
        "outline" => t("大纲", "Outlined"),
        "checkpoint" => t("存档", "Checkpointed"),
        "web_fetch" => t("抓取网页", "Fetched"),
        "generate_image" => t("生成图片", "Generated"),
        other if other.starts_with("! ") => t("本地运行", "Ran locally"),
        // Unknown tools (MCP too) keep their real name; `mcp__a__b` reads
        // as `a/b`.
        other => {
            return match other.strip_prefix("mcp__") {
                Some(rest) => rest.replacen("__", "/", 1),
                None => other.to_string(),
            };
        }
    };
    verb.to_string()
}

fn is_shell_tool(name: &str) -> bool {
    matches!(name, "bash" | "exec_command" | "execute" | "shell_command")
}

fn json_error(value: &serde_json::Value) -> Option<String> {
    value
        .get("error")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn first_line(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default().to_string();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

/// One output line as the transcript shows it: tabs as four spaces, other
/// control characters dropped (a terminal would act on them), and long
/// lines cut.
fn clip(line: &str) -> String {
    let mut clipped = String::with_capacity(line.len().min(MAX_LINE_CHARS + 4));
    let mut count = 0usize;
    for ch in line.chars() {
        if count >= MAX_LINE_CHARS {
            clipped.push('…');
            break;
        }
        match ch {
            '\t' => {
                clipped.push_str("    ");
                count += 4;
            }
            ch if ch.is_control() => {}
            ch => {
                clipped.push(ch);
                count += 1;
            }
        }
    }
    clipped
}

fn dim_line(text: String) -> UiLine {
    UiLine::new(
        UiKind::Tool,
        Line::from(Span::styled(clip(&text), theme::dim())),
    )
}

fn text_lines(output: &str) -> Vec<UiLine> {
    output
        .trim_end_matches('\n')
        .lines()
        .map(|line| dim_line(line.to_string()))
        .collect()
}

/// A shell result: the command as the target, the exit status as the
/// result, stdout then stderr below.
fn shell_view(view: &mut ToolView, value: &serde_json::Value, running: bool) {
    if let Some(command) = value.get("command").and_then(serde_json::Value::as_str) {
        view.arg = first_line(command);
    }
    view.body_mode = BodyMode::Tail;
    let background = value
        .get("running")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if background {
        // A long-running command reported mid-flight carries a session.
        view.extra = vec![Span::styled(t("后台运行中", "still running"), theme::dim())];
    } else if !running {
        let exit_code = value.get("exit_code").and_then(serde_json::Value::as_i64);
        let timed_out = value
            .get("timed_out")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if timed_out {
            view.state = ToolState::Failed;
            view.extra = vec![Span::styled(t("超时", "timed out"), theme::err())];
        } else if let Some(code) = exit_code.filter(|code| *code != 0) {
            view.state = ToolState::Failed;
            let label = match crate::lang::lang() {
                crate::lang::Lang::Zh => format!("退出码 {code}"),
                crate::lang::Lang::En => format!("exit {code}"),
            };
            view.extra = vec![Span::styled(label, theme::err())];
        }
    }
    for key in ["stdout", "stderr"] {
        if let Some(body) = value.get(key).and_then(serde_json::Value::as_str) {
            view.body.extend(text_lines(body));
        }
    }
}

/// Tools whose JSON result reads better as one line: `read`, `ls`,
/// `generate_image`. Returns false for the rest.
fn projected_view(view: &mut ToolView, name: &str, value: &serde_json::Value) -> bool {
    match name {
        "read" => {
            let Some(path) = value.get("path").and_then(serde_json::Value::as_str) else {
                return false;
            };
            view.arg = display_path(path);
            let lines = value
                .get("lines_read")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let offset = value
                .get("offset")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1)
                .max(1);
            let truncated = value
                .get("truncated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let range = if lines == 0 {
                String::new()
            } else if offset > 1 || truncated {
                let end = offset + lines - 1;
                match crate::lang::lang() {
                    crate::lang::Lang::Zh => format!("{offset}–{end} 行"),
                    crate::lang::Lang::En => format!("lines {offset}–{end}"),
                }
            } else {
                match crate::lang::lang() {
                    crate::lang::Lang::Zh => format!("{lines} 行"),
                    crate::lang::Lang::En => format!("{lines} lines"),
                }
            };
            if !range.is_empty() {
                view.extra = vec![Span::styled(range, theme::dim())];
            }
            true
        }
        "ls" => {
            let Some(path) = value.get("path").and_then(serde_json::Value::as_str) else {
                return false;
            };
            view.arg = display_path(path);
            true
        }
        "generate_image" => {
            let Some(paths) = value.get("paths").and_then(serde_json::Value::as_array) else {
                return false;
            };
            let mut paths = paths.iter().filter_map(serde_json::Value::as_str);
            view.arg = paths.next().map(display_path).unwrap_or_default();
            view.body = paths.map(|path| dim_line(display_path(path))).collect();
            true
        }
        _ => false,
    }
}

/// A path relative to the working folder when inside it, else with `~`.
fn display_path(path: &str) -> String {
    let cwd = std::env::current_dir().ok();
    if let Some(cwd) = cwd {
        if let Ok(rest) = Path::new(path).strip_prefix(&cwd) {
            let rest = rest.to_string_lossy().replace('\\', "/");
            if !rest.is_empty() {
                return rest;
            }
        }
    }
    crate::compact_home_path(path)
}

fn rejected_hunks_line(output: &str) -> Option<UiLine> {
    let rejected = serde_json::from_str::<serde_json::Value>(output)
        .ok()?
        .get("rejected_hunks")?
        .as_array()?
        .len();
    if rejected == 0 {
        return None;
    }
    let text = match crate::lang::lang() {
        crate::lang::Lang::Zh => format!("你拒绝了 {rejected} 处改动（没有应用）"),
        crate::lang::Lang::En => format!("{rejected} hunk(s) rejected by user (not applied)"),
    };
    Some(dim_line(text))
}

fn diff_from_tool_output(output: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(output).ok()?;
    value
        .get("diff")
        .and_then(serde_json::Value::as_str)
        .filter(|diff| !diff.trim().is_empty())
        .map(str::to_string)
}

/// An edit: the path and `+a −r` on the header, numbered diff lines below.
fn diff_view(view: &mut ToolView, diff: &str) {
    view.gutter = false;
    view.body_mode = BodyMode::Head(DIFF_HEAD_LINES);
    match parse_unified_diff(diff) {
        Some(parsed) => {
            view.arg = parsed.path.clone();
            view.extra = vec![
                Span::styled(format!("+{}", parsed.additions), theme::add()),
                Span::raw(" "),
                Span::styled(format!("−{}", parsed.removals), theme::del()),
            ];
            view.body = render_edit_lines(&parsed.lines);
        }
        None => {
            view.body = render_intra_line_diff(&diff.lines().collect::<Vec<_>>());
        }
    }
}

struct ParsedEditDiff {
    path: String,
    additions: usize,
    removals: usize,
    lines: Vec<EditDiffLine>,
}

enum EditDiffLine {
    Context { line: Option<usize>, text: String },
    Add { line: usize, text: String },
    Remove { line: usize, text: String },
    Gap,
}

/// The highlight for the changed part of a line: a deeper fill where fills
/// are drawn, reverse video where they are not.
fn word_style(surface: Surface) -> Style {
    match theme::surface(surface) {
        Some(color) => Style::new().bg(color),
        None => Style::new().add_modifier(Modifier::REVERSED),
    }
}

fn line_fill(surface: Surface) -> Style {
    match theme::surface(surface) {
        Some(color) => Style::new().bg(color),
        None => Style::new(),
    }
}

/// `  1469 - text`: the number untinted, the sign and the text on the
/// line's fill (the painter extends the last span's fill to the edge).
fn numbered_line(
    kind: UiKind,
    number: Option<usize>,
    sign: char,
    text: &str,
    changed: Option<(usize, usize)>,
) -> UiLine {
    let (fg, fill, word) = match kind {
        UiKind::DiffAdd => (
            theme::add(),
            line_fill(Surface::AddLine),
            word_style(Surface::AddWord),
        ),
        UiKind::DiffRemove => (
            theme::del(),
            line_fill(Surface::DelLine),
            word_style(Surface::DelWord),
        ),
        _ => (Style::new(), Style::new(), Style::new()),
    };
    let number = number.map(|n| n.to_string()).unwrap_or_default();
    let mut spans = vec![
        Span::styled(format!("{number:>5} "), theme::dim()),
        Span::styled(format!("{sign} "), fg.patch(fill)),
    ];
    let text = clip(text);
    match changed {
        Some((start, end)) if start < end => {
            let before: String = text.chars().take(start).collect();
            let middle: String = text.chars().skip(start).take(end - start).collect();
            let after: String = text.chars().skip(end).collect();
            spans.push(Span::styled(before, fg.patch(fill)));
            spans.push(Span::styled(middle, fg.patch(fill).patch(word)));
            spans.push(Span::styled(after, fg.patch(fill)));
        }
        _ => spans.push(Span::styled(text, fg.patch(fill))),
    }
    UiLine::new(kind, Line::from(spans))
}

fn render_edit_lines(lines: &[EditDiffLine]) -> Vec<UiLine> {
    let mut rendered = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        // A single removed line followed by a single added one: highlight
        // the part that changed.
        if let (
            EditDiffLine::Remove {
                line: old_line,
                text: old,
            },
            Some(EditDiffLine::Add {
                line: new_line,
                text: new,
            }),
        ) = (&lines[index], lines.get(index + 1))
        {
            let single = !matches!(lines.get(index + 2), Some(EditDiffLine::Add { .. }))
                && !matches!(
                    index.checked_sub(1).and_then(|i| lines.get(i)),
                    Some(EditDiffLine::Remove { .. })
                );
            if single {
                let (prefix, old_end, new_end) = changed_span(old, new);
                rendered.push(numbered_line(
                    UiKind::DiffRemove,
                    Some(*old_line),
                    '-',
                    old,
                    Some((prefix, old_end)),
                ));
                rendered.push(numbered_line(
                    UiKind::DiffAdd,
                    Some(*new_line),
                    '+',
                    new,
                    Some((prefix, new_end)),
                ));
                index += 2;
                continue;
            }
        }
        rendered.push(match &lines[index] {
            EditDiffLine::Context { line, text } => {
                let number = line.map(|n| n.to_string()).unwrap_or_default();
                UiLine::new(
                    UiKind::Tool,
                    Line::from(vec![
                        Span::styled(format!("{number:>5} "), theme::dim()),
                        Span::raw(format!("  {}", clip(text))),
                    ]),
                )
            }
            EditDiffLine::Add { line, text } => {
                numbered_line(UiKind::DiffAdd, Some(*line), '+', text, None)
            }
            EditDiffLine::Remove { line, text } => {
                numbered_line(UiKind::DiffRemove, Some(*line), '-', text, None)
            }
            EditDiffLine::Gap => UiLine::new(
                UiKind::Tool,
                Line::from(Span::styled(format!("{:>5} ", "⋮"), theme::faint())),
            ),
        });
        index += 1;
    }
    rendered
}

/// Common prefix length and the end of the changed part in each line (in
/// chars).
fn changed_span(old: &str, new: &str) -> (usize, usize, usize) {
    let old_chars = old.chars().collect::<Vec<_>>();
    let new_chars = new.chars().collect::<Vec<_>>();
    let mut prefix = 0usize;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut old_end = old_chars.len();
    let mut new_end = new_chars.len();
    while old_end > prefix && new_end > prefix && old_chars[old_end - 1] == new_chars[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    (prefix, old_end, new_end)
}

fn parse_unified_diff(diff: &str) -> Option<ParsedEditDiff> {
    let mut path = None;
    let mut lines = Vec::new();
    let mut old_line = 0usize;
    let mut new_line = 0usize;
    let mut in_hunk = false;
    let mut additions = 0usize;
    let mut removals = 0usize;

    for raw in diff.lines() {
        if raw.starts_with("diff --git ") {
            path.get_or_insert_with(|| diff_file_path(raw));
            if in_hunk {
                lines.push(EditDiffLine::Gap);
                in_hunk = false;
            }
            continue;
        }
        if raw.starts_with("--- ") || raw.starts_with("+++ ") || raw.starts_with("index ") {
            continue;
        }
        if raw.starts_with("@@") {
            let (old_start, new_start) = parse_hunk_header(raw)?;
            if in_hunk {
                lines.push(EditDiffLine::Gap);
            }
            old_line = old_start;
            new_line = new_start;
            in_hunk = true;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if let Some(text) = raw.strip_prefix('+') {
            additions += 1;
            lines.push(EditDiffLine::Add {
                line: new_line,
                text: text.to_string(),
            });
            new_line += 1;
        } else if let Some(text) = raw.strip_prefix('-') {
            removals += 1;
            lines.push(EditDiffLine::Remove {
                line: old_line,
                text: text.to_string(),
            });
            old_line += 1;
        } else if let Some(text) = raw.strip_prefix(' ') {
            lines.push(EditDiffLine::Context {
                line: Some(new_line),
                text: text.to_string(),
            });
            old_line += 1;
            new_line += 1;
        } else if raw == r"\ No newline at end of file" {
            lines.push(EditDiffLine::Context {
                line: None,
                text: raw.to_string(),
            });
        }
    }

    Some(ParsedEditDiff {
        path: path?,
        additions,
        removals,
        lines,
    })
}

fn diff_file_path(line: &str) -> String {
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 4 {
        return line.to_string();
    }
    let old_path = parts[2].strip_prefix("a/").unwrap_or(parts[2]);
    let new_path = parts[3].strip_prefix("b/").unwrap_or(parts[3]);
    if old_path == new_path {
        new_path.to_string()
    } else {
        format!("{old_path} → {new_path}")
    }
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old = parts.next()?;
    let new = parts.next()?;
    if !old.starts_with('-') || !new.starts_with('+') {
        return None;
    }
    Some((parse_hunk_start(&old[1..])?, parse_hunk_start(&new[1..])?))
}

fn parse_hunk_start(value: &str) -> Option<usize> {
    value
        .split_once(',')
        .map(|(start, _)| start)
        .unwrap_or(value)
        .parse()
        .ok()
}

fn is_diff_change_line(line: &str) -> bool {
    (line.starts_with('+') && !line.starts_with("+++"))
        || (line.starts_with('-') && !line.starts_with("---"))
}

/// A diff without a parseable header: `+`/`-` lines coloured, a lone
/// removed/added pair highlighted where it changed.
fn render_intra_line_diff(lines: &[&str]) -> Vec<UiLine> {
    let mut rendered = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        if !is_diff_change_line(lines[index]) {
            let style = if lines[index].starts_with("@@") || lines[index].starts_with("diff ") {
                theme::dim()
            } else {
                Style::new()
            };
            rendered.push(UiLine::new(
                UiKind::Tool,
                Line::from(Span::styled(clip(lines[index]), style)),
            ));
            index += 1;
            continue;
        }
        let removed_start = index;
        while index < lines.len()
            && is_diff_change_line(lines[index])
            && lines[index].starts_with('-')
        {
            index += 1;
        }
        let added_start = index;
        while index < lines.len()
            && is_diff_change_line(lines[index])
            && lines[index].starts_with('+')
        {
            index += 1;
        }
        let removed = &lines[removed_start..added_start];
        let added = &lines[added_start..index];
        if removed.len() == 1 && added.len() == 1 {
            let old = &removed[0][1..];
            let new = &added[0][1..];
            let (prefix, old_end, new_end) = changed_span(old, new);
            rendered.push(numbered_line(
                UiKind::DiffRemove,
                None,
                '-',
                old,
                Some((prefix, old_end)),
            ));
            rendered.push(numbered_line(
                UiKind::DiffAdd,
                None,
                '+',
                new,
                Some((prefix, new_end)),
            ));
        } else {
            for line in removed {
                rendered.push(numbered_line(
                    UiKind::DiffRemove,
                    None,
                    '-',
                    &line[1..],
                    None,
                ));
            }
            for line in added {
                rendered.push(numbered_line(UiKind::DiffAdd, None, '+', &line[1..], None));
            }
        }
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_text(view: &ToolView) -> String {
        view.body
            .iter()
            .map(UiLine::plain)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn extra_text(view: &ToolView) -> String {
        view.extra
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn failed_command_shows_exit_code_and_tail_mode() {
        let output = serde_json::json!({
            "command": "cargo test",
            "exit_code": 101,
            "stdout": "running 4 tests\ntest a ... FAILED",
            "stderr": "",
        })
        .to_string();
        let view = tool_view("bash", &output, false);
        assert_eq!(view.verb, "运行");
        assert_eq!(view.arg, "cargo test");
        assert_eq!(view.state, ToolState::Failed);
        assert_eq!(extra_text(&view), "退出码 101");
        assert_eq!(view.body_mode, BodyMode::Tail);
        assert!(body_text(&view).contains("FAILED"));
    }

    #[test]
    fn successful_command_has_no_result_label() {
        let output =
            serde_json::json!({"command": "ls", "exit_code": 0, "stdout": "a\nb"}).to_string();
        let view = tool_view("bash", &output, false);
        assert_eq!(view.state, ToolState::Done);
        assert!(view.extra.is_empty());
        assert_eq!(view.body.len(), 2);
    }

    #[test]
    fn edit_diff_has_counts_numbers_and_word_highlight() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n@@ -10,3 +10,3 @@\n fn a() {\n-    let w = 1;\n+    let w = ch.width();\n }\n";
        let output = serde_json::json!({ "diff": diff }).to_string();
        let view = tool_view("edit", &output, false);
        assert_eq!(view.verb, "编辑");
        assert_eq!(view.arg, "src/lib.rs");
        assert_eq!(extra_text(&view), "+1 −1");
        assert!(!view.gutter);
        let text = body_text(&view);
        assert!(text.contains("   11 -     let w = 1;"), "{text}");
        assert!(text.contains("   11 +     let w = ch.width();"), "{text}");
        // The changed part sits on its own span.
        let added = view
            .body
            .iter()
            .find(|line| line.kind == UiKind::DiffAdd)
            .unwrap();
        assert!(added
            .line
            .spans
            .iter()
            .any(|span| span.content == "ch.width()"));
    }

    #[test]
    fn error_result_shows_the_message_in_the_error_colour() {
        let view = tool_view("read", r#"{"error":"no such file"}"#, false);
        assert_eq!(view.state, ToolState::Failed);
        assert_eq!(extra_text(&view), "no such file");
        assert_eq!(view.extra[0].style, theme::err());
    }

    #[test]
    fn read_shows_path_and_line_range() {
        let output = r#"{"path":"notes.md","lines_read":91,"offset":1440,"truncated":false}"#;
        let view = tool_view("read", output, false);
        assert_eq!(view.verb, "读取");
        assert_eq!(view.arg, "notes.md");
        assert_eq!(extra_text(&view), "1440–1530 行");
        assert!(view.body.is_empty());
    }

    #[test]
    fn rejected_hunks_are_counted_after_the_diff() {
        let diff = "diff --git a/a b/a\n@@ -1 +1 @@\n-x\n+y\n";
        let output = serde_json::json!({ "diff": diff, "rejected_hunks": ["h2"] }).to_string();
        let view = tool_view("str_replace", &output, false);
        assert!(body_text(&view).contains("你拒绝了 1 处改动"));
    }

    #[test]
    fn output_tabs_become_spaces_and_control_characters_go() {
        let output = serde_json::json!({
            "command": "du -sh target",
            "exit_code": 0,
            "stdout": "4.1G\ttarget\r\n\u{1b}[0mdone",
        })
        .to_string();
        let view = tool_view("bash", &output, false);
        assert_eq!(body_text(&view), "4.1G    target\n[0mdone");
    }

    #[test]
    fn mcp_tools_read_as_server_and_tool() {
        assert_eq!(
            tool_view("mcp__github__list_issues", "", false).verb,
            "github/list_issues"
        );
    }

    #[test]
    fn english_wording_when_the_locale_is_english() {
        crate::lang::with_lang(crate::lang::Lang::En, || {
            let output = serde_json::json!({"command": "false", "exit_code": 1}).to_string();
            let view = tool_view("bash", &output, false);
            assert_eq!(view.verb, "Ran");
            assert_eq!(extra_text(&view), "exit 1");
        });
    }
}
