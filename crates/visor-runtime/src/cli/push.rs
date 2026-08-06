//! `visor push` — push an image to a registry.
//!
//! Sends a push request to the daemon for the given image tag. The
//! daemon-side push endpoint is not yet implemented (WS5.3), so this
//! command currently functions as a client-side stub that will integrate
//! with the server once the registry push workflow is complete.

use anyhow::Context;

use super::PushArgs;

/// Executes the `visor push` subcommand.
///
/// POSTs to `/images/{tag}/push` to initiate pushing the image to its
/// upstream registry. Streams the response and prints status updates.
///
/// The server-side push endpoint is scheduled for WS5.3. Until then
/// the daemon will return an error if contacted.
///
/// # Errors
///
/// Returns an error if the daemon connection fails or the push returns
/// an error response.
pub async fn execute(addr: &str, args: PushArgs) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let encoded = encode_tag(&args.tag);
    let url = format!("{addr}/images/{encoded}/push");

    eprintln!("Pushing {}...", args.tag);

    let resp = client
        .post(&url)
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
        anyhow::bail!("push failed ({status}): {msg}");
    }

    stream_push_response(resp, &args.tag).await
}

/// URL-encodes an image tag for use in API paths.
///
/// Replaces `/` and `:` with their percent-encoded equivalents so the
/// tag can be safely embedded in a URL path segment.
#[must_use]
fn encode_tag(tag: &str) -> String {
    tag.replace('/', "%2F").replace(':', "%3A")
}

/// Streams and prints the push response from the daemon.
///
/// Parses each line as JSON. Prints `{"status": "..."}` values and
/// fails on `{"error": "..."}` responses.
///
/// # Errors
///
/// Returns an error if the response body cannot be read or the push
/// produced an error.
async fn stream_push_response(resp: reqwest::Response, tag: &str) -> anyhow::Result<()> {
    let text = resp.text().await.context("failed to read push response")?;

    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(error) = obj.get("error").and_then(|v| v.as_str()) {
            anyhow::bail!("push error: {error}");
        }
        if let Some(s) = obj.get("status").and_then(|v| v.as_str()) {
            eprintln!("{s}");
        }
    }

    println!("Successfully pushed {tag}");
    Ok(())
}

#[cfg(test)]
#[path = "push_test.rs"]
mod tests;
