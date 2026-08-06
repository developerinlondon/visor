use clap::Parser;

use crate::cli::{Cli, Command};

// ── visor push ──────────────────────────────────────────────────

#[test]
fn push_args_tag_required() {
    let result = Cli::try_parse_from(["visor", "push"]);
    assert!(result.is_err());
}

#[test]
fn push_args_parsed() {
    let cli = Cli::try_parse_from(["visor", "push", "myapp:latest"]).unwrap();
    match cli.command {
        Command::Push(args) => {
            assert_eq!(args.tag, "myapp:latest");
        }
        other => panic!("expected Push, got {other:?}"),
    }
}

#[test]
fn push_args_with_registry_prefix() {
    let cli = Cli::try_parse_from(["visor", "push", "registry.example.com/myapp:latest"]).unwrap();
    match cli.command {
        Command::Push(args) => {
            assert_eq!(args.tag, "registry.example.com/myapp:latest");
        }
        other => panic!("expected Push, got {other:?}"),
    }
}

#[test]
fn push_args_with_digest() {
    let cli = Cli::try_parse_from(["visor", "push", "myapp@sha256:abcdef1234567890"]).unwrap();
    match cli.command {
        Command::Push(args) => {
            assert_eq!(args.tag, "myapp@sha256:abcdef1234567890");
        }
        other => panic!("expected Push, got {other:?}"),
    }
}
