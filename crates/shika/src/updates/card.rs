//! The update notice's states, the words and buttons each one shows, and
//! the Sparkle decision behind each click.
use super::{available_title, percent_line, version_sentence, version_title};

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
