//! `visor rmi` — remove one or more cached images.
//!
//! Sends DELETE requests to the daemon for each image reference.

use anyhow::Context;

use super::RmiArgs;

/// Executes the `visor rmi` subcommand.
///
/// DELETEs `/v1/images/{reference}` for each given image reference.
/// Prints removed image references on success.
///
/// # Errors
///
/// Returns an error if the HTTP request fails.
pub async fn execute(addr: &str, args: RmiArgs) -> anyhow::Result<()> {
    let client = super::http_client()?;

    for image in &args.images {
        let encoded = image.replace('/', "%2F").replace(':', "%3A");
        let url = format!("{addr}/v1/images/{encoded}");
        let resp = client
            .delete(&url)
            .send()
            .await
            .context("failed to connect to visor daemon")?;

        if !resp.status().is_success() {
            let body: serde_json::Value = resp
                .json()
                .await
                .context("failed to parse daemon error response")?;
            let msg = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            eprintln!("Error removing {image}: {msg}");
            continue;
        }

        println!("Untagged: {image}");
    }

    Ok(())
}
