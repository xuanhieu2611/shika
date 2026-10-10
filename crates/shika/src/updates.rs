//! Software updates through Sparkle. Only the bundle from
//! `scripts/release-app.sh` embeds Sparkle.framework, so `cargo run` and local
//! bundles start no updater and show no menu item, and never replace themselves
//! with a release.
//!
//! Sparkle fetches the signed feed, verifies the update, and installs it on
//! quit. Shika draws the corner notice itself. Sparkle's own windows are not
//! used. See docs/updates.md.

/// What the corner notice is asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateCard {
    /// Second launch, before automatic checks are allowed.
    Permission,
    /// The menu item started a check.
    Checking,
    /// A version is available and has not been downloaded.
    Available {
        version: String,
        body: String,
        offer: Option<Offer>,
    },
    /// The download is in progress. Cancel is offered while Sparkle allows it.
    Downloading {
        version: String,
        percent: Option<u8>,
        can_cancel: bool,
    },
    /// The download finished and Sparkle is extracting it.
    Preparing {
        version: String,
        percent: Option<u8>,
    },
    /// The update is on disk. Restart installs it now. Later installs it on quit.
    Ready { version: String },
    /// Sparkle is quitting Shika to finish the install.
    Installing { version: String, can_retry: bool },
    /// The check or the install failed. Close acknowledges it.
    Failed { message: String },
}

/// The primary action on an available update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    Download,
    Open(String),
}

/// A click on the notice. It does not take focus, so these are not key bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardAction {
    Primary,
    Secondary,
}

/// Words and buttons for one notice. Buttons have no key caps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateNotice {
    pub title: String,
    pub body: String,
    pub secondary: Option<&'static str>,
    pub primary: Option<&'static str>,
}

/// What a click means to Sparkle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    AllowChecks(bool),
    Install,
    Dismiss,
    Cancel,
    Retry,
    Acknowledge,
}

/// Something the notice should do. Applied on the UI thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateEvent {
    Show(UpdateCard),
    Clear,
    Toast(String),
}

impl UpdateCard {
    /// Ready to install, or already quitting to finish it. Those sit in the
    /// center. The rest stay in the corner.
    pub fn centered(&self) -> bool {
        matches!(self, Self::Ready { .. } | Self::Installing { .. })
    }

    pub fn view(&self) -> UpdateNotice {
        match self {
            Self::Permission => UpdateNotice {
                title: "Check for updates automatically?".into(),
                body: "Shika looks once a day. A new version waits here until you download it."
                    .into(),
                secondary: Some("Not now"),
                primary: Some("Check automatically"),
            },
            Self::Checking => UpdateNotice {
                title: "Checking for updates...".into(),
                body: "Looking for a newer version.".into(),
                secondary: Some("Cancel"),
                primary: None,
            },
            Self::Available {
                version,
                body,
                offer,
            } => UpdateNotice {
                title: available_title(version),
                body: body.clone(),
                secondary: Some("Ignore"),
                primary: match offer {
                    Some(Offer::Download) => Some("Download"),
                    Some(Offer::Open(_)) => Some("Open"),
                    None => None,
                },
            },
            Self::Downloading {
                version,
                percent,
                can_cancel,
            } => UpdateNotice {
                title: version_title("Downloading", version, "Downloading the update"),
                body: percent_line(*percent, "downloaded", "Downloading."),
                secondary: can_cancel.then_some("Cancel"),
                primary: None,
            },
            Self::Preparing { version, percent } => UpdateNotice {
                title: version_title("Preparing", version, "Preparing the update"),
                body: percent_line(*percent, "prepared", "Preparing the update."),
                secondary: None,
                primary: None,
            },
            Self::Ready { version } => UpdateNotice {
                title: "Restart to update".into(),
                body: if version.is_empty() {
                    "The update is ready. Later installs it the next time Shika quits.".into()
                } else {
                    format!(
                        "Shika {version} is ready. Later installs it the next time Shika quits."
                    )
                },
                secondary: Some("Later"),
                primary: Some("Restart"),
            },
            Self::Installing { version, can_retry } => UpdateNotice {
                title: "Installing the update".into(),
                body: if *can_retry {
                    version_sentence(
                        version,
                        "Shika needs to quit before the update can finish.",
                        |version| {
                            format!("Shika {version} needs to quit before the update can finish.")
                        },
                    )
                } else {
                    version_sentence(version, "Shika will quit and reopen.", |version| {
                        format!("Shika {version} will quit and reopen.")
                    })
                },
                secondary: None,
                primary: can_retry.then_some("Quit and install"),
            },
            Self::Failed { message } => UpdateNotice {
                title: "Could not update".into(),
                body: message.clone(),
                secondary: Some("Close"),
                primary: None,
            },
        }
    }
}

