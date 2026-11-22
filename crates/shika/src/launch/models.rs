//! New Lead's model dropdown. The catalog is fetched only when expanded, and
//! belongs to the current provider selection. A row never starts a Lead.
use super::*;
use gpui::ScrollHandle;
use shika_core::LeadModelChoice;

#[derive(Clone)]
pub(crate) struct ModelPicker {
    request: Arc<()>,
    catalog: Catalog,
    selected: Option<String>,
    editor: Option<Entity<name_input::NameInput>>,
    open: bool,
    cursor: usize,
    scroll: ScrollHandle,
}

#[derive(Clone, Default)]
enum Catalog {
    #[default]
    Unread,
    Loading,
    Ready(Arc<[LeadModelChoice]>),
    Unavailable,
}

#[derive(Clone)]
enum ModelAction {
    Select(Option<String>),
    Custom,
}

impl ModelPicker {
    pub(super) fn new(selected: Option<&str>) -> Self {
        Self {
            request: Arc::new(()),
            catalog: Catalog::Unread,
            selected: selected.map(str::to_owned),
            editor: None,
            open: false,
            cursor: 0,
            scroll: ScrollHandle::new(),
        }
    }
    pub(super) fn composing(&self, cx: &App) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|field| field.read(cx).composing())
    }
    pub(super) fn selected(&self, cx: &App) -> shika_core::Result<Option<String>> {
        if let Some(field) = &self.editor {
            shika_core::lead_model_override(field.read(cx).value())
        } else {
            Ok(self.selected.clone())
        }
    }
    fn choices(&self) -> &[LeadModelChoice] {
        match &self.catalog {
            Catalog::Ready(choices) => choices,
            _ => &[],
        }
    }
    fn selected_choice(&self) -> Option<usize> {
        match self.selected.as_deref() {
            None => Some(0),
            Some(id) => self
                .choices()
                .iter()
                .position(|m| m.id == id)
                .map(|i| i + 1),
        }
    }
    fn reveal_cursor(&self) {
        self.scroll.scroll_to_item(self.cursor);
    }
    fn open(&mut self) {
        self.open = true;
        self.cursor = self.selected_choice().unwrap_or(self.choices().len() + 1);
        self.scroll = ScrollHandle::new();
        self.reveal_cursor();
    }
}

impl Shika {
    fn model_picker_mut(&mut self) -> Option<(&mut ModelPicker, &mut Option<String>)> {
        let Some(Overlay::Picker(picker)) = &mut self.overlay else {
            return None;
        };
        Some((picker.lead_model.as_mut()?, &mut picker.error))
    }

