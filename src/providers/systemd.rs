use std::{fs, path::PathBuf};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    User,
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SystemdOperationalState {
    Initializing,
    Starting,
    Running,
    Degraded,
    Maintenance,
    Stopping,
    Offline,
    Unknown,
}

impl SystemdOperationalState {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "initializing" => Some(Self::Initializing),
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "degraded" => Some(Self::Degraded),
            "maintenance" => Some(Self::Maintenance),
            "stopping" => Some(Self::Stopping),
            "offline" => Some(Self::Offline),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    fn is_available(self) -> bool {
        // Per task requirements: treat "offline"/failure as unavailable; allow degraded.
        !matches!(self, Self::Offline | Self::Unknown)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SystemdProvider;

impl SystemdProvider {
    pub fn new() -> Self {
        Self
    }

    fn scope(&self, svc: &Service) -> Result<Scope> {
        match svc.spec.runtime.get("scope") {
            None => Ok(Scope::User),
            Some(v) => {
                let Some(s) = v.as_str() else {
                    return Err(bad_request(
                        "runtime.scope must be a string (\"user\"|\"system\")",
                    ));
                };
                match s.trim() {
                    "" | "user" => Ok(Scope::User),
                    "system" => Ok(Scope::System),
                    other => Err(bad_request(format!(
                        "runtime.scope must be \"user\" or \"system\" (got {other:?})"
                    ))),
                }
            }
        }
    }

    fn unit_name(&self, svc: &Service) -> Result<String> {
        if let Some(v) = svc.spec.runtime.get("unit") {
            let Some(s) = v.as_str() else {
                return Err(bad_request("runtime.unit must be a string"));
            };
            let s = s.trim();
            if s.is_empty() {
                return Err(bad_request("runtime.unit must be non-empty"));
            }
            if s.ends_with(".service") {
                return Ok(s.to_string());
            }
            return Ok(format!("{s}.service"));
        }
        Ok(format!("service-manager-{}.service", svc.spec.name))
    }

    fn systemctl_base_args(scope: Scope) -> Vec<String> {
        match scope {
            Scope::User => vec!["--user".to_string()],
            Scope::System => vec![],
        }
    }

    async fn systemctl(scope: Scope, mut args: Vec<String>) -> Result<std::process::Output> {
        let mut all = Self::systemctl_base_args(scope);
        all.append(&mut args);
        run_command_output("systemctl".to_string(), all).await
    }

    async fn journalctl(scope: Scope, mut args: Vec<String>) -> Result<std::process::Output> {
        let mut all = match scope {
            Scope::User => vec!["--user".to_string()],
            Scope::System => vec![],
        };
        all.append(&mut args);
        run_command_output("journalctl".to_string(), all).await
    }

    fn unit_file_path(&self, unit: &str) -> Result<PathBuf> {
        // User-scoped unit files live in ~/.config/systemd/user
        let home = std::env::var_os("HOME").ok_or_else(|| bad_request("HOME not set"))?;
        Ok(PathBuf::from(home)
            .join(".config")
            .join("systemd")
            .join("user")
            .join(unit))
    }

    fn escape_systemd_value(s: &str) -> String {
        // systemd expands specifiers (%i, etc.) unless escaped via %%.
        s.replace('%', "%%")
    }

    fn quote_item_if_needed(s: &str) -> String {
        let needs = s
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\');
        if !needs {
            return Self::escape_systemd_value(s);
        }
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '%' => out.push_str("%%"),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    fn unit_file_contents(&self, svc: &Service, unit: &str) -> Result<String> {
        if svc.spec.command.is_empty() {
            return Err(bad_request("command is required for systemd provider"));
        }
        let desc = if svc.spec.description.trim().is_empty() {
            svc.spec.name.clone()
        } else {
            svc.spec.description.clone()
        };

        let mut out = String::new();
        out.push_str("[Unit]\n");
        out.push_str("Description=");
        out.push_str(&Self::escape_systemd_value(&desc));
        out.push('\n');
        out.push('\n');

        out.push_str("[Service]\n");
        out.push_str("Type=simple\n");

        if !svc.spec.working_dir.trim().is_empty() {
            out.push_str("WorkingDirectory=");
            out.push_str(&Self::escape_systemd_value(&svc.spec.working_dir));
            out.push('\n');
        }

        for (k, v) in &svc.spec.env {
            let assign = format!("{k}={v}");
            out.push_str("Environment=");
            out.push_str(&Self::quote_item_if_needed(&assign));
            out.push('\n');
        }

        // Restart policy (best-effort; doesn't map 1:1).
        let restart = match svc
            .spec
            .restart
            .mode
            .unwrap_or(crate::model::RestartMode::No)
        {
            crate::model::RestartMode::No => "no",
            crate::model::RestartMode::OnFailure => "on-failure",
            crate::model::RestartMode::Always => "always",
        };
        out.push_str("Restart=");
        out.push_str(restart);
        out.push('\n');

        out.push_str("ExecStart=");
        for (i, part) in svc.spec.command.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&Self::quote_item_if_needed(part));
        }
        out.push('\n');
        out.push('\n');

        out.push_str("[Install]\n");
        out.push_str("WantedBy=default.target\n");

        // Let the unit name appear somewhere in the file to aid debugging.
        out.push_str("\n# unit=");
        out.push_str(unit);
        out.push('\n');

        Ok(out)
    }

    fn parse_systemctl_show(output: &str) -> std::collections::BTreeMap<String, String> {
        let mut m = std::collections::BTreeMap::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        m
    }

    fn parse_journal_line(line: &str) -> Option<LogEntry> {
        // journalctl -o short-iso: "<rfc3339-ish> <rest...>"
        let line = line.trim_end();
        if line.trim().is_empty() {
            return None;
        }
        let (ts, rest) = line.split_once(' ')?;

        // short-iso uses RFC3339 profile but with timezone like +0200 (no colon).
        // chrono accepts %z without colon; parse via strptime first.
        let time = DateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.f%z")
            .or_else(|_| DateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%z"))
            .or_else(|_| DateTime::parse_from_rfc3339(ts))
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        Some(LogEntry {
            time,
            stream: "system".to_string(),
            message: rest.to_string(),
        })
    }
}

#[async_trait]
impl Provider for SystemdProvider {
    fn id(&self) -> ProviderId {
        ProviderId("systemd".to_string())
    }

    fn display_name(&self) -> String {
        "systemd".to_string()
    }

    fn description(&self) -> String {
        "Manage a service as a systemd unit (default: --user scope).".to_string()
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
        if find_in_path("systemctl", &[]).is_none() {
            return Ok(DetectResult {
                detected: false,
                details: "systemctl not found".to_string(),
            });
        }

        // Check that systemd is actually the manager (and not "offline"/"unknown").
        // We run both user+system probes; if either is usable, consider systemd present.
        let user = Self::systemctl(Scope::User, vec!["is-system-running".to_string()]).await;
        let sys = Self::systemctl(Scope::System, vec!["is-system-running".to_string()]).await;

        let mut details = Vec::new();

        if let Ok(o) = user {
            let stdout = lossy_stdout(&o);
            let stderr = lossy_stderr(&o);
            let state = SystemdOperationalState::parse(&stdout);
            details.push(format!(
                "user={}{} ({})",
                stdout,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(" stderr={stderr:?}")
                },
                if state.map(|s| s.is_available()).unwrap_or(false) {
                    "available"
                } else {
                    "unavailable"
                }
            ));
            if let Some(s) = state
                && s.is_available()
            {
                return Ok(DetectResult {
                    detected: true,
                    details: details.join(", "),
                });
            }
        } else {
            details.push("user=error".to_string());
        }

        if let Ok(o) = sys {
            let stdout = lossy_stdout(&o);
            let stderr = lossy_stderr(&o);
            let state = SystemdOperationalState::parse(&stdout);
            details.push(format!(
                "system={}{} ({})",
                stdout,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(" stderr={stderr:?}")
                },
                if state.map(|s| s.is_available()).unwrap_or(false) {
                    "available"
                } else {
                    "unavailable"
                }
            ));
            if let Some(s) = state
                && s.is_available()
            {
                return Ok(DetectResult {
                    detected: true,
                    details: details.join(", "),
                });
            }
        } else {
            details.push("system=error".to_string());
        }

        Ok(DetectResult {
            detected: false,
            details: details.join(", "),
        })
    }

