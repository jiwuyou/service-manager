use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{
    error::Result,
    model::{
        Capability, DetectResult, LogEntry, LogsOptions, Provider, ProviderId, Service,
        ServiceState, ServiceStatus,
    },
};

use super::{bad_request, find_in_path, lossy_stderr, lossy_stdout, run_command_output};

#[derive(Clone, Debug, Default)]
pub struct DockerProvider;

impl DockerProvider {
    pub fn new() -> Self {
        Self
    }

    fn container_name(&self, svc: &Service) -> Result<String> {
        if let Some(v) = svc.spec.runtime.get("container_name") {
            if let Some(s) = v.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    return Ok(s.to_string());
                }
            }
            return Err(bad_request(
                "runtime.container_name must be a non-empty string",
            ));
        }
        Ok(svc.spec.name.clone())
    }

    fn image(&self, svc: &Service) -> Result<String> {
        let Some(v) = svc.spec.runtime.get("image") else {
            return Err(bad_request(
                "runtime.image is required for docker provider (e.g. \"nginx:latest\")",
            ));
        };
        let Some(s) = v.as_str() else {
            return Err(bad_request("runtime.image must be a string"));
        };
        let s = s.trim();
        if s.is_empty() {
            return Err(bad_request("runtime.image must be a non-empty string"));
        }
        Ok(s.to_string())
    }

    fn build_create_args(&self, svc: &Service, name: &str, image: &str) -> Vec<String> {
        let mut args = vec!["create".to_string(), "--name".to_string(), name.to_string()];

        if !svc.spec.working_dir.trim().is_empty() {
            args.push("--workdir".to_string());
            args.push(svc.spec.working_dir.clone());
        }

        for (k, v) in &svc.spec.env {
            args.push("-e".to_string());
            args.push(format!("{k}={v}"));
        }

        args.push(image.to_string());

        // Optional container command override.
        args.extend(svc.spec.command.iter().cloned());
        args
    }

    fn build_start_args(name: &str) -> Vec<String> {
        vec!["start".to_string(), name.to_string()]
    }

    fn build_stop_args(name: &str) -> Vec<String> {
        vec!["stop".to_string(), name.to_string()]
    }

    fn build_restart_args(name: &str) -> Vec<String> {
        vec!["restart".to_string(), name.to_string()]
    }

    fn build_rm_args(name: &str) -> Vec<String> {
        vec!["rm".to_string(), "-f".to_string(), name.to_string()]
    }

    fn build_inspect_format_args(name: &str, format: &str) -> Vec<String> {
        vec![
            "inspect".to_string(),
            "--format".to_string(),
            format.to_string(),
            name.to_string(),
        ]
    }

    fn daemon_unavailable(stderr: &str) -> bool {
        let s = stderr.to_ascii_lowercase();
        s.contains("cannot connect to the docker daemon")
            || s.contains("is the docker daemon running")
            || s.contains("error during connect")
            || s.contains("this error may indicate that the docker daemon is not running")
    }

    async fn ensure_cli(&self) -> Result<()> {
        if find_in_path("docker", &[]).is_none() {
            return Err(bad_request("docker CLI not found in PATH"));
        }
        Ok(())
    }

    async fn ensure_daemon(&self) -> Result<()> {
        // `docker info` is a decent "daemon reachable?" check.
        let out = run_command_output("docker".to_string(), vec!["info".to_string()]).await?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = lossy_stderr(&out);
        if Self::daemon_unavailable(&stderr) {
            return Err(bad_request(format!("docker daemon unavailable: {stderr}")));
        }
        Err(bad_request(format!("docker info failed: {}", stderr)))
    }

    async fn container_exists(&self, name: &str) -> Result<bool> {
        // If the daemon is down, this will fail; callers typically call ensure_daemon first.
        let out = run_command_output(
            "docker".to_string(),
            Self::build_inspect_format_args(name, "{{.Id}}"),
        )
        .await?;
        Ok(out.status.success())
    }

    fn parse_docker_timestamp(ts: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(ts.trim())
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }
}

#[async_trait]
impl Provider for DockerProvider {
    fn id(&self) -> ProviderId {
        ProviderId("docker".to_string())
    }

