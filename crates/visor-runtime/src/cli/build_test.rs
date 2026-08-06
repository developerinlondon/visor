use clap::Parser;

use crate::cli::{Cli, Command};

// ── visor build ─────────────────────────────────────────────────

#[test]
fn build_args_default_context() {
    let cli = Cli::try_parse_from(["visor", "build"]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.context, ".");
            assert_eq!(args.file, "Dockerfile");
            assert!(args.tag.is_none());
            assert!(args.build_arg.is_empty());
            assert!(args.target.is_none());
            assert!(!args.no_cache);
            assert!(!args.quiet);
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_with_tag() {
    let cli = Cli::try_parse_from(["visor", "build", "-t", "myapp:latest"]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.tag.as_deref(), Some("myapp:latest"));
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_with_build_arg() {
    let cli = Cli::try_parse_from(["visor", "build", "--build-arg", "KEY=VAL"]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.build_arg, vec!["KEY=VAL"]);
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_with_target() {
    let cli = Cli::try_parse_from(["visor", "build", "--target", "stage"]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.target.as_deref(), Some("stage"));
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_no_cache() {
    let cli = Cli::try_parse_from(["visor", "build", "--no-cache"]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert!(args.no_cache);
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_quiet() {
    let cli = Cli::try_parse_from(["visor", "build", "-q"]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert!(args.quiet);
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_custom_context() {
    let cli = Cli::try_parse_from(["visor", "build", "./myapp"]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.context, "./myapp");
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_custom_dockerfile() {
    let cli = Cli::try_parse_from(["visor", "build", "-f", "Dockerfile.prod", "."]).unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.file, "Dockerfile.prod");
            assert_eq!(args.context, ".");
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_multiple_build_args() {
    let cli = Cli::try_parse_from([
        "visor",
        "build",
        "--build-arg",
        "KEY1=val1",
        "--build-arg",
        "KEY2=val2",
    ])
    .unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.build_arg, vec!["KEY1=val1", "KEY2=val2"]);
        }
        other => panic!("expected Build, got {other:?}"),
    }
}

#[test]
fn build_args_all_options() {
    let cli = Cli::try_parse_from([
        "visor",
        "build",
        "-t",
        "myapp:v1",
        "-f",
        "Dockerfile.dev",
        "--build-arg",
        "ENV=prod",
        "--target",
        "runtime",
        "--no-cache",
        "-q",
        "./src",
    ])
    .unwrap();
    match cli.command {
        Command::Build(args) => {
            assert_eq!(args.tag.as_deref(), Some("myapp:v1"));
            assert_eq!(args.file, "Dockerfile.dev");
            assert_eq!(args.build_arg, vec!["ENV=prod"]);
            assert_eq!(args.target.as_deref(), Some("runtime"));
            assert!(args.no_cache);
            assert!(args.quiet);
            assert_eq!(args.context, "./src");
        }
        other => panic!("expected Build, got {other:?}"),
    }
}
