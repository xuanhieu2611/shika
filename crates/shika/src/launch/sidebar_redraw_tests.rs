use super::*;
use gpui::{TestAppContext, VisualTestContext, size};

#[gpui::test]
fn terminal_redraw_reuses_sidebar_but_app_updates_rebuild_it(cx: &mut TestAppContext) {
    let path = std::env::temp_dir().join(format!(
        "shika-sidebar-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let core = Arc::new(Core::open(&path).unwrap());
    let handle = cx.open_window(size(px(1400.), px(880.)), move |window, cx| {
        let mut app = Shika::new(
            core.clone(),
            Ok(Settings::default()),
            None,
            false,
            window,
            cx,
        );
        let pane = Pane::new(
            core,
            false,
            1.,
            app.font_size.points(),
            app.terminal_palette(window),
            window,
            cx,
        );
        let launch = Launch::Task { prompt: None };
        let card = launch_card(
            "fixture",
            &shika_core::terminal_choice(),
            pane,
            false,
            &launch,
            LaunchOwner::of(&launch),
            PreparationControl::default(),
        );
        app.projects.push(Project {
            id: "fixture".into(),
            name: "Fixture".into(),
            path: app.core.data_dir().to_owned(),
            base_branch: None,
            approved_preparation: None,
            worktree_setup_reviewed: false,
            lead: None,
        });
        app.cards.push(card);
        app.selection.set(Some(Selection::Card(0)));
        app
    });
    let app = handle.entity(cx).unwrap();
    let cx = &mut VisualTestContext::from_window(handle.into(), cx);
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let view = app.update(cx, |app, _| {
        assert_eq!(app.last_revealed_selection, Some(Selection::Card(0)));
        // A marker written back only when the production sidebar runs again.
        app.last_revealed_selection = None;
        app.cards[0].agent.view.clone()
    });
    let frames = view.read_with(cx, |view, _| view.stats().frames);
    assert!(frames > 0);
    view.update(cx, |_, cx| cx.notify());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(view.read_with(cx, |view, _| view.stats().frames) > frames);
    app.update(cx, |app, cx| {
        assert!(app.last_revealed_selection.is_none());
        cx.notify();
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    app.read_with(cx, |app, _| {
        assert_eq!(app.last_revealed_selection, Some(Selection::Card(0)));
    });
    cx.update(|window, _| window.remove_window());
    std::fs::remove_dir_all(path).unwrap();
}