    fn display_name(&self) -> String {
        "Docker (CLI)".to_string()
    }

    fn description(&self) -> String {
        "Manage a container via docker CLI (create/start/stop/inspect/logs). Requires docker CLI and a reachable daemon."
            .to_string()
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Register,
            Capability::Unregister,
            Capability::Start,
            Capability::Stop,
            Capability::Restart,
            Capability::Status,
            Capability::Logs,
        ]
    }

    async fn detect(&self) -> Result<DetectResult> {
        if find_in_path("docker", &[]).is_none() {
            return Ok(DetectResult {
                detected: false,
                details: "docker CLI not found".to_string(),
            });
        }

        let out = run_command_output("docker".to_string(), vec!["info".to_string()]).await?;
        if out.status.success() {
            return Ok(DetectResult {
                detected: true,
                details: "ok".to_string(),
            });
        }

        let stderr = lossy_stderr(&out);
        if Self::daemon_unavailable(&stderr) {
            return Ok(DetectResult {
                detected: false,
                details: format!("daemon unavailable: {stderr}"),
            });
        }
        Ok(DetectResult {
            detected: false,
            details: format!("docker info failed: {stderr}"),
        })
    }

    async fn register(&self, svc: &Service) -> Result<()> {
        self.ensure_cli().await?;
        self.ensure_daemon().await?;

        let name = self.container_name(svc)?;
        if self.container_exists(&name).await? {
            return Ok(());
        }

        let image = self.image(svc)?;
        let args = self.build_create_args(svc, &name, &image);
        let out = run_command_output("docker".to_string(), args).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "docker create failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn unregister(&self, svc: &Service) -> Result<()> {
        self.ensure_cli().await?;
        self.ensure_daemon().await?;
        let name = self.container_name(svc)?;
        let out = run_command_output("docker".to_string(), Self::build_rm_args(&name)).await?;
        if !out.status.success() {
            // If it doesn't exist, treat as success.
            let stderr = lossy_stderr(&out);
            if stderr.to_ascii_lowercase().contains("no such container") {
                return Ok(());
            }
            return Err(bad_request(format!("docker rm failed: {stderr}")));
        }
        Ok(())
    }

    async fn start(&self, svc: &Service) -> Result<()> {
        self.ensure_cli().await?;
        self.ensure_daemon().await?;
        let name = self.container_name(svc)?;
        let out = run_command_output("docker".to_string(), Self::build_start_args(&name)).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "docker start failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn stop(&self, svc: &Service) -> Result<()> {
        self.ensure_cli().await?;
        self.ensure_daemon().await?;
        let name = self.container_name(svc)?;
        let out = run_command_output("docker".to_string(), Self::build_stop_args(&name)).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "docker stop failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn restart(&self, svc: &Service) -> Result<()> {
        self.ensure_cli().await?;
        self.ensure_daemon().await?;
        let name = self.container_name(svc)?;
        let out = run_command_output("docker".to_string(), Self::build_restart_args(&name)).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "docker restart failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn status(&self, svc: &Service) -> Result<ServiceStatus> {
        self.ensure_cli().await?;
        self.ensure_daemon().await?;
        let name = self.container_name(svc)?;
        let observed_at = Utc::now();

        let out = run_command_output(
            "docker".to_string(),
            Self::build_inspect_format_args(&name, "{{.State.Status}}"),
        )
        .await?;
        if !out.status.success() {
            return Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Unknown,
                message: format!("inspect failed: {}", lossy_stderr(&out)),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: None,
                exit_code: None,
            });
        }

        let status = lossy_stdout(&out);
        let (state, msg) = match status.as_str() {
            "running" => (ServiceState::Running, "running".to_string()),
            "exited" => (ServiceState::Stopped, "exited".to_string()),
            "created" => (ServiceState::Stopped, "created".to_string()),
            "paused" => (ServiceState::Running, "paused".to_string()),
            other if other.trim().is_empty() => (ServiceState::Unknown, "unknown".to_string()),
            other => (ServiceState::Unknown, format!("state={other}")),
        };

        // Best-effort pid/exit_code.
        let pid = run_command_output(
            "docker".to_string(),
            Self::build_inspect_format_args(&name, "{{.State.Pid}}"),
        )
        .await
        .ok()
        .and_then(|o| lossy_stdout(&o).parse::<u32>().ok())
        .filter(|p| *p != 0);

        let exit_code = run_command_output(
            "docker".to_string(),
            Self::build_inspect_format_args(&name, "{{.State.ExitCode}}"),
        )
        .await
        .ok()
        .and_then(|o| lossy_stdout(&o).parse::<i32>().ok());

        Ok(ServiceStatus {
            service_id: svc.id.clone(),
            state,
            message: msg,
            provider: self.id(),
            observed_at,
            started_at: None,
            pid,
            exit_code,
        })
    }

    async fn logs(&self, svc: &Service, opts: LogsOptions) -> Result<Vec<LogEntry>> {
        self.ensure_cli().await?;
        self.ensure_daemon().await?;
        let name = self.container_name(svc)?;

        let mut args = vec!["logs".to_string(), "--timestamps".to_string()];
        if let Some(n) = opts.limit {
            args.push("--tail".to_string());
            args.push(n.to_string());
        }
        if let Some(since) = opts.since {
            args.push("--since".to_string());
            args.push(since.to_rfc3339());
        }
        if let Some(until) = opts.until {
            args.push("--until".to_string());
            args.push(until.to_rfc3339());
        }
        args.push(name);

        let out = run_command_output("docker".to_string(), args).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "docker logs failed: {}",
                lossy_stderr(&out)
            )));
        }

        let s = String::from_utf8_lossy(&out.stdout);
        let mut entries = Vec::new();
        for line in s.lines() {
            let line = line.trim_end();
            if line.trim().is_empty() {
                continue;
            }
            // Format: "<rfc3339> <message...>"
            let (ts, msg) = match line.split_once(' ') {
                Some((a, b)) => (a, b),
                None => ("", line),
            };
            let t = Self::parse_docker_timestamp(ts).unwrap_or_else(Utc::now);
            entries.push(LogEntry {
                time: t,
                stream: "unknown".to_string(),
                message: msg.to_string(),
            });
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderId, ServiceId, ServiceSpec};
    use std::collections::BTreeMap;

    fn svc_with_runtime(runtime: serde_json::Value) -> Service {
        Service {
            id: ServiceId("svc1".to_string()),
            spec: ServiceSpec {
                name: "demo".to_string(),
                description: String::new(),
                provider: ProviderId("docker".to_string()),
                command: vec!["echo".to_string(), "hi".to_string()],
                working_dir: "/work".to_string(),
                env: BTreeMap::from_iter([("A".to_string(), "B".to_string())]),
                runtime: runtime
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
                restart: Default::default(),
                health: vec![],
                enabled: true,
                tags: vec![],
            },
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
        }
    }

    #[test]
    fn build_create_args_includes_name_env_workdir_and_command() {
        let p = DockerProvider::new();
        let svc =
            svc_with_runtime(serde_json::json!({"image":"alpine:3.20","container_name":"c1"}));
        let args = p.build_create_args(&svc, "c1", "alpine:3.20");
        assert_eq!(args[0], "create");
        assert!(args.windows(2).any(|w| w == ["--name", "c1"]));
        assert!(args.windows(2).any(|w| w == ["--workdir", "/work"]));
        assert!(args.windows(2).any(|w| w == ["-e", "A=B"]));
        assert!(args.contains(&"alpine:3.20".to_string()));
        assert!(args.ends_with(&["echo".to_string(), "hi".to_string()]));
    }

    #[test]
    fn daemon_unavailable_heuristic_matches_common_errors() {
        assert!(DockerProvider::daemon_unavailable(
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?"
        ));
        assert!(DockerProvider::daemon_unavailable("Error during connect"));
        assert!(!DockerProvider::daemon_unavailable("some other error"));
    }

    #[test]
    fn container_name_default_is_service_name() {
        let p = DockerProvider::new();
        let svc = svc_with_runtime(serde_json::json!({"image":"alpine:3.20"}));
        assert_eq!(p.container_name(&svc).unwrap(), "demo");
    }
}
