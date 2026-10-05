//! Two live terminals in one window, one shown at a time. Both keep
//! reading their PTY while hidden.
//!
//! ```sh
//! cargo run -p shika-terminal --example demo -- --cwd ~/code/repo -- claude
//! ```
//!
//! Terminal 1 runs `$SHELL -l`. Terminal 2 runs the command after `--`
//! (another login shell when there is none).
//!
//! Keys: cmd-1 shell, cmd-2 command, cmd-g toggle, cmd-= / cmd-- font size,
//! cmd-q quit.
//!
//! Options:
//!   --cwd DIR        working directory for both (default: current)
//!   --type TEXT      type TEXT and Enter into the shell once it starts
//!   --show N         which terminal to show first, 1 or 2
//!   --font-size PX   font size (default 14)
//!   --stats          print paint counters once a second

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Window, WindowBounds,
    WindowOptions, actions, div, prelude::FluentBuilder, px, rgb, size,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use shika_terminal::{
    InputSource, Palette, PtyHost, PtyWriter, Terminal, TerminalConfig, TerminalEvent,
    TerminalOptions, TerminalSize, TerminalView,
};

actions!(
    demo,
    [ShowShell, ShowCommand, Toggle, Bigger, Smaller, Quit]
);

struct Args {
    cwd: PathBuf,
    command: Vec<String>,
    type_into_shell: Option<String>,
    show: usize,
    font_size: f32,
    stats: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        command: Vec::new(),
        type_into_shell: None,
        show: 0,
        font_size: 14.,
        stats: false,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--cwd" => args.cwd = PathBuf::from(iter.next().expect("--cwd needs a directory")),
            "--type" => args.type_into_shell = iter.next(),
            "--show" => {
                args.show = match iter.next().as_deref() {
                    Some("2") => 1,
                    _ => 0,
                }
            }
            "--font-size" => {
                args.font_size = iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--font-size needs a number")
            }
            "--stats" => args.stats = true,
            "--" => {
                args.command = iter.by_ref().collect();
            }
            other => {
                eprintln!("unknown argument {other:?}");
                std::process::exit(2);
            }
        }
    }
    args
}

/// The PTY side the terminal talks to: a writer thread for input, and the
/// master for resizes.
struct Pty {
    writer: PtyWriter,
    master: Mutex<Box<dyn MasterPty + Send>>,
}

impl PtyHost for Pty {
    fn write(&self, bytes: &[u8], _: InputSource) {
        self.writer.write(bytes);
    }

    fn resize(&self, size: TerminalSize) {
        let master = self.master.lock().unwrap_or_else(|err| err.into_inner());
        let _ = master.resize(PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.cols * size.cell_width,
            pixel_height: size.rows * size.cell_height,
        });
    }
}

/// Spawn `argv` in a PTY and connect it to a new terminal.
fn spawn(argv: &[String], cwd: &PathBuf) -> anyhow::Result<Terminal> {
    let size = TerminalSize::default();
    let pair = native_pty_system().openpty(PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("PWD", cwd);
    if cmd.get_env("LANG").is_none() {
        cmd.env("LANG", "en_US.UTF-8");
    }
    // Inherited git variables would point git at another checkout.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        cmd.env_remove(key);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let reader = pair.master.try_clone_reader()?;
    let writer = PtyWriter::spawn(pair.master.take_writer()?)?;
    let terminal = Terminal::new(
        TerminalOptions {
            size,
            ..TerminalOptions::default()
        },
        Pty {
            writer,
            master: Mutex::new(pair.master),
        },
    );
    let on_end = terminal.clone();
    terminal.spawn_reader(reader, move || {
        let code = child.wait().map(|status| status.exit_code()).unwrap_or(1);
        on_end.feed(format!("\r\n[process exited with code {code}]\r\n").as_bytes());
    })?;
    Ok(terminal)
}

struct Demo {
    views: [Entity<TerminalView>; 2],
    labels: [SharedString; 2],
    titles: [Option<String>; 2],
    active: usize,
    cwd: SharedString,
    focus_handle: FocusHandle,
}

impl Demo {
    fn show(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index;
        let handle = self.views[index].focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        for view in &self.views {
            view.update(cx, |view, cx| {
                let mut config = view.config().clone();
                config.font_size = px((f32::from(config.font_size) + delta).clamp(8.0, 32.0));
                view.set_config(config, cx);
            });
        }
    }
}

impl Focusable for Demo {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = |index: usize, this: &Self| {
            let active = index == this.active;
            let label = match &this.titles[index] {
                Some(title) => format!("{}  {}  {}", index + 1, this.labels[index], title),
                None => format!("{}  {}", index + 1, this.labels[index]),
            };
            div()
                .px_2()
                .py_0p5()
                .rounded_sm()
                .when(active, |d| d.bg(rgb(0x353932)).text_color(rgb(0xF2F5EC)))
                .when(!active, |d| d.text_color(rgb(0x878C80)))
                .child(label)
        };
        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x131512))
            .font_family("Menlo")
            .text_size(px(11.5))
            .on_action(cx.listener(|this, _: &ShowShell, window, cx| this.show(0, window, cx)))
            .on_action(cx.listener(|this, _: &ShowCommand, window, cx| this.show(1, window, cx)))
            .on_action(cx.listener(|this, _: &Toggle, window, cx| {
                let next = 1 - this.active;
                this.show(next, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Bigger, _, cx| this.zoom(1.0, cx)))
            .on_action(cx.listener(|this, _: &Smaller, _, cx| this.zoom(-1.0, cx)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .bg(rgb(0x181A17))
                    .border_b_1()
                    .border_color(rgb(0x262924))
                    .child(tab(0, self))
                    .child(tab(1, self))
                    .child(div().flex_1())
                    .child(div().text_color(rgb(0x757A6E)).child(self.cwd.clone()))
                    .child(div().text_color(rgb(0x5A5F55)).child("cmd-g switch")),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.views[self.active].clone()),
            )
    }
}

