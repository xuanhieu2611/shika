//! The New picker and the worktree setup approval dialog: their state,
//! keys, and views. Launching itself lives in `launch/mod.rs`.
use super::*;

/// The prompt field's hint while an agent row is selected.
const PROMPT_HINT: &str = "What should it do? Leave empty to type in the terminal.";
/// The hint while Terminal is selected and an agent could take a prompt:
/// typing moves the selection to the first agent.
const TERMINAL_PROMPT_HINT: &str = "Type a prompt to start an agent instead.";
/// The hint when Terminal is the only choice: the text would be ignored.
const TERMINAL_ONLY_HINT: &str = "A terminal starts without a prompt.";
/// The Terminal row's detail while the prompt has text.
const TERMINAL_CLOSED: &str = "Clear the prompt to choose";

/// The New picker. `index` is a row in `new_choices(mode)` for `project`.
/// Task and checkout pickers have the optional prompt field; New Lead has
/// none. `error` is why the last start was refused, such as a prompt the
/// CLI would read as a flag.
#[derive(Clone)]
pub(crate) struct PickerDialog {
    pub(crate) project: String,
    pub(crate) index: usize,
    pub(crate) mode: PickerMode,
    pub(crate) prompt: Option<Entity<chat_input::ChatInput>>,
    pub(crate) error: Option<String>,
}

/// The picker's prompt field: multi-line, Shift+Enter for a new line, with
/// `text` and the caret at its end.
pub(super) fn new_prompt_input(
    text: String,
    chrome: Chrome,
    cx: &mut Context<Shika>,
) -> Entity<chat_input::ChatInput> {
    let input = cx.new(|cx| {
        let mut input = chat_input::ChatInput::new(String::new(), chrome, cx);
        input.label("Prompt".into());
        input.placeholder(PROMPT_HINT);
        input.field_id("new-prompt");
        input.max_height(px(140.));
        input.set_value(text, cx);
        input
    });
    // Every edit, typed or pasted, can close Terminal or clear a refusal.
    cx.subscribe(&input, |this, _, event, cx| {
        if matches!(event, chat_input::InputEvent::Changed) {
            this.picker_prompt_changed(cx);
        }
    })
    .detach();
    input
}

/// Whether the prompt counts as written. Whitespace starts the CLI with no
/// argument, so it does not.
fn has_prompt(text: &str) -> bool {
    !text.trim().is_empty()
}

/// Whether row `i` can be selected. Terminal takes no prompt, so while the
/// field has text it is closed, unless it is the only choice.
pub(super) fn choice_open(choices: &[CliPreset], i: usize, prompt: bool) -> bool {
    choices
        .get(i)
        .is_some_and(|c| !(prompt && c.id == "terminal" && choices.len() > 1))
}

/// The next open row after `index`, wrapping; `index` when none is.
pub(super) fn step_choice(
    choices: &[CliPreset],
    index: usize,
    forward: bool,
    prompt: bool,
) -> usize {
    let len = choices.len();
    let mut at = index;
    for _ in 0..len {
        at = if forward {
            (at + 1) % len
        } else {
            (at + len - 1) % len
        };
        if choice_open(choices, at, prompt) {
            return at;
        }
    }
    index
}

/// The selection after the prompt changed: a closed Terminal gives way to
/// the first agent.
pub(super) fn settle_choice(choices: &[CliPreset], index: usize, prompt: bool) -> usize {
    if index < choices.len() && !choice_open(choices, index, prompt) {
        0
    } else {
        index
    }
}

impl PickerDialog {
    /// The prompt field's text, untrimmed. Empty without a field.
    pub(super) fn prompt_text(&self, cx: &App) -> String {
        self.prompt
            .as_ref()
            .map(|prompt| prompt.read(cx).value().to_owned())
            .unwrap_or_default()
    }

    /// Changing location never starts a session. Terminal is in both lists,
    /// so the selected row stays. Lead has no switch.
    pub(super) fn toggle_worktree(&mut self) -> bool {
        self.mode = match self.mode {
            PickerMode::Task => PickerMode::Checkout,
            PickerMode::Checkout => PickerMode::Task,
            PickerMode::Lead => return false,
        };
        true
    }
}