/// The click Sparkle should hear, when this notice offers that button.
pub fn decision(card: &UpdateCard, action: CardAction) -> Option<Decision> {
    match (card, action) {
        (UpdateCard::Permission, CardAction::Primary) => Some(Decision::AllowChecks(true)),
        (UpdateCard::Permission, CardAction::Secondary) => Some(Decision::AllowChecks(false)),
        (UpdateCard::Checking, CardAction::Secondary) => Some(Decision::Cancel),
        (
            UpdateCard::Available {
                offer: Some(Offer::Download),
                ..
            },
            CardAction::Primary,
        ) => Some(Decision::Install),
        (
            UpdateCard::Available {
                offer: Some(Offer::Open(_)),
                ..
            },
            CardAction::Primary,
        ) => Some(Decision::Dismiss),
        (UpdateCard::Available { .. }, CardAction::Secondary) => Some(Decision::Dismiss),
        (
            UpdateCard::Downloading {
                can_cancel: true, ..
            },
            CardAction::Secondary,
        ) => Some(Decision::Cancel),
        (UpdateCard::Ready { .. }, CardAction::Primary) => Some(Decision::Install),
        (UpdateCard::Ready { .. }, CardAction::Secondary) => Some(Decision::Dismiss),
        (
            UpdateCard::Installing {
                can_retry: true, ..
            },
            CardAction::Primary,
        ) => Some(Decision::Retry),
        (UpdateCard::Failed { .. }, CardAction::Secondary) => Some(Decision::Acknowledge),
        _ => None,
    }
}

pub fn available_title(version: &str) -> String {
    if version.is_empty() {
        "A new version is available".into()
    } else {
        format!("Shika {version} is available")
    }
}

pub fn available_body(notes: &str, critical: bool, offer: Option<&Offer>) -> String {
    let fallback = match offer {
        Some(Offer::Open(_)) => "Open it in the browser, or ignore it until the next check.",
        Some(Offer::Download) => {
            "Download it when you want. Ignore leaves it until the next check."
        }
        None => "Ignore leaves it until the next check.",
    };
    let body = note_excerpt(notes).unwrap_or_else(|| fallback.to_string());
    if critical {
        format!("This update is critical. {body}")
    } else {
        body
    }
}

/// The first readable line of embedded release notes, capped so the corner
/// card stays one sentence. Markup is dropped.
pub fn note_excerpt(notes: &str) -> Option<String> {
    let plain = strip_tags(notes);
    let line = plain.lines().map(str::trim).find(|line| !line.is_empty())?;
    let line = line
        .trim_start_matches('#')
        .trim()
        .trim_start_matches(['*', '-', '>'])
        .trim()
        .replace('`', "");
    let text = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        None
    } else {
        Some(limit_chars(&text, 140))
    }
}

/// A display version safe to put in a title. Empty and control characters go.
pub fn version_name(raw: &str) -> Option<String> {
    let text: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    if text.is_empty() { None } else { Some(text) }
}

/// An http(s) link from an information-only update. Anything else stays closed.
pub fn browser_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        Some(url.to_string())
    } else {
        None
    }
}

pub fn download_percent(received: u64, expected: u64) -> Option<u8> {
    received
        .saturating_mul(100)
        .checked_div(expected)
        .map(|percent| percent.min(100) as u8)
}

pub fn extraction_percent(progress: f64) -> Option<u8> {
    if !progress.is_finite() || progress < 0.0 {
        None
    } else {
        Some((progress * 100.0).round().clamp(0.0, 100.0) as u8)
    }
}

/// Sparkle's no-update reason, as `SPUNoUpdateFoundReason`'s integer value.
pub fn no_update_message(reason: isize) -> &'static str {
    match reason {
        1 | 2 => "No newer version found.",
        3 => "This Mac is too old for the latest version.",
        4 => "This Mac is too new for the latest version.",
        5 => "This Mac cannot run the latest version.",
        _ => "No update found.",
    }
}

pub fn plain_error(text: &str) -> String {
    let text = text.replace(['\u{2014}', '\u{2013}'], "-").replace('!', "");
    limit_chars(&text.split_whitespace().collect::<Vec<_>>().join(" "), 280)
}

fn version_title(verb: &str, version: &str, fallback: &str) -> String {
    if version.is_empty() {
        fallback.into()
    } else {
        format!("{verb} Shika {version}")
    }
}

fn version_sentence(version: &str, fallback: &str, with: impl Fn(&str) -> String) -> String {
    if version.is_empty() {
        fallback.into()
    } else {
        with(version)
    }
}

