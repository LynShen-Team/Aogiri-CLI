# TUI Architecture

This document explains how LynShen-CLI's TUI is structured today and how data flows from the agent runtime to terminal output.

## Overview

LynShen's TUI is a ratatui application running on the terminal's alternate screen. It keeps a thin state layer but renders natively with ratatui's `Layout` and widgets. The pipeline is:

1. `AgentCore` produces `AgentEvent`s.
2. `TuiApp` owns the interactive TUI state and applies those events.
3. `UiBuilder` converts TUI state into a `UiDocument` (a transcript plus a flat list of control lines).
4. `TerminalRenderer` owns a `ratatui::Terminal` on the alternate screen. Each draw it splits the screen into regions, paints each region's styled lines into its rect, and draws the chrome (the composer's two rules, the scrollbar) itself.
5. Ratatui diffs the frame buffer against the previous frame and writes only the cells that changed.

This keeps the architecture close to the product model: runtime events in, state update, document build, terminal render.

The TUI runs on the terminal's **alternate screen** (a ratatui-owned full-screen surface), not the main scrollback buffer. Because the alternate screen has no native scrollback, the transcript is scrolled in-app with PageUp/PageDown; any other key snaps the viewport back to the live tail.

### Screen layout

`draw()` splits the frame vertically, bottom-up, so the input and status always fit:

- **transcript** — scrollable history viewport with a `Scrollbar` on its right column; takes the remaining height.
- **live** — pickers and confirmations (approval, trust, plan), pending messages, the progress row and one-line notices.
- **input box** — the composer between a faint rule above and below it, no side borders. A confirmation takes its place (no `Input` lines are emitted).
- **candidates** — the `/` command palette, `@` file matches or the `?` shortcut sheet (`Candidate`/`CandidateSelected` lines).
- **status bar** — one plain row: model and effort, approval mode, branch, context use, balance. Hidden while candidates show.

The renderer derives these regions from the flat `UiDocument.controls` list using the builder's ordering invariants (the status line is last, the candidates are the trailing run of candidate kinds, the input box is the trailing run of `Input` lines, and everything above is the live region).

The composer follows the conversation: while the transcript is shorter than the space left, the regions stack from the top with one blank row after the transcript; once it fills the space, the composer sits at the bottom. While the view is scrolled up, the last transcript row says how to get back.

### Look

- **Colours** come from `theme.rs`: text keeps the terminal's own foreground; the brand accent (teal) marks the prompt, the selection and running work; green/red mark additions and deletions (and failures); an attention tone marks what waits on the user. Each role resolves through the colour depth (`NO_COLOR` or `TERM=dumb` → bold/dim/reverse only; 16 named colours; the 256 palette; truecolor, read from `COLORTERM`, `TERM`, `TERM_PROGRAM`, `WT_SESSION`) and the background, queried once at startup with OSC 11 (answered or not, a DA1 request ends the read), else `COLORFGBG`; `LYNSHEN_THEME=dark|light` forces it. Background fills (user turns, diff lines, the selected row, drag selection) are blended from the detected background and only drawn when it is known and the terminal has at least 256 colours.
- **Words** come from `lang.rs`: Chinese unless the locale is English (`LYNSHEN_LANG=zh|en` wins over `LC_ALL`/`LC_MESSAGES`/`LANG`). Command names, model names and the engine's own messages are not translated; built-in commands get a Chinese description in the palette.
- **Welcome**: the LynShen mark (two trunks and a light, in quadrant blocks, four rows) beside the name and version, the LynShen account and plan (looked up off the UI thread, `lynshen_agent_core::lynshen_account`), the model, and the folder; below it the `/ @ ! ?` hints. Below 48 columns, or where the console font lacks block elements, only the lines show.
- **Tool calls** (`tool_preview.rs` → `ToolView`): a status dot (accent while running, dim when done, error tone when failed), a verb, the target and the result on one clickable row; the output below hangs off a `│` rule, folded to its last lines (commands) or first lines (diffs, listings). A click on the row or ctrl+o (all blocks) opens it. Diffs carry line numbers and highlight the changed part of a lone removed/added pair.

## Entry Point and Runtime Boundary

The binary entry point is `src/main.rs`.

