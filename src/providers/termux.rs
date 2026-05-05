use std::{
    fs,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use chrono::{NaiveDateTime, Utc};

use crate::{
    error::Result,
    model::{
        Capability, DetectResult, LogEntry, LogsOptions, Provider, ProviderId, Service,
        ServiceState, ServiceStatus,
    },
};

use super::{bad_request, find_in_path, lossy_stderr, lossy_stdout, run_command_output};

#[derive(Clone, Debug, Default)]
pub struct TermuxServicesProvider;

impl TermuxServicesProvider {
    pub fn new() -> Self {
        Self
    }

    fn termux_bin_dirs() -> Vec<PathBuf> {
        vec![PathBuf::from("/data/data/com.termux/files/usr/bin")]
    }

    fn prefix(&self) -> PathBuf {
        if let Some(v) = std::env::var_os("PREFIX") {
            return PathBuf::from(v);
        }
        PathBuf::from("/data/data/com.termux/files/usr")
    }

    fn service_name(&self, svc: &Service) -> Result<String> {
        if let Some(v) = svc.spec.runtime.get("service_name") {
            let Some(s) = v.as_str() else {
                return Err(bad_request("runtime.service_name must be a string"));
            };
            let s = s.trim();
            if s.is_empty() {
                return Err(bad_request("runtime.service_name must be non-empty"));
            }
            return Ok(s.to_string());
        }
        Ok(svc.spec.name.clone())
    }

    fn service_dir(&self, name: &str) -> PathBuf {
        self.prefix().join("var").join("service").join(name)
    }

    fn log_dir(&self, name: &str) -> PathBuf {
        self.prefix().join("var").join("log").join("sv").join(name)
    }

    fn run_path(&self, name: &str) -> PathBuf {
        self.service_dir(name).join("run")
    }

    fn down_path(&self, name: &str) -> PathBuf {
        self.service_dir(name).join("down")
    }

    fn log_current_path(&self, name: &str) -> PathBuf {
        self.log_dir(name).join("current")
    }

    fn log_run_path(&self, name: &str) -> PathBuf {
        self.service_dir(name).join("log").join("run")
    }

    fn shell_path(&self) -> PathBuf {
        // On Termux, /data/data/com.termux/files/usr/bin/sh exists and is the right shebang.
        self.prefix().join("bin").join("sh")
    }

    fn ensure_termux_sv(&self) -> bool {
        find_in_path("sv", &Self::termux_bin_dirs()).is_some()
    }

    fn make_executable(path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(path, perms)?;
        }
        let _ = path;
        Ok(())
    }

    fn write_run_script(&self, svc: &Service, name: &str) -> Result<()> {
        if svc.spec.command.is_empty() {
            return Err(bad_request(
                "command is required for termux-services provider",
            ));
        }
        let run_path = self.run_path(name);
        if let Some(parent) = run_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut script = String::new();
        script.push_str("#!");
        script.push_str(&self.shell_path().to_string_lossy());
        script.push('\n');

        if !svc.spec.working_dir.trim().is_empty() {
            script.push_str("cd ");
            script.push_str(&Self::shell_quote(&svc.spec.working_dir));
            script.push_str(" || exit 1\n");
        }

        for (k, v) in &svc.spec.env {
            if !Self::valid_shell_var_name(k) {
                return Err(bad_request(format!(
                    "invalid environment variable name for shell export: {k:?}"
                )));
            }
            script.push_str("export ");
            script.push_str(k);
            script.push('=');
            script.push_str(&Self::shell_quote(v));
            script.push('\n');
        }

        script.push_str("exec ");
        for (i, part) in svc.spec.command.iter().enumerate() {
            if i > 0 {
                script.push(' ');
            }
            script.push_str(&Self::shell_quote(part));
        }
        script.push('\n');

        fs::write(&run_path, script)?;
        Self::make_executable(&run_path)?;
        Ok(())
    }

    fn valid_shell_var_name(s: &str) -> bool {
        let mut chars = s.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return false;
        }
        for c in chars {
            if !(c.is_ascii_alphanumeric() || c == '_') {
                return false;
            }
        }
        true
    }

    fn shell_quote(s: &str) -> String {
        // Double-quote with minimal escaping. (We avoid single-quote gymnastics.)
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for ch in s.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '$' => out.push_str("\\$"),
                '`' => out.push_str("\\`"),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    fn write_log_script(&self, name: &str) -> Result<()> {
        let log_run = self.log_run_path(name);
        if let Some(parent) = log_run.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(self.log_dir(name))?;

        // Prefer svlogd -tt for readable UTC timestamps.
        let mut script = String::new();
        script.push_str("#!");
        script.push_str(&self.shell_path().to_string_lossy());
        script.push('\n');
        script.push_str("exec svlogd -tt ");
        script.push_str(&self.log_dir(name).to_string_lossy());
        script.push('\n');

        fs::write(&log_run, script)?;
        Self::make_executable(&log_run)?;
        Ok(())
    }

    async fn sv(&self, args: Vec<String>) -> Result<std::process::Output> {
        // If sv isn't in PATH, try Termux default bin dir explicitly.
        let exe = if find_in_path("sv", &[]).is_some() {
            "sv".to_string()
        } else if let Some(p) = find_in_path("sv", &Self::termux_bin_dirs()) {
            p.to_string_lossy().to_string()
        } else {
            return Err(bad_request("sv not found (is termux-services installed?)"));
        };
        run_command_output(exe, args).await
    }

    fn parse_sv_status(stdout: &str) -> (ServiceState, Option<u32>, String) {
        // Examples:
        // "run: sshd: (pid 12345) 60s"
        // "down: sshd: 1s, normally up"
        let s = stdout.trim();
        if s.starts_with("run:") {
            let pid = s
                .split("(pid ")
                .nth(1)
                .and_then(|rest| rest.split(')').next())
                .and_then(|pid| pid.trim().parse::<u32>().ok());
            return (ServiceState::Running, pid, s.to_string());
        }
        if s.starts_with("down:") {
            return (ServiceState::Stopped, None, s.to_string());
        }
        (ServiceState::Unknown, None, s.to_string())
    }

    fn parse_svlogd_tt_line(line: &str) -> Option<LogEntry> {
        // svlogd -tt: "YYYY-MM-DD_HH:MM:SS.xxxxx message..."
        let line = line.trim_end();
        if line.trim().is_empty() {
            return None;
        }
        let (ts, msg) = line.split_once(' ')?;
        let time = NaiveDateTime::parse_from_str(ts, "%Y-%m-%d_%H:%M:%S%.f")
            .ok()
            .map(|dt| dt.and_utc())
            .unwrap_or_else(Utc::now);
        Some(LogEntry {
            time,
            stream: "stdout".to_string(),
            message: msg.to_string(),
        })
    }

    fn tail_lines(s: &str, limit: Option<usize>) -> Vec<&str> {
        let mut lines: Vec<&str> = s.lines().collect();
        if let Some(n) = limit
            && lines.len() > n
        {
            lines = lines[lines.len() - n..].to_vec();
        }
        lines
    }
}

