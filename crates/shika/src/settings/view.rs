//! Painting the Settings dialog: navigation, rows, and section panels.
use super::*;

impl Shika {
    /// System, Light, or Dark. `h` / `l` step one segment.
    fn mode_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row, .. }) if *row == MODE_ROW
        );
        let shadow = chrome.control_shadow;
        let choice = |id: &'static str, label: &'static str, mode: ThemeMode| {
            let chosen = self.theme.mode == mode;
            segment(id, label, chosen, chrome.raised, chrome.ink_1, chrome.ink_3)
                .when(chosen, |d| {
                    d.shadow(vec![
                        BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                    ])
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_theme_mode(mode, window, cx);
                    this.select_setting(MODE_ROW, window, cx);
                }))
        };
        list_row(selected, chrome)
            .justify_between()
            .child("Theme")
            .child(
                div()
                    .flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(7.))
                    .bg(chrome.sunken)
                    .child(choice("theme-mode-system", "System", ThemeMode::System))
                    .child(choice("theme-mode-light", "Light", ThemeMode::Light))
                    .child(choice("theme-mode-dark", "Dark", ThemeMode::Dark)),
            )
    }

    /// The light or dark theme: minus, the theme's name, plus, like the
    /// number rows. `h` / `l` step through that side of the catalog.
    fn theme_row(
        &self,
        row: usize,
        label: &'static str,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row: at, .. }) if *at == row
        );
        let name = appearance::resolve_theme(&self.theme, row == DARK_ROW).name;
        let step = |delta: i64| {
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.step_setting(row, delta, window, cx);
                this.select_setting(row, window, cx);
            })
        };
        let field = text_field(
            SharedString::from(format!("setting-{row}-value")),
            THEME_FIELD_WIDTH,
            false,
            chrome,
        )
        .font_family(UI_FONT)
        .cursor_default()
        .child(div().min_w_0().truncate().child(name))
        .on_click(cx.listener(move |this, _, window, cx| this.select_setting(row, window, cx)));
        list_row(selected, chrome)
            .justify_between()
            .child(label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-less")),
                            "-",
                            chrome,
                        )
                        .on_click(step(-1)),
                    )
                    .child(field)
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-more")),
                            "+",
                            chrome,
                        )
                        .on_click(step(1)),
                    ),
            )
    }

    /// The branch prefix row: a text field. Enter or a click starts typing.
    fn prefix_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let (selected, edit) = match &self.overlay {
            Some(Overlay::Settings { row, edit, .. }) => (
                *row == PREFIX_ROW,
                edit.as_deref().filter(|_| *row == PREFIX_ROW),
            ),
            _ => (false, None),
        };
        let text = edit.unwrap_or(&self.branch_prefix).to_string();
        let empty = text.is_empty();
        let field = text_field("setting-prefix-value", 168., edit.is_some(), chrome)
            .when(!empty, |d| d.child(text))
            .when(edit.is_some(), |d| {
                d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
            })
            .when(empty && edit.is_none(), |d| {
                d.child(div().text_color(chrome.ink_4).child("none"))
            })
            .on_click(cx.listener(|this, _, window, cx| {
                if !matches!(
                    this.overlay,
                    Some(Overlay::Settings {
                        row: PREFIX_ROW,
                        edit: Some(_),
                        ..
                    })
                ) {
                    let prefix = this.branch_prefix.clone();
                    this.edit_setting(PREFIX_ROW, &prefix, window, cx);
                }
            }));
        list_row(selected, chrome)
            .justify_between()
            .child("Branch prefix")
            .child(field)
    }

    /// Off or On. `h` turns the alert off, `l` turns it on.
    fn sound_row(&self, chrome: &Chrome, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings { row, .. }) if *row == SOUND_ROW
        );
        let on = self.notification_sound;
        let shadow = chrome.control_shadow;
        let choice = |id: &'static str, label: &'static str, chosen: bool, delta: i64| {
            segment(id, label, chosen, chrome.raised, chrome.ink_1, chrome.ink_3)
                .when(chosen, |d| {
                    d.shadow(vec![
                        BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                    ])
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.step_setting(SOUND_ROW, delta, window, cx);
                    if let Some(Overlay::Settings { row, .. }) = &mut this.overlay {
                        *row = SOUND_ROW;
                    }
                    cx.notify();
                }))
        };
        list_row(selected, chrome)
            .justify_between()
            .child("Notification sound")
            .child(
                div()
                    .flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(7.))
                    .bg(chrome.sunken)
                    .child(choice("notification-sound-off", "Off", !on, -1))
                    .child(choice("notification-sound-on", "On", on, 1)),
            )
    }

    /// A settings row: minus, a number field that takes typed digits, plus.
    fn setting_row(
        &self,
        row: usize,
        label: &'static str,
        value: &str,
        unit: &'static str,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (selected, edit) = match &self.overlay {
            Some(Overlay::Settings { row: at, edit, .. }) => {
                (*at == row, edit.as_deref().filter(|_| *at == row))
            }
            _ => (false, None),
        };
        let step = |delta: i64| {
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.step_setting(row, delta, window, cx);
                if let Some(Overlay::Settings { row: at, .. }) = &mut this.overlay {
                    *at = row;
                }
            })
        };
        let text = match edit {
            Some("") | None => value.to_string(),
            Some(digits) => digits.to_string(),
        };
        let field = text_field(
            SharedString::from(format!("setting-{row}-value")),
            76.,
            edit.is_some(),
            chrome,
        )
        .justify_end()
        .child(
            div()
                .when(edit == Some(""), |d| d.text_color(chrome.ink_4))
                .child(text),
        )
        .when(edit.is_some(), |d| {
            d.child(div().w(px(1.)).h(px(14.)).bg(chrome.focus))
        })
        .child(div().text_color(chrome.ink_4).child(unit))
        .on_click(cx.listener(move |this, _, window, cx| this.edit_setting(row, "", window, cx)));
        list_row(selected, chrome)
            .justify_between()
            .child(label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-less")),
                            "-",
                            chrome,
                        )
                        .on_click(step(-1)),
                    )
                    .child(field)
                    .child(
                        step_button(
                            SharedString::from(format!("setting-{row}-more")),
                            "+",
                            chrome,
                        )
                        .on_click(step(1)),
                    ),
            )
    }

    fn settings_nav(
        &self,
        current: SettingsSection,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut nav = div()
            .w(px(SETTINGS_NAV_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .pr(px(12.))
            .border_r_1()
            .border_color(chrome.line_2);
        for section in SettingsSection::ALL {
            nav = nav.child(
                list_row(section == current, chrome)
                    .id(SharedString::from(format!(
                        "settings-section-{}",
                        section.label()
                    )))
                    .cursor_pointer()
                    .child(section.label())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_section(section, window, cx);
                    })),
            );
        }
        nav
    }

    /// One preset: its name, where the binary is, and whether New offers it.
    fn agent_row(
        &self,
        index: usize,
        preset: &CliPreset,
        home: Option<&std::path::Path>,
        chrome: &Chrome,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = matches!(
            &self.overlay,
            Some(Overlay::Settings {
                section: SettingsSection::Agents,
                row,
                ..
            }) if *row == index
        );
        let on = self.agents.enabled(&preset.id);
        let detail = match &preset.path {
            Some(path) => model::tilde(path, home),
            None => format!("{} not found on PATH", preset.binary),
        };
        let name_color = if preset.found() {
            chrome.ink_1
        } else {
            chrome.ink_4
        };
        let shadow = chrome.control_shadow;
        let off_id = preset.id.clone();
        let on_id = preset.id.clone();
        let segment_shadow = move |chosen: bool, control: gpui::Stateful<gpui::Div>| {
            control.when(chosen, |d| {
                d.shadow(vec![
                    BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                ])
            })
        };
        list_row(selected, chrome)
            .id(SharedString::from(format!("agent-setting-{}", preset.id)))
            .gap(px(12.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_setting(index, window, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(name_color)
                    .child(preset.name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(MONO)
                    .text_size(px(11.))
                    .text_color(chrome.ink_3)
                    .child(detail),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .p(px(2.))
                    .gap(px(2.))
                    .rounded(px(7.))
                    .bg(chrome.sunken)
                    .child(
                        segment_shadow(
                            !on,
                            segment(
                                SharedString::from(format!("agent-{}-off", preset.id)),
                                "Off",
                                !on,
                                chrome.raised,
                                chrome.ink_1,
                                chrome.ink_3,
                            ),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.set_agent_enabled(&off_id, false, cx);
                                this.select_setting(index, window, cx);
                            },
                        )),
                    )
                    .child(
                        segment_shadow(
                            on,
                            segment(
                                SharedString::from(format!("agent-{}-on", preset.id)),
                                "On",
                                on,
                                chrome.raised,
                                chrome.ink_1,
                                chrome.ink_3,
                            ),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.set_agent_enabled(&on_id, true, cx);
                                this.select_setting(index, window, cx);
                            },
                        )),
                    ),
            )
    }

    pub(crate) fn settings_panel(
        &self,
        section: SettingsSection,
        row: usize,
        max_h: Pixels,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let panel_h = if max_h < px(SETTINGS_HEIGHT) {
            max_h
        } else {
            px(SETTINGS_HEIGHT)
        };
        let editing = matches!(self.overlay, Some(Overlay::Settings { edit: Some(_), .. }));
        let keys = self.settings_keys(section, row, editing);
        let mut rows = div()
            .id("settings-rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.settings_scroll)
            .flex()
            .flex_col()
            .gap(px(4.));
        match section {
            SettingsSection::Appearance => {
                let opacity = self.appearance.opacity.to_string();
                let blur = self.appearance.blur.to_string();
                let font = self.font_size.text();
                let both = self.appearance.translucency == Translucency::SidebarAndTerminal;
                let shadow = chrome.control_shadow;
                let choice = |id: &'static str, label: &'static str, on: bool| {
                    segment(id, label, on, chrome.raised, chrome.ink_1, chrome.ink_3).when(
                        on,
                        |d| {
                            d.shadow(vec![
                                BoxShadow::new(px(0.), px(1.), shadow.into()).blur_radius(px(1.)),
                            ])
                        },
                    )
                };
                rows = rows
                    .child(self.mode_row(chrome, cx))
                    .child(self.theme_row(LIGHT_ROW, "Light theme", chrome, cx))
                    .child(self.theme_row(DARK_ROW, "Dark theme", chrome, cx))
                    .child(self.image_row(chrome, cx))
                    .child(self.effect_row(chrome, cx))
                    .child(self.setting_row(
                        OPACITY_ROW,
                        "Background opacity",
                        &opacity,
                        "%",
                        chrome,
                        cx,
                    ))
                    .child(self.setting_row(BLUR_ROW, "Background blur", &blur, "", chrome, cx))
                    .child(
                        list_row(row == APPLY_ROW, chrome)
                            .justify_between()
                            .child("Apply to")
                            .child(
                                div()
                                    .flex()
                                    .p(px(2.))
                                    .gap(px(2.))
                                    .rounded(px(7.))
                                    .bg(chrome.sunken)
                                    .child(
                                        choice("translucent-sidebar", "Sidebar", !both).on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.step_setting(APPLY_ROW, -1, window, cx);
                                                this.select_setting(APPLY_ROW, window, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        choice("translucent-both", "Sidebar and terminal", both)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.step_setting(APPLY_ROW, 1, window, cx);
                                                this.select_setting(APPLY_ROW, window, cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(self.setting_row(
                        FONT_ROW,
                        "Terminal font size",
                        &font,
                        "px",
                        chrome,
                        cx,
                    ))
                    .child(self.prefix_row(chrome, cx))
                    .child(self.sound_row(chrome, cx));
            }
            SettingsSection::Agents => {
                if let Some(presets) = self.catalog.as_ref().map(|catalog| catalog.presets.clone())
                {
                    for (index, preset) in presets.iter().enumerate() {
                        rows = rows.child(self.agent_row(index, preset, home, chrome, cx));
                    }
                } else {
                    rows = rows.child(
                        list_row(false, chrome)
                            .text_color(chrome.ink_3)
                            .child("Resolving login-shell PATH..."),
                    );
                }
            }
            SettingsSection::Projects => {
                if self.projects.is_empty() {
                    rows = rows.child(
                        dialog_text(chrome).child("Add a project to configure worktree setup."),
                    );
                }
                for (index, project) in self.projects.iter().enumerate() {
                    let id = project.id.clone();
                    rows = rows.child(
                        list_row(row == index, chrome)
                            .id(SharedString::from(format!("project-setting-{id}")))
                            .flex_col()
                            .items_start()
                            .gap(px(4.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_worktree_setup(id.clone(), window, cx)
                            }))
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(project.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(chrome.ink_3)
                                            .child("Worktree setup…"),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .truncate()
                                    .font_family(MONO)
                                    .text_size(px(11.))
                                    .text_color(chrome.ink_3)
                                    .child(model::tilde(&project.path, home)),
                            ),
                    );
                }
            }
            SettingsSection::Keyboard => {
                for (index, command) in shortcuts::COMMANDS.iter().enumerate() {
                    rows = rows.child(self.shortcut_row(index, command, chrome, cx));
                }
            }
            SettingsSection::Usage => rows = rows.child(self.usage_rows(chrome, cx)),
        }
        dialog_shell(SETTINGS_WIDTH, panel_h, chrome)
            .h(panel_h)
            .overflow_hidden()
            .pt(px(20.))
            .px(px(20.))
            .pb(px(16.))
            .gap(px(8.))
            .child(dialog_title(div()).child("Settings"))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(12.))
                    .child(self.settings_nav(section, chrome, cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(rows)
                            .child(scrollbar::vertical(
                                &self.settings_scroll,
                                &self.settings_grab,
                                chrome.ink_4,
                                chrome.ink_3,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .flex_wrap()
                    .gap_x(px(12.))
                    .gap_y(px(6.))
                    .px(px(10.))
                    .when_some(
                        (section == SettingsSection::Keyboard)
                            .then(|| self.key_note.clone())
                            .flatten(),
                        |row, note| {
                            row.child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .text_color(chrome.ink_1)
                                    .child(note),
                            )
                        },
                    )
                    .children(keys.iter().map(|(key, label)| hint(key, label, chrome))),
            )
            .child(dialog_buttons().child(self.cancel_button("Done", chrome, cx)))
    }

    /// Settings > Usage, from the snapshot read when it first showed: the
    /// range, the provider totals that filter the chart, then the chart.
    fn usage_rows(&self, chrome: &Chrome, cx: &mut Context<Self>) -> gpui::Div {
        let mut panel = div().flex().flex_col().gap(px(12.)).px(px(10.)).pt(px(4.));
        let Some(Overlay::Settings {
            usage: Some(view), ..
        }) = &self.overlay
        else {
            return panel;
        };
        let (range, filter) = (view.range, view.filter);
        let mut segments = div()
            .flex()
            .flex_none()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(7.))
            .bg(chrome.sunken);
        for (index, choice) in UsageRange::CHOICES.into_iter().enumerate() {
            segments = segments.child(
                segment(
                    ("usage-range", index),
                    choice.label(),
                    range == choice,
                    chrome.raised,
                    chrome.ink_1,
                    chrome.ink_3,
                )
                .when(range == choice, |d| {
                    d.shadow(vec![
                        BoxShadow::new(px(0.), px(1.), chrome.control_shadow.into())
                            .blur_radius(px(1.)),
                    ])
                })
                .on_click(cx.listener(move |this, _, _, cx| this.set_usage_range(choice, cx))),
            );
        }
        panel = panel.child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap(px(12.))
                .child(
                    dialog_text(chrome)
                        .min_w_0()
                        .child("Tokens from Claude Code and Codex in Shika. Not a bill."),
                )
                .child(segments),
        );
        let Some(report) = &view.report else {
            return panel.child(dialog_text(chrome).child("Reading usage..."));
        };
        if report.unreadable {
            panel = panel.child(dialog_text(chrome).child("Could not read usage."));
        } else if let Some(calendar) = &report.calendar {
            panel = panel.child(usage_dashboard::totals(
                calendar,
                range,
                filter,
                chrome,
                |choice, column| {
                    column.on_click(
                        cx.listener(move |this, _, _, cx| this.set_usage_filter(choice, cx)),
                    )
                },
            ));
            if let Some(chart) = &view.chart {
                panel = panel.child(usage_dashboard::chart(
                    calendar, range, filter, chrome, chart,
                ));
            }
            if range.totals(calendar).combined() == 0 {
                panel = panel.child(dialog_text(chrome).child(if report.empty {
                    "No tokens recorded yet."
                } else {
                    "No tokens recorded in this range."
                }));
            }
        }
        if report.pending {
            panel =
                panel.child(dialog_text(chrome).child(
                    "Some usage is pending or could not be read. Saved totals are retained.",
                ));
        }
        panel
    }
}