- `Runtime(AgentCore)` is a thin adapter.
- It implements the `lynshen_tui::TuiRuntime` trait.
- `main()` creates `AgentCore`, starts the update check, and runs `TuiApp::new(Runtime(core)).run()`.

`TuiRuntime` is the main boundary between the TUI crate and the agent runtime:

- `startup_events()`
- `model_status_event()`
- `submit_user_message()`
- `interrupt()`
- `handle_command()`
- `poll_events()`

Steering mid-turn messages is not a trait method: queued input is submitted
through `submit_user_message`, and the core `steer` command is reached via
`handle_command` like other slash commands.

This separation is useful because the TUI does not need to know agent internals. It only consumes events and sends user intent back through a small trait.

## Core State: `TuiApp`

`crates/tui/src/lib.rs` defines `TuiApp<R>`, which is the main controller for the TUI.

Its responsibilities are:

- hold interactive state
- receive keyboard and paste input
- poll runtime events
- maintain chat/history state
- build the UI document
- drive rendering timing

Important state fields include:

- `input: InputBuffer`: line editor state, selection, cursor, large-paste placeholders
- `chat: Vec<ChatLine>`: persisted visible transcript in TUI form
- `assistant_index: Option<usize>`: the `ChatLine::Assistant` entry currently receiving streamed deltas (streaming text lives directly in `chat`)
- `reasoning_index: Option<usize>` and `thinking_tokens`: reasoning display state
- `activity: ActivityState`: current phase such as connecting, thinking, output, tool, compacting
- `commands` and `completion_index`: slash-command completion
- `picker_view: Option<PickerState>`: temporary selection UIs for tree/resume/model flows
- `pending_messages`: queued user messages waiting for steering behavior
- `rendered_history_cache`: cached rendered transcript lines keyed by width and revision
- model/status counters such as provider, model, context usage, input/output tokens

Conceptually, `TuiApp` is both the application state store and the interaction controller.

## Event Model

The TUI is event-driven around `AgentEvent`.

`apply_events()` is the central reducer-like function. It maps runtime events into TUI state changes.

Examples:

- `Startup` -> push startup box into chat history
- `UserMessage` -> append a user line
- `ThinkingStart` / `ReasoningDelta` -> update reasoning state
- `AssistantStart` / `AssistantDelta` -> stream live assistant output
- `ToolStart` / `ToolUpdate` / `ToolOutput` -> update tool blocks in transcript
- `ModelStatus` -> refresh provider/model/status metadata
- `TreeView`, `ResumeView`, `ModelView` -> open a picker UI
- `Goal`, `Info`, `Error` -> append system or error lines
- `Status("ready")` -> commit live assistant text and finish current activity

This is one of the cleanest parts of the design: most UI behavior is expressed as a direct reaction to runtime events.

## Input System

The input editor lives in `crates/tui/src/input.rs`.

### `InputBuffer`

`InputBuffer` is a small editor model with:

- per-cell storage
- cursor position
- optional selection anchor
- movement by character, word, line, and document
- backspace/delete behavior
- multi-line editing

It also supports a special `LargePaste(String)` cell. Large pasted content is stored as one logical cell and displayed as a placeholder like `[Pasted: N chars]`. That avoids turning very large pastes into huge per-character editor state.

### Rendered Cursor and Selection

The input renderer embeds a logical cursor marker into the text and reverse-video ANSI sequences around selected ranges. The renderer extracts that marker from the input region (`extract_cursor`) to position the terminal's real hardware cursor inside the input box, so the caret sits between characters (insert-style) like a normal text field. Selection stays reverse-video highlighted in the buffer.

### Paste Burst Handling

`TuiApp` also uses `PasteBurst` to distinguish typed ASCII from a fast burst that is probably a paste. This reduces noisy per-character behavior during terminal paste and helps preserve responsive rendering.

## Interaction Modes

The TUI has two main interaction modes.

### 1. Normal editor mode

Handled by `handle_key_at()`.

Key behaviors include:

