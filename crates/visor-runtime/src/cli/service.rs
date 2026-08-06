//! `visor service` — install/uninstall visor as a system service.
//!
//! Generates systemd unit files (Linux) or launchd plist files (macOS)
//! and installs them to the appropriate system location.

use std::path::PathBuf;

use anyhow::Context;
use clap::Subcommand;

/// Subcommands for service management.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum ServiceCommand {
    /// Install visor as a system service.
    Install(ServiceInstallArgs),
    /// Uninstall the visor system service.
    Uninstall,
}

/// Arguments for `visor service install`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct ServiceInstallArgs {
    /// Listen address for the daemon.
    #[arg(long, default_value = "0.0.0.0:7800")]
    pub listen: String,
    /// Run in user mode (systemd `--user` / `LaunchAgents`).
    #[arg(long)]
    pub user: bool,
}

/// Platform service manager type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServicePlatform {
    /// systemd (Linux).
    Systemd,
    /// launchd (macOS).
    Launchd,
}

/// Detect the current platform's service manager.
///
/// # Errors
///
/// Returns an error if the platform is not supported (neither Linux nor macOS).
pub fn detect_platform() -> anyhow::Result<ServicePlatform> {
    if cfg!(target_os = "linux") {
        Ok(ServicePlatform::Systemd)
    } else if cfg!(target_os = "macos") {
        Ok(ServicePlatform::Launchd)
    } else {
        anyhow::bail!(
            "unsupported platform: service installation requires Linux (systemd) or macOS (launchd)"
        )
    }
}

/// Generate a systemd unit file for visor.
///
/// Produces a complete unit file with `[Unit]`, `[Service]`, and `[Install]`
/// sections configured to run the visor daemon.
#[must_use]
pub fn generate_systemd_unit(args: &ServiceInstallArgs) -> String {
    format!(
        "\
[Unit]
Description=Visor Container Runtime
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/visor start --listen {listen} --foreground
Restart=on-failure
RestartSec=5
LimitNOFILE=1048576
LimitNPROC=infinity
LimitCORE=infinity

[Install]
WantedBy=multi-user.target
",
        listen = args.listen,
    )
}

/// Generate a launchd plist for visor.
///
/// Produces a complete property list with the label `rs.visor.daemon`,
/// `ProgramArguments` pointing to the visor binary, and keep-alive enabled.
#[must_use]
pub fn generate_launchd_plist(args: &ServiceInstallArgs) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>rs.visor.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/visor</string>
        <string>start</string>
        <string>--listen</string>
        <string>{listen}</string>
        <string>--foreground</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
        listen = args.listen,
    )
}

/// Return the file path where the service file should be installed.
///
/// For systemd (Linux):
/// - System mode: `/etc/systemd/system/visor.service`
/// - User mode: `~/.config/systemd/user/visor.service`
///
/// For launchd (macOS):
/// - System mode: `/Library/LaunchDaemons/rs.visor.daemon.plist`
/// - User mode: `~/Library/LaunchAgents/rs.visor.daemon.plist`
#[must_use]
pub fn service_file_path(platform: ServicePlatform, user: bool) -> PathBuf {
    match (platform, user) {
        (ServicePlatform::Systemd, false) => PathBuf::from("/etc/systemd/system/visor.service"),
        (ServicePlatform::Systemd, true) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
            PathBuf::from(format!("{home}/.config/systemd/user/visor.service"))
        }
        (ServicePlatform::Launchd, false) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
            PathBuf::from(format!("{home}/Library/LaunchAgents/rs.visor.daemon.plist"))
        }
        (ServicePlatform::Launchd, true) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
            PathBuf::from(format!("{home}/Library/LaunchAgents/rs.visor.daemon.plist"))
        }
    }
}

/// Execute a service subcommand.
///
/// Detects the current platform, generates the appropriate service file,
/// and installs or uninstalls it.
///
/// # Errors
///
/// Returns an error if:
/// - The platform is not supported
/// - The service file cannot be written or removed
/// - Parent directories cannot be created
pub fn execute(cmd: ServiceCommand) -> anyhow::Result<()> {
    let platform = detect_platform()?;

    match cmd {
        ServiceCommand::Install(args) => {
            let path = service_file_path(platform, args.user);

            let content = match platform {
                ServicePlatform::Systemd => generate_systemd_unit(&args),
                ServicePlatform::Launchd => generate_launchd_plist(&args),
            };

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }

            std::fs::write(&path, &content)
                .with_context(|| format!("failed to write service file to {}", path.display()))?;

            println!("Installed service file: {}", path.display());

            match platform {
                ServicePlatform::Systemd => {
                    if args.user {
                        println!("Enable with: systemctl --user enable --now visor");
                    } else {
                        println!("Enable with: systemctl enable --now visor");
                    }
                }
                ServicePlatform::Launchd => {
                    println!("Load with: launchctl load {}", path.display());
                }
            }

            Ok(())
        }
        ServiceCommand::Uninstall => {
            let path = service_file_path(platform, false);

            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("failed to remove service file {}", path.display()))?;
                println!("Removed service file: {}", path.display());
            } else {
                println!("No service file found at {}", path.display());
            }

            match platform {
                ServicePlatform::Systemd => {
                    println!("Run: systemctl daemon-reload");
                }
                ServicePlatform::Launchd => {
                    println!("Run: launchctl unload {}", path.display());
                }
            }

            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "service_test.rs"]
mod tests;