/// Approval for a project's worktree setup before New starts `preset`.
/// `retry` is the failed card's agent state when this is a Retry. `prompt`
/// is the picker's, passed to the CLI once setup is approved.
pub(crate) struct PreparationDialog {
    pub(crate) project: String,
    pub(crate) preset: CliPreset,
    pub(crate) config: PreparationConfig,
    pub(crate) retry: Option<Arc<Mutex<HostState>>>,
    pub(crate) prompt: Option<String>,
}

impl Shika {
    /// A plain key while the picker is open. Number keys and arrows only
    /// select; Enter starts.
    pub(crate) fn picker_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if key == "space" {
            self.toggle_picker_worktree(cx);
            return;
        }
        let (choices, prompt) = match &self.overlay {
            Some(Overlay::Picker(dialog)) => (
                self.new_choices(dialog.mode),
                has_prompt(&dialog.prompt_text(cx)),
            ),
            _ => return,
        };
        let catalog_ready = self.catalog.is_some();
        // A project the picker moved to: fetch the base, or read checkout HEAD.
        let mut moved_to = None;
        if let Some(Overlay::Picker(PickerDialog {
            index,
            project,
            mode,
            ..
        })) = &mut self.overlay
        {
            let chosen = key
                .parse::<usize>()
                .ok()
                .and_then(|n| n.checked_sub(1))
                .filter(|&i| choice_open(&choices, i, prompt));
            match key {
                "j" | "down" => *index = step_choice(&choices, *index, true, prompt),
                "k" | "up" => *index = step_choice(&choices, *index, false, prompt),
                "enter" => {
                    if catalog_ready && choices.is_empty() {
                        self.show_settings(SettingsSection::Agents, window, cx);
                    } else {
                        self.launch(window, cx);
                    }
                }
                "escape" => self.cancel_overlay(window, cx),
                "tab" => {
                    if let Some(at) = self.projects.iter().position(|p| &p.id == project) {
                        *project = self.projects[(at + 1) % self.projects.len()].id.clone();
                        moved_to = Some((project.clone(), *mode));
                    }
                }
                _ => {
                    if let Some(i) = chosen {
                        *index = i;
                    }
                }
            }
        }
        cx.notify();
        if let Some((project, mode)) = moved_to {
            if mode == PickerMode::Checkout {
                self.refresh_checkout_head(project, cx);
            } else {
                self.prefetch_base(project, cx);
            }
        }
    }

    /// A keystroke while the picker's prompt field has focus. False when the
    /// field is absent or unfocused, so the picker takes plain keys as
    /// before. Letters, `j` and `k` included, type; Shift+Enter adds a line.
    /// Enter starts, Escape cancels, Tab changes project, and Up/Down choose
    /// the row. While the field is empty, number keys still choose a row and
    /// Space still toggles Create worktree. IME composition keeps every key.
    pub(crate) fn picker_prompt_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(Overlay::Picker(PickerDialog {
            prompt: Some(prompt),
            ..
        })) = &self.overlay
        else {
            return false;
        };
        if !prompt.focus_handle(cx).is_focused(window) {
            return false;
        }
        let (composing, empty) = {
            let input = prompt.read(cx);
            (input.composing(), input.value().is_empty())
        };
        if composing {
            return true;
        }
        if self.busy {
            cx.stop_propagation();
            return true;
        }
        let stroke = &event.keystroke;
        let m = stroke.modifiers;
        let plain = !m.platform && !m.control && !m.alt && !m.shift;
        let key = stroke.key.as_str();
        if plain && picker_takes_from_prompt(key, empty) {
            self.picker_key(key, window, cx);
        } else if key != "tab" {
            return true;
        }
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// The prompt changed: a refusal no longer describes it, and a written
    /// prompt moves the selection off Terminal.
    fn picker_prompt_changed(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::Picker(dialog)) = &self.overlay else {
            return;
        };
        let prompt = has_prompt(&dialog.prompt_text(cx));
        let choices = self.new_choices(dialog.mode);
        if let Some(Overlay::Picker(dialog)) = &mut self.overlay {
            dialog.index = settle_choice(&choices, dialog.index, prompt);
            dialog.error = None;
        }
        cx.notify();
    }

    /// A click on a row selects it and keeps typing in the prompt; a double
    /// click starts it.
    fn picker_row_click(
        &mut self,
        i: usize,
        event: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::Picker(dialog)) = &mut self.overlay else {
            return;
        };
        dialog.index = i;
        if let Some(prompt) = &dialog.prompt {
            window.focus(&prompt.focus_handle(cx), cx);
        }
        if event.click_count() >= 2 {
            self.launch(window, cx);
        }
        cx.notify();
    }

    /// Reuse the existing launch routes, including core's one-checkout guard.
    fn toggle_picker_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(Overlay::Picker(dialog)) = &mut self.overlay else {
            return;
        };
        if !dialog.toggle_worktree() {
            return;
        }
        let (project, mode) = (dialog.project.clone(), dialog.mode);
        if mode == PickerMode::Checkout {
            self.refresh_checkout_head(project, cx);
        } else {
            self.prefetch_base(project, cx);
        }
        cx.notify();
    }

    /// A plain key while the setup approval dialog is open.
    pub(crate) fn preparation_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match key {
            "enter" => self.approve_preparation(window, cx),
            "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }

    pub(crate) fn picker_view(
        &self,
        picker: &PickerDialog,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        max_h: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let PickerDialog {
            project,
            index,
            mode,
            prompt,
            error,
        } = picker;
        let name = self
            .projects
            .iter()
            .find(|p| &p.id == project)
            .map(|p| p.name.as_str())
            .unwrap_or("");
        let mut panel = dialog_shell(420., max_h, chrome)
            .p(px(8.))
            .gap(px(2.))
            .child(picker_header(*mode, name, chrome));
        let offered = self.new_choices(*mode);
        let offer_settings = self.catalog.is_some() && offered.is_empty();
        let written = has_prompt(&picker.prompt_text(cx));
        if let Some(prompt) = prompt {
            let terminal = offered.get(*index).is_some_and(|p| p.id == "terminal");
            let hint = match (terminal, offered.len() > 1) {
                (false, _) => PROMPT_HINT,
                (true, true) => TERMINAL_PROMPT_HINT,
                (true, false) => TERMINAL_ONLY_HINT,
            };
            panel = panel.child(picker_prompt_field(
                prompt, terminal, hint, chrome, window, cx,
            ));
            if let Some(error) = error {
                panel = panel.child(
                    div().px(px(10.)).pb(px(6.)).child(
                        dialog_text(chrome)
                            .text_color(chrome.ink_1)
                            .child(error.clone()),
                    ),
                );
            }
        }
        if self.catalog.is_none() {
            panel = panel.child(
                list_row(false, chrome)
                    .text_color(chrome.ink_3)
                    .child("Resolving login-shell PATH..."),
            );
        } else if offered.is_empty() {
            panel = picker_no_agents(panel, chrome, cx);
        } else {
            for (i, preset) in offered.iter().enumerate() {
                if preset.id == "terminal" && i > 0 {
                    panel = panel.child(
                        div()
                            .mt(px(6.))
                            .mb(px(6.))
                            .border_t_1()
                            .border_color(chrome.line_2),
                    );
                }
                let row = PresetRow {
                    i,
                    selected: i == *index,
                    open: choice_open(&offered, i, written),
                };
                panel = panel.child(picker_preset_row(row, preset, home, chrome, cx));
            }
        }
        if *mode == PickerMode::Checkout {
            panel = panel.child(
                div()
                    .px(px(10.))
                    .pt(px(6.))
                    .text_size(px(11.))
                    .text_color(chrome.ink_3)
                    .child("Runs in your project folder on ")
                    .child(
                        div().flex_1().min_w_0().truncate().font_family(MONO).child(
                            self.checkout_branch(project)
                                .unwrap_or("Reading branch...")
                                .to_owned(),
                        ),
                    )
                    .flex()
                    .flex_wrap()
                    .items_center(),
            );
        }
        let start = if offer_settings {
            "Open Settings".to_owned()
        } else {
            offered
                .get(*index)
                .map_or_else(|| "Start".to_owned(), |p| format!("Start {}", p.name))
        };
        panel.child(self.picker_footer(project, *mode, start, chrome, cx))
    }

    /// The picker's footer: where the start lands on the left, then Cancel
    /// and the start button naming the selected row.
    fn picker_footer(
        &self,
        project: &str,
        mode: PickerMode,
        start: String,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let place = if mode == PickerMode::Lead {
            let fact = match self.bases.get(project).and_then(|b| b.name.clone()) {
                Some(name) => div()
                    .flex()
                    .gap(px(4.))
                    .child("Reads a checkout of")
                    .child(div().font_family(MONO).child(name)),
                None => div().child("Reads a detached worktree"),
            };
            fact.min_w_0().truncate().into_any_element()
        } else {
            self.picker_worktree_switch(project, mode, chrome, cx)
                .into_any_element()
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .mt(px(6.))
            .pt(px(12.))
            .px(px(10.))
            .pb(px(4.))
            .border_t_1()
            .border_color(chrome.line_2)
            .text_size(px(12.))
            .text_color(chrome.ink_3)
            .child(place)
            .child(div().flex_1())
            .child(self.cancel_button("Cancel", chrome, cx))
            .child(
                primary_button("picker-start", start, "↵", chrome).on_click(
                    cx.listener(|this, _, window, cx| this.picker_key("enter", window, cx)),
                ),
            )
    }

    /// Quiet, neutral switch. The label and track share one click target;
    /// focus stays on the picker so navigation and Escape keep working.
    fn picker_worktree_switch(
        &self,
        project: &str,
        mode: PickerMode,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let on = mode == PickerMode::Task;
        let tip = if on {
            match self.bases.get(project).and_then(|b| b.name.as_deref()) {
                Some(base) => format!("Creates an isolated worktree from {base}. Space to toggle."),
                None => "Creates an isolated worktree. Space to toggle.".to_owned(),
            }
        } else {
            "Runs in your project folder, no worktree. Space to toggle.".to_owned()
        };
        let (tip_bg, tip_fg) = (chrome.toast_bg, chrome.toast_fg);
        let ink = chrome.ink_1;
        div()
            .id("picker-create-worktree")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .cursor_pointer()
            .hover(move |style| style.text_color(ink))
            .child("Create worktree")
            .child(switch_track(on, chrome))
            .tooltip(move |_, cx| {
                cx.new(|_| KeyTip {
                    bg: tip_bg,
                    fg: tip_fg,
                    text: tip.clone().into(),
                })
                .into()
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle_picker_worktree(cx)))
    }

    pub(crate) fn preparation_view(
        &self,
        preparation: &PreparationDialog,
        chrome: &Chrome,
        max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let PreparationDialog {
            project, config, ..
        } = preparation;
        let name = self
            .projects
            .iter()
            .find(|p| &p.id == project)
            .map(|p| p.name.as_str())
            .unwrap_or("");
        let mut panel = dialog(chrome, max_h)
            .child(dialog_title(div()).child(format!("Prepare worktrees for {name}?")))
            .child(dialog_text(chrome).child("These commands run with your permissions before each agent starts. Approve only a repository you trust, including its scripts."))
            .child(dialog_text(chrome).font_family(MONO).child(".shika/worktrees.json"));
        for path in &config.copy_files {
            panel = panel.child(
                dialog_text(chrome)
                    .font_family(MONO)
                    .child(format!("Copy {path}")),
            );
        }
        if config.clone_dependencies {
            panel = panel.child(dialog_text(chrome).child(crate::worktree_setup::CLONE_FACT));
        }
        for command in &config.commands {
            panel = panel.child(dialog_text(chrome).font_family(MONO).child(command.clone()));
        }
        panel
            .child(dialog_text(chrome).child(format!(
                "Setup timeout: {} seconds. Configuration changes ask again.",
                config.timeout_seconds
            )))
            .child(
                dialog_buttons()
                    .child(self.cancel_button("Cancel", chrome, cx))
                    .child(
                        primary_button("approve-setup", "Approve and start", "↵", chrome).on_click(
                            cx.listener(|this, _, window, cx| this.approve_preparation(window, cx)),
                        ),
                    ),
            )
    }
}

/// The plain keys the picker keeps while its prompt field has focus.
pub(super) fn picker_takes_from_prompt(key: &str, empty: bool) -> bool {
    match key {
        "enter" | "escape" | "tab" | "up" | "down" => true,
        "space" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => empty,
        _ => false,
    }
}

/// The optional prompt, under the title: two lines tall at rest, growing as
/// it fills, with the new-line key in its corner. It dims while Terminal is
/// selected; typing there moves to the first agent.
fn picker_prompt_field(
    prompt: &Entity<chat_input::ChatInput>,
    terminal: bool,
    hint: &'static str,
    chrome: &Chrome,
    window: &Window,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    let mut ink = *chrome;
    if terminal {
        ink.ink_1 = chrome.ink_3;
        ink.ink_3 = chrome.ink_4;
    }
    let focus = prompt.focus_handle(cx);
    let focused = focus.is_focused(window);
    prompt.update(cx, |input, _| {
        input.set_chrome(ink);
        input.placeholder(hint);
    });
    div()
        .id("picker-prompt")
        .mx(px(2.))
        .mb(px(8.))
        .px(px(10.))
        .pt(px(8.))
        .pb(px(6.))
        .rounded(px(8.))
        .bg(chrome.raised)
        .border_1()
        .border_color(if focused {
            chrome.focus
        } else {
            chrome.line_control
        })
        .cursor_text()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().min_h(px(40.)).child(prompt.clone()))
        .child(
            div()
                .flex()
                .justify_end()
                .child(hint_quiet("⇧↵", "new line", chrome)),
        )
        // The padding and the corner focus the field too.
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.focus(&focus, cx);
        })
}