- plain text editing
- multi-line input with `Shift+Enter`, `Ctrl+Enter` or `Ctrl+J`
- slash-command palette (fuzzy, `fuzzy.rs`) and `@` file matches via `Tab`, `Up`, `Down`
- word navigation with `Ctrl`/`Alt` + arrows
- `Up`/`Down` on the first/last line walk the messages sent from this composer (the draft comes back at the end)
- `Esc` clears input when idle, or sends `interrupt()` while a turn is running (queued pending messages still start the next turn)
- `Ctrl+C` interrupts a running turn, else clears the draft, else asks for a second press within two seconds to quit; `Ctrl+D` quits on an empty, idle composer
- `Ctrl+O` opens or folds every tool block; `?` on an empty composer opens the shortcut sheet (any key closes it)
- `BackTab` cycles the approval mode; `Ctrl+T` cycles reasoning effort for the current model

### 2. Picker mode

Handled by `handle_picker_key()` and `handle_picker_prompt_key()`.

Picker mode is used for:

- branch/tree checkout
- session resume
- model selection
- fork/delete prompts inside the tree picker

This mode is intentionally modal and simple. Instead of building a general widget system, the app swaps into a dedicated picker state object.

List pickers (model, resume, rewind, login) take a typed filter (fuzzy, matched letters highlighted); the model picker's effort moves with Left/Right or Tab. Confirmations (approval, trust, plan) number their options: a digit picks and confirms. Esc (or Ctrl+C) closes a picker; an approval closed that way is answered "deny".

## UI Data Model

The TUI uses a small intermediate representation instead of rendering directly from raw state.

### Transcript-level items

`ChatLine` represents semantic transcript entries:

- `Startup`
- `User`
- `Assistant`
- `Reasoning`
- `Tool`
- `System`
- `Error`

### Render-level items

`UiLine` is a lower-level visual line with:

- `kind: UiKind`
- `text: String`

`UiKind` carries styling intent such as user, assistant, tool, error, selected, diff-add, diff-remove, and so on.

### Document-level structure

`UiDocument` splits the screen into two conceptual parts:

- `history`: transcript/history area
- `controls`: live area below history

The live area contains things like:

- live assistant streaming output
- thinking indicator
- picker UI
- pending-message notice
- input box text
- progress line
- bottom status line

This is a good fit for chat-style TUIs: immutable-ish transcript above, active controls below.

## UI Composition: `UiBuilder`

`crates/tui/src/ui_builder.rs` builds a `UiDocument` from current TUI state.

`TuiApp::build_document()` prepares the inputs, then chains builder calls like:

- `rendered_history_lines(...)`
- `thinking_indicator(...)`
- `live_assistant(...)`
- `picker(...)`
- `pending_messages(...)`
- `input(...)`
- `progress(...)`
- `bottom_status(...)`
- `reset_screen(...)`

This is effectively a manual view-composition pipeline.

### What `UiBuilder` renders

- startup welcome box with branded ASCII layout
- markdown-rendered assistant and reasoning text
- tool output blocks and compact previews
- command completion candidates
- picker rows with selection highlighting
- progress line with colored spinner
- bottom status line with model and token/context info

The builder is intentionally string-first. It produces styled text lines, not nested widgets.

## Supporting Presentation Modules

### `markdown.rs`

Renders a limited markdown subset for assistant/reasoning output:

- emphasis
- inline formatting
- code blocks
- tables

This allows the assistant transcript to look structured without depending on a full markdown UI engine.

### `tool_preview.rs`

Builds compact previews for tool output.

It includes special handling for:

- bash output projection
- diff extraction
- edit diff parsing
- intra-line diff highlighting

This is a strong product-oriented choice: tool output is not dumped raw by default, but transformed into something readable in a tight terminal layout.

### `picker.rs`

Implements `PickerState` and tree/model/resume navigation behavior.

Notably, the tree picker maintains both:

- all tree rows
- visible rows derived from expansion state

So the UI can stay simple while still supporting hierarchical navigation.

## Rendering Pipeline

The rendering pipeline has a few layers.

### 1. Build a `UiDocument`

`TuiApp::build_document()` creates the current document.

### 2. Render with `TerminalRenderer`

`crates/tui/src/terminal_renderer.rs` owns a `ratatui::Terminal<CrosstermBackend<Stdout>>` on the alternate screen. Each frame it calls `terminal.draw(...)`, and `draw()`:

