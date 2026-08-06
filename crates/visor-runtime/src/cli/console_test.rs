use super::*;
use clap::Parser;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::cli::{Cli, Command};

// ── visor console ───────────────────────────────────────────────

#[test]
fn console_args_parse() {
    let cli = Cli::try_parse_from(["visor", "console", "vm-123"]).unwrap();
    match cli.command {
        Command::Console(args) => {
            assert_eq!(args.vm_id, "vm-123");
            assert_eq!(args.escape_key, "^]");
        }
        other => panic!("expected Console, got {other:?}"),
    }
}

#[test]
fn console_requires_vm_id() {
    let result = Cli::try_parse_from(["visor", "console"]);
    assert!(result.is_err());
}

#[test]
fn console_args_with_escape_key() {
    let cli = Cli::try_parse_from(["visor", "console", "vm-123", "--escape-key", "^a"]).unwrap();
    match cli.command {
        Command::Console(args) => {
            assert_eq!(args.vm_id, "vm-123");
            assert_eq!(args.escape_key, "^a");
        }
        other => panic!("expected Console, got {other:?}"),
    }
}

#[test]
fn parse_escape_key_supports_ctrl_sequences() {
    assert_eq!(
        parse_escape_key("^]").unwrap(),
        EscapeKey {
            code: KeyCode::Char(']'),
            modifiers: KeyModifiers::CONTROL,
        }
    );
    assert_eq!(
        parse_escape_key("^a").unwrap(),
        EscapeKey {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::CONTROL,
        }
    );
}

#[test]
fn parse_escape_key_rejects_long_sequences() {
    assert!(parse_escape_key("^ab").is_err());
    assert!(parse_escape_key("abc").is_err());
}

#[test]
fn matches_escape_key_recognizes_control_binding() {
    let escape_key = parse_escape_key("^]").unwrap();
    let event = KeyEvent::new_with_kind(
        KeyCode::Char(']'),
        KeyModifiers::CONTROL,
        KeyEventKind::Press,
    );

    assert!(matches_escape_key(event, escape_key));
}

#[test]
fn console_connect_message_mentions_related_surfaces() {
    let message = console_connect_message(&ConsoleArgs {
        vm_id: "vm-123".to_owned(),
        escape_key: "^]".to_owned(),
    });

    assert!(message.contains("serial console for vm-123"));
    assert!(message.contains("detach with ^]"));
    assert!(message.contains("visor shell vm-123"));
    assert!(message.contains("visor exec vm-123 -- <cmd>"));
}
