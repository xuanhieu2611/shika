//! Tests for agent navigation in row order.
use super::*;

#[test]
fn agent_navigation_skips_headers_and_wraps_in_both_directions() {
    let rows = vec![
        Selection::Project("a".into()),
        Selection::Card(2),
        Selection::Card(0),
        Selection::Project("b".into()),
        Selection::Card(1),
    ];
    let card = |i| Some(Selection::Card(i));
    assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(0)), 1), card(1));
    assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(1)), 1), card(2));
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Card(2)), -1),
        card(1)
    );
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Project("b".into())), -1),
        card(0)
    );
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Project("b".into())), 1),
        card(1)
    );
}

#[test]
fn a_resting_lead_is_an_agent_stop_in_row_order() {
    let rows = vec![
        Selection::Resting("a".into()),
        Selection::Card(0),
        Selection::Resting("c".into()),
        Selection::Project("a".into()),
        Selection::Card(1),
    ];
    let resting = |p: &str| Some(Selection::Resting(p.into()));
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Resting("a".into())), 1),
        Some(Selection::Card(0))
    );
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Card(0)), 1),
        resting("c")
    );
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Resting("c".into())), 1),
        Some(Selection::Card(1))
    );
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Resting("a".into())), -1),
        Some(Selection::Card(1))
    );
    assert_eq!(nth_agent(&rows, 0), resting("a"));
    assert_eq!(nth_agent(&rows, 2), resting("c"));
    assert_eq!(nth_agent(&rows, 3), Some(Selection::Card(1)));
    assert_eq!(nth_agent(&rows, 4), None);
}

#[test]
fn agent_navigation_handles_empty_single_and_missing_selection() {
    let card = |i| Some(Selection::Card(i));
    assert_eq!(adjacent_agent(&[], None, 1), None);
    let rows = vec![Selection::Project("a".into()), Selection::Card(0)];
    assert_eq!(adjacent_agent(&rows, None, 1), card(0));
    assert_eq!(adjacent_agent(&rows, None, -1), card(0));
    assert_eq!(adjacent_agent(&rows, Some(&Selection::Card(0)), 1), card(0));
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Card(0)), -1),
        card(0)
    );
    assert_eq!(adjacent_agent(&rows[..1], None, 1), None);
    assert_eq!(
        adjacent_agent(&rows, Some(&Selection::Card(9)), -1),
        card(0)
    );
}

#[test]
fn numbered_agents_follow_column_order_and_stop_when_the_card_is_missing() {
    let rows = vec![
        Selection::Project("a".into()),
        Selection::Card(2),
        Selection::Card(0),
        Selection::Project("b".into()),
        Selection::Card(1),
    ];
    assert_eq!(nth_agent(&rows, 0), Some(Selection::Card(2)));
    assert_eq!(nth_agent(&rows, 1), Some(Selection::Card(0)));
    assert_eq!(nth_agent(&rows, 2), Some(Selection::Card(1)));
    assert_eq!(nth_agent(&rows, 3), None);
    assert_eq!(nth_agent(&rows, 8), None);
    assert_eq!(nth_agent(&[], 0), None);
}
