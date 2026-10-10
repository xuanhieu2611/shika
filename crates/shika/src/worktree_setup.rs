//! Settings editor and optional first-New onboarding for project preparation.
//! Reuses the native single-line input; no watcher, runner, or secret preview.
use super::*;

pub(super) struct Entry {
    input: Entity<name_input::NameInput>,
    remove: FocusHandle,
}
#[derive(Clone)]
enum EntryPoint {
    Settings(usize),
    New {
        preset: CliPreset,
        picker_index: usize,
    },
}
pub(super) struct Editor {
    project: String,
    entry_point: EntryPoint,
    customized: bool,
    customize: FocusHandle,
    skip: FocusHandle,
    note: String,
    expected: Option<PreparationConfig>,
    files: Vec<Entry>,
    commands: Vec<Entry>,
    timeout: Entity<name_input::NameInput>,
    add_file: FocusHandle,
    add_command: FocusHandle,
    disable: FocusHandle,
    cancel: FocusHandle,
    save: FocusHandle,
    error: Option<String>,
    scroll: gpui::ScrollHandle,
}

/// Direct children of the dialog, in tab order. Input and Remove share a row;
/// Cancel and Save share the footer. Keep keyboard focus visible in long setups.
fn focus_rows(
    files: usize,
    commands: usize,
    configured: bool,
    error: bool,
    onboarding: bool,
) -> Vec<usize> {
    let mut row = 4; // title, fact, path, saved/suggested status
    let mut rows = Vec::new();
    for count in [files, commands] {
        row += 2; // heading and help
        for _ in 0..count {
            rows.extend([row, row]);
            row += 1;
        }
        rows.push(row); // Add
        row += 1;
    }
    row += 1; // timeout label
    rows.push(row);
    row += 1 + usize::from(error);
    if configured {
        rows.push(row); // Disable
        row += 1;
    }
    rows.extend([row, row]); // footer
    if onboarding {
        rows.push(row);
    } // Start without setup
    rows
}
/// Compact preview: four facts, optional file/command headings and values,
/// timeout, optional error, Customize, then the three footer controls.
fn compact_focus_rows(files: usize, commands: usize, error: bool) -> Vec<usize> {
    let mut row = 4;
    if files + commands == 0 {
        row += 1;
    }
    for count in [files, commands] {
        if count > 0 {
            row += 1 + count;
        }
    }
    row += 1 + usize::from(error);
    vec![row, row + 1, row + 1, row + 1]
}

