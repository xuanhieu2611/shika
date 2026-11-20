//! The setup editor's and first-New preview's rendering: the intro facts,
//! the file and command lists, the Clone node_modules switch, and the footer.
use super::*;

/// The top of the setup dialog: the title, what setup does, where the
/// config lives, and the editor's note.
fn setup_intro(
    editor: &Editor,
    name: &str,
    config_path: String,
    chrome: &Chrome,
    max_h: Pixels,
) -> gpui::Stateful<gpui::Div> {
    let panel = dialog(chrome, max_h)
        .track_scroll(&editor.scroll)
        .child(dialog_title(div()).child(if editor.onboarding() { format!("Set up worktrees for {name}?") } else { format!("Worktree setup for {name}") }))
        .child(dialog_text(chrome).child(if editor.onboarding() {
            "Setup is optional. Commands run with your permissions before each agent starts. Approve only a repository you trust, including its scripts."
        } else {
            "Applies to new agents. Saving does not run setup; New asks for approval when configuration changes."
        }))
        .child(dialog_text(chrome).font_family(MONO).child(config_path));
    panel.child(dialog_text(chrome).child(editor.note.clone()))
}
/// The full editor's two lists, Files to copy then Setup commands: a label,
/// help, one row per entry, and an Add button.
fn setup_lists(
    mut panel: gpui::Stateful<gpui::Div>,
    editor: &Editor,
    chrome: &Chrome,
    window: &Window,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
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
            panel = panel.child(setup_entry_row(command, index, entry, chrome, window, cx));
        }
        panel = panel.child(add_entry_button(command, add_focus, chrome, window, cx));
    }
    let mut help = "Before the commands, clone the main checkout's node_modules copy-on-write when every package.json and lockfile matches. Setup still runs the install.".to_string();
    if editor.clone_discarded(cx) {
        help.push_str(" npm ci deletes node_modules first and throws the clone away.");
    }
    panel
        .child(dialog_title(div()).child("Dependencies"))
        .child(dialog_text(chrome).child(help))
        .child(clone_switch(editor, chrome, window, cx))
}
/// The Clone node_modules switch: the picker's track beside a dialog label,
/// focusable like the editor's buttons.
fn clone_switch(
    editor: &Editor,
    chrome: &Chrome,
    window: &Window,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("clone-dependencies")
        .track_focus(&editor.clone_toggle)
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(8.))
        .py(px(4.))
        .rounded(px(7.))
        .border_1()
        .border_color(if editor.clone_toggle.is_focused(window) {
            chrome.focus
        } else {
            gpui::transparent_black().into()
        })
        .cursor_pointer()
        .text_size(px(13.))
        .line_height(px(19.))
        .text_color(chrome.ink_1)
        .child(switch_track(editor.clone_dependencies, chrome))
        .child("Clone node_modules")
        .on_click(cx.listener(|this, _, window, cx| this.toggle_clone_dependencies(window, cx)))
}
/// One file or command: its input and a Remove button.
fn setup_entry_row(
    command: bool,
    index: usize,
    entry: &Entry,
    chrome: &Chrome,
    window: &Window,
    cx: &mut Context<Shika>,
) -> gpui::Div {
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
            .on_click(cx.listener(move |this, _, window, cx| {
                this.remove_setup_entry(command, index, window, cx)
            })),
        )
}
/// Add file path or Add command, under its list.
fn add_entry_button(
    command: bool,
    add_focus: &FocusHandle,
    chrome: &Chrome,
    window: &Window,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
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
        .on_click(cx.listener(move |this, _, window, cx| this.add_setup_entry(command, window, cx)))
}
/// A compact preview list: its heading and each value in mono, or nothing.
fn preview_list(
    mut panel: gpui::Stateful<gpui::Div>,
    list: &[Entry],
    heading: &'static str,
    chrome: &Chrome,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
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
    panel
}
/// Disable setup, shown when the project already has a configuration.
fn disable_setup_button(
    editor: &Editor,
    chrome: &Chrome,
    window: &Window,
    cx: &mut Context<Shika>,
) -> gpui::Stateful<gpui::Div> {
    secondary_button("disable-setup", chrome)
        .track_focus(&editor.disable)
        .when(editor.disable.is_focused(window), |d| {
            d.border_color(chrome.focus)
        })
        .px(px(12.))
        .py(px(6.))
        .child("Disable setup")
        .on_click(cx.listener(|this, _, window, cx| this.save_worktree_setup(true, window, cx)))
}
impl Shika {
    pub(crate) fn worktree_setup_view(
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
        let panel = setup_intro(editor, name, config_path, chrome, max_h);
        if editor.compact() {
            return self.compact_setup_preview(panel, editor, chrome, window, cx);
        }
        let mut panel = setup_lists(panel, editor, chrome, window, cx);
        panel = panel
            .child(dialog_text(chrome).child("Timeout in seconds (1 to 3600)"))
            .child(editor.timeout.clone());
        if let Some(error) = &editor.error {
            panel = panel.child(dialog_text(chrome).child(error.clone()));
        }
        if editor.expected.is_some() {
            panel = panel.child(disable_setup_button(editor, chrome, window, cx));
        }
        panel.child(self.worktree_setup_buttons(editor, chrome, window, cx))
    }
    /// The compact onboarding preview: the suggested files and commands as
    /// text, the timeout, Customize setup…, and the footer.
    fn compact_setup_preview(
        &self,
        mut panel: gpui::Stateful<gpui::Div>,
        editor: &Editor,
        chrome: &Chrome,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !editor.has_setup() {
            panel = panel.child(dialog_text(chrome).child("No files or commands selected."));
        }
        panel = preview_list(panel, &editor.files, "Copy before each agent", chrome, cx);
        if editor.clone_dependencies {
            panel = panel.child(dialog_text(chrome).child(CLONE_FACT));
        }
        panel = preview_list(panel, &editor.commands, "Run before each agent", chrome, cx);
        panel = panel.child(dialog_text(chrome).child(format!(
            "Timeout: {} seconds",
            editor.timeout.read(cx).value()
        )));
        if let Some(error) = &editor.error {
            panel = panel.child(dialog_text(chrome).child(error.clone()));
        }
        panel
            .child(
                secondary_button("customize-setup", chrome)
                    .track_focus(&editor.customize)
                    .when(editor.customize.is_focused(window), |d| {
                        d.border_color(chrome.focus)
                    })
                    .px(px(12.))
                    .py(px(6.))
                    .child("Customize setup…")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.customize_preparation(window, cx)),
                    ),
            )
            .child(self.worktree_setup_buttons(editor, chrome, window, cx))
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
