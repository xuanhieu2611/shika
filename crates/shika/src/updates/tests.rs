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