impl Editor {
    fn new(
        project: String,
        entry_point: EntryPoint,
        draft: shika_core::PreparationDraft,
        chrome: Chrome,
        cx: &mut Context<Shika>,
    ) -> Self {
        let customized = matches!(entry_point, EntryPoint::Settings(_));
        Self {
            project,
            entry_point,
            customized,
            customize: cx.focus_handle(),
            skip: cx.focus_handle(),
            note: draft.note,
            expected: draft.existing,
            files: draft
                .config
                .copy_files
                .into_iter()
                .map(|v| entry(v, chrome, cx))
                .collect(),
            commands: draft
                .config
                .commands
                .into_iter()
                .map(|v| entry(v, chrome, cx))
                .collect(),
            timeout: cx.new(|cx| {
                name_input::NameInput::new(draft.config.timeout_seconds.to_string(), chrome, cx)
            }),
            add_file: cx.focus_handle(),
            add_command: cx.focus_handle(),
            disable: cx.focus_handle(),
            cancel: cx.focus_handle(),
            save: cx.focus_handle(),
            error: None,
            scroll: gpui::ScrollHandle::new(),
        }
    }
    fn onboarding(&self) -> bool {
        matches!(self.entry_point, EntryPoint::New { .. })
    }
    fn compact(&self) -> bool {
        self.onboarding() && !self.customized
    }
    fn has_setup(&self) -> bool {
        !self.files.is_empty() || !self.commands.is_empty()
    }
    fn initial_focus(&self, cx: &App) -> FocusHandle {
        if self.onboarding() {
            if self.has_setup() {
                self.save.clone()
            } else {
                self.skip.clone()
            }
        } else {
            self.focuses(cx)[0].clone()
        }
    }
    fn rows(&self) -> Vec<usize> {
        if self.compact() {
            compact_focus_rows(self.files.len(), self.commands.len(), self.error.is_some())
        } else {
            focus_rows(
                self.files.len(),
                self.commands.len(),
                self.expected.is_some(),
                self.error.is_some(),
                self.onboarding(),
            )
        }
    }
    fn show_error(&mut self, error: String) {
        self.error = Some(error);
        let row = if self.compact() {
            compact_focus_rows(self.files.len(), self.commands.len(), false)[0]
        } else {
            12 + self.files.len() + self.commands.len()
        };
        self.scroll.scroll_to_item(row);
    }
    fn reveal_focus(&self, window: &Window, cx: &App) {
        if let Some(at) = self.focuses(cx).iter().position(|f| f.is_focused(window)) {
            let rows = self.rows();
            self.scroll.scroll_to_item(rows[at]);
        }
    }
    fn inputs(&self) -> impl Iterator<Item = &Entity<name_input::NameInput>> {
        self.files
            .iter()
            .chain(&self.commands)
            .map(|e| &e.input)
            .chain(std::iter::once(&self.timeout))
    }
    fn composing(&self, cx: &App) -> bool {
        self.inputs().any(|input| input.read(cx).composing())
    }
    fn focuses(&self, cx: &App) -> Vec<FocusHandle> {
        if self.compact() {
            return vec![
                self.customize.clone(),
                self.cancel.clone(),
                self.skip.clone(),
                self.save.clone(),
            ];
        }
        let mut handles = Vec::new();
        for entry in &self.files {
            handles.extend([entry.input.focus_handle(cx), entry.remove.clone()]);
        }
        handles.push(self.add_file.clone());
        for entry in &self.commands {
            handles.extend([entry.input.focus_handle(cx), entry.remove.clone()]);
        }
        handles.extend([self.add_command.clone(), self.timeout.focus_handle(cx)]);
        if self.expected.is_some() {
            handles.push(self.disable.clone());
        }
        handles.push(self.cancel.clone());
        if self.onboarding() {
            handles.push(self.skip.clone());
        }
        handles.push(self.save.clone());
        handles
    }
    fn config(&self, cx: &App) -> Result<PreparationConfig, String> {
        let timeout_seconds = self
            .timeout
            .read(cx)
            .value()
            .parse::<u64>()
            .map_err(|_| "Timeout must be a whole number between 1 and 3600.".to_string())?;
        Ok(PreparationConfig {
            copy_files: self
                .files
                .iter()
                .map(|e| e.input.read(cx).value().to_string())
                .collect(),
            commands: self
                .commands
                .iter()
                .map(|e| e.input.read(cx).value().to_string())
                .collect(),
            timeout_seconds,
        })
    }
}
fn entry(value: String, chrome: Chrome, cx: &mut Context<Shika>) -> Entry {
    Entry {
        input: cx.new(|cx| name_input::NameInput::new(value, chrome, cx)),
        remove: cx.focus_handle(),
    }
}
impl Shika {
    pub(super) fn open_worktree_setup(
        &mut self,
        project: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy
            || !matches!(
                self.overlay,
                Some(Overlay::Settings {
                    section: SettingsSection::Projects,
                    ..
                })
            )
        {
            return;
        }
        let Some(settings_row) = self.projects.iter().position(|p| p.id == project) else {
            return;
        };
        self.select_setting(settings_row, window, cx);
        self.busy = true;
        let core = self.core.clone();
        cx.spawn_in(window, async move |this, cx| {
            let lookup = project.clone();
            let result = cx
                .background_executor()
                .spawn(async move { core.project_preparation_draft(&lookup) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(draft) => {
                        let editor = Editor::new(
                            project,
                            EntryPoint::Settings(settings_row),
                            draft,
                            this.chrome(window),
                            cx,
                        );
                        window.focus(&editor.focuses(cx)[0], cx);
                        editor.reveal_focus(window, cx);
                        this.overlay = Some(Overlay::WorktreeSetup(Box::new(editor)));
                    }
                    Err(e) => {
                        this.message(e.to_string());
                        window.focus(&this.focus, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn show_preparation_onboarding(
        &mut self,
        project: String,
        preset: CliPreset,
        draft: shika_core::PreparationDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let picker_index = match &self.overlay {
            Some(Overlay::Picker { index, .. }) => *index,
            _ => 0,
        };
        let editor = Editor::new(
            project,
            EntryPoint::New {
                preset,
                picker_index,
            },
            draft,
            self.chrome(window),
            cx,
        );
        // Keep the summary at the top for review; Tab reveals focused controls.
        window.focus(&editor.initial_focus(cx), cx);
        self.overlay = Some(Overlay::WorktreeSetup(Box::new(editor)));
        cx.notify();
    }
    fn customize_preparation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::WorktreeSetup(editor)) = &mut self.overlay else {
            return;
        };
        editor.customized = true;
        window.focus(&editor.focuses(cx)[0], cx);
        editor.reveal_focus(window, cx);
        cx.notify();
    }
    fn skip_preparation_onboarding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::WorktreeSetup(editor)) = &self.overlay else {
            return;
        };
        let EntryPoint::New { preset, .. } = &editor.entry_point else {
            return;
        };
        if editor.composing(cx) {
            return;
        }
        let (project, preset) = (editor.project.clone(), preset.clone());
        let core = self.core.clone();
        let lookup = project.clone();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.skip_preparation_onboarding(&lookup) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => this.request_launch(project, preset, None, window, cx),
                    Err(e) => {
                        if let Some(Overlay::WorktreeSetup(editor)) = &mut this.overlay {
                            editor.show_error(e.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    /// Return to the opening Settings row or New picker without consuming the
    /// original terminal/card focus return or recording a cancelled decision.
    pub(super) fn finish_worktree_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::WorktreeSetup(editor)) = &self.overlay else {
            return;
        };
        match &editor.entry_point {
            EntryPoint::New { picker_index, .. } => {
                self.overlay = Some(Overlay::Picker {
                    project: editor.project.clone(),
                    index: *picker_index,
                    lead: false,
                });
            }
            EntryPoint::Settings(settings_row) => {
                let row = self
                    .projects
                    .iter()
                    .position(|p| p.id == editor.project)
                    .unwrap_or((*settings_row).min(self.projects.len().saturating_sub(1)));
                self.overlay = Some(Overlay::Settings {
                    section: SettingsSection::Projects,
                    row,
                    edit: None,
                });
                self.settings_scroll.scroll_to_item(row);
            }
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }
    pub(super) fn on_projects_key(
        &mut self,
        row: usize,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.projects.len();
        match key {
            "j" | "down" | "tab" if count > 0 => self.select_setting((row + 1) % count, window, cx),
            "k" | "up" if count > 0 => self.select_setting((row + count - 1) % count, window, cx),
            "enter" => {
                if let Some(project) = self.projects.get(row) {
                    self.open_worktree_setup(project.id.clone(), window, cx);
                }
            }
            "escape" => self.cancel_overlay(window, cx),
            _ => {}
        }
    }
    fn add_setup_entry(&mut self, command: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let chrome = self.chrome(window);
        let Some(Overlay::WorktreeSetup(editor)) = &mut self.overlay else {
            return;
        };
        let list = if command {
            &mut editor.commands
        } else {
            &mut editor.files
        };
        if list.len() >= if command { 64 } else { 128 } {
            return;
        }
        let added = entry(String::new(), chrome, cx);
        window.focus(&added.input.focus_handle(cx), cx);
        list.push(added);
        editor.error = None;
        editor.reveal_focus(window, cx);
        cx.notify();
    }
    fn remove_setup_entry(
        &mut self,
        command: bool,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let Some(Overlay::WorktreeSetup(editor)) = &mut self.overlay else {
            return;
        };
        let list = if command {
            &mut editor.commands
        } else {
            &mut editor.files
        };
        if index >= list.len() {
            return;
        }
        list.remove(index);
        let focus = list
            .get(index)
            .or_else(|| list.last())
            .map(|e| e.input.focus_handle(cx))
            .unwrap_or_else(|| {
                if command {
                    editor.add_command.clone()
                } else {
                    editor.add_file.clone()
                }
            });
        window.focus(&focus, cx);
        editor.error = None;
        editor.reveal_focus(window, cx);
        cx.notify();
    }
    fn save_worktree_setup(&mut self, disable: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::WorktreeSetup(editor)) = &mut self.overlay else {
            return;
        };
        if editor.composing(cx) {
            return;
        }
        let config = if disable {
            None
        } else {
            match editor.config(cx) {
                Ok(config) => Some(config),
                Err(error) => {
                    editor.show_error(error);
                    cx.notify();
                    return;
                }
            }
        };
        let expected = editor.expected.clone();
        let project = editor.project.clone();
        let entry_point = editor.entry_point.clone();
        let core = self.core.clone();
        let lookup = project.clone();
        let approve = editor.onboarding();
        self.busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if approve {
                        let config = config.as_ref().ok_or_else(|| {
                            shika_core::Error::Preparation(
                                "Onboarding cannot disable a saved configuration.".into(),
                            )
                        })?;
                        core.save_and_approve_project_preparation(
                            &lookup,
                            expected.as_ref(),
                            config,
                        )
                    } else {
                        core.save_project_preparation(&lookup, expected.as_ref(), config.as_ref())
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(()) => match entry_point {
                        EntryPoint::New { preset, .. } => this.begin_launch(
                            project,
                            preset,
                            true,
                            None,
                            Launch::Task,
                            None,
                            window,
                            cx,
                        ),
                        EntryPoint::Settings(_) => {
                            this.finish_worktree_setup(window, cx);
                            this.message(if disable {
                                "Worktree setup disabled. Applies to new agents.".into()
                            } else {
                                "Worktree setup saved. Applies to new agents.".into()
                            });
                        }
                    },
                    Err(e) => {
                        if let Some(Overlay::WorktreeSetup(editor)) = &mut this.overlay {
                            editor.show_error(e.to_string());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(super) fn worktree_setup_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Overlay::WorktreeSetup(editor)) = &self.overlay else {
            return;
        };
        if self.busy {
            cx.stop_propagation();
            return;
        }
        if editor.composing(cx) {
            return;
        }
        let stroke = &event.keystroke;
        if stroke.key == "enter" && event.is_held {
            cx.stop_propagation();
            return;
        }
        if stroke.modifiers.platform || stroke.modifiers.control || stroke.modifiers.alt {
            return;
        }
        match stroke.key.as_str() {
            "escape" => self.cancel_overlay(window, cx),
            "tab" => {
                let handles = editor.focuses(cx);
                let at = handles
                    .iter()
                    .position(|f| f.is_focused(window))
                    .unwrap_or(0);
                let next =
                    (at + if stroke.modifiers.shift {
                        handles.len() - 1
                    } else {
                        1
                    }) % handles.len();
                window.focus(&handles[next], cx);
                editor.reveal_focus(window, cx);
                cx.notify();
            }
            "enter" => {
                if editor.compact() && editor.customize.is_focused(window) {
                    self.customize_preparation(window, cx);
                } else if editor.onboarding() && editor.skip.is_focused(window) {
                    self.skip_preparation_onboarding(window, cx);
                } else if editor.add_file.is_focused(window) {
                    self.add_setup_entry(false, window, cx);
                } else if editor.add_command.is_focused(window) {
                    self.add_setup_entry(true, window, cx);
                } else if editor.disable.is_focused(window) {
                    self.save_worktree_setup(true, window, cx);
                } else if editor.cancel.is_focused(window) {
                    self.cancel_overlay(window, cx);
                } else if let Some(index) = editor
                    .files
                    .iter()
                    .position(|e| e.remove.is_focused(window))
                {
                    self.remove_setup_entry(false, index, window, cx);
                } else if let Some(index) = editor
                    .commands
                    .iter()
                    .position(|e| e.remove.is_focused(window))
                {
                    self.remove_setup_entry(true, index, window, cx);
                } else {
                    self.save_worktree_setup(false, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
    pub(super) fn worktree_setup_view(
        &self,
        editor: &Editor,
        chrome: &Chrome,
        max_h: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let project = self.projects.iter().find(|p| p.id == editor.project);
        let name = project.map(|p| p.name.as_str()).unwrap_or("");
        let config_path = project
            .map(|p| model::tilde(&p.path.join(".shika/worktrees.json"), home_dir().as_deref()))
            .unwrap_or_else(|| ".shika/worktrees.json".into());
        for input in editor.inputs() {
            input.update(cx, |input, _| input.set_chrome(*chrome));
        }
        let mut panel = dialog(chrome, max_h)
            .track_scroll(&editor.scroll)
            .child(dialog_title(div()).child(if editor.onboarding() { format!("Set up worktrees for {name}?") } else { format!("Worktree setup for {name}") }))
            .child(dialog_text(chrome).child(if editor.onboarding() {
                "Setup is optional. Commands run with your permissions before each agent starts. Approve only a repository you trust, including its scripts."
            } else {
                "Applies to new agents. Saving does not run setup; New asks for approval when configuration changes."
            }))
            .child(dialog_text(chrome).font_family(MONO).child(config_path));
        panel = panel.child(dialog_text(chrome).child(editor.note.clone()));
        if editor.compact() {
            if !editor.has_setup() {
                panel = panel.child(dialog_text(chrome).child("No files or commands selected."));
            }
            for (list, heading) in [
                (&editor.files, "Copy before each agent"),
                (&editor.commands, "Run before each agent"),
            ] {
                if !list.is_empty() {
                    panel = panel.child(dialog_title(div()).child(heading));
                    for entry in list {
                        panel = panel.child(
                            dialog_text(chrome)
                                .font_family(MONO)
                                .child(entry.input.read(cx).value().to_string()),
                        );
                    }
                }
            }
            panel = panel.child(dialog_text(chrome).child(format!(
                "Timeout: {} seconds",
                editor.timeout.read(cx).value()
            )));
            if let Some(error) = &editor.error {
                panel = panel.child(dialog_text(chrome).child(error.clone()));
            }
            return panel
                .child(
                    secondary_button("customize-setup", chrome)
                        .track_focus(&editor.customize)
                        .when(editor.customize.is_focused(window), |d| {
                            d.border_color(chrome.focus)
                        })
                        .px(px(12.))
                        .py(px(6.))
                        .child("Customize setup…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.customize_preparation(window, cx)
                        })),
                )
                .child(self.worktree_setup_buttons(editor, chrome, window, cx));
        }
        for (command, list, label, help, add_focus) in [
            (
                false,
                &editor.files,
                "Files to copy",
                "Literal paths from the main checkout, such as .env.local. Must be ignored on the task's base too. Contents are never shown.",
                &editor.add_file,
            ),
            (
                true,
                &editor.commands,
                "Setup commands",
                "Run in order before the agent starts. Trusted commands, not a sandbox. Use unattended commands, not a dev server.",
                &editor.add_command,
            ),
        ] {
            panel = panel
                .child(dialog_title(div()).child(label))
                .child(dialog_text(chrome).child(help));
            for (index, entry) in list.iter().enumerate() {
                panel = panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().flex_1().min_w_0().child(entry.input.clone()))
                        .child(
                            secondary_button(
                                (
                                    if command {
                                        "remove-command"
                                    } else {
                                        "remove-copy"
                                    },
                                    index,
                                ),
                                chrome,
                            )
                            .track_focus(&entry.remove)
                            .when(entry.remove.is_focused(window), |d| {
                                d.border_color(chrome.focus)
                            })
                            .px(px(8.))
                            .py(px(4.))
                            .child("Remove")
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.remove_setup_entry(command, index, window, cx)
                                },
                            )),
                        ),
                );
            }
            panel = panel.child(
                secondary_button(if command { "add-command" } else { "add-copy" }, chrome)
                    .track_focus(add_focus)
                    .when(add_focus.is_focused(window), |d| {
                        d.border_color(chrome.focus)
                    })
                    .px(px(12.))
                    .py(px(6.))
                    .child(if command {
                        "Add command"
                    } else {
                        "Add file path"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.add_setup_entry(command, window, cx)
                    })),
            );
        }
        panel = panel
            .child(dialog_text(chrome).child("Timeout in seconds (1 to 3600)"))
            .child(editor.timeout.clone());
        if let Some(error) = &editor.error {
            panel = panel.child(dialog_text(chrome).child(error.clone()));
        }
        if editor.expected.is_some() {
            panel = panel.child(
                secondary_button("disable-setup", chrome)
                    .track_focus(&editor.disable)
                    .when(editor.disable.is_focused(window), |d| {
                        d.border_color(chrome.focus)
                    })
                    .px(px(12.))
                    .py(px(6.))
                    .child("Disable setup")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.save_worktree_setup(true, window, cx)
                    })),
            );
        }
        panel.child(self.worktree_setup_buttons(editor, chrome, window, cx))
    }
    fn worktree_setup_buttons(
        &self,
        editor: &Editor,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let primary_save = !editor.onboarding() || editor.has_setup();
        let mut footer = dialog_buttons().flex_wrap().child(
            self.cancel_button("Cancel", chrome, cx)
                .track_focus(&editor.cancel)
                .when(editor.cancel.is_focused(window), |d| {
                    d.border_color(chrome.focus)
                }),
        );
        if editor.onboarding() {
            let skip = if primary_save {
                secondary_button("skip-setup", chrome)
                    .px(px(12.))
                    .py(px(6.))
                    .child("Start without setup")
                    .when(editor.skip.is_focused(window), |d| {
                        d.border_color(chrome.focus)
                    })
            } else {
                primary_button("skip-setup", "Start without setup", "↵", chrome)
                    .border_1()
                    .border_color(if editor.skip.is_focused(window) {
                        chrome.primary_fg
                    } else {
                        chrome.primary_bg
                    })
            };
            footer = footer.child(skip.track_focus(&editor.skip).on_click(
                cx.listener(|this, _, window, cx| this.skip_preparation_onboarding(window, cx)),
            ));
        }
        let label = if self.busy && editor.onboarding() {
            "Working..."
        } else if self.busy {
            "Saving..."
        } else if editor.onboarding() {
            "Save, approve and start"
        } else {
            "Save setup"
        };
        let save = if primary_save {
            primary_button("save-setup", label, "↵", chrome)
                .border_1()
                .border_color(if editor.save.is_focused(window) {
                    chrome.primary_fg
                } else {
                    chrome.primary_bg
                })
        } else {
            secondary_button("save-setup", chrome)
                .px(px(12.))
                .py(px(6.))
                .child(label)
                .when(editor.save.is_focused(window), |d| {
                    d.border_color(chrome.focus)
                })
        };
        footer.child(save.track_focus(&editor.save).on_click(
            cx.listener(|this, _, window, cx| this.save_worktree_setup(false, window, cx)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_focus_maps_to_dialog_rows_in_empty_and_configured_editors() {
        assert_eq!(
            focus_rows(0, 0, false, false, false),
            vec![6, 9, 11, 12, 12]
        );
        assert_eq!(
            focus_rows(1, 1, true, false, false),
            vec![6, 6, 7, 10, 10, 11, 13, 14, 15, 15]
        );
        assert_eq!(
            focus_rows(1, 1, true, true, false),
            vec![6, 6, 7, 10, 10, 11, 13, 15, 16, 16]
        );
    }

    #[test]
    fn compact_onboarding_focus_matches_empty_and_suggested_summary_rows() {
        assert_eq!(compact_focus_rows(0, 0, false), [6, 7, 7, 7]);
        assert_eq!(compact_focus_rows(0, 0, true), [7, 8, 8, 8]);
        assert_eq!(compact_focus_rows(2, 1, false), [10, 11, 11, 11]);
        assert_eq!(compact_focus_rows(2, 0, false), [8, 9, 9, 9]);
        assert_eq!(compact_focus_rows(0, 1, false), [7, 8, 8, 8]);
    }
    #[test]
    fn customized_onboarding_adds_skip_in_the_existing_footer() {
        for (files, commands, error) in [(0, 0, false), (2, 1, true), (128, 64, false)] {
            let rows = focus_rows(files, commands, false, error, true);
            assert_eq!(rows.len(), 2 * (files + commands) + 6);
            assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]));
            assert_eq!(&rows[rows.len() - 3..], &[rows[rows.len() - 1]; 3]);
        }
    }
    #[test]
    fn long_setups_keep_focus_targets_ordered_and_repair_after_row_removal() {
        let rows = focus_rows(128, 64, true, true, false);
        assert_eq!(rows.len(), 2 * (128 + 64) + 6);
        assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]));
        // Removing a file shifts all command/timeout/footer rows back by one.
        let before = focus_rows(2, 1, true, false, false);
        let after = focus_rows(1, 1, true, false, false);
        assert_eq!(
            &before[5..],
            &after[3..].iter().map(|row| row + 1).collect::<Vec<_>>()
        );
    }
}
