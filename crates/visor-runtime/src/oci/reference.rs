//! OCI image reference parsing.
//!
//! Parses image references like `alpine:3.20`, `docker.io/library/ubuntu:latest`,
//! and `registry.example.com/repo@sha256:abc123...`.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, ensure};

/// Default registry when none is specified in the image reference.
const DEFAULT_REGISTRY: &str = "docker.io";

/// Default tag when neither tag nor digest is specified.
const DEFAULT_TAG: &str = "latest";

/// Prefix added to single-name repositories on Docker Hub.
const LIBRARY_PREFIX: &str = "library/";

/// A parsed OCI image reference.
///
/// Represents a fully qualified container image reference with registry,
/// repository, optional tag, and optional digest components.
///
/// # Examples
///
/// ```ignore
/// let r = ImageReference::parse("alpine:3.20").unwrap();
/// assert_eq!(r.registry().as_ref(), "docker.io");
/// assert_eq!(r.repository().as_ref(), "library/alpine");
/// assert_eq!(r.tag().unwrap().as_ref(), "3.20");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageReference {
    registry: Arc<str>,
    repository: Arc<str>,
    tag: Option<Arc<str>>,
    digest: Option<Arc<str>>,
}

impl ImageReference {
    /// Parses an OCI image reference string into its components.
    ///
    /// Handles bare names (`alpine`), tagged references (`alpine:3.20`),
    /// digest references (`alpine@sha256:...`), and fully qualified
    /// references (`ghcr.io/owner/repo:tag`).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The input is empty
    /// - The input contains invalid characters
    /// - A tag is empty (trailing `:`)
    /// - A digest is empty (trailing `@`)
    pub fn parse(input: &str) -> anyhow::Result<Self> {
        ensure!(!input.is_empty(), "image reference must not be empty");

        validate_characters(input).context("invalid image reference")?;

        let (name_part, tag, digest) =
            split_reference(input).context("failed to split image reference")?;

        let (registry, repository) =
            split_name(name_part).context("failed to parse registry/repository")?;

        // Apply default tag when neither tag nor digest is present.
        let tag = if tag.is_none() && digest.is_none() {
            Some(Arc::from(DEFAULT_TAG))
        } else {
            tag.map(Arc::from)
        };

        let digest = digest.map(Arc::from);

        Ok(Self {
            registry: Arc::from(registry),
            repository: Arc::from(repository),
            tag,
            digest,
        })
    }

    /// Returns the registry component (e.g. `docker.io`, `ghcr.io`).
    #[must_use]
    pub fn registry(&self) -> &Arc<str> {
        &self.registry
    }

    /// Returns the repository component (e.g. `library/alpine`, `owner/repo`).
    #[must_use]
    pub fn repository(&self) -> &Arc<str> {
        &self.repository
    }

    /// Returns the tag if present (e.g. `latest`, `3.20`).
    #[must_use]
    pub fn tag(&self) -> Option<&Arc<str>> {
        self.tag.as_ref()
    }

    /// Returns the digest if present (e.g. `sha256:abc123...`).
    #[must_use]
    pub fn digest(&self) -> Option<&Arc<str>> {
        self.digest.as_ref()
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        if let Some(tag) = &self.tag {
            write!(f, ":{tag}")?;
        }
        if let Some(digest) = &self.digest {
            write!(f, "@{digest}")?;
        }
        Ok(())
    }
}

impl FromStr for ImageReference {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Self::parse(s)
    }
}

/// Validates that the input only contains allowed characters for an OCI reference.
///
/// Allowed: alphanumeric, `.`, `-`, `_`, `/`, `:`, `@`
fn validate_characters(input: &str) -> anyhow::Result<()> {
    for ch in input.chars() {
        ensure!(
            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '/' | ':' | '@'),
            "invalid character '{ch}' in image reference"
        );
    }
    Ok(())
}

/// Splits a reference into `(name, Option<tag>, Option<digest>)`.
///
/// Handles `name`, `name:tag`, `name@digest`, and `name:tag@digest` forms.
fn split_reference(input: &str) -> anyhow::Result<(&str, Option<&str>, Option<&str>)> {
    // Split off digest first (rightmost @).
    let (before_digest, digest) = if let Some(at_pos) = input.rfind('@') {
        let digest_val = &input[at_pos + 1..];
        ensure!(!digest_val.is_empty(), "digest must not be empty after '@'");
        (&input[..at_pos], Some(digest_val))
    } else {
        (input, None)
    };

    // Now split the remaining part into name and tag.
    // We need to find the tag colon, but NOT a port colon in the registry.
    let (name, tag) = split_name_and_tag(before_digest).context("failed to split name and tag")?;

    Ok((name, tag, digest))
}

/// Splits `name_part` (without digest) into `(name, Option<tag>)`.
///
/// The tricky part: a colon in `registry.example.com:5000/repo` is a port,
/// not a tag separator. The tag colon is the LAST colon that appears AFTER
/// the last `/`.
fn split_name_and_tag(input: &str) -> anyhow::Result<(&str, Option<&str>)> {
    // Find the last `/` — if there's a colon after it, that's the tag.
    if let Some(last_slash) = input.rfind('/') {
        let after_slash = &input[last_slash + 1..];
        if let Some(colon_in_last) = after_slash.rfind(':') {
            let tag = &after_slash[colon_in_last + 1..];
            ensure!(!tag.is_empty(), "tag must not be empty after ':'");
            let name_end = last_slash + 1 + colon_in_last;
            return Ok((&input[..name_end], Some(tag)));
        }
        // No colon after last slash — no tag.
        Ok((input, None))
    } else {
        // No slash at all — simple name, possibly with tag.
        if let Some(colon_pos) = input.rfind(':') {
            let tag = &input[colon_pos + 1..];
            ensure!(!tag.is_empty(), "tag must not be empty after ':'");
            Ok((&input[..colon_pos], Some(tag)))
        } else {
            Ok((input, None))
        }
    }
}

/// Splits the name portion into `(registry, repository)`.
///
/// Registry detection: the first path component is a registry if it contains
/// `.` or `:` (port), or if it is `localhost`. Otherwise, Docker Hub is assumed.
fn split_name(name: &str) -> anyhow::Result<(&str, String)> {
    if let Some(slash_pos) = name.find('/') {
        let first_component = &name[..slash_pos];
        let rest = &name[slash_pos + 1..];

        if is_registry(first_component) {
            ensure!(!rest.is_empty(), "repository must not be empty");
            return Ok((first_component, rest.to_owned()));
        }

        // Not a registry — it's a user/repo on Docker Hub.
        Ok((DEFAULT_REGISTRY, name.to_owned()))
    } else {
        // Single name — Docker Hub library image.
        let repository = format!("{LIBRARY_PREFIX}{name}");
        Ok((DEFAULT_REGISTRY, repository))
    }
}

/// Returns `true` if the component looks like a registry hostname.
///
/// A component is a registry if it contains `.` (domain), `:` (port),
/// or is literally `localhost`.
fn is_registry(component: &str) -> bool {
    component.contains('.') || component.contains(':') || component == "localhost"
}

#[cfg(test)]
#[path = "reference_test.rs"]
mod tests;
