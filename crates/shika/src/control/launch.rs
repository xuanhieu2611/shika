//! Starting CLIs for the Lead: the prompts a Lead and its workers start
//! with, and `shika new`, which starts a worker card.
use super::{Responder, failure_reply, refused};
use crate::{HostState, Launch, Shika, lock};
use gpui::{Context, Window};
use shika_core::control::Reply;
use shika_core::{LaunchOptions, WorkerEnv};
use std::path::Path;
use std::time::Instant;

/// Appended to the prompt of every worker a Lead starts with `shika new`,
/// after a blank line. Not added by `send`.
pub const WORKER_FOOTER: &str = "When you finish or get blocked, run: {command} report \"<what you changed, the tests you ran and their result, your commit>\". Wait for it to print reported. If reporting fails, retry after correcting the error, or include the exact failure in your final response. Do not push.";

/// Shell-quoted absolute executable: a provider's login shell may reset PATH.
/// Never put credentials in a prompt.
pub fn command_path(bin_dir: &Path) -> String {
    format!(
        "'{}'",
        bin_dir
            .join("shika")
            .to_string_lossy()
            .replace('\'', "'\"'\"'")
    )
}

pub fn lead_prompt(project: &str, bin_dir: &Path) -> String {
    format!(
        "You are the Lead agent for the \"{project}\" project in Shika. Run {0} help and {0} memory, follow them, then ask the author what they want done.",
        command_path(bin_dir)
    )
}

pub fn lead_guide(bin_dir: &Path) -> String {
    format!(
        "Shika command for this session: {}. In every shell command below, replace `shika` with this absolute executable path. Do not rely on PATH: a CLI's login shell can reset it.\n\n{LEAD_GUIDE}",
        command_path(bin_dir)
    )
}

/// The prompt a Lead-started worker receives: the Lead's text, then the
/// report instruction. The first line, which names the card and branch, is
/// still the Lead's.
pub fn worker_prompt(prompt: &str, bin_dir: &Path) -> String {
    let footer = WORKER_FOOTER.replace("{command}", &command_path(bin_dir));
    format!("{}\n\n{footer}", prompt.trim_end())
}

/// What `shika help` prints.
pub(super) const LEAD_GUIDE: &str = include_str!("../lead_guide.md");

/// Live workers one Lead may have at a time.
pub const MAX_WORKERS: usize = 4;

impl Shika {
    /// Why this Lead may not start another worker with `cli` now.
    fn new_refusal(&self, lead_id: &str, cli: &str) -> Option<Reply> {
        let Some(catalog) = &self.catalog else {
            return Some(refused(
                "Shika is still looking for installed CLIs. Try again.",
            ));
        };
        let Some(preset) = catalog.presets.iter().find(|preset| preset.id == cli) else {
            return Some(refused(format!(
                "Unknown CLI {cli}. Use one of {}.",
                shika_core::control::CLI_IDS.join(", ")
            )));
        };
        if !preset.found() {
            return Some(refused(format!(
                "{} is not installed (no `{}` on PATH). Pick another CLI.",
                preset.name, preset.binary
            )));
        }
        let live = self
            .cards
            .iter()
            .filter(|card| card.is_live_worker_of(lead_id))
            .count();
        (live >= MAX_WORKERS).then(|| {
            refused(format!(
                "This Lead already has {MAX_WORKERS} live workers. Wait for them, and ask the author to close finished tasks before starting more."
            ))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn control_new(
        &mut self,
        lead: usize,
        cli: String,
        base: Option<String>,
        prompt: String,
        reply: Responder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(lead_id) = self.cards[lead].session.as_ref().map(|s| s.id.clone()) else {
            return;
        };
        if let Some(refusal) = self.new_refusal(&lead_id, &cli) {
            let _ = reply.send(refusal);
            return;
        }
        let Some(preset) = self
            .catalog
            .as_ref()
            .and_then(|catalog| catalog.presets.iter().find(|preset| preset.id == cli))
            .cloned()
        else {
            return;
        };
        let project = self.cards[lead].project.clone();
        let core = self.core.clone();
        let lookup = project.clone();
        cx.spawn_in(window, async move |this, cx| {
            // Same approval check as New. The Lead can never approve setup.
            let checked = cx
                .background_executor()
                .spawn(async move {
                    let config = core.project_preparation(&lookup)?;
                    let approved = match &config {
                        Some(config) => core.preparation_approved(&lookup, config)?,
                        None => true,
                    };
                    Ok::<_, shika_core::Error>((config, approved))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The Lead may have closed, or parallel calls may have filled
                // its slots, while the check ran.
                if !this.cards.iter().any(|card| {
                    card.lead.is_some() && card.session.as_ref().is_some_and(|s| s.id == lead_id)
                }) {
                    let _ = reply.send(refused("This Lead was closed."));
                    return;
                }
                if let Some(refusal) = this.new_refusal(&lead_id, &cli) {
                    let _ = reply.send(refusal);
                    return;
                }
                match checked {
                    Ok((Some(_), false)) => {
                        let _ = reply.send(refused(
                            "This project's setup needs the author's approval in Shika first: start one task with Cmd+N and approve it.",
                        ));
                    }
                    Ok((config, _)) => {
                        let Some(server) = &this.control else {
                            let _ = reply.send(refused("Shika's control socket is not running."));
                            return;
                        };
                        let options = LaunchOptions {
                            prompt: Some(worker_prompt(&prompt, &server.bin_dir())),
                            started_by: Some(lead_id),
                            base,
                            control: Some(WorkerEnv {
                                socket: server.socket(),
                                token: shika_core::control::new_token(),
                                bin_dir: server.bin_dir(),
                            }),
                        };
                        this.begin_launch(
                            project,
                            preset,
                            config.is_some(),
                            None,
                            Launch::Worker(options),
                            Some(reply),
                            window,
                            cx,
                        );
                    }
                    Err(error) => {
                        let _ = reply.send(failure_reply(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The reply to `shika new` once the card at `index` has its session.
    pub(crate) fn started_reply(&self, index: usize) -> Reply {
        let card = &self.cards[index];
        match card
            .started_by
            .as_deref()
            .and_then(|lead_id| card.task_info(lead_id))
        {
            Some(mut task) => {
                // The launch prompt names the card on the next tick; say
                // that name now rather than the `New <CLI>` placeholder.
                if let Some(title) = &lock(&card.agent.state).title {
                    task.title = crate::model::card_title(title);
                }
                Reply::Started { task }
            }
            None => Reply::Error {
                message: "The task started but has no session.".into(),
            },
        }
    }
}

/// Start and stop of the Lead's own launch prompt, in one place for the host.
impl HostState {
    /// The CLI was launched with `prompt`. Counts as the first submitted
    /// line, through the fields a typed submission sets, so the existing
    /// turn machinery (candidate turn, timer, once-per-turn notification,
    /// lifecycle fencing, branch naming from the first line) needs no second
    /// path. `name` is false for the Lead, which keeps its title.
    pub(crate) fn seed_launch_prompt(&mut self, prompt: &str, name: bool, now: Instant) {
        if name {
            self.title = crate::model::prompt_title(prompt);
        }
        // A line typed later must not name the task or rename its branch.
        self.prompt.finish();
        self.submission += 1;
        self.last_submission = Some(now);
        self.launch_turn = true;
    }
}
