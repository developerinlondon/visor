//! YAML parsing and validation for docker-compose.yml files.
//!
//! Handles deserialization of compose files, environment variable
//! interpolation (`${VAR}`, `${VAR:-default}`, `${VAR-default}`),
//! and project validation.

use std::path::Path;

use anyhow::Context;

use super::types::ComposeProject;

/// Intermediate representation that accepts the optional `version` field.
///
/// The `version` field in docker-compose is informational only and does not
/// affect parsing. This struct captures it to avoid serde errors, then
/// converts to the public [`ComposeProject`] type.
#[derive(serde::Deserialize)]
struct RawComposeFile {
    #[allow(dead_code)]
    version: Option<String>,

    name: Option<String>,

    #[serde(default)]
    services: std::collections::HashMap<String, super::types::ComposeService>,

    #[serde(default)]
    networks: std::collections::HashMap<String, super::types::ComposeNetwork>,

    #[serde(default)]
    volumes: std::collections::HashMap<String, super::types::ComposeVolumeConfig>,
}

/// Parses a docker-compose.yml string into a [`ComposeProject`].
///
/// Performs environment variable interpolation on the raw YAML string
/// before deserializing, then validates the resulting project.
///
/// # Errors
///
/// Returns an error if:
/// - The YAML is malformed
/// - Required fields are missing
/// - Validation fails (e.g. `depends_on` references a non-existent service)
#[must_use = "parsing result should be checked"]
pub fn parse_compose(yaml: &str) -> anyhow::Result<ComposeProject> {
    parse_compose_with_vars(yaml, |key: &str| std::env::var(key))
}

/// Parses a docker-compose.yml string with a custom variable resolver.
///
/// This is the core parsing function. The `var_lookup` closure receives a
/// variable name and returns `Ok(value)` if set, or `Err` if unset.
///
/// # Errors
///
/// Returns an error if the YAML is malformed or validation fails.
pub fn parse_compose_with_vars<F>(yaml: &str, var_lookup: F) -> anyhow::Result<ComposeProject>
where
    F: Fn(&str) -> Result<String, std::env::VarError>,
{
    let expanded = expand_env_vars(yaml, &var_lookup);

    let raw: RawComposeFile =
        serde_yaml::from_str(&expanded).context("failed to parse compose YAML")?;

    let project = ComposeProject {
        name: raw.name,
        services: raw.services,
        networks: raw.networks,
        volumes: raw.volumes,
    };

    project
        .validate()
        .context("compose project validation failed")?;

    Ok(project)
}

/// Parses a docker-compose.yml file from the given path.
///
/// Reads the file contents and delegates to [`parse_compose`].
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed.
#[must_use = "parsing result should be checked"]
pub fn parse_compose_file(path: &Path) -> anyhow::Result<ComposeProject> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read compose file: {}", path.display()))?;

    parse_compose(&content)
        .with_context(|| format!("failed to parse compose file: {}", path.display()))
}

/// Expands environment variables in a string using the given lookup function.
///
/// Supported syntaxes:
/// - `${VAR}` — replaced with the value of `VAR`, or empty string if unset
/// - `${VAR:-default}` — replaced with `VAR` if set and non-empty, otherwise `default`
/// - `${VAR-default}` — replaced with `VAR` if set (even if empty), otherwise `default`
#[must_use]
fn expand_env_vars<F>(input: &str, var_lookup: &F) -> String
where
    F: Fn(&str) -> Result<String, std::env::VarError>,
{
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            // Consume the '{'.
            chars.next();

            // Collect everything until '}'.
            let mut expr = String::new();
            for c in chars.by_ref() {
                if c == '}' {
                    break;
                }
                expr.push(c);
            }

            let expanded = resolve_var_expr(&expr, var_lookup);
            result.push_str(&expanded);
        } else {
            result.push(ch);
        }
    }

    result
}

/// Resolves a single variable expression (the part between `${` and `}`).
///
/// Handles `VAR`, `VAR:-default`, and `VAR-default` forms.
#[must_use]
fn resolve_var_expr<F>(expr: &str, var_lookup: &F) -> String
where
    F: Fn(&str) -> Result<String, std::env::VarError>,
{
    // Check for `:-` (colon-dash) default — uses default if unset OR empty.
    if let Some((var_name, default_val)) = expr.split_once(":-") {
        return match var_lookup(var_name) {
            Ok(val) if !val.is_empty() => val,
            _ => default_val.to_owned(),
        };
    }

    // Check for `-` (dash) default — uses default only if unset.
    if let Some((var_name, default_val)) = expr.split_once('-') {
        return match var_lookup(var_name) {
            Ok(val) => val,
            Err(_) => default_val.to_owned(),
        };
    }

    // Plain `${VAR}` — empty string if unset.
    var_lookup(expr).unwrap_or_default()
}

#[cfg(test)]
#[path = "parser_test.rs"]
mod tests;
