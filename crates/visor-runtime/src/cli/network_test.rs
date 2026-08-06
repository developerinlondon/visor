use clap::Parser;

use super::NetworkCommand;
use crate::cli::{Cli, Command};

// ── visor network ───────────────────────────────────────────────

#[test]
fn network_create_parse() {
    let cli = Cli::try_parse_from(["visor", "network", "create", "mynet"]).unwrap();
    match cli.command {
        Command::Network(NetworkCommand::Create(args)) => {
            assert_eq!(args.name, "mynet");
        }
        other => panic!("expected Network Create, got {other:?}"),
    }
}

#[test]
fn network_ls_parse() {
    let cli = Cli::try_parse_from(["visor", "network", "ls"]).unwrap();
    match cli.command {
        Command::Network(NetworkCommand::Ls) => {}
        other => panic!("expected Network Ls, got {other:?}"),
    }
}

#[test]
fn network_rm_parse() {
    let cli = Cli::try_parse_from(["visor", "network", "rm", "net-123"]).unwrap();
    match cli.command {
        Command::Network(NetworkCommand::Rm(args)) => {
            assert_eq!(args.name, "net-123");
        }
        other => panic!("expected Network Rm, got {other:?}"),
    }
}

#[test]
fn network_connect_parse() {
    let cli = Cli::try_parse_from(["visor", "network", "connect", "net-123", "vm-456"]).unwrap();
    match cli.command {
        Command::Network(NetworkCommand::Connect(args)) => {
            assert_eq!(args.network, "net-123");
            assert_eq!(args.vm_id, "vm-456");
        }
        other => panic!("expected Network Connect, got {other:?}"),
    }
}

#[test]
fn network_disconnect_parse() {
    let cli = Cli::try_parse_from(["visor", "network", "disconnect", "net-123", "vm-456"]).unwrap();
    match cli.command {
        Command::Network(NetworkCommand::Disconnect(args)) => {
            assert_eq!(args.network, "net-123");
            assert_eq!(args.vm_id, "vm-456");
        }
        other => panic!("expected Network Disconnect, got {other:?}"),
    }
}

#[test]
fn network_inspect_parse() {
    let cli = Cli::try_parse_from(["visor", "network", "inspect", "net-123"]).unwrap();
    match cli.command {
        Command::Network(NetworkCommand::Inspect(args)) => {
            assert_eq!(args.name, "net-123");
        }
        other => panic!("expected Network Inspect, got {other:?}"),
    }
}