fn main() -> anyhow::Result<()> {
    let args = parse_args();
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let shell_argv = vec![shell.clone(), "-l".to_string()];
    let command_argv = if args.command.is_empty() {
        shell_argv.clone()
    } else {
        args.command.clone()
    };

    let shell_terminal = spawn(&shell_argv, &args.cwd)?;
    let command_terminal = spawn(&command_argv, &args.cwd)?;
    if let Some(text) = &args.type_into_shell {
        // Give the login shell a moment to print its prompt first.
        let terminal = shell_terminal.clone();
        let line = format!("{text}\r");
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            terminal.write(line.as_bytes());
        });
    }

    let labels: [SharedString; 2] = [
        format!("shell ({})", shell.rsplit('/').next().unwrap_or("sh")).into(),
        command_argv.join(" ").into(),
    ];
    let cwd: SharedString = args.cwd.display().to_string().into();
    let font_size = args.font_size;
    let show = args.show;
    let stats = args.stats;

    gpui_platform::application().run(move |cx: &mut App| {
        shika_terminal::init(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-1", ShowShell, None),
            KeyBinding::new("cmd-2", ShowCommand, None),
            KeyBinding::new("cmd-g", Toggle, None),
            KeyBinding::new("cmd-=", Bigger, None),
            KeyBinding::new("cmd--", Smaller, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Shika terminal demo".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let config = TerminalConfig {
                        font_size: px(font_size),
                        ..TerminalConfig::default()
                    };
                    let make = |terminal: Terminal, window: &mut Window, cx: &mut App| {
                        cx.new(|cx| {
                            TerminalView::new(
                                terminal,
                                config.clone(),
                                Palette::shika(),
                                window,
                                cx,
                            )
                        })
                    };
                    let views = [
                        make(shell_terminal.clone(), window, cx),
                        make(command_terminal.clone(), window, cx),
                    ];
                    let demo = cx.new(|cx| Demo {
                        views: views.clone(),
                        labels: labels.clone(),
                        titles: [None, None],
                        active: show,
                        cwd: cwd.clone(),
                        focus_handle: cx.focus_handle(),
                    });
                    for (index, view) in views.iter().enumerate() {
                        let demo = demo.clone();
                        cx.subscribe(view, move |_, event: &TerminalEvent, cx| match event {
                            TerminalEvent::TitleChanged(title) => {
                                demo.update(cx, |demo, cx| {
                                    demo.titles[index] = title.clone();
                                    cx.notify();
                                });
                            }
                            TerminalEvent::Bell => {}
                        })
                        .detach();
                    }
                    let handle = views[show].focus_handle(cx);
                    window.focus(&handle, cx);
                    demo
                },
            )
            .expect("open window");
        cx.activate(true);

        if stats {
            cx.spawn(async move |cx| {
                let mut last = [shika_terminal::FrameStats::default(); 2];
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let ok = window.update(cx, |demo, _, cx| {
                        let mut line = String::new();
                        for (index, view) in demo.views.iter().enumerate() {
                            let view = view.read(cx);
                            let now = view.stats();
                            line.push_str(&format!(
                                "t{}: {} frames {} snapshots ({}) | ",
                                index + 1,
                                now.frames - last[index].frames,
                                now.snapshots - last[index].snapshots,
                                view.font_family().as_deref().unwrap_or("-"),
                            ));
                            last[index] = now;
                        }
                        println!("{line}");
                        let _ = std::io::stdout().flush();
                    });
                    if ok.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
    });
    Ok(())
}