/// A footer-style key hint, one step quieter, for inside a field.
fn hint_quiet(key: &str, label: &str, chrome: &Chrome) -> gpui::Div {
    hint(key, label, chrome).text_color(chrome.ink_4)
}

/// The picker's title row: the mode, the project, and the Tab hint.
fn picker_header(mode: PickerMode, name: &str, chrome: &Chrome) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .pt(px(8.))
        .px(px(10.))
        .pb(px(10.))
        .child(dialog_title(div()).child(mode.title()))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(px(13.))
                .text_color(chrome.ink_3)
                .child(format!("in {name}")),
        )
        .child(div().flex_1())
        .child(hint("tab", "project", chrome))
}

/// No agent is on: say so and offer Settings.
fn picker_no_agents(
    panel: gpui::Stateful<gpui::Div>,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    panel
        .child(
            div()
                .px(px(10.))
                .pt(px(4.))
                .pb(px(2.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(dialog_text(chrome).child("No agents available"))
                .child(
                    dialog_text(chrome)
                        .text_color(chrome.ink_3)
                        .child("Turn one on in Settings."),
                ),
        )
        .child(
            list_row(true, chrome)
                .id("picker-open-settings")
                .cursor_pointer()
                .child("Open Settings")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.show_settings(SettingsSection::Agents, window, cx);
                })),
        )
}