fn percent_line(percent: Option<u8>, word: &str, fallback: &str) -> String {
    match percent {
        Some(percent) => format!("{percent}% {word}."),
        None => fallback.into(),
    }
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn limit_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{cut}...")
}

/// Sparkle's updater, kept for the life of the app. Background checks and the
/// menu item both come through here. The corner notice is drawn by the app.
pub struct Updater {
    #[cfg(target_os = "macos")]
    inner: native::Inner,
}

impl gpui::Global for Updater {}

impl Updater {
    /// Load the embedded framework and start the updater. `None` when this build
    /// has no Sparkle, or it fails to load. Call on the main thread.
    pub fn start() -> Option<Self> {
        #[cfg(target_os = "macos")]
        return native::start().map(|inner| Self { inner });
        #[cfg(not(target_os = "macos"))]
        None
    }

    /// A check the user asked for from the menu.
    pub fn check(&self) {
        #[cfg(target_os = "macos")]
        self.inner.check();
    }

    /// Notices posted since the last poll. Sparkle calls the driver on the main
    /// thread, and the window reads these on that same thread.
    pub fn poll(&self) -> Vec<UpdateEvent> {
        #[cfg(target_os = "macos")]
        return self.inner.poll();
        #[cfg(not(target_os = "macos"))]
        Vec::new()
    }

