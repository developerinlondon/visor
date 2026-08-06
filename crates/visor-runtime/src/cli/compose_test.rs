use clap::Parser;

use super::ComposeCommand;
use crate::cli::{Cli, Command};

// ── visor compose ───────────────────────────────────────────────

#[test]
fn compose_up_parse() {
    let cli = Cli::try_parse_from(["visor", "compose", "up"]).unwrap();
    match cli.command {
        Command::Compose(ComposeCommand::Up(args)) => {
            assert_eq!(args.file, "compose.yml");
            assert!(!args.detach);
        }
        other => panic!("expected Compose Up, got {other:?}"),
    }
}

#[test]
fn compose_down_parse() {
    let cli = Cli::try_parse_from(["visor", "compose", "down"]).unwrap();
    match cli.command {
        Command::Compose(ComposeCommand::Down(args)) => {
            assert_eq!(args.file, "compose.yml");
        }
        other => panic!("expected Compose Down, got {other:?}"),
    }
}

#[test]
fn compose_ps_parse() {
    let cli = Cli::try_parse_from(["visor", "compose", "ps"]).unwrap();
    match cli.command {
        Command::Compose(ComposeCommand::Ps) => {}
        other => panic!("expected Compose Ps, got {other:?}"),
    }
}

#[test]
fn compose_up_with_file() {
    let cli = Cli::try_parse_from(["visor", "compose", "up", "-f", "custom.yml"]).unwrap();
    match cli.command {
        Command::Compose(ComposeCommand::Up(args)) => {
            assert_eq!(args.file, "custom.yml");
        }
        other => panic!("expected Compose Up, got {other:?}"),
    }
}

#[test]
fn compose_up_detach() {
    let cli = Cli::try_parse_from(["visor", "compose", "up", "-d"]).unwrap();
    match cli.command {
        Command::Compose(ComposeCommand::Up(args)) => {
            assert!(args.detach);
        }
        other => panic!("expected Compose Up, got {other:?}"),
    }
}

#[test]
fn compose_logs_parse() {
    let cli = Cli::try_parse_from(["visor", "compose", "logs"]).unwrap();
    match cli.command {
        Command::Compose(ComposeCommand::Logs(args)) => {
            assert_eq!(args.file, "compose.yml");
            assert!(args.service.is_none());
        }
        other => panic!("expected Compose Logs, got {other:?}"),
    }
}

#[test]
fn compose_logs_with_service() {
    let cli = Cli::try_parse_from(["visor", "compose", "logs", "web"]).unwrap();
    match cli.command {
        Command::Compose(ComposeCommand::Logs(args)) => {
            assert_eq!(args.service.as_deref(), Some("web"));
        }
        other => panic!("expected Compose Logs, got {other:?}"),
    }
}
