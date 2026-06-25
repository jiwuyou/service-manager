use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    error::{AppError, Result},
    model::Service,
};

pub async fn run_repair_hook(svc: &Service) -> Result<()> {
    let svc = svc.clone();
    tokio::task::spawn_blocking(move || run_repair_hook_blocking(&svc))
        .await
        .map_err(|_| AppError::Internal)?
}

fn run_repair_hook_blocking(svc: &Service) -> Result<()> {
    let mut hook = svc
        .spec
        .repair_hook()?
        .ok_or_else(|| AppError::BadRequest("repair hook is not configured".to_string()))?;
    hook.validate()?;

    let executable = hook.command[0].trim();
    let mut cmd = Command::new(executable);
    cmd.args(hook.command.iter().skip(1));

    let hook_working_dir = hook.working_dir.trim();
    let service_working_dir = svc.spec.working_dir.trim();
    let working_dir = if hook_working_dir.is_empty() {
        service_working_dir
    } else {
        hook_working_dir
    };
    if !working_dir.is_empty() {
        cmd.current_dir(working_dir);
    }

    cmd.env("SERVICE_MANAGER_SERVICE_ID", &svc.id.0)
        .env("SERVICE_MANAGER_SERVICE_NAME", &svc.spec.name)
        .env("SERVICE_MANAGER_PROVIDER", &svc.spec.provider.0)
        .envs(&hook.env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::BadRequest(format!("repair hook could not start: {}", e.kind())))?;

    let deadline = Instant::now() + hook.timeout.0;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let status_text = status
                .code()
                .map(|code| format!("exit code {code}"))
                .unwrap_or_else(|| "terminated by signal".to_string());
            return Err(AppError::BadRequest(format!(
                "repair hook failed: {status_text}"
            )));
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::BadRequest("repair hook timed out".to_string()));
        }

        thread::sleep(Duration::from_millis(50));
    }
}