    async fn register(&self, svc: &Service) -> Result<()> {
        let scope = self.scope(svc)?;
        if scope == Scope::System {
            return Err(bad_request(
                "systemd provider supports only user units in this build (runtime.scope=\"user\")",
            ));
        }
        if find_in_path("systemctl", &[]).is_none() {
            return Err(bad_request("systemctl not found"));
        }

        let unit = self.unit_name(svc)?;
        let path = self.unit_file_path(&unit)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = self.unit_file_contents(svc, &unit)?;
        fs::write(&path, content)?;

        // Reload user manager so it sees the new unit file.
        let out = Self::systemctl(scope, vec!["daemon-reload".to_string()]).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "systemctl daemon-reload failed: {}",
                lossy_stderr(&out)
            )));
        }

        if svc.spec.enabled {
            let out = Self::systemctl(scope, vec!["enable".to_string(), unit]).await?;
            if !out.status.success() {
                return Err(bad_request(format!(
                    "systemctl enable failed: {}",
                    lossy_stderr(&out)
                )));
            }
        } else {
            // Best-effort disable; ignore errors for "not enabled".
            let _ = Self::systemctl(scope, vec!["disable".to_string(), unit]).await;
        }

        Ok(())
    }

    async fn unregister(&self, svc: &Service) -> Result<()> {
        let scope = self.scope(svc)?;
        if scope == Scope::System {
            return Err(bad_request(
                "systemd provider supports only user units in this build (runtime.scope=\"user\")",
            ));
        }
        if find_in_path("systemctl", &[]).is_none() {
            return Err(bad_request("systemctl not found"));
        }

        let unit = self.unit_name(svc)?;
        let _ = Self::systemctl(scope, vec!["stop".to_string(), unit.clone()]).await;
        let _ = Self::systemctl(scope, vec!["disable".to_string(), unit.clone()]).await;

        let path = self.unit_file_path(&unit)?;
        let _ = fs::remove_file(&path);

        let _ = Self::systemctl(scope, vec!["daemon-reload".to_string()]).await;
        Ok(())
    }

    async fn start(&self, svc: &Service) -> Result<()> {
        let scope = self.scope(svc)?;
        if scope == Scope::System {
            return Err(bad_request(
                "systemd provider supports only user units in this build (runtime.scope=\"user\")",
            ));
        }
        let unit = self.unit_name(svc)?;
        let out = Self::systemctl(scope, vec!["start".to_string(), unit]).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "systemctl start failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn stop(&self, svc: &Service) -> Result<()> {
        let scope = self.scope(svc)?;
        if scope == Scope::System {
            return Err(bad_request(
                "systemd provider supports only user units in this build (runtime.scope=\"user\")",
            ));
        }
        let unit = self.unit_name(svc)?;
        let out = Self::systemctl(scope, vec!["stop".to_string(), unit]).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "systemctl stop failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn restart(&self, svc: &Service) -> Result<()> {
        let scope = self.scope(svc)?;
        if scope == Scope::System {
            return Err(bad_request(
                "systemd provider supports only user units in this build (runtime.scope=\"user\")",
            ));
        }
        let unit = self.unit_name(svc)?;
        let out = Self::systemctl(scope, vec!["restart".to_string(), unit]).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "systemctl restart failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn status(&self, svc: &Service) -> Result<ServiceStatus> {
        let scope = self.scope(svc)?;
        let unit = self.unit_name(svc)?;
        let observed_at = Utc::now();

        if find_in_path("systemctl", &[]).is_none() {
            return Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Unknown,
                message: "systemctl not found".to_string(),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: None,
                exit_code: None,
            });
        }

        // Use `show` for stable parsing.
        let out = Self::systemctl(
            scope,
            vec![
                "show".to_string(),
                unit.clone(),
                "-p".to_string(),
                "ActiveState".to_string(),
                "-p".to_string(),
                "SubState".to_string(),
                "-p".to_string(),
                "MainPID".to_string(),
                "-p".to_string(),
                "ExecMainStatus".to_string(),
            ],
        )
        .await?;
        if !out.status.success() {
            return Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Unknown,
                message: format!("systemctl show failed: {}", lossy_stderr(&out)),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: None,
                exit_code: None,
            });
        }

        let props = Self::parse_systemctl_show(&lossy_stdout(&out));
        let active = props.get("ActiveState").cloned().unwrap_or_default();
        let sub = props.get("SubState").cloned().unwrap_or_default();
        let pid = props
            .get("MainPID")
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|p| *p != 0);
        let exit_code = props
            .get("ExecMainStatus")
            .and_then(|s| s.parse::<i32>().ok());

        let state = match active.as_str() {
            "active" => ServiceState::Running,
            "inactive" => ServiceState::Stopped,
            "failed" => ServiceState::Failed,
            "activating" => ServiceState::Starting,
            "deactivating" => ServiceState::Stopping,
            _ => ServiceState::Unknown,
        };

        Ok(ServiceStatus {
            service_id: svc.id.clone(),
            state,
            message: if sub.is_empty() {
                active
            } else {
                format!("{active}/{sub}")
            },
            provider: self.id(),
            observed_at,
            started_at: None,
            pid,
            exit_code,
        })
    }

    async fn logs(&self, svc: &Service, opts: LogsOptions) -> Result<Vec<LogEntry>> {
        let scope = self.scope(svc)?;
        if find_in_path("journalctl", &[]).is_none() {
            return Err(bad_request("journalctl not found"));
        }

        let unit = self.unit_name(svc)?;
        let mut args = vec![
            "-u".to_string(),
            unit,
            "--no-pager".to_string(),
            "--output".to_string(),
            "short-iso".to_string(),
            "--truncate-newline".to_string(),
        ];
        if let Some(n) = opts.limit {
            args.push("-n".to_string());
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

        let out = Self::journalctl(scope, args).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "journalctl failed: {}",
                lossy_stderr(&out)
            )));
        }

        let s = String::from_utf8_lossy(&out.stdout);
        Ok(s.lines().filter_map(Self::parse_journal_line).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderId, ServiceId, ServiceSpec};
    use std::collections::BTreeMap;

    fn svc() -> Service {
        Service {
            id: ServiceId("svc1".to_string()),
            spec: ServiceSpec {
                name: "demo".to_string(),
                description: "Demo service".to_string(),
                provider: ProviderId("systemd".to_string()),
                command: vec!["/bin/echo".to_string(), "hi".to_string()],
                working_dir: "/tmp".to_string(),
                env: BTreeMap::from_iter([("A".to_string(), "B".to_string())]),
                runtime: BTreeMap::new(),
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
    fn operational_state_parse() {
        assert_eq!(
            SystemdOperationalState::parse("running"),
            Some(SystemdOperationalState::Running)
        );
        assert_eq!(
            SystemdOperationalState::parse("OFFLINE\n"),
            Some(SystemdOperationalState::Offline)
        );
        assert_eq!(SystemdOperationalState::parse("???"), None);
    }

    #[test]
    fn operational_state_availability() {
        assert!(SystemdOperationalState::Running.is_available());
        assert!(SystemdOperationalState::Degraded.is_available());
        assert!(!SystemdOperationalState::Offline.is_available());
        assert!(!SystemdOperationalState::Unknown.is_available());
    }

    #[test]
    fn unit_name_default_is_prefixed_service_name() {
        let p = SystemdProvider::new();
        let svc = svc();
        assert_eq!(p.unit_name(&svc).unwrap(), "service-manager-demo.service");
    }

    #[test]
    fn unit_file_contents_quotes_environment_assignment() {
        let p = SystemdProvider::new();
        let mut svc = svc();
        svc.spec
            .env
            .insert("SPACEY".to_string(), "hello world".to_string());
        let unit = p.unit_name(&svc).unwrap();
        let s = p.unit_file_contents(&svc, &unit).unwrap();
        assert!(s.contains("Environment="));
        assert!(s.contains("SPACEY=hello world") || s.contains("SPACEY=hello\\sworld"));
    }

    #[test]
    fn parse_systemctl_show_extracts_key_values() {
        let m = SystemdProvider::parse_systemctl_show("ActiveState=active\nMainPID=123\n");
        assert_eq!(m.get("ActiveState").unwrap(), "active");
        assert_eq!(m.get("MainPID").unwrap(), "123");
    }
}
