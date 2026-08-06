use super::*;

#[test]
fn generate_name_returns_adjective_underscore_hero() {
    let name = generate_name();
    assert!(name.contains('_'), "name should contain underscore: {name}");
    let parts: Vec<&str> = name.split('_').collect();
    assert_eq!(parts.len(), 2, "name should have exactly 2 parts: {name}");
    assert!(!parts[0].is_empty());
    assert!(!parts[1].is_empty());
}

#[test]
fn generate_name_uses_supported_short_word_lists() {
    let name = generate_name();
    let parts: Vec<&str> = name.split('_').collect();

    assert_eq!(parts.len(), 2, "name should have exactly 2 parts: {name}");
    assert!(
        ADJECTIVES.contains(&parts[0]),
        "adjective should come from the supported short list: {name}"
    );
    assert!(
        HEROES.contains(&parts[1]),
        "hero should come from the supported short list: {name}"
    );
    assert!(
        name.len() <= 14,
        "name should stay compact for easy CLI use: {name}"
    );
}

#[test]
fn generate_name_is_lowercase() {
    let name = generate_name();
    assert_eq!(
        name,
        name.to_lowercase(),
        "name should be lowercase: {name}"
    );
}

#[test]
fn generate_name_produces_different_names() {
    let names: std::collections::HashSet<String> = (0..10).map(|_| generate_name()).collect();
    assert!(
        names.len() > 1,
        "should generate varied names, got: {names:?}"
    );
}
