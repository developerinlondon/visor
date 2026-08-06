//! `visor images` — list cached OCI images.
//!
//! Queries the daemon for all cached OCI image metadata and prints
//! them in either table or JSON format.

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Arguments for the `visor images` subcommand.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct ImagesArgs {
    /// Output format.
    #[arg(long, default_value = "table")]
    pub format: OutputFormat,
}

/// Output format for image listing.
#[derive(Debug, Clone, clap::ValueEnum)]
#[non_exhaustive]
pub enum OutputFormat {
    /// Human-readable table.
    Table,
    /// JSON output.
    Json,
}

/// Cached OCI image metadata.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ImageInfo {
    /// OCI image reference (e.g., `docker.io/library/alpine:latest`).
    pub reference: String,
    /// Registry host.
    pub registry: String,
    /// Repository name.
    pub repository: String,
    /// Tag or digest.
    pub tag: String,
    /// Image size in bytes.
    pub size_bytes: u64,
    /// Number of layers.
    pub layers: usize,
}

/// Formats a byte count into a human-readable string.
///
/// Uses binary prefixes (KiB, MiB, GiB, TiB). Precision loss from
/// Uses binary prefixes (KiB, MiB, GiB, TiB) with integer arithmetic.
#[must_use]
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    const TIB: u64 = 1024 * GIB;

    if bytes >= TIB {
        let whole = bytes / TIB;
        let frac = (bytes % TIB) * 10 / TIB;
        format!("{whole}.{frac} TiB")
    } else if bytes >= GIB {
        let whole = bytes / GIB;
        let frac = (bytes % GIB) * 10 / GIB;
        format!("{whole}.{frac} GiB")
    } else if bytes >= MIB {
        let whole = bytes / MIB;
        let frac = (bytes % MIB) * 10 / MIB;
        format!("{whole}.{frac} MiB")
    } else if bytes >= KIB {
        let whole = bytes / KIB;
        let frac = (bytes % KIB) * 10 / KIB;
        format!("{whole}.{frac} KiB")
    } else {
        format!("{bytes} B")
    }
}

/// Executes the `visor images` subcommand.
///
/// GETs the list of cached images from the daemon's `/v1/images` endpoint
/// and prints them in the requested format.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the daemon returns a
/// non-success status.
pub async fn execute(args: &ImagesArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/images");

    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    let status = resp.status();
    if !status.is_success() {
        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse daemon error response")?;
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("daemon error ({status}): {msg}");
    }

    let images: Vec<ImageInfo> = resp.json().await.context("failed to parse image list")?;

    match args.format {
        OutputFormat::Table => {
            println!("{:<40}  {:<12}  {:>10}  LAYERS", "REFERENCE", "TAG", "SIZE");
            for img in &images {
                println!(
                    "{:<40}  {:<12}  {:>10}  {}",
                    img.reference,
                    img.tag,
                    format_size(img.size_bytes),
                    img.layers,
                );
            }
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&images)
                .context("failed to serialize images as JSON")?;
            println!("{json}");
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "images_test.rs"]
mod tests;
