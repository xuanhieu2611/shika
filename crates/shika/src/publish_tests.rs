//! Tests for Create PR labels.
use super::*;

#[test]
fn publish_change_label_names_an_empty_commit_and_pluralizes() {
    assert_eq!(publish_change_label(0), "Nothing new to commit");
    assert_eq!(publish_change_label(1), "1 changed file");
    assert_eq!(publish_change_label(2), "2 changed files");
}
