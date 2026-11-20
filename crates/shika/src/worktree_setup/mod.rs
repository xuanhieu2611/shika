//! Settings editor and optional first-New onboarding for project preparation.
//! Reuses the native single-line input; no watcher, runner, or secret preview.
use super::*;

mod view;

/// The approval and first-New fact for `clone-dependencies`.
pub(crate) const CLONE_FACT: &str =
    "Clone node_modules from the main checkout, copy-on-write, when package files match.";

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
    clone_dependencies: bool,
    clone_toggle: FocusHandle,
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
    row += 2; // Dependencies heading and help
    rows.push(row); // Clone node_modules switch
    row += 2; // and the timeout label
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
/// Compact preview: four facts, optional file heading and values, the clone
/// fact, optional command heading and values, timeout, optional error,
/// Customize, then the three footer controls.
fn compact_focus_rows(files: usize, clone: bool, commands: usize, error: bool) -> Vec<usize> {
    let mut row = 4;
    if files + commands == 0 && !clone {
        row += 1;
    }
    if files > 0 {
        row += 1 + files;
    }
    row += usize::from(clone);
    if commands > 0 {
        row += 1 + commands;
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
            clone_dependencies: draft.config.clone_dependencies,
            clone_toggle: cx.focus_handle(),
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
        !self.files.is_empty() || !self.commands.is_empty() || self.clone_dependencies
    }
    /// `npm ci` deletes node_modules before installing, so it throws the
    /// clone away. Only warned about, since the command is the author's.
    fn clone_discarded(&self, cx: &App) -> bool {
        self.clone_dependencies
            && self.commands.iter().any(|entry| {
                let words: Vec<&str> = entry.input.read(cx).value().split_whitespace().collect();
                words.windows(2).any(|pair| pair == ["npm", "ci"])
            })
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
            compact_focus_rows(
                self.files.len(),
                self.clone_dependencies,
                self.commands.len(),
                self.error.is_some(),
            )
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
            compact_focus_rows(
                self.files.len(),
                self.clone_dependencies,
                self.commands.len(),
                false,
            )[0]
        } else {
            15 + self.files.len() + self.commands.len()
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
        handles.extend([
            self.add_command.clone(),
            self.clone_toggle.clone(),
            self.timeout.focus_handle(cx),
        ]);
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
            clone_dependencies: self.clone_dependencies,
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
                &self.overlay,
                Some(Overlay::Settings(dialog)) if dialog.section() == SettingsSection::Projects
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
            Some(Overlay::Picker(picker)) => picker.index,
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
                self.overlay = Some(Overlay::Picker(crate::PickerDialog {
                    project: editor.project.clone(),
                    index: *picker_index,
                    mode: crate::PickerMode::Task,
                }));
            }
            EntryPoint::Settings(settings_row) => {
                let row = self
                    .projects
                    .iter()
                    .position(|p| p.id == editor.project)
                    .unwrap_or((*settings_row).min(self.projects.len().saturating_sub(1)));
                self.overlay = Some(Overlay::Settings(SettingsDialog::new(
                    SettingsSection::Projects,
                    row,
                )));
                self.settings_ui.scroll_to(row);
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
    fn toggle_clone_dependencies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Overlay::WorktreeSetup(editor)) = &mut self.overlay else {
            return;
        };
        editor.clone_dependencies = !editor.clone_dependencies;
        editor.error = None;
        window.focus(&editor.clone_toggle, cx);
        cx.notify();
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
                } else if editor.clone_toggle.is_focused(window) {
                    self.toggle_clone_dependencies(window, cx);
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
            "space" if editor.clone_toggle.is_focused(window) => {
                self.toggle_clone_dependencies(window, cx)
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_focus_maps_to_dialog_rows_in_empty_and_configured_editors() {
        assert_eq!(
            focus_rows(0, 0, false, false, false),
            vec![6, 9, 12, 14, 15, 15]
        );
        assert_eq!(
            focus_rows(1, 1, true, false, false),
            vec![6, 6, 7, 10, 10, 11, 14, 16, 17, 18, 18]
        );
        assert_eq!(
            focus_rows(1, 1, true, true, false),
            vec![6, 6, 7, 10, 10, 11, 14, 16, 18, 19, 19]
        );
    }

    #[test]
    fn compact_onboarding_focus_matches_empty_and_suggested_summary_rows() {
        assert_eq!(compact_focus_rows(0, false, 0, false), [6, 7, 7, 7]);
        assert_eq!(compact_focus_rows(0, false, 0, true), [7, 8, 8, 8]);
        assert_eq!(compact_focus_rows(2, false, 1, false), [10, 11, 11, 11]);
        assert_eq!(compact_focus_rows(2, false, 0, false), [8, 9, 9, 9]);
        assert_eq!(compact_focus_rows(0, false, 1, false), [7, 8, 8, 8]);
        // The clone fact replaces "No files or commands selected." when alone.
        assert_eq!(compact_focus_rows(0, true, 0, false), [6, 7, 7, 7]);
        assert_eq!(compact_focus_rows(2, true, 1, false), [11, 12, 12, 12]);
    }
    #[test]
    fn customized_onboarding_adds_skip_in_the_existing_footer() {
        for (files, commands, error) in [(0, 0, false), (2, 1, true), (128, 64, false)] {
            let rows = focus_rows(files, commands, false, error, true);
            assert_eq!(rows.len(), 2 * (files + commands) + 7);
            assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]));
            assert_eq!(&rows[rows.len() - 3..], &[rows[rows.len() - 1]; 3]);
        }
    }
    #[test]
    fn long_setups_keep_focus_targets_ordered_and_repair_after_row_removal() {
        let rows = focus_rows(128, 64, true, true, false);
        assert_eq!(rows.len(), 2 * (128 + 64) + 7);
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
