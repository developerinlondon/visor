use clap::Parser;

use super::{OutputFormat, format_size};
use crate::cli::{Cli, Command};

// ── visor images ────────────────────────────────────────────────

#[test]
fn images_args_default() {
    let cli = Cli::try_parse_from(["visor", "images"]).unwrap();
    match cli.command {
        Command::Images(args) => {
            assert!(matches!(args.format, OutputFormat::Table));
        }
        other => panic!("expected Images, got {other:?}"),
    }
}

#[test]
fn images_args_format_json() {
    let cli = Cli::try_parse_from(["visor", "images", "--format", "json"]).unwrap();
    match cli.command {
        Command::Images(args) => {
            assert!(matches!(args.format, OutputFormat::Json));
        }
        other => panic!("expected Images, got {other:?}"),
    }
}

#[test]
fn images_args_format_table() {
    let cli = Cli::try_parse_from(["visor", "images", "--format", "table"]).unwrap();
    match cli.command {
        Command::Images(args) => {
            assert!(matches!(args.format, OutputFormat::Table));
        }
        other => panic!("expected Images, got {other:?}"),
    }
}

#[test]
fn format_size_bytes() {
    assert_eq!(format_size(1_048_576), "1.0 MiB");
    assert_eq!(format_size(1_073_741_824), "1.0 GiB");
    assert_eq!(format_size(512), "512 B");
    assert_eq!(format_size(2048), "2.0 KiB");
}

#[test]
fn format_size_zero() {
    assert_eq!(format_size(0), "0 B");
}

#[test]
fn images_args_invalid_format() {
    let result = Cli::try_parse_from(["visor", "images", "--format", "yaml"]);
    assert!(result.is_err());
}

#[test]
fn image_info_deserializes_from_api_response() {
    // Simulates what the API actually returns
    let json = r#"{
        "reference": "docker.io/library/alpine:latest",
        "registry": "registry-1.docker.io",
        "repository": "library/alpine",
        "tag": "latest",
        "size_bytes": 3401024,
        "layers": 1
    }"#;
    let info: super::ImageInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.reference, "docker.io/library/alpine:latest");
    assert_eq!(info.size_bytes, 3_401_024);
}

#[test]
fn image_info_vec_deserializes() {
    let json = r#"[{
        "reference": "docker.io/library/alpine:latest",
        "registry": "registry-1.docker.io",
        "repository": "library/alpine",
        "tag": "latest",
        "size_bytes": 3401024,
        "layers": 1
    }]"#;
    let images: Vec<super::ImageInfo> = serde_json::from_str(json).unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].reference, "docker.io/library/alpine:latest");
}