#[async_trait]
impl Provider for TermuxServicesProvider {
    fn id(&self) -> ProviderId {
        ProviderId("termux-services".to_string())
    }

    fn display_name(&self) -> String {
        "Termux services (runit)".to_string()
    }

    fn description(&self) -> String {
        "Manage a service under Termux termux-services (runit) using `sv` and run scripts under $PREFIX/var/service."
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
        if !self.ensure_termux_sv() {
            return Ok(DetectResult {
                detected: false,
                details: "sv not found (termux-services/runit not installed?)".to_string(),
            });
        }

        let svdir = self.prefix().join("var").join("service");
        if !svdir.exists() {
            return Ok(DetectResult {
                detected: false,
                details: format!("missing {}", svdir.to_string_lossy()),
            });
        }

        Ok(DetectResult {
            detected: true,
            details: "ok".to_string(),
        })
    }

    async fn register(&self, svc: &Service) -> Result<()> {
        if !self.ensure_termux_sv() {
            return Err(bad_request("sv not found (is termux-services installed?)"));
        }
        let name = self.service_name(svc)?;

        self.write_run_script(svc, &name)?;
        self.write_log_script(&name)?;

        // Enabled/disabled is controlled by a "down" file.
        let down = self.down_path(&name);
        if svc.spec.enabled {
            let _ = fs::remove_file(&down);
        } else {
            if let Some(parent) = down.parent() {
                fs::create_dir_all(parent)?;
            }
            let _ = fs::write(&down, b"");
        }

        Ok(())
    }

