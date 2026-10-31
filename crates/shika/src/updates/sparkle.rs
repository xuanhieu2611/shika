//! The Sparkle bridge: loads the embedded framework, starts `SPUUpdater`
//! with Shika's user driver, and turns its callbacks into notice events.
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};

use block2::{Block, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSBundle, NSError, NSNumber, NSObject, NSObjectProtocol, NSString, NSURL};

use super::{
    CardAction, Decision, Offer, UpdateCard, UpdateEvent, available_body, browser_url, decision,
    download_percent, extraction_percent, no_update_message, plain_error, version_name,
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

fn on_not_found(events: &Sender<UpdateEvent>, error: &NSError, acknowledgement: &Block<dyn Fn()>) {
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

fn on_installed(events: &Sender<UpdateEvent>, relaunched: Bool, acknowledgement: &Block<dyn Fn()>) {
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