- splits the flat `controls` list into regions (`ControlRegions::split`)
- computes the region rects bottom-up
- selects the visible window of transcript lines (`visible_window`): the live tail by default, or lifted by the scroll offset, and draws a `Scrollbar`
- draws the composer's rules and paints the prompt between them
- paints the live, candidate, and status regions into their rects

Each region's lines are still styled with ANSI escapes (built by `UiBuilder`/`markdown`/`tool_preview`); `paint_ansi_line` translates those into `ratatui::Buffer` cell styles within a region's rect. Ratatui keeps the previous frame buffer and writes only the cells that changed, so the renderer implements no diffing of its own.

`ProjectedDocument` still exists as a flat projection (transcript plus control lines plus cursor position) but is now used only by tests to assert layout invariants.

With `--features preview`, `preview_support` draws sample scenes (welcome, a working turn, an approval, the palette, the model and session pickers, the shortcut sheet, a narrow terminal) in the dark, light and colourless themes and returns the cells as JSON; `cargo run -p lynshen-tui --example tui_preview --features preview` prints them for screenshots.

## Rendering Strategy and Performance Choices

This TUI is optimized around a few practical ideas.

### Cached transcript rendering

`rendered_history_cache` stores already-rendered history lines by:

- transcript revision
- width

So input changes or progress animation do not require rebuilding the whole transcript every frame.

### Window painting plus ratatui diffing

The renderer only paints the visible window (one screen of lines) into the frame buffer; the rest of the transcript stays out of the buffer. Ratatui then writes only the cells that changed against the previous frame.

This keeps per-frame terminal I/O small and the interface smooth during:

- streamed output
- spinner animation
- input editing
- scrolling the transcript

### Explicit frame scheduling

`FrameScheduler` decides when the next render should happen. It avoids a constant redraw loop and requests frames only when needed.

That keeps the UI responsive without wasting CPU.

## Terminal Control Model

The app manages terminal mode itself.

`TerminalGuard`:

- enables raw mode on entry
- enters the alternate screen
- hides the cursor
- enables bracketed paste
- on drop: disables bracketed paste, shows the cursor, leaves the alternate screen, and disables raw mode

Leaving the alternate screen restores the user's previous terminal contents, so the session does not leave its transcript behind in the shell. The renderer still builds styled lines with ANSI sequences (color, selection/caret display) in the `UiDocument`; ratatui's crossterm backend is responsible for clearing, cursor moves, and emitting the cell-level escapes during each draw.

Because the alternate screen has no native scrollback, the transcript is browsed in-app: PageUp/PageDown move a `scroll_offset` (clamped to the available range by the renderer), and any other key resets it to the live tail.

## Activity and Progress Model

`ActivityState` tracks the current phase of the agent turn:

- `Idle`
- `Connecting`
- `Compacting`
- `Thinking`
- `Reconnecting`
- `Output`
- `Tool`

It stores timing and token estimates, and `progress()` maps that into a `ProgressState` used by the bottom progress line.

This is a nice separation: the app tracks semantic activity, and the UI layer only asks for a renderable progress view.

## Design Characteristics

The current TUI design has several clear traits.

### Strengths

- small number of concepts
- direct event-to-state flow
- good terminal performance awareness
- minimal dependency on widget abstractions
- strong support for streaming and tool-heavy interaction
- clear runtime boundary through `TuiRuntime`

### Tradeoffs

- `TuiApp` is large and owns many responsibilities
- most UI composition is string-based, so some behavior is harder to validate structurally
- normal mode, picker mode, rendering policy, and event reduction all live close together
- there is no explicit reducer/view-model split yet

## Recommended Improvement

If I were to make one architectural improvement, I would split `TuiApp` into a dedicated state reducer and a controller shell.

Concretely:

- keep terminal polling, frame scheduling, and runtime I/O in `TuiApp`
- move `apply_events()`, status transitions, transcript mutation, and activity updates into a new `TuiState`

That would improve the code in three ways:

1. `TuiApp` would become easier to read because it would focus on orchestration.
2. State transitions could be unit-tested without terminal/rendering setup.
3. Future features like scrollback controls, richer transcript operations, or alternative frontends would be easier to add without growing one very large type.

This would be a good next step because it preserves the current lightweight design instead of replacing it with a framework.