/// How one picker row draws: its position, whether it is selected, and
/// whether it can be chosen (Terminal is closed while the prompt has text).
struct PresetRow {
    i: usize,
    selected: bool,
    open: bool,
}

/// One choice: its number key, name, and binary path. A click selects it, a
/// double click starts it; a closed row takes no click.
fn picker_preset_row(
    row: PresetRow,
    preset: &CliPreset,
    home: Option<&std::path::Path>,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    let PresetRow { i, selected, open } = row;
    let detail = match preset.path.as_deref() {
        Some(path) => model::tilde(path, home),
        None if !open => TERMINAL_CLOSED.into(),
        None if preset.id == "terminal" => "Login shell".into(),
        None => String::new(),
    };
    let hover = chrome.row_selected;
    let cap = if open { chrome.ink_3 } else { chrome.ink_4 };
    list_row(selected, chrome)
        .id(SharedString::from(format!("preset-{i}")))
        .gap(px(12.))
        .when(open, |row| {
            row.cursor_pointer()
                .when(!selected, |row| {
                    row.hover(move |style| {
                        style.bg(Rgba {
                            a: hover.a * 0.5,
                            ..hover
                        })
                    })
                })
                .on_click(cx.listener(move |this, event, window, cx| {
                    this.picker_row_click(i, event, window, cx)
                }))
        })
        .child(kbd((i + 1).to_string(), chrome.sunken, cap))
        .child(
            div()
                .flex_none()
                .text_size(px(13.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if open { chrome.ink_1 } else { chrome.ink_4 })
                .child(preset.name.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_right()
                .when(open, |d| d.font_family(MONO).text_size(px(11.)))
                .when(!open, |d| d.text_size(px(12.)))
                .text_color(if open { chrome.ink_3 } else { chrome.ink_4 })
                .child(detail),
        )
}