    pub(super) fn toggle_model_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Overlay::Picker(picker)) = &self.overlay else {
            return;
        };
        let Some(preset) = self
            .new_choices(picker.mode)
            .into_iter()
            .nth(picker.index)
            .filter(|preset| shika_core::lead_chat_supported(&preset.id))
        else {
            return;
        };
        let focus = self.focus.clone();
        let Some((model, error)) = self.model_picker_mut() else {
            return;
        };
        if model.composing(cx) {
            return;
        }
        *error = None;
        model.editor = None;
        window.focus(&focus, cx);
        if model.open {
            model.open = false;
            cx.notify();
            return;
        }
        model.open();
        cx.notify();
        if !matches!(model.catalog, Catalog::Unread | Catalog::Unavailable) {
            return;
        }
        model.catalog = Catalog::Loading;
        let request = model.request.clone();
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.lead_models(&preset) })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Quit can temporarily park the picker. Keep its catalog
                // ready if the author cancels quit; never apply to a new picker.
                let overlay = match this.overlay.as_mut() {
                    Some(Overlay::Quit(quit)) => quit.previous.as_deref_mut(),
                    other => other,
                };
                let Some(Overlay::Picker(picker)) = overlay else {
                    return;
                };
                let Some(model) = &mut picker.lead_model else {
                    return;
                };
                if !Arc::ptr_eq(&request, &model.request) {
                    return;
                }
                let cursor_was_custom = model.cursor == model.choices().len() + 1;
                model.catalog = result.map_or(Catalog::Unavailable, |choices| {
                    Catalog::Ready(choices.into())
                });
                if cursor_was_custom {
                    model.cursor = model
                        .selected_choice()
                        .filter(|&i| i != 0)
                        .unwrap_or(model.choices().len() + 1);
                    if model.open {
                        model.reveal_cursor();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn picker_lead_model_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((model, _)) = self.model_picker_mut() else {
            return false;
        };
        let key = &event.keystroke;
        let modifiers = key.modifiers;
        let plain = !modifiers.platform && !modifiers.control && !modifiers.alt && !modifiers.shift;
        let key = key.key.as_str();
        // A held Enter after selecting a model must not start the Lead.
        if plain && key == "enter" && event.is_held {
            cx.stop_propagation();
            return true;
        }
        if let Some(editor) = &model.editor
            && editor.focus_handle(cx).is_focused(window)
        {
            if !plain || editor.read(cx).composing() {
                return true;
            }
            match key {
                "enter" => self.apply_model_id(window, cx),
                "escape" | "tab" => {
                    model.editor = None;
                    window.focus(&self.focus, cx);
                    if key == "tab" {
                        self.picker_key("tab", window, cx);
                    }
                    cx.notify();
                }
                _ => return true,
            }
        } else if model.open && plain {
            match key {
                "j" | "down" | "k" | "up" => {
                    let count = model.choices().len() + 2;
                    let step = if matches!(key, "j" | "down") {
                        1
                    } else {
                        count - 1
                    };
                    model.cursor = (model.cursor + step) % count;
                    model.reveal_cursor();
                    cx.notify();
                }
                "enter" => {
                    let action = match model.cursor.checked_sub(1) {
                        None => ModelAction::Select(None),
                        Some(i) => model
                            .choices()
                            .get(i)
                            .map_or(ModelAction::Custom, |choice| {
                                ModelAction::Select(Some(choice.id.clone()))
                            }),
                    };
                    self.choose_model(action, window, cx);
                }
                "escape" | "m" => {
                    model.open = false;
                    cx.notify();
                }
                "tab" => {
                    model.open = false;
                    return false;
                }
                _ => return false,
            }
        } else {
            return false;
        }
        cx.stop_propagation();
        true
    }

    fn choose_model(&mut self, action: ModelAction, window: &mut Window, cx: &mut Context<Self>) {
        let chrome = self.chrome(window);
        let Some((model, error)) = self.model_picker_mut() else {
            return;
        };
        if !model.open {
            return;
        }
        match action {
            ModelAction::Custom => {
                let value = model.selected.clone().unwrap_or_default();
                let field = cx.new(|cx| name_input::NameInput::new(value, chrome, cx));
                window.focus(&field.focus_handle(cx), cx);
                model.editor = Some(field);
            }
            ModelAction::Select(id) => model.selected = id,
        }
        model.open = false;
        *error = None;
        cx.notify();
    }

    fn apply_model_id(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((model, error)) = self.model_picker_mut() else {
            return;
        };
        match model.selected(cx) {
            Ok(id) => {
                model.selected = id;
                model.editor = None;
                *error = None;
                window.focus(&self.focus, cx);
            }
            Err(err) => *error = Some(err.to_string()),
        }
        cx.notify();
    }
}

pub(super) fn model_picker_view(
    model: &ModelPicker,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> gpui::Div {
    let mut section = div().px(px(10.)).py(px(6.)).flex().flex_col().gap(px(4.));
    if let Some(field) = &model.editor {
        field.update(cx, |field, _| field.set_chrome(*chrome));
        return section
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(chrome.ink_3)
                    .child("Model ID"),
            )
            .child(field.clone())
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(chrome.ink_3)
                    .child("Enter to use this ID. Leave blank for CLI default."),
            );
    }
    let label = model
        .selected_choice()
        .and_then(|i| i.checked_sub(1))
        .map(|i| model.choices()[i].label.as_str())
        .or(model.selected.as_deref())
        .unwrap_or("CLI default")
        .to_owned();
    section = section.child(
        div()
            .id("lead-model-dropdown")
            .role(gpui::Role::Button)
            .aria_label(format!("Model: {label}"))
            .w_full()
            .h(px(30.))
            .px(px(8.))
            .rounded(px(6.))
            .bg(chrome.raised)
            .border_1()
            .border_color(if model.open {
                chrome.focus
            } else {
                chrome.line_control
            })
            .hover(move |d| d.bg(chrome.raised_hover))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(8.))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_model_list(window, cx)))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(chrome.ink_3)
                    .child("Model"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(chrome.ink_1)
                    .child(label),
            )
            .child(kbd("m", chrome.sunken, chrome.ink_2))
            .child(
                div()
                    .text_color(chrome.ink_3)
                    .child(if model.open { "⌃" } else { "⌄" }),
            ),
    );
    if !model.open {
        return section;
    }
    let chosen = model.selected_choice();
    let list = div()
        .id("lead-model-options")
        .p(px(4.))
        .rounded(px(7.))
        .border_1()
        .border_color(chrome.line_2)
        .max_h(px(172.))
        .overflow_y_scroll()
        .track_scroll(&model.scroll)
        .child(model_row(
            0,
            "CLI default".into(),
            ModelAction::Select(None),
            model.cursor == 0,
            chosen == Some(0),
            chrome,
            cx,
        ))
        .children(model.choices().iter().enumerate().map(|(i, choice)| {
            let at = i + 1;
            model_row(
                at,
                choice.label.clone(),
                ModelAction::Select(Some(choice.id.clone())),
                model.cursor == at,
                chosen == Some(at),
                chrome,
                cx,
            )
        }));
    let last = model.choices().len() + 1;
    section = section.child(list.child(model_row(
        last,
        "Enter model ID…".into(),
        ModelAction::Custom,
        model.cursor == last,
        chosen.is_none(),
        chrome,
        cx,
    )));
    match model.catalog {
        Catalog::Loading => section.child(model_hint("Reading models...", chrome)),
        Catalog::Unavailable => section.child(model_hint(
            "Could not list models. Use CLI default or enter an ID.",
            chrome,
        )),
        _ => section,
    }
}

