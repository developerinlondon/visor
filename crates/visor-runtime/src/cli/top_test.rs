use clap::Parser;

use crate::cli::{Cli, Command};

// ── visor top ───────────────────────────────────────────────────

#[test]
fn top_args_parse() {
    let cli = Cli::try_parse_from(["visor", "top", "vm-123"]).unwrap();
    match cli.command {
        Command::Top(args) => {
            assert_eq!(args.vm_id, "vm-123");
            assert_eq!(args.sort, "pid");
        }
        other => panic!("expected Top, got {other:?}"),
    }
}

#[test]
fn top_requires_vm_id() {
    let result = Cli::try_parse_from(["visor", "top"]);
    assert!(result.is_err());
}

#[test]
fn top_args_with_sort() {
    let cli = Cli::try_parse_from(["visor", "top", "vm-123", "--sort", "cpu"]).unwrap();
    match cli.command {
        Command::Top(args) => {
            assert_eq!(args.vm_id, "vm-123");
            assert_eq!(args.sort, "cpu");
        }
        other => panic!("expected Top, got {other:?}"),
    }
}
