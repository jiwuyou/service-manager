//! Provider implementations and registration helpers.
//!
//! This module is intentionally self-contained so the server can wire providers into its
//! `ProviderRegistry` without providers needing to touch server internals.

pub mod docker;
pub mod launchd;
pub mod process;
pub mod proot;
pub mod systemd;
pub mod termux;

use std::{path::PathBuf, sync::Arc};

use crate::{
    error::{AppError, Result},
    model::Provider,
    server::ProviderRegistry,
};

/// Register the default provider set into the registry.
///
/// Integration note (Lead): call this early during server startup (before creating `Engine`).
pub fn register_defaults(registry: &ProviderRegistry, data_dir: PathBuf) -> Result<()> {
    // Keep this list ordered by "most likely to work everywhere".
    let ps: Vec<Arc<dyn Provider>> = vec![
        Arc::new(process::ProcessProvider::new(data_dir.clone())),
        Arc::new(docker::DockerProvider::new()),
        Arc::new(systemd::SystemdProvider::new()),
        Arc::new(launchd::LaunchdProvider::new()),
        Arc::new(termux::TermuxServicesProvider::new()),
        Arc::new(proot::ProotDistroProvider::new(data_dir)),
    ];

    for p in ps {
        // ProviderRegistry::add already validates non-empty id and uniqueness.
        registry.add(p)?;
    }
    Ok(())
}

pub(crate) fn bad_request(msg: impl Into<String>) -> AppError {
    AppError::BadRequest(msg.into())
}

pub(crate) fn internal(msg: impl Into<String>) -> AppError {
    // We don't have a "structured internal error with message" in `AppError` yet.
    // For now, treat unexpected provider/runtime errors as BadRequest with context so
    // the caller gets actionable diagnostics.
    AppError::BadRequest(format!("internal: {}", msg.into()))
}

pub(crate) fn find_in_path(program: &str, extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    // Minimal `which` implementation; avoids spawning `which` and is portable enough for our needs.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(path) = std::env::var("PATH") {
        for part in path.split(':') {
            if part.trim().is_empty() {
                continue;
            }
            dirs.push(PathBuf::from(part));
        }
    }
    dirs.extend_from_slice(extra_dirs);

    for dir in dirs {
        let p = dir.join(program);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

pub(crate) async fn run_command_output(
    program: String,
    args: Vec<String>,
) -> Result<std::process::Output> {
    tokio::task::spawn_blocking(move || std::process::Command::new(program).args(args).output())
        .await
        .map_err(|e| internal(format!("join error: {e}")))?
        .map_err(AppError::Io)
}

pub(crate) fn lossy_stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub(crate) fn lossy_stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}