    /// The notice button the user clicked.
    pub fn respond(&self, action: CardAction) {
        #[cfg(target_os = "macos")]
        self.inner.respond(action);
        #[cfg(not(target_os = "macos"))]
        let _ = action;
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::cell::RefCell;
    use std::sync::mpsc::{self, Receiver, Sender};

    use block2::{Block, RcBlock};
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{AnyThread, DefinedClass, define_class, msg_send};
    use objc2_foundation::{
        NSBundle, NSError, NSNumber, NSObject, NSObjectProtocol, NSString, NSURL,
    };

    use super::{
        CardAction, Decision, Offer, UpdateCard, UpdateEvent, available_body, browser_url,
        decision, download_percent, extraction_percent, no_update_message, plain_error,
        version_name,
    };

    /// `SPUUserUpdateChoiceInstall`.
    const INSTALL: isize = 1;
    /// `SPUUserUpdateChoiceDismiss`. Skip is 0 and is never used: Ignore and
    /// Later ask again later, they do not hide this version forever.
    const DISMISS: isize = 2;
    /// `SPUUserUpdateStageDownloaded`.
    const STAGE_DOWNLOADED: isize = 1;
    /// `SPUUserUpdateStageInstalling`.
    const STAGE_INSTALLING: isize = 2;

    struct DriverIvars {
        events: Sender<UpdateEvent>,
    }

    define_class!(
        #[unsafe(super = NSObject)]
        #[ivars = DriverIvars]
        struct ShikaSparkleDriver;

        unsafe impl NSObjectProtocol for ShikaSparkleDriver {}

        impl ShikaSparkleDriver {
            #[unsafe(method(showUpdatePermissionRequest:reply:))]
            fn permission(&self, _request: &AnyObject, reply: &Block<dyn Fn(*mut AnyObject)>) {
                on_permission(&self.ivars().events, reply);
            }

            #[unsafe(method(showUserInitiatedUpdateCheckWithCancellation:))]
            fn checking(&self, cancellation: &Block<dyn Fn()>) {
                on_checking(&self.ivars().events, cancellation);
            }

            #[unsafe(method(showUpdateFoundWithAppcastItem:state:reply:))]
            fn found(
                &self,
                item: &AnyObject,
                state: &AnyObject,
                reply: &Block<dyn Fn(isize)>,
            ) {
                on_found(&self.ivars().events, item, state, reply);
            }

            #[unsafe(method(showUpdateReleaseNotesWithDownloadData:))]
            fn _notes(&self, _data: &AnyObject) {}

            #[unsafe(method(showUpdateReleaseNotesFailedToDownloadWithError:))]
            fn _notes_failed(&self, _error: &NSError) {}

            #[unsafe(method(showUpdateNotFoundWithError:acknowledgement:))]
            fn not_found(&self, error: &NSError, acknowledgement: &Block<dyn Fn()>) {
                on_not_found(&self.ivars().events, error, acknowledgement);
            }

            #[unsafe(method(showUpdaterError:acknowledgement:))]
            fn failed(&self, error: &NSError, acknowledgement: &Block<dyn Fn()>) {
                on_failed(&self.ivars().events, error, acknowledgement);
            }

            #[unsafe(method(showDownloadInitiatedWithCancellation:))]
            fn download(&self, cancellation: &Block<dyn Fn()>) {
                on_download(&self.ivars().events, cancellation);
            }

            #[unsafe(method(showDownloadDidReceiveExpectedContentLength:))]
            fn expected(&self, length: u64) {
                on_expected(&self.ivars().events, length);
            }

            #[unsafe(method(showDownloadDidReceiveDataOfLength:))]
            fn received(&self, length: u64) {
                on_received(&self.ivars().events, length);
            }

            #[unsafe(method(showDownloadDidStartExtractingUpdate))]
            fn extracting(&self) {
                on_extracting(&self.ivars().events);
            }

            #[unsafe(method(showExtractionReceivedProgress:))]
            fn extraction(&self, progress: f64) {
                on_extraction(&self.ivars().events, progress);
            }

            #[unsafe(method(showReadyToInstallAndRelaunch:))]
            fn ready(&self, reply: &Block<dyn Fn(isize)>) {
                on_ready(&self.ivars().events, reply);
            }

            #[unsafe(method(showInstallingUpdateWithApplicationTerminated:retryTerminatingApplication:))]
            fn installing(&self, terminated: Bool, retry: &Block<dyn Fn()>) {
                on_installing(&self.ivars().events, terminated, retry);
            }

            #[unsafe(method(showUpdateInstalledAndRelaunched:acknowledgement:))]
            fn installed(&self, relaunched: Bool, acknowledgement: &Block<dyn Fn()>) {
                on_installed(&self.ivars().events, relaunched, acknowledgement);
            }

            #[unsafe(method(dismissUpdateInstallation))]
            fn dismiss(&self) {
                on_dismiss(&self.ivars().events);
            }

            #[unsafe(method(showUpdateInFocus))]
            fn focus(&self) {
                if let Some(card) = with_session(|session| session.card.clone()) {
                    let _ = self.ivars().events.send(UpdateEvent::Show(card));
                }
            }
        }
    );

    enum Pending {
        None,
        Permission(RcBlock<dyn Fn(*mut AnyObject)>),
        Choice(RcBlock<dyn Fn(isize)>),
        Ack(RcBlock<dyn Fn()>),
        Cancel(RcBlock<dyn Fn()>),
        Retry(RcBlock<dyn Fn()>),
    }

    impl Pending {
        fn take(&mut self) -> Self {
            std::mem::replace(self, Self::None)
        }
    }

    struct Session {
        events: Option<Sender<UpdateEvent>>,
        card: Option<UpdateCard>,
        pending: Pending,
        version: String,
        received: u64,
        expected: Option<u64>,
    }

    impl Default for Session {
        fn default() -> Self {
            Self {
                events: None,
                card: None,
                pending: Pending::None,
                version: String::new(),
                received: 0,
                expected: None,
            }
        }
    }

    // Sparkle calls the driver on the main thread, and notice clicks are on
    // that thread too. The reply blocks stay here, off the UI model.
    thread_local! {
        static SESSION: RefCell<Session> = RefCell::new(Session::default());
    }

    fn with_session<T>(f: impl FnOnce(&mut Session) -> T) -> T {
        SESSION.with(|session| f(&mut session.borrow_mut()))
    }

    fn show(events: &Sender<UpdateEvent>, card: UpdateCard) {
        let _ = events.send(UpdateEvent::Show(card));
    }

    pub struct Inner {
        events: Receiver<UpdateEvent>,
        _driver: Retained<ShikaSparkleDriver>,
        updater: Retained<AnyObject>,
    }

    pub fn start() -> Option<Inner> {
        let frameworks = NSBundle::mainBundle().privateFrameworksPath()?;
        let path = format!("{frameworks}/Sparkle.framework");
        if !std::path::Path::new(&path).exists() {
            return None;
        }
        let Some(bundle) = NSBundle::bundleWithPath(&NSString::from_str(&path)) else {
            eprintln!("Sparkle: {path} is not a bundle");
            return None;
        };
        // SAFETY: the release build embeds and signs this framework, and the
        // hardened runtime only loads it with the app's own Team ID.
        if let Err(error) = unsafe { bundle.loadAndReturnError() } {
            eprintln!("Sparkle: {}", error.localizedDescription());
            return None;
        }
        let Some(class) = AnyClass::get(c"SPUUpdater") else {
            eprintln!("Sparkle: SPUUpdater is missing");
            return None;
        };
        let (sender, events) = mpsc::channel();
        // Permission replies do not dismiss the notice. Clicks send Clear
        // through this copy.
        with_session(|session| {
            *session = Session {
                events: Some(sender.clone()),
                ..Session::default()
            };
        });
        let allocated = ShikaSparkleDriver::alloc().set_ivars(DriverIvars { events: sender });
        let driver: Retained<ShikaSparkleDriver> = unsafe { msg_send![super(allocated), init] };
        let host = NSBundle::mainBundle();
        // SAFETY: Sparkle 2's documented initializer. The driver is retained
        // for the life of the updater, and again here so the notice can answer.
        let updater: Option<Retained<AnyObject>> = unsafe {
            let allocated: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![
                allocated,
                initWithHostBundle: &*host,
                applicationBundle: &*host,
                userDriver: &*driver,
                delegate: None::<&AnyObject>
            ]
        };
        let updater = updater?;
        // SAFETY: NULL skips the error object. A misconfigured bundle simply
        // grows no menu item.
        let started: Bool = unsafe {
            msg_send![
                &*updater,
                startUpdater: std::ptr::null_mut::<*mut NSError>()
            ]
        };
        if !started.as_bool() {
            eprintln!("Sparkle: the updater did not start");
            return None;
        }
        Some(Inner {
            events,
            _driver: driver,
            updater,
        })
    }

    impl Inner {
        pub fn check(&self) {
            // SAFETY: Sparkle's documented menu action. No sender.
            unsafe {
                let _: () = msg_send![&*self.updater, checkForUpdates];
            }
        }

        pub fn poll(&self) -> Vec<UpdateEvent> {
            let mut events = Vec::new();
            while let Ok(event) = self.events.try_recv() {
                events.push(event);
            }
            events
        }

        pub fn respond(&self, action: CardAction) {
            let Some(card) = with_session(|session| session.card.clone()) else {
                return;
            };
            let Some(decision) = decision(&card, action) else {
                return;
            };
            let pending = with_session(|session| session.pending.take());
            apply(decision, pending);
        }
    }

    fn apply(decision: Decision, pending: Pending) {
        match (decision, pending) {
            (Decision::AllowChecks(checks), Pending::Permission(reply)) => {
                if send_permission(&reply, checks) {
                    // Sparkle saves the choice on the next turn and does not
                    // dismiss the notice itself.
                    clear_card();
                } else {
                    with_session(|session| session.pending = Pending::Permission(reply));
                }
            }
            (Decision::Install, Pending::Choice(reply)) => reply.call((INSTALL,)),
            (Decision::Dismiss, Pending::Choice(reply)) => reply.call((DISMISS,)),
            (Decision::Cancel, Pending::Cancel(reply)) => reply.call(()),
            (Decision::Retry, Pending::Retry(reply)) => reply.call(()),
            (Decision::Acknowledge, Pending::Ack(reply)) => {
                reply.call(());
                // Sparkle dismisses on the next turn. Close the card now.
                clear_card();
            }
            (_, pending) => with_session(|session| session.pending = pending),
        }
    }

    fn clear_card() {
        let sender = with_session(|session| {
            session.card = None;
            session.events.clone()
        });
        if let Some(sender) = sender {
            let _ = sender.send(UpdateEvent::Clear);
        }
    }

    fn reset_session() {
        with_session(|session| {
            let events = session.events.take();
            *session = Session::default();
            session.events = events;
        });
    }

    fn send_permission(reply: &Block<dyn Fn(*mut AnyObject)>, checks: bool) -> bool {
        let Some(class) = AnyClass::get(c"SUUpdatePermissionResponse") else {
            eprintln!("Sparkle: SUUpdatePermissionResponse is missing");
            return false;
        };
        let downloading = NSNumber::numberWithBool(false);
        // SAFETY: Sparkle's documented initializer. Automatic download stays
        // off: the corner card is where a download starts.
        let response: Option<Retained<AnyObject>> = unsafe {
            let allocated: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![
                allocated,
                initWithAutomaticUpdateChecks: Bool::new(checks),
                automaticUpdateDownloading: &*downloading,
                sendSystemProfile: Bool::new(false)
            ]
        };
        let Some(response) = response else {
            eprintln!("Sparkle: could not record the update choice");
            return false;
        };
        reply.call((Retained::as_ptr(&response).cast_mut(),));
        true
    }

    fn on_permission(events: &Sender<UpdateEvent>, reply: &Block<dyn Fn(*mut AnyObject)>) {
        let card = UpdateCard::Permission;
        with_session(|session| {
            session.pending = Pending::Permission(reply.copy());
            session.card = Some(card.clone());
        });
        show(events, card);
    }

    fn on_checking(events: &Sender<UpdateEvent>, cancellation: &Block<dyn Fn()>) {
        let card = UpdateCard::Checking;
        with_session(|session| {
            session.pending = Pending::Cancel(cancellation.copy());
            session.card = Some(card.clone());
        });
        show(events, card);
    }

    fn on_found(
        events: &Sender<UpdateEvent>,
        item: &AnyObject,
        state: &AnyObject,
        reply: &Block<dyn Fn(isize)>,
    ) {
        let reply = reply.copy();
        let version = display_version(item);
        let notes = cocoa_string(unsafe { msg_send![item, itemDescription] });
        let critical: Bool = unsafe { msg_send![item, isCriticalUpdate] };
        let info_only: Bool = unsafe { msg_send![item, isInformationOnlyUpdate] };
        let stage: isize = unsafe { msg_send![state, stage] };
        let offer = if info_only.as_bool() {
            info_url(item).map(Offer::Open)
        } else {
            Some(Offer::Download)
        };
        let body = available_body(&notes, critical.as_bool(), offer.as_ref());
        // Installation already started. Restart quits now. Later installs on quit.
        if stage == STAGE_INSTALLING && !info_only.as_bool() {
            let card = UpdateCard::Ready {
                version: version.clone(),
            };
            with_session(|session| {
                session.version = version;
                session.pending = Pending::Choice(reply);
                session.card = Some(card.clone());
            });
            show(events, card);
            return;
        }
        // Already downloaded in the background: extract, then ask to restart
        // once, instead of asking twice.
        if stage == STAGE_DOWNLOADED && !info_only.as_bool() {
            let card = UpdateCard::Preparing {
                version: version.clone(),
                percent: None,
            };
            with_session(|session| {
                session.version = version;
                session.pending = Pending::None;
                session.received = 0;
                session.expected = None;
                session.card = Some(card.clone());
            });
            show(events, card);
            reply.call((INSTALL,));
            return;
        }
        let card = UpdateCard::Available {
            version: version.clone(),
            body,
            offer,
        };
        with_session(|session| {
            session.version = version;
            session.pending = Pending::Choice(reply);
            session.card = Some(card.clone());
        });
        show(events, card);
    }

    fn on_not_found(
        events: &Sender<UpdateEvent>,
        error: &NSError,
        acknowledgement: &Block<dyn Fn()>,
    ) {
        let reason = no_update_reason(error);
        let message = if reason == 0 {
            let text = plain_error(&error.localizedDescription().to_string());
            if text.is_empty() {
                no_update_message(0).to_string()
            } else {
                text
            }
        } else {
            no_update_message(reason).to_string()
        };
        with_session(|session| {
            session.pending = Pending::None;
            session.card = None;
        });
        let _ = events.send(UpdateEvent::Clear);
        let _ = events.send(UpdateEvent::Toast(message));
        acknowledgement.call(());
    }

    fn on_failed(events: &Sender<UpdateEvent>, error: &NSError, acknowledgement: &Block<dyn Fn()>) {
        let message = plain_error(&error.localizedDescription().to_string());
        let message = if message.is_empty() {
            "The update could not be installed.".into()
        } else {
            message
        };
        let card = UpdateCard::Failed { message };
        with_session(|session| {
            session.pending = Pending::Ack(acknowledgement.copy());
            session.card = Some(card.clone());
        });
        show(events, card);
    }

    fn on_download(events: &Sender<UpdateEvent>, cancellation: &Block<dyn Fn()>) {
        let card = with_session(|session| {
            session.received = 0;
            session.expected = None;
            session.pending = Pending::Cancel(cancellation.copy());
            let card = UpdateCard::Downloading {
                version: session.version.clone(),
                percent: None,
                can_cancel: true,
            };
            session.card = Some(card.clone());
            card
        });
        show(events, card);
    }

    fn on_expected(events: &Sender<UpdateEvent>, length: u64) {
        let Some(card) = with_session(|session| {
            session.expected = Some(length);
            publish_download(session)
        }) else {
            return;
        };
        show(events, card);
    }

    fn on_received(events: &Sender<UpdateEvent>, length: u64) {
        let Some(card) = with_session(|session| {
            session.received = session.received.saturating_add(length);
            publish_download(session)
        }) else {
            return;
        };
        show(events, card);
    }

    fn publish_download(session: &mut Session) -> Option<UpdateCard> {
        let percent = session
            .expected
            .and_then(|expected| download_percent(session.received, expected));
        let card = UpdateCard::Downloading {
            version: session.version.clone(),
            percent,
            can_cancel: matches!(session.pending, Pending::Cancel(_)),
        };
        if session.card.as_ref() == Some(&card) {
            None
        } else {
            session.card = Some(card.clone());
            Some(card)
        }
    }

    fn on_extracting(events: &Sender<UpdateEvent>) {
        let card = with_session(|session| {
            session.pending = Pending::None;
            let card = UpdateCard::Preparing {
                version: session.version.clone(),
                percent: None,
            };
            session.card = Some(card.clone());
            card
        });
        show(events, card);
    }

    fn on_extraction(events: &Sender<UpdateEvent>, progress: f64) {
        let percent = extraction_percent(progress);
        let Some(card) = with_session(|session| {
            let card = UpdateCard::Preparing {
                version: session.version.clone(),
                percent,
            };
            if session.card.as_ref() == Some(&card) {
                None
            } else {
                session.card = Some(card.clone());
                Some(card)
            }
        }) else {
            return;
        };
        show(events, card);
    }

    fn on_ready(events: &Sender<UpdateEvent>, reply: &Block<dyn Fn(isize)>) {
        let card = with_session(|session| {
            session.pending = Pending::Choice(reply.copy());
            let card = UpdateCard::Ready {
                version: session.version.clone(),
            };
            session.card = Some(card.clone());
            card
        });
        show(events, card);
    }

    fn on_installing(events: &Sender<UpdateEvent>, terminated: Bool, retry: &Block<dyn Fn()>) {
        let can_retry = !terminated.as_bool();
        let retry = retry.copy();
        let card = with_session(|session| {
            session.pending = if can_retry {
                Pending::Retry(retry)
            } else {
                Pending::None
            };
            let card = UpdateCard::Installing {
                version: session.version.clone(),
                can_retry,
            };
            session.card = Some(card.clone());
            card
        });
        show(events, card);
    }

    fn on_installed(
        events: &Sender<UpdateEvent>,
        relaunched: Bool,
        acknowledgement: &Block<dyn Fn()>,
    ) {
        reset_session();
        let _ = events.send(UpdateEvent::Clear);
        if !relaunched.as_bool() {
            let _ = events.send(UpdateEvent::Toast("The update installed.".into()));
        }
        acknowledgement.call(());
    }

    fn on_dismiss(events: &Sender<UpdateEvent>) {
        reset_session();
        let _ = events.send(UpdateEvent::Clear);
    }

    fn display_version(item: &AnyObject) -> String {
        let value: Option<Retained<NSString>> = unsafe { msg_send![item, displayVersionString] };
        version_name(&cocoa_string(value)).unwrap_or_default()
    }

    fn info_url(item: &AnyObject) -> Option<String> {
        let url: Option<Retained<NSURL>> = unsafe { msg_send![item, infoURL] };
        let url = url?;
        let text = url.absoluteString()?;
        browser_url(&text.to_string())
    }

    fn cocoa_string(value: Option<Retained<NSString>>) -> String {
        value.map(|text| text.to_string()).unwrap_or_default()
    }

    fn no_update_reason(error: &NSError) -> isize {
        let key = NSString::from_str("SUNoUpdateFoundReason");
        let info = error.userInfo();
        let value: *mut AnyObject = unsafe { msg_send![&*info, objectForKey: &*key] };
        if value.is_null() {
            0
        } else {
            unsafe { msg_send![value, integerValue] }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_without_embedded_sparkle_has_no_updater() {
        assert!(Updater::start().is_none());
    }

    #[test]
    fn notice_copy_names_the_outcome_and_stays_plain() {
        let cards = [
            UpdateCard::Permission,
            UpdateCard::Checking,
            UpdateCard::Available {
                version: "0.6.0".into(),
                body: available_body("", false, Some(&Offer::Download)),
                offer: Some(Offer::Download),
            },
            UpdateCard::Available {
                version: String::new(),
                body: available_body(
                    "<h2>Read this</h2>",
                    true,
                    Some(&Offer::Open("https://useshika.com".into())),
                ),
                offer: Some(Offer::Open("https://useshika.com".into())),
            },
            UpdateCard::Downloading {
                version: "0.6.0".into(),
                percent: Some(42),
                can_cancel: true,
            },
            UpdateCard::Preparing {
                version: "0.6.0".into(),
                percent: None,
            },
            UpdateCard::Ready {
                version: "0.6.0".into(),
            },
            UpdateCard::Installing {
                version: "0.6.0".into(),
                can_retry: false,
            },
            UpdateCard::Failed {
                message: "The feed could not be read.".into(),
            },
        ];
        for card in cards {
            let notice = card.view();
            assert!(!notice.title.contains('!'), "{}", notice.title);
            assert!(!notice.title.contains('\u{2014}'), "{}", notice.title);
            assert!(!notice.body.contains('!'), "{}", notice.body);
            assert!(!notice.body.contains('\u{2014}'), "{}", notice.body);
        }
        let available = UpdateCard::Available {
            version: "0.6.0".into(),
            body: "Download it when you want. Ignore leaves it until the next check.".into(),
            offer: Some(Offer::Download),
        }
        .view();
        assert_eq!(available.title, "Shika 0.6.0 is available");
        assert_eq!(available.secondary, Some("Ignore"));
        assert_eq!(available.primary, Some("Download"));
        assert!(
            !UpdateCard::Available {
                version: "0.6.0".into(),
                body: String::new(),
                offer: Some(Offer::Download),
            }
            .centered()
        );
        let ready_card = UpdateCard::Ready {
            version: "0.6.0".into(),
        };
        assert!(ready_card.centered());
        assert!(
            UpdateCard::Installing {
                version: "0.6.0".into(),
                can_retry: false,
            }
            .centered()
        );
        let ready = ready_card.view();
        assert_eq!(ready.title, "Restart to update");
        assert_eq!(ready.secondary, Some("Later"));
        assert_eq!(ready.primary, Some("Restart"));
        assert!(ready.body.contains("next time Shika quits"));
    }

    #[test]
    fn clicks_map_to_download_then_restart() {
        let available = UpdateCard::Available {
            version: "0.6.0".into(),
            body: String::new(),
            offer: Some(Offer::Download),
        };
        assert_eq!(
            decision(&available, CardAction::Primary),
            Some(Decision::Install)
        );
        assert_eq!(
            decision(&available, CardAction::Secondary),
            Some(Decision::Dismiss)
        );
        let open = UpdateCard::Available {
            version: "0.6.0".into(),
            body: String::new(),
            offer: Some(Offer::Open("https://useshika.com".into())),
        };
        assert_eq!(
            decision(&open, CardAction::Primary),
            Some(Decision::Dismiss)
        );
        let ready = UpdateCard::Ready {
            version: "0.6.0".into(),
        };
        assert_eq!(
            decision(&ready, CardAction::Primary),
            Some(Decision::Install)
        );
        assert_eq!(
            decision(&ready, CardAction::Secondary),
            Some(Decision::Dismiss)
        );
        assert_eq!(
            decision(&UpdateCard::Permission, CardAction::Primary),
            Some(Decision::AllowChecks(true))
        );
        assert_eq!(
            decision(&UpdateCard::Permission, CardAction::Secondary),
            Some(Decision::AllowChecks(false))
        );
        assert_eq!(decision(&UpdateCard::Checking, CardAction::Primary), None);
        assert_eq!(
            decision(
                &UpdateCard::Downloading {
                    version: "0.6.0".into(),
                    percent: None,
                    can_cancel: true,
                },
                CardAction::Secondary
            ),
            Some(Decision::Cancel)
        );
        assert_eq!(
            decision(
                &UpdateCard::Failed {
                    message: "no".into(),
                },
                CardAction::Secondary
            ),
            Some(Decision::Acknowledge)
        );
    }

    #[test]
    fn notes_progress_and_links_stay_short_and_safe() {
        assert_eq!(
            note_excerpt("## Fixed the resize bug\n\nMore detail."),
            Some("Fixed the resize bug".into())
        );
        assert_eq!(note_excerpt("<h2>Read this</h2>"), Some("Read this".into()));
        assert_eq!(note_excerpt("   \n"), None);
        assert!(note_excerpt(&"a".repeat(200)).unwrap().ends_with("..."));
        assert_eq!(note_excerpt(&"a".repeat(200)).unwrap().chars().count(), 140);
        assert_eq!(version_name("  0.6.0\nbad"), Some("0.6.0 bad".into()));
        assert_eq!(version_name(" \n"), None);
        assert_eq!(
            browser_url(" https://useshika.com/notes "),
            Some("https://useshika.com/notes".into())
        );
        assert_eq!(browser_url("file:///tmp/shika.dmg"), None);
        assert_eq!(download_percent(50, 200), Some(25));
        assert_eq!(download_percent(500, 200), Some(100));
        assert_eq!(download_percent(1, 0), None);
        assert_eq!(extraction_percent(0.425), Some(43));
        assert_eq!(extraction_percent(f64::NAN), None);
        assert_eq!(no_update_message(1), "No newer version found.");
        assert_eq!(
            no_update_message(3),
            "This Mac is too old for the latest version."
        );
        assert_eq!(plain_error("You're up to date!"), "You're up to date");
        assert_eq!(
            available_body("", true, Some(&Offer::Download)),
            "This update is critical. Download it when you want. Ignore leaves it until the next check."
        );
    }
}
