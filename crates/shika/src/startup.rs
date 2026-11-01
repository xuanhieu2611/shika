//! Startup: command-line options, the saved state read at launch, the CLI
//! catalog, the bundled fonts, app-wide handlers, and the main window.
use super::*;

/// What the app saved, as read at startup. A read that fails falls back to
/// empty or the defaults, and its error goes to the startup toast.
pub(crate) struct Saved {
    pub(crate) projects: Vec<Project>,
    pub(crate) selection: Option<Selection>,
    pub(crate) leftovers: Vec<JournalEntry>,
    pub(crate) settings: Settings,
    pub(crate) load_errors: Vec<String>,
}

impl Saved {
    pub(crate) fn load(core: &Core, settings: shika_core::Result<Settings>) -> Self {
        let mut load_errors = Vec::new();
        let projects = core.projects().unwrap_or_else(|e| {
            load_errors.push(e.to_string());
            Vec::new()
        });
        let selection = projects.first().map(|p| Selection::Project(p.id.clone()));
        let leftovers = core.leftovers_list().unwrap_or_else(|e| {
            load_errors.push(e.to_string());
            Vec::new()
        });
        let settings = settings.unwrap_or_else(|e| {
            load_errors.push(e.to_string());
            Settings::default()
        });
        Self {
            projects,
            selection,
            leftovers,
            settings,
            load_errors,
        }
    }
}

/// The CLI catalog from the login-shell PATH. With `--diagnostics-file`, it
/// also writes where each binary was found.
pub(crate) fn read_cli_catalog(core: &Core, diagnostics: Option<PathBuf>) -> CliCatalog {
    let c = core.cli_catalog();
    if let Some(path) = diagnostics {
        let text = c
            .presets
            .iter()
            .map(|p| {
                format!(
                    "{}={}\n",
                    p.binary,
                    p.path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or("not found".into())
                )
            })
            .collect::<String>();
        let _ = std::fs::write(path, text);
    }
    c
}

/// `shika help`, `shika new`, and the rest run in a Lead's terminal as this
/// same binary. They only talk to the socket and exit here: no GPUI,
/// settings, or data.
pub(crate) fn run_client_if_invoked() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Inside a Lead terminal (either variable set) this is always the client,
    // so a bare or mistyped `shika` never starts a second app on real data.
    let lead_env =
        std::env::var_os("SHIKA_SOCKET").is_some() || std::env::var_os("SHIKA_TOKEN").is_some();
    if shika_core::control::is_client_invocation(&args, lead_env) {
        std::process::exit(control_client::run(&args));
    }
}

/// The app's own command-line options.
pub(crate) struct StartupOptions {
    pub(crate) data: Option<PathBuf>,
    pub(crate) diagnostics: Option<PathBuf>,
    pub(crate) preview_update: bool,
}

impl StartupOptions {
    pub(crate) fn parse() -> anyhow::Result<Self> {
        let mut data = None;
        let mut diagnostics = None;
        let mut preview_update = false;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--preview-update" => preview_update = true,
                "--data-dir" => {
                    data = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| anyhow::anyhow!("--data-dir needs a path"))?,
                    ))
                }
                "--diagnostics-file" => {
                    diagnostics =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            anyhow::anyhow!("--diagnostics-file needs a path")
                        })?))
                }
                other => anyhow::bail!("Unknown option {other}"),
            }
        }
        Ok(Self {
            data,
            diagnostics,
            preview_update,
        })
    }
}

/// JetBrains Mono, the terminal font, from the bundled files.
pub(crate) fn add_bundled_fonts(cx: &mut App) {
    let fonts = [
        include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/JetBrainsMono-Italic.ttf").as_slice(),
        include_bytes!("../../../assets/fonts/JetBrainsMono-BoldItalic.ttf").as_slice(),
    ]
    .into_iter()
    .map(std::borrow::Cow::Borrowed)
    .collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        eprintln!("JetBrains Mono: {error}");
    }
}

/// App-wide handlers: stopping setup and Lead chats on quit, the Hide
/// actions, the updater, the default key bindings, and quitting when the
/// last window closes.
pub(crate) fn install_app_handlers(cx: &mut App, core: &Arc<Core>) {
    let quitting_core = core.clone();
    cx.on_app_quit(move |_| {
        quitting_core.cancel_preparations();
        quitting_core.stop_subscription_limits();
        quitting_core.stop_lead_chats();
        async {}
    })
    .detach();
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    // Only a release bundle has Sparkle, so only it offers the check.
    if let Some(updater) = updates::Updater::start() {
        cx.set_global(updater);
        cx.on_action(|_: &CheckForUpdates, cx| cx.global::<updates::Updater>().check());
    }
    install_keys(cx, &KeyOverrides::default());
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}

pub(crate) fn main_window_bounds(preview_update: bool, cx: &App) -> Bounds<Pixels> {
    let mut bounds = Bounds::centered(None, size(px(1400.), px(880.)), cx);
    // The sample sits beside a normal window instead of covering it.
    if preview_update {
        bounds.origin.x += px(48.);
        bounds.origin.y += px(36.);
    }
    bounds
}

pub(crate) fn main_window_options(
    bounds: Bounds<Pixels>,
    preview_update: bool,
    start: &Appearance,
    start_image: bool,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(if preview_update {
                "Shika preview".into()
            } else {
                "Shika".into()
            }),
            appears_transparent: true,
            // Centers the traffic lights in the 48px top row.
            traffic_light_position: Some(gpui::point(px(TRAFFIC_LIGHTS.0), px(TRAFFIC_LIGHTS.1))),
        }),
        // The column at 30% (320px here) plus a terminal wide enough to read.
        window_min_size: Some(size(px(960.), px(600.))),
        // The column toggle lives in the title bar, so clicks
        // there have to reach the app. The bar starts the drag itself.
        app_owns_titlebar_drag: true,
        window_background: appearance::background(start, start_image),
        ..Default::default()
    }
}

/// Routes Quit and the window's close button to the app's Quit dialog,
/// applies the blur once the window is on screen, and closes the control
/// socket on quit.
pub(crate) fn wire_main_window(
    handle: gpui::WindowHandle<Shika>,
    start: &Appearance,
    start_image: bool,
    cx: &mut App,
) {
    cx.on_action(move |_: &Quit, cx| {
        // A native menu action may arrive while GPUI has the window out
        // of its store for dispatch. Reenter it only after dispatch ends.
        cx.defer(move |cx| {
            let _ = handle.update(cx, |app, window, cx| app.request_quit(window, cx));
        });
    });
    let _ = handle.update(cx, |_, window, cx| {
        let app = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = app.update(cx, |app, cx| app.request_quit(window, cx));
            false
        });
    });
    // The blur radius needs the window on screen, which it is now.
    let _ = handle.update(cx, |_, window, _| {
        appearance::apply(start, start_image, window)
    });
    // Closes the control socket and removes its directory on quit; the
    // window's state may never be dropped.
    cx.on_app_quit(move |cx| {
        let _ = handle.update(cx, |app, _, _| app.control = None);
        async {}
    })
    .detach();
}
