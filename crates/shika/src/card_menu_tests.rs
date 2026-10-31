//! Tests for the card context menu target.
use super::*;

#[test]
fn card_menu_stays_bound_to_the_clicked_card_after_reordering_or_removal() {
    let first = gpui::EntityId::from(1);
    let clicked = gpui::EntityId::from(2);
    let third = gpui::EntityId::from(3);
    let menu = CardMenu {
        target: clicked,
        position: gpui::point(px(100.), px(200.)),
        action: CardMenuAction::Close,
    };
    assert_eq!(
        menu.target_index([first, clicked, third].into_iter()),
        Some(1)
    );
    assert_eq!(
        menu.target_index([clicked, third, first].into_iter()),
        Some(0)
    );
    assert_eq!(menu.target_index([clicked, third].into_iter()), Some(0));
    // A disappeared target must not close the card that inherited its index.
    assert_eq!(menu.target_index([first, third].into_iter()), None);
    assert_eq!(menu.target_index(std::iter::empty()), None);
}
