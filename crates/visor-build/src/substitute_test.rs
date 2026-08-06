use std::collections::HashMap;

use super::*;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn simple_dollar_var() {
    let v = vars(&[("NAME", "world")]);
    let result = substitute_vars("hello $NAME", &v).unwrap();
    assert_eq!(result, "hello world");
}

#[test]
fn braced_var() {
    let v = vars(&[("NAME", "world")]);
    let result = substitute_vars("hello ${NAME}", &v).unwrap();
    assert_eq!(result, "hello world");
}

#[test]
fn default_value_when_unset() {
    let v = HashMap::new();
    let result = substitute_vars("${MISSING:-fallback}", &v).unwrap();
    assert_eq!(result, "fallback");
}

#[test]
fn default_value_when_set() {
    let v = vars(&[("PRESENT", "actual")]);
    let result = substitute_vars("${PRESENT:-fallback}", &v).unwrap();
    assert_eq!(result, "actual");
}

#[test]
fn default_value_when_empty() {
    let v = vars(&[("EMPTY", "")]);
    let result = substitute_vars("${EMPTY:-fallback}", &v).unwrap();
    assert_eq!(result, "fallback");
}

#[test]
fn alt_value_when_set() {
    let v = vars(&[("PRESENT", "yes")]);
    let result = substitute_vars("${PRESENT:+replacement}", &v).unwrap();
    assert_eq!(result, "replacement");
}

#[test]
fn alt_value_when_unset() {
    let v = HashMap::new();
    let result = substitute_vars("${MISSING:+replacement}", &v).unwrap();
    assert_eq!(result, "");
}

#[test]
fn alt_value_when_empty() {
    let v = vars(&[("EMPTY", "")]);
    let result = substitute_vars("${EMPTY:+replacement}", &v).unwrap();
    assert_eq!(result, "");
}

#[test]
fn escaped_dollar() {
    let v = vars(&[("X", "val")]);
    let result = substitute_vars("price is $$5", &v).unwrap();
    assert_eq!(result, "price is $5");
}

#[test]
fn multiple_substitutions() {
    let v = vars(&[("A", "alpha"), ("B", "beta")]);
    let result = substitute_vars("$A and ${B} end", &v).unwrap();
    assert_eq!(result, "alpha and beta end");
}

#[test]
fn unknown_var_becomes_empty() {
    let v = HashMap::new();
    let result = substitute_vars("hello $UNKNOWN world", &v).unwrap();
    assert_eq!(result, "hello  world");
}

#[test]
fn no_substitution_plain_string() {
    let v = vars(&[("X", "val")]);
    let result = substitute_vars("just a plain string", &v).unwrap();
    assert_eq!(result, "just a plain string");
}

#[test]
fn empty_input() {
    let v = HashMap::new();
    let result = substitute_vars("", &v).unwrap();
    assert_eq!(result, "");
}

#[test]
fn empty_default() {
    let v = HashMap::new();
    let result = substitute_vars("${VAR:-}", &v).unwrap();
    assert_eq!(result, "");
}

#[test]
fn dollar_at_end_of_string() {
    let v = HashMap::new();
    let result = substitute_vars("trailing$", &v).unwrap();
    assert_eq!(result, "trailing$");
}

#[test]
fn adjacent_vars() {
    let v = vars(&[("A", "hello"), ("B", "world")]);
    let result = substitute_vars("${A}${B}", &v).unwrap();
    assert_eq!(result, "helloworld");
}

#[test]
fn var_with_underscores_and_digits() {
    let v = vars(&[("MY_VAR_2", "value")]);
    let result = substitute_vars("$MY_VAR_2", &v).unwrap();
    assert_eq!(result, "value");
}
