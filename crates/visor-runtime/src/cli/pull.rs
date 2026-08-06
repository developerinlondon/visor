//! `visor pull` — download an OCI image from a registry.
//!
//! Sends a pull request to the daemon, which downloads the manifest and
//! all layers into the local cache.

use anyhow::Context;

use super::PullArgs;

/// Executes the `visor pull` subcommand.
///
/// POSTs to `/v1/images/pull` with the image reference. Prints the
/// image reference on success.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the image cannot be found.
pub async fn execute(addr: &str, args: PullArgs) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/images/pull");

    eprintln!("Pulling {}...", args.image);

    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "image": args.image }))
        .timeout(std::time::Duration::from_secs(300))
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

    let info: serde_json::Value = resp.json().await.context("failed to parse pull response")?;

    let reference = info
        .get("reference")
        .and_then(|v| v.as_str())
        .unwrap_or(&args.image);
    let layers = info
        .get("layers")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    println!("{reference}: {layers} layers cached");
    Ok(())
}
