mod activity;
mod agent_marks;
mod appearance;
mod background;
mod background_view;
mod base_branch;
mod card;
mod card_menu;
mod changes;
mod chat_input;
mod checkout;
mod checks;
mod close;
mod column;
mod control;
mod control_client;
mod focus;
mod host;
mod keys;
mod launch;
mod lead;
mod lead_chat;
mod lead_steps;
mod lifecycle;
mod model;
mod name_input;
mod notifications;
mod overlay;
mod project_motion;
mod projects;
mod publish;
mod quota_dashboard;
mod recovery;
mod rename;
mod scrollbar;
mod selection;
mod settings;
mod shortcuts;
mod startup;
mod switch_agent;
mod tabs;
mod terminal_side;
mod text_edit;
mod theme;
mod tick;
mod ui;
mod update_notice;
mod updates;
mod usage_dashboard;
mod worktree_setup;

use base_branch::*;
use card::*;
use card_menu::*;
use close::*;
use column::*;
use host::*;
use launch::*;
use lead::*;
use publish::*;
use selection::*;
use settings::*;
use startup::*;
use tabs::*;
use terminal_side::*;
use ui::*;
use update_notice::*;

use appearance::{Chrome, with_alpha};
use gpui::{
    AnimationExt, App, AppContext, Bounds, BoxShadow, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    PathPromptOptions, Pixels, Render, Rgba, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder, px, size,
};
use model::{PromptCapture, Status, TitleWatch};
use notifications::Notifications;
use overlay::Overlay;
use shika_core::{
    AgentSettings, Appearance, CliCatalog, CliPreset, Column, Core, DiffStat, FontSize,
    JournalEntry, KeyOverrides, KnownBranches, LaunchOptions, LeadLaunch, PreparationConfig,
    PreparationControl, PreparationEvent, Project, ProjectBase, PtyEvent, PtyId, PtySize,
    PublishPreview, Session, SessionGitState, Settings, ThemeMode, ThemeSettings, Translucency,
    WorkerEnv,
};
use shika_terminal::{
    InputSource, Palette, PtyHost, Terminal, TerminalConfig, TerminalEvent, TerminalOptions,
    TerminalSize, TerminalView, Theme, openable_uri,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use usage_dashboard::{UsageFilter, UsageRange};

gpui::actions!(
    shika,
    [
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        OpenSettings,
        CheckForUpdates,
        NewAgent,
        NewInCheckout,
        NewTerminal,
        NewLead,
        AddProject,
        BaseBranch,
        CreatePr,
        SwitchAgent,
        CloseTerminal,
        CloseTask,
        RenameTask,
        NextTerminal,
        PreviousTerminal,
        NextAgent,
        PreviousAgent,
        ToggleColumn,
        ToggleChanges,
        ToggleUsage
    ]
);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
#[derive(Clone, Debug, PartialEq)]
enum Selection {
    Project(String),
    Card(usize),
    /// The saved, stopped Lead of this project. It has no card.
    Resting(String),
}
struct Shika {
    core: Arc<Core>,
    projects: Vec<Project>,
    cards: Vec<Card>,
    leads: lead::Leads,
    checkout_heads: HashMap<String, checkout::Head>,
    checkout_selection_epoch: u64,
    /// When the confirmation dialog on screen appeared, to ignore keys that
    /// were already in flight.
    dialog_grace: model::DialogGrace,
    selection: model::Tracked<Option<Selection>>,
    focus: FocusHandle,
    catalog: Option<CliCatalog>,
    overlay: Option<Overlay>,
    /// Non-workflow overlays return to the surface that opened them.
    overlay_return_focus: Option<FocusHandle>,
    /// A project header clicked while a terminal stayed on screen: where
    /// New, Lead, and Base branch aim until the selection next changes. The
    /// selection epoch it was recorded under makes that check, so any change
    /// ends it (`model::Tracked`, `model::header_target`).
    project_target: Option<(String, u64)>,
    /// The background image from settings, painted behind the glass.
    background: Option<shika_core::Background>,
    /// The image's rasters on screen and the one being built.
    backdrop: background::Backdrop,
    /// The image was picked or removed, so the window's own background
    /// (opaque or transparent) changes on the next paint.
    window_backdrop_dirty: bool,
    sidebar_scroll: gpui::ScrollHandle,
    /// The Settings row list's scroll offset and scrollbar drag.
    settings_ui: SettingsUi,
    last_revealed_selection: Option<Selection>,
    busy: bool,
    toast: Option<(String, Instant)>,
    /// The corner update notice. A release build sets it from Sparkle.
    /// `--preview-update` paints a sample so the layout can be seen locally.
    update_notice: Option<updates::UpdateCard>,
    /// Clicks on the sample card only change the picture.
    preview_update: bool,
    leftovers: recovery::LeftoverList,
    clock: Instant,
    /// How long the selected card has been on screen, for clearing its dot.
    dwell: model::Dwell,
    branch_check_at: Instant,
    branch_check_pending: bool,
    notifications: Notifications,
    clicks: std::sync::mpsc::Receiver<String>,
    appearance: Appearance,
    /// Theme mode and the light and dark picks, as saved. Ids resolve to a
    /// catalog theme when painting.
    theme: ThemeSettings,
    /// Terminal text size. New panes and live ones both use it.
    font_size: FontSize,
    /// Last read of macOS Reduce transparency. Glass stays off while this is set.
    reduce_transparency: bool,
    /// Keeps the light and dark listener alive for the life of the window.
    #[allow(dead_code)]
    appearance_watch: Subscription,
    /// Mouse is down on the title bar and has not moved yet. The drag starts
    /// on the first move, so a double-click can still zoom.
    title_drag: bool,
    /// As typed in Settings, already normalized. Core reads it from disk.
    branch_prefix: String,
    /// Play the system alert sound with a ready notification.
    notification_sound: bool,
    /// Which agents New lists. A missing id is on.
    agents: AgentSettings,
    /// Shortcut overrides. Missing entries keep the built-in chords.
    keys: KeyOverrides,
    /// The agent column's stored width and whether it is hidden.
    column: Column,
    /// The column as it was when the current drag on its edge began. The
    /// file is written once the drag ends.
    column_drag_from: Option<Column>,
    /// Each project's base branch, resolved off the main thread. A project
    /// missing here shows no base label.
    bases: HashMap<String, ProjectBase>,
    /// The read-only Changes panel right of the terminal.
    changes: changes::Panel,
    /// The Lead's control socket, or why there is none.
    control: Option<control::Server>,
    /// The Close flow a Lead's `shika close` is waiting on. See
    /// `control::LeadDialog`. `shika pr` has no dialog.
    lead_dialog: Option<control::LeadDialog>,
    control_error: Option<String>,
    /// Closed cards playing their exit. See [`Departing`].
    departing: Vec<Departing>,
    departing_serial: u64,
    /// Projects promoted during this app run. Closing their cards never
    /// demotes them; the saved repository list stays unchanged.
    promoted_projects: Vec<String>,
    /// Last rendered project order, independent of the saved repository list.
    project_order: Vec<String>,
    project_order_epoch: u64,
}
impl Shika {
    fn new(
        core: Arc<Core>,
        settings: shika_core::Result<Settings>,
        diagnostics: Option<PathBuf>,
        preview_update: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (notifications, clicks) = Notifications::new();
        if let Some(path) = &diagnostics {
            notifications.set_diagnostics_file(path);
        }
        let Saved {
            projects,
            selection,
            leftovers,
            settings,
            load_errors,
        } = Saved::load(&core, settings);
        let appearance = settings.appearance;
        let theme = settings.theme.clone();
        let font_size = settings.font_size;
        let branch_prefix = shika_core::normalize_branch_prefix(&settings.branch_prefix);
        let notification_sound = settings.notification_sound;
        let agents = settings.agents.clone();
        let keys = settings.keys.clone();
        let column = settings.column;
        let background = settings.background.clone();
        let changes = changes::Panel::new(settings.changes, cx.focus_handle(), diagnostics.clone());
        let (control, control_error) = match control::Server::start(core.data_dir()) {
            Ok(server) => (Some(server), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let reduce_transparency = appearance::reduce_transparency();
        let appearance_watch = Self::watch_window_appearance(window, cx);
        let this = Self {
            core: core.clone(),
            projects,
            cards: vec![],
            leads: lead::Leads::default(),
            checkout_heads: HashMap::new(),
            checkout_selection_epoch: u64::MAX,
            dialog_grace: model::DialogGrace::default(),
            selection: model::Tracked::new(selection),
            focus: cx.focus_handle(),
            catalog: None,
            overlay: if leftovers.is_empty() {
                None
            } else {
                Some(Overlay::Leftovers)
            },
            overlay_return_focus: None,
            project_target: None,
            background,
            backdrop: background::Backdrop::default(),
            window_backdrop_dirty: false,
            sidebar_scroll: gpui::ScrollHandle::new(),
            settings_ui: SettingsUi::default(),
            last_revealed_selection: None,
            busy: false,
            toast: if load_errors.is_empty() {
                None
            } else {
                Some((load_errors.join("; "), Instant::now()))
            },
            update_notice: preview_update.then(sample_update_notice),
            preview_update,
            leftovers: recovery::LeftoverList::new(leftovers),
            clock: Instant::now(),
            dwell: model::Dwell::default(),
            branch_check_at: Instant::now(),
            branch_check_pending: false,
            notifications,
            clicks,
            appearance,
            theme,
            font_size,
            reduce_transparency,
            appearance_watch,
            title_drag: false,
            branch_prefix,
            notification_sound,
            agents,
            keys,
            column,
            column_drag_from: None,
            bases: HashMap::new(),
            changes,
            control,
            lead_dialog: None,
            control_error,
            departing: Vec::new(),
            departing_serial: 0,
            promoted_projects: Vec::new(),
            project_order: Vec::new(),
            project_order_epoch: 0,
        };
        Self::load_catalog_then_tick(core, diagnostics, window, cx);
        install_keys(cx, &this.keys);
        this
    }
    /// Repaints the terminals in the light or dark theme when macOS switches.
    /// The subscription lives as long as the window.
    fn watch_window_appearance(window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        let entity = cx.entity().downgrade();
        window.observe_window_appearance(move |window, cx| {
            let _ = entity.update(cx, |this, cx| {
                this.push_terminal_theme(window, cx);
                cx.notify();
            });
        })
    }
    /// Finds the agent CLIs off the main thread, then runs the tick every
    /// 100ms for as long as the window lives.
    fn load_catalog_then_tick(
        core: Arc<Core>,
        diagnostics: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let catalog = cx
                .background_executor()
                .spawn(async move { read_cli_catalog(&core, diagnostics) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(error) = &catalog.error {
                    this.message(format!("Login shell PATH: {error:?}"));
                }
                this.catalog = Some(catalog);
                let ids = this.projects.iter().map(|p| p.id.clone()).collect();
                this.refresh_bases(ids, cx);
                cx.notify();
            });
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| this.tick(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }
    fn message(&mut self, text: String) {
        self.toast = Some((text, Instant::now()));
    }
}
impl Focusable for Shika {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for Shika {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_claude_lead_chats(window, cx);
        self.sync_window_backdrop(window, cx);
        self.observe_dialog(Instant::now());
        let cards_focused = self.focus.is_focused(window);
        let chrome = self.chrome(window);
        let backdrop = self.backdrop_layers(&chrome, window, cx);
        let home = home_dir();
        let reveal_selection = *self.selection != self.last_revealed_selection;
        self.last_revealed_selection = self.selection.clone();
        // The Changes panel follows the selected card. Closed, this is a no-op.
        self.sync_changes(cx);
        self.sync_checkout_selection(cx);
        let projects = self.column_list(
            &chrome,
            home.as_deref(),
            cards_focused,
            reveal_selection,
            cx,
        );
        let (column_width, changes_width) = self.pane_widths(window);
        let sidebar = column_width.map(|width| {
            div()
                .w(width)
                .flex_shrink_0()
                .h_full()
                .overflow_hidden()
                .flex()
                .flex_col()
                .bg(chrome.column)
                .border_r_1()
                .border_color(chrome.hairline)
                .child(self.top_row(&chrome, window, cx))
                .child(projects)
                .child(self.footer(&chrome, width, cx))
        });
        let root = div().key_context("Shika");
        let mut root = Self::bind_drag_moves(Self::bind_actions(root, cx), cx)
            .relative()
            .size_full()
            .flex()
            .font_family(UI_FONT)
            .text_size(px(12.))
            .line_height(gpui::relative(1.3))
            .text_color(chrome.ink_1)
            // The cards' focus handle sits on an element with no size, so no
            // mouse-down can land on it: GPUI focuses a tracked element on any
            // click it did not prevent, which sent a stray click in the
            // column, the title bar, or the footer's gaps to the card list and
            // stopped the author's typing from reaching the terminal. It is
            // still inside the root, so key context, actions, and the capture
            // handler apply when the cards have focus.
            .child(div().absolute().size_0().track_focus(&self.focus))
            .children(backdrop)
            .children(sidebar)
            .child(self.terminal_side(&chrome, cards_focused, home.as_deref(), window, cx))
            .children(changes_width.map(|width| self.changes_panel(width, &chrome, window, cx)))
            .children(column_width.map(|width| self.column_handle(width, &chrome, cx)))
            .children(changes_width.map(|width| {
                let left = window.viewport_size().width - width;
                self.changes_handle(left, &chrome, cx)
            }));
        if let Some(card) = &self.update_notice
            && card.centered()
        {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.update_notice_panel(card, &chrome, window, cx)),
            );
        }
        if let Some(overlay) = &self.overlay {
            root = root.child(self.overlay_view(overlay, &chrome, home.as_deref(), window, cx));
        }
        root
    }
}
impl Shika {
    /// Every project whose saved Lead runs Claude Code gets its Lead chat.
    fn ensure_claude_lead_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chat_projects = self
            .projects
            .iter()
            .filter(|p| p.lead.as_ref().is_some_and(|lead| lead.cli == "claude"))
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();
        for project in chat_projects {
            self.ensure_lead_chat(&project, window, cx);
        }
    }
    /// The scrolling list under the column's top row: the Leads section,
    /// then the Projects header and each project's group of cards.
    fn column_list(
        &mut self,
        chrome: &Chrome,
        home: Option<&std::path::Path>,
        cards_focused: bool,
        reveal_selection: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut projects = div()
            .id("projects")
            .track_scroll(&self.sidebar_scroll)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(12.))
            .pt(px(8.))
            .pb(px(18.))
            .flex()
            .flex_col()
            .gap(px(20.));
        let layout = self.column_layout();
        projects = projects.child(self.leads_section(
            &layout,
            chrome,
            cards_focused,
            reveal_selection,
            cx,
        ));
        let order = layout
            .projects
            .iter()
            .map(|e| e.project.clone())
            .collect::<Vec<_>>();
        if order != self.project_order {
            self.project_order = order;
            self.project_order_epoch += 1;
        }
        let mut groups = div().flex_none().flex().flex_col().gap(px(20.));
        for entry in &layout.projects {
            let Some(project) = self.projects.iter().find(|p| p.id == entry.project) else {
                continue;
            };
            groups = groups.child(project_motion::project_motion(
                SharedString::from(format!("project-motion-{}", project.id)),
                self.project_group(
                    project,
                    entry.cards.clone(),
                    home,
                    chrome,
                    cards_focused,
                    reveal_selection,
                    cx,
                ),
                self.project_order_epoch,
                self.sidebar_scroll.clone(),
            ));
        }
        projects = projects.child(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(CARD_GAP))
                .child(self.projects_header(chrome, cx))
                .child(groups),
        );
        projects
    }
    /// The app actions the root view handles, plus the key capture that
    /// runs before any focused view sees a key.
    fn bind_actions(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        root.on_action(cx.listener(|this, _: &Quit, window, cx| this.request_quit(window, cx)))
            .on_action(cx.listener(|this, _: &NewAgent, window, cx| this.picker(window, cx)))
            .on_action(cx.listener(|this, _: &NewLead, window, cx| this.new_lead(window, cx)))
            .on_action(
                cx.listener(|this, _: &NewInCheckout, window, cx| this.new_in_checkout(window, cx)),
            )
            .on_action(cx.listener(|this, _: &AddProject, window, cx| {
                if !this.busy && this.overlay.is_none() {
                    this.add_project(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &BaseBranch, window, cx| {
                if let Some(project) = this.project_id() {
                    this.open_base(project, window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &NewTerminal, window, cx| this.new_shell(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &CloseTerminal, window, cx| {
                if let Some(i) = this.selected_card() {
                    this.close_tab(this.cards[i].active_tab, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &RenameTask, window, cx| this.rename_task(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTask, window, cx| {
                if matches!(this.overlay, Some(Overlay::CardMenu(_))) {
                    this.close_from_card_menu(window, cx);
                } else if matches!(this.overlay, Some(Overlay::RestingMenu(_))) {
                    this.forget_from_menu(window, cx);
                } else if !this.busy && this.overlay.is_none() {
                    this.close(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &NextTerminal, window, cx| this.cycle_tab(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousTerminal, window, cx| {
                this.cycle_tab(false, window, cx)
            }))
            .on_action(cx.listener(|this, select: &SelectTerminal, window, cx| {
                this.select_tab(select.0, window, cx);
            }))
            .on_action(cx.listener(|this, select: &SelectAgent, window, cx| {
                this.select_agent(select.0, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &NextAgent, window, cx| this.move_agent(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PreviousAgent, window, cx| this.move_agent(-1, window, cx)),
            )
            .capture_key_down(cx.listener(Self::key))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleColumn, _, cx| this.toggle_column(cx)))
            .on_action(
                cx.listener(|this, _: &ToggleChanges, window, cx| this.toggle_changes(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleUsage, window, cx| this.toggle_usage(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CreatePr, window, cx| this.create_pr(window, cx)))
            .on_action(
                cx.listener(|this, _: &SwitchAgent, window, cx| this.switch_agent(window, cx)),
            )
    }
    /// Drags that follow the pointer anywhere in the window: the column and
    /// Changes edges, and the background image's pan and zoom.
    fn bind_drag_moves(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        root.on_drag_move(cx.listener(
            |this, event: &gpui::DragMoveEvent<ColumnDrag>, window, cx| {
                this.drag_column(event.event.position.x, window, cx)
            },
        ))
        .on_drag_move(cx.listener(
            |this, event: &gpui::DragMoveEvent<changes::ChangesDrag>, window, cx| {
                this.drag_changes(event.event.position.x, window, cx)
            },
        ))
        .on_drag_move(cx.listener(
            |this, event: &gpui::DragMoveEvent<background_view::PanDrag>, _, cx| {
                this.drag_preview(event.event.position, cx)
            },
        ))
        .on_drag_move(cx.listener(
            |this, event: &gpui::DragMoveEvent<background_view::ZoomDrag>, _, cx| {
                this.drag_zoom(event.event.position.x, cx)
            },
        ))
    }
}
/// Bind the terminal, the fixed app shortcuts, and the user's overrides, then
/// refresh the menus so they show the chords that are actually bound.
fn install_keys(cx: &mut App, keys: &KeyOverrides) {
    cx.clear_key_bindings();
    shika_terminal::init(cx);
    let mut bindings = vec![
        gpui::KeyBinding::new("cmd-n", NewAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-l", NewLead, Some("Shika")),
        gpui::KeyBinding::new("cmd-o", AddProject, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-b", BaseBranch, Some("Shika")),
        gpui::KeyBinding::new("cmd-t", NewTerminal, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-p", CreatePr, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-a", SwitchAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-w", CloseTerminal, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-w", CloseTask, Some("Shika")),
        gpui::KeyBinding::new("cmd-shift-r", RenameTask, Some("Shika")),
        gpui::KeyBinding::new("ctrl-tab", NextTerminal, Some("Shika")),
        gpui::KeyBinding::new("ctrl-shift-tab", PreviousTerminal, Some("Shika")),
        gpui::KeyBinding::new("cmd-]", NextAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-[", PreviousAgent, Some("Shika")),
        gpui::KeyBinding::new("cmd-b", ToggleColumn, Some("Shika")),
        gpui::KeyBinding::new("cmd-q", Quit, None),
        gpui::KeyBinding::new("cmd-,", OpenSettings, None),
        gpui::KeyBinding::new("cmd-h", Hide, None),
        gpui::KeyBinding::new("cmd-alt-h", HideOthers, None),
    ];
    for binding in shortcuts::bindings(keys) {
        let Some(chord) = binding.chord.as_deref() else {
            continue;
        };
        if binding.id == "toggleChanges" {
            bindings.push(gpui::KeyBinding::new(chord, ToggleChanges, Some("Shika")));
        } else if binding.id == "newInCheckout" {
            bindings.push(gpui::KeyBinding::new(chord, NewInCheckout, Some("Shika")));
        } else if binding.id == "toggleUsage" {
            bindings.push(gpui::KeyBinding::new(chord, ToggleUsage, Some("Shika")));
        } else if let Some(index) = shortcuts::agent_slot(binding.id) {
            bindings.push(gpui::KeyBinding::new(
                chord,
                SelectAgent(index),
                Some("Shika"),
            ));
        } else if let Some(index) = shortcuts::tab_slot(binding.id) {
            bindings.push(gpui::KeyBinding::new(
                chord,
                SelectTerminal(index),
                Some("Shika"),
            ));
        }
    }
    cx.bind_keys(bindings);
    install_menus(cx);
}

fn install_menus(cx: &mut App) {
    let mut app_menu = Vec::new();
    if cx.has_global::<updates::Updater>() {
        app_menu.push(gpui::MenuItem::action(
            "Check for updates...",
            CheckForUpdates,
        ));
    }
    app_menu.extend([
        gpui::MenuItem::action("Settings...", OpenSettings),
        gpui::MenuItem::separator(),
        gpui::MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
        gpui::MenuItem::separator(),
        gpui::MenuItem::action("Hide Shika", Hide),
        gpui::MenuItem::action("Hide others", HideOthers),
        gpui::MenuItem::action("Show all", ShowAll),
        gpui::MenuItem::separator(),
        gpui::MenuItem::action("Quit Shika", Quit),
    ]);
    cx.set_menus([
        gpui::Menu::new("Shika").items(app_menu),
        gpui::Menu::new("Agent").items([
            gpui::MenuItem::action("Add project…", AddProject),
            gpui::MenuItem::action("New agent", NewAgent),
            gpui::MenuItem::action("New Agent in Checkout", NewInCheckout),
            gpui::MenuItem::action("New Lead…", NewLead),
            gpui::MenuItem::action("New terminal tab", NewTerminal),
            gpui::MenuItem::action("Create PR", CreatePr),
            gpui::MenuItem::action("Switch agent…", SwitchAgent),
            gpui::MenuItem::action("Base branch…", BaseBranch),
            gpui::MenuItem::action("Close terminal tab", CloseTerminal),
            gpui::MenuItem::action("Rename task…", RenameTask),
            gpui::MenuItem::action("Close task", CloseTask),
            gpui::MenuItem::action("Next terminal tab", NextTerminal),
            gpui::MenuItem::action("Previous terminal tab", PreviousTerminal),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Next agent", NextAgent),
            gpui::MenuItem::action("Previous agent", PreviousAgent),
        ]),
        gpui::Menu::new("View").items([
            gpui::MenuItem::action("Hide or show agent column", ToggleColumn),
            gpui::MenuItem::action("Hide or show changes", ToggleChanges),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Usage", ToggleUsage),
        ]),
    ]);
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--shika-statusline") {
        std::process::exit(shika_core::run_quota_statusline(&args[1..]));
    }
    run_client_if_invoked();
    let StartupOptions {
        data,
        diagnostics,
        preview_update,
    } = StartupOptions::parse()?;
    let core = Arc::new(Core::open(data.unwrap_or(shika_core::app_data_dir()?))?);
    core.start_subscription_limits(std::env::current_exe()?);
    core.start_usage_collection();
    gpui_platform::application().run(move |cx: &mut App| {
        add_bundled_fonts(cx);
        install_app_handlers(cx, &core);
        let bounds = main_window_bounds(preview_update, cx);
        let settings = core.settings();
        let start = settings.as_ref().map(|s| s.appearance).unwrap_or_default();
        let start_image = settings.as_ref().is_ok_and(|s| s.background.is_some());
        // Before the window opens, so it starts in the forced appearance.
        appearance::apply_mode(settings.as_ref().map(|s| s.theme.mode).unwrap_or_default());
        let handle = cx
            .open_window(
                main_window_options(bounds, preview_update, &start, start_image),
                |window, cx| {
                    let app = cx.new(|cx| {
                        Shika::new(core, settings, diagnostics, preview_update, window, cx)
                    });
                    window.focus(&app.focus_handle(cx), cx);
                    app
                },
            )
            .expect("Open Shika window");
        wire_main_window(handle, &start, start_image, cx);
        cx.activate(true);
    });
    Ok(())
}
