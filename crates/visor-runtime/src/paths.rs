use std::path::{Path, PathBuf};

use anyhow::Context;

pub(crate) fn visor_home_dir_from_env(
    visor_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(visor_home) = visor_home {
        return Ok(visor_home.to_path_buf());
    }

    let home_dir = home_dir.context("VISOR_HOME or HOME environment variable must be set")?;
    Ok(home_dir.join(".visor"))
}

pub(crate) fn persistent_subdir(name: &str) -> anyhow::Result<PathBuf> {
    persistent_subdir_from_env(
        name,
        std::env::var_os("VISOR_HOME").as_deref().map(Path::new),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

pub(crate) fn persistent_subdir_from_env(
    name: &str,
    visor_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    Ok(visor_home_dir_from_env(visor_home, home_dir)?.join(name))
}

pub(crate) fn daemon_log_path() -> anyhow::Result<PathBuf> {
    daemon_log_path_from_env(
        std::env::var_os("VISOR_HOME").as_deref().map(Path::new),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

pub(crate) fn daemon_log_path_from_env(
    visor_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    Ok(visor_home_dir_from_env(visor_home, home_dir)?.join("visor-daemon.log"))
}

pub(crate) fn best_effort_persistent_subdir(name: &str) -> PathBuf {
    best_effort_persistent_subdir_from_env(
        name,
        std::env::var_os("VISOR_HOME").as_deref().map(Path::new),
        std::env::var_os("HOME").as_deref().map(Path::new),
    )
}

pub(crate) fn best_effort_persistent_subdir_from_env(
    name: &str,
    visor_home: Option<&Path>,
    home_dir: Option<&Path>,
) -> PathBuf {
    persistent_subdir_from_env(name, visor_home, home_dir)
        .unwrap_or_else(|_| std::env::temp_dir().join(".visor").join(name))
}

#[cfg(test)]
#[path = "paths_test.rs"]
mod tests;
