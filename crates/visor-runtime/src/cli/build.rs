//! `visor build` — build an image from a Dockerfile.
//!
//! Packages the build context directory as a tar archive and sends it to
//! the daemon for building. Build output is streamed line by line, printing
//! `{"stream": "..."}` values and failing on `{"error": "..."}` responses.

use std::path::Path;

use anyhow::Context;

use super::BuildArgs;

/// Executes the `visor build` subcommand.
///
/// Packages the build context directory as a tar archive and POSTs it to
/// the daemon's `/build` endpoint with the specified build options.
/// Build output is streamed line by line.
///
/// # Errors
///
/// Returns an error if the context directory cannot be read, the daemon
/// connection fails, or the build returns an error response.
pub async fn execute(addr: &str, args: BuildArgs) -> anyhow::Result<()> {
    let client = super::http_client()?;

    let context_path = Path::new(&args.context);
    anyhow::ensure!(
        context_path.exists(),
        "build context directory '{}' does not exist",
        args.context
    );
    anyhow::ensure!(
        context_path.is_dir(),
        "build context '{}' is not a directory",
        args.context
    );

    let tar_body = package_context(context_path).context("failed to package build context")?;

    let query_params = build_query_params(&args).context("failed to build query parameters")?;

    if !args.quiet {
        if let Some(ref tag) = args.tag {
            eprintln!("Building {tag} from {}...", args.context);
        } else {
            eprintln!("Building from {}...", args.context);
        }
    }

    let query_string = build_query_string(&query_params);
    let url = format!("{addr}/build?{query_string}");

    let resp = client
        .post(&url)
        .header("Content-Type", "application/x-tar")
        .body(tar_body)
        .timeout(std::time::Duration::from_secs(600))
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
        anyhow::bail!("build failed ({status}): {msg}");
    }

    stream_build_output(resp, args.quiet).await
}

/// Builds the query parameter list from [`BuildArgs`].
///
/// # Errors
///
/// Returns an error if build arguments cannot be serialized to JSON.
fn build_query_params(args: &BuildArgs) -> anyhow::Result<Vec<(String, String)>> {
    let mut params = vec![("dockerfile".to_owned(), args.file.clone())];

    if let Some(ref tag) = args.tag {
        params.push(("t".to_owned(), tag.clone()));
    }
    if let Some(ref target) = args.target {
        params.push(("target".to_owned(), target.clone()));
    }
    if args.no_cache {
        params.push(("nocache".to_owned(), "1".to_owned()));
    }
    if !args.build_arg.is_empty() {
        let build_args_json =
            serde_json::to_string(&args.build_arg).context("failed to serialize build args")?;
        params.push(("buildargs".to_owned(), build_args_json));
    }

    Ok(params)
}

/// Formats query parameters into a URL query string.
///
/// Percent-encodes values that may contain special characters.
fn build_query_string(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| {
            let encoded_v = v
                .replace('%', "%25")
                .replace('&', "%26")
                .replace('=', "%3D")
                .replace('+', "%2B")
                .replace(' ', "%20");
            format!("{k}={encoded_v}")
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Packages a directory into a tar archive in memory.
///
/// Creates an in-memory tar archive containing all files from the given
/// directory, rooted at `.` within the archive.
///
/// # Errors
///
/// Returns an error if the directory cannot be read or the tar archive
/// cannot be constructed.
fn package_context(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut archive = tar::Builder::new(Vec::new());
    archive
        .append_dir_all(".", path)
        .context("failed to add directory to tar archive")?;
    archive
        .into_inner()
        .context("failed to finalize tar archive")
}

/// Streams build output from the daemon response.
///
/// Parses each line as JSON. Prints `{"stream": "..."}` values to stdout
/// (unless quiet mode). Returns an error on `{"error": "..."}` responses.
///
/// # Errors
///
/// Returns an error if the response body cannot be read or the build
/// produced an error.
async fn stream_build_output(resp: reqwest::Response, quiet: bool) -> anyhow::Result<()> {
    let text = resp.text().await.context("failed to read build output")?;

    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(error) = obj.get("error").and_then(|v| v.as_str()) {
            anyhow::bail!("build error: {error}");
        }
        if !quiet {
            if let Some(stream) = obj.get("stream").and_then(|v| v.as_str()) {
                print!("{stream}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "build_test.rs"]
mod tests;