fn model_hint(text: &'static str, chrome: &Chrome) -> gpui::Div {
    div()
        .px(px(4.))
        .text_size(px(11.))
        .text_color(chrome.ink_3)
        .child(text)
}

fn model_row(
    index: usize,
    label: String,
    action: ModelAction,
    highlighted: bool,
    selected: bool,
    chrome: &Chrome,
    cx: &mut Context<Shika>,
) -> impl IntoElement + use<> {
    let id = match &action {
        ModelAction::Select(Some(id)) if id != &label => Some(id.clone()),
        _ => None,
    };
    list_row(highlighted, chrome)
        .id(("lead-model-option", index))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .py(px(4.))
        .gap(px(8.))
        .hover(move |d| d.bg(chrome.row_selected))
        .child(
            div()
                .w(px(12.))
                .flex_none()
                .text_color(chrome.ink_2)
                .child(if selected { "✓" } else { "" }),
        )
        .child(div().min_w_0().truncate().child(label))
        .when_some(id, |row, id| {
            row.child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .text_color(chrome.ink_3)
                    .child(id),
            )
        })
        .on_click(
            cx.listener(move |this, _, window, cx| this.choose_model(action.clone(), window, cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn model_dropdown_enter_selects_without_launch_and_escape_only_collapses(
        cx: &mut gpui::TestAppContext,
    ) {
        let data = std::env::temp_dir().join(format!(
            "shika-model-ui-{}",
            shika_core::control::new_token()
        ));
        std::fs::create_dir(&data).unwrap();
        let core = Arc::new(Core::open(&data).unwrap());
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = Shika::new(core, Ok(Settings::default()), None, false, window, cx);
            app.catalog = Some(shika_core::CliCatalog {
                presets: vec![CliPreset {
                    id: "codex".into(),
                    name: "Codex".into(),
                    binary: "codex".into(),
                    path: Some("/fixture/codex".into()),
                    args: vec![],
                }],
                error: None,
            });
            app.show_picker("fixture".into(), PickerMode::Lead, window, cx);
            app.model_picker_mut().unwrap().0.catalog = Catalog::Ready(
                vec![LeadModelChoice {
                    id: "test-model".into(),
                    label: "Test model".into(),
                }]
                .into(),
            );
            app
        });
        cx.simulate_keystrokes("m down enter");
        app.update(cx, |app, _| {
            let model = app.model_picker_mut().expect("picker stays open").0;
            assert_eq!(model.selected.as_deref(), Some("test-model"));
            assert!(!model.open);
            assert!(app.cards.is_empty());
        });
        cx.simulate_keystrokes("m up escape");
        app.update(cx, |app, _| {
            let model = app
                .model_picker_mut()
                .expect("Escape collapses only the model list")
                .0;
            assert_eq!(model.selected.as_deref(), Some("test-model"));
            assert!(!model.open);
        });
        cx.update(|window, _| window.remove_window());
        std::fs::remove_dir_all(data).unwrap();
    }
}