    async fn unregister(&self, svc: &Service) -> Result<()> {
        let name = self.service_name(svc)?;
        let _ = self.stop(svc).await;
        let dir = self.service_dir(&name);
        let _ = fs::remove_dir_all(dir);
        Ok(())
    }

    async fn start(&self, svc: &Service) -> Result<()> {
        let name = self.service_name(svc)?;
        let out = self.sv(vec!["up".to_string(), name]).await?;
        if !out.status.success() {
            return Err(bad_request(format!("sv up failed: {}", lossy_stderr(&out))));
        }
        Ok(())
    }

    async fn stop(&self, svc: &Service) -> Result<()> {
        let name = self.service_name(svc)?;
        let out = self.sv(vec!["down".to_string(), name]).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "sv down failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn restart(&self, svc: &Service) -> Result<()> {
        let name = self.service_name(svc)?;
        let out = self.sv(vec!["restart".to_string(), name]).await?;
        if !out.status.success() {
            return Err(bad_request(format!(
                "sv restart failed: {}",
                lossy_stderr(&out)
            )));
        }
        Ok(())
    }

    async fn status(&self, svc: &Service) -> Result<ServiceStatus> {
        let name = self.service_name(svc)?;
        let observed_at = Utc::now();

        let out = self.sv(vec!["status".to_string(), name.clone()]).await?;
        if !out.status.success() {
            return Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Unknown,
                message: format!("sv status failed: {}", lossy_stderr(&out)),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: None,
                exit_code: None,
            });
        }

        let stdout = lossy_stdout(&out);
        let (state, pid, msg) = Self::parse_sv_status(&stdout);
        Ok(ServiceStatus {
            service_id: svc.id.clone(),
            state,
            message: msg,
            provider: self.id(),
            observed_at,
            started_at: None,
            pid,
            exit_code: None,
        })
    }

    async fn logs(&self, svc: &Service, opts: LogsOptions) -> Result<Vec<LogEntry>> {
        let name = self.service_name(svc)?;
        let p = self.log_current_path(&name);
        let s = fs::read_to_string(&p).unwrap_or_default();
        let mut out = Vec::new();
        for line in Self::tail_lines(&s, opts.limit) {
            if let Some(e) = Self::parse_svlogd_tt_line(line) {
                out.push(e);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sv_status_run_extracts_pid() {
        let (st, pid, msg) = TermuxServicesProvider::parse_sv_status("run: x: (pid 123) 5s");
        assert_eq!(st, ServiceState::Running);
        assert_eq!(pid, Some(123));
        assert!(msg.starts_with("run:"));
    }

    #[test]
    fn parse_sv_status_down_is_stopped() {
        let (st, pid, _) = TermuxServicesProvider::parse_sv_status("down: x: 1s");
        assert_eq!(st, ServiceState::Stopped);
        assert_eq!(pid, None);
    }

    #[test]
    fn termux_bin_dirs_includes_default_prefix_bin() {
        let dirs = TermuxServicesProvider::termux_bin_dirs();
        assert!(
            dirs.iter()
                .any(|p| p == Path::new("/data/data/com.termux/files/usr/bin"))
        );
    }
}
