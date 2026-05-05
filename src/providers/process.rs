use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    error::Result,
    model::{
        Capability, DetectResult, LogEntry, LogsOptions, Provider, ProviderId, Service,
        ServiceState, ServiceStatus,
    },
};

use super::{bad_request, find_in_path, lossy_stderr, run_command_output};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProcMetaV1 {
    version: u32,
    provider: String,
    service_id: String,
    pid: u32,
    #[serde(default)]
    pgid: Option<i32>,
    argv: Vec<String>,
    #[serde(default)]
    proc_starttime_ticks: Option<u64>,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
enum ProcVerify {
    Managed,
    Stale(String),
    Unverifiable(String),
}

#[derive(Clone, Debug)]
pub struct ProcessProvider {
    data_dir: PathBuf,
}

impl ProcessProvider {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    fn state_dir(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self
            .data_dir
            .clone()
            .join("providers")
            .join("process")
            .join(&svc.id.0))
    }

    fn pid_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("pid"))
    }

    fn started_at_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("started_at"))
    }

    fn stdout_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("stdout.log"))
    }

    fn stderr_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("stderr.log"))
    }

    fn meta_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("meta.json"))
    }

    fn ensure_dirs(&self, svc: &Service) -> Result<()> {
        let dir = self.state_dir(svc)?;
        fs::create_dir_all(&dir)?;
        Ok(())
    }

    fn read_pid(path: &Path) -> Result<Option<u32>> {
        let mut f = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::error::AppError::Io(e)),
        };
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let pid: u32 = s
            .parse()
            .map_err(|_| bad_request(format!("invalid pid file contents: {s:?}")))?;
        Ok(Some(pid))
    }

    fn write_pid(path: &Path, pid: u32) -> Result<()> {
        let mut f = fs::File::create(path)?;
        writeln!(f, "{pid}")?;
        Ok(())
    }

    fn write_started_at(path: &Path, t: DateTime<Utc>) -> Result<()> {
        let mut f = fs::File::create(path)?;
        writeln!(f, "{}", t.to_rfc3339())?;
        Ok(())
    }

    fn read_started_at(path: &Path) -> Result<Option<DateTime<Utc>>> {
        let mut f = match fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::error::AppError::Io(e)),
        };
        let mut s = String::new();
        f.read_to_string(&mut s)?;
        let s = s.trim();
        if s.is_empty() {
            return Ok(None);
        }
        let dt = DateTime::parse_from_rfc3339(s)
            .map_err(|e| bad_request(format!("invalid started_at file: {e}")))?
            .with_timezone(&Utc);
        Ok(Some(dt))
    }

    async fn pid_is_alive(&self, pid: u32) -> Result<bool> {
        #[cfg(unix)]
        {
            // `kill -0 <pid>` is the most portable "exists?" check without extra deps.
            let out =
                run_command_output("kill".to_string(), vec!["-0".to_string(), pid.to_string()])
                    .await?;
            Ok(out.status.success())
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Ok(false)
        }
    }

    async fn kill_term(&self, pid: u32) -> Result<()> {
        #[cfg(unix)]
        {
            // Signal the process group we created at spawn time (`process_group(0)`), but only
            // after PID identity has been verified to avoid PID-reuse footguns.
            let out = run_command_output(
                "kill".to_string(),
                vec!["-TERM".to_string(), "--".to_string(), format!("-{pid}")],
            )
            .await?;
            if !out.status.success() {
                return Err(bad_request(format!(
                    "kill -TERM -- -{pid} failed: {}",
                    lossy_stderr(&out)
                )));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Err(bad_request("process provider is only supported on unix"))
        }
    }

    fn slice_log_lines(s: &str, limit: Option<usize>) -> Vec<&str> {
        let mut lines: Vec<&str> = s.lines().collect();
        if let Some(n) = limit
            && lines.len() > n
        {
            lines = lines[lines.len() - n..].to_vec();
        }
        lines
    }

    fn proc_root() -> PathBuf {
        // Test hook: allow overriding procfs root so unit tests can provide fixture files.
        if let Ok(v) = std::env::var("SERVICE_MANAGER_PROCFS_ROOT") {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
        PathBuf::from("/proc")
    }

    fn proc_stat_starttime_from_str(s: &str) -> Option<u64> {
        // /proc/<pid>/stat has a tricky format due to `comm` in parentheses. We locate the last
        // ')' and parse tokens after it. `starttime` is field 22 overall, i.e. token 19 after comm.
        let end = s.rfind(')')?;
        let after = s[end + 1..].trim();
        let mut it = after.split_whitespace();
        let _state = it.next()?;
        for _ in 0..18 {
            it.next()?;
        }
        it.next()?.parse::<u64>().ok()
    }

    fn proc_starttime_ticks(pid: u32) -> Option<u64> {
        let p = Self::proc_root().join(pid.to_string()).join("stat");
        let s = fs::read_to_string(p).ok()?;
        Self::proc_stat_starttime_from_str(&s)
    }

    fn proc_cmdline(pid: u32) -> Option<Vec<String>> {
        let p = Self::proc_root().join(pid.to_string()).join("cmdline");
        let b = fs::read(p).ok()?;
        if b.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for part in b.split(|c| *c == 0u8) {
            if part.is_empty() {
                continue;
            }
            out.push(String::from_utf8_lossy(part).to_string());
        }
        if out.is_empty() { None } else { Some(out) }
    }

    fn read_meta(path: &Path) -> Option<ProcMetaV1> {
        let b = fs::read(path).ok()?;
        serde_json::from_slice::<ProcMetaV1>(&b).ok()
    }

    fn write_meta(path: &Path, meta: &ProcMetaV1) -> Result<()> {
        let b = serde_json::to_vec_pretty(meta).map_err(crate::error::AppError::Json)?;
        fs::write(path, b)?;
        Ok(())
    }

    fn verify_pid_with_obs(
        svc: &Service,
        pid: u32,
        meta: Option<&ProcMetaV1>,
        start: Option<u64>,
        cmdline: Option<Vec<String>>,
    ) -> ProcVerify {
        let Some(meta) = meta else {
            // Newer builds always write meta; if missing, treat as unsafe (PID reuse risk).
            return ProcVerify::Unverifiable("missing meta.json".to_string());
        };

        if meta.pid != pid {
            return ProcVerify::Stale("pid mismatch".to_string());
        }
        if meta.provider != "process" {
            return ProcVerify::Stale(format!("provider mismatch: {}", meta.provider));
        }
        if meta.service_id != svc.id.0 {
            return ProcVerify::Stale("service_id mismatch".to_string());
        }

        // Strong identity check: /proc starttime, when available.
        match (meta.proc_starttime_ticks, start) {
            (Some(a), Some(b)) if a != b => {
                return ProcVerify::Stale("proc starttime mismatch".to_string());
            }
            (Some(_), None) => {
                return ProcVerify::Unverifiable(
                    "proc starttime unavailable; refusing to manage pid".to_string(),
                );
            }
            _ => {}
        }

        // Command-line signature check: compare argv vectors if /proc cmdline is readable.
        if let Some(cl) = cmdline {
            if cl != meta.argv {
                return ProcVerify::Stale("cmdline mismatch".to_string());
            }
        } else {
            return ProcVerify::Unverifiable(
                "proc cmdline unavailable; refusing to manage pid".to_string(),
            );
        }

        ProcVerify::Managed
    }

    fn verify_pid(svc: &Service, pid: u32, meta: Option<&ProcMetaV1>) -> ProcVerify {
        let start = Self::proc_starttime_ticks(pid);
        let cmdline = Self::proc_cmdline(pid);
        Self::verify_pid_with_obs(svc, pid, meta, start, cmdline)
    }

    fn cleanup_stale(pid_path: &Path, meta_path: &Path) {
        let _ = fs::remove_file(pid_path);
        let _ = fs::remove_file(meta_path);
    }
}

#[async_trait]
impl Provider for ProcessProvider {
    fn id(&self) -> ProviderId {
        ProviderId("process".to_string())
    }

    fn display_name(&self) -> String {
        "Raw process".to_string()
    }

    fn description(&self) -> String {
        "Spawn/stop a background OS process with PID+log files under the service-manager data dir."
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
        #[cfg(unix)]
        {
            // Soft check for `kill` availability; some environments can be weird.
            let kill = find_in_path("kill", &[]);
            if kill.is_none() {
                return Ok(DetectResult {
                    detected: false,
                    details: "missing `kill` in PATH".to_string(),
                });
            }
            Ok(DetectResult {
                detected: true,
                details: "ok".to_string(),
            })
        }
        #[cfg(not(unix))]
        {
            Ok(DetectResult {
                detected: false,
                details: "unsupported platform".to_string(),
            })
        }
    }

    async fn register(&self, svc: &Service) -> Result<()> {
        self.ensure_dirs(svc)?;
        Ok(())
    }

    async fn unregister(&self, svc: &Service) -> Result<()> {
        // Best-effort: stop the process, then delete state dir.
        let _ = self.stop(svc).await;
        let dir = self.state_dir(svc)?;
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    async fn start(&self, svc: &Service) -> Result<()> {
        if svc.spec.command.is_empty() {
            return Err(bad_request("command is required"));
        }
        self.ensure_dirs(svc)?;

        let pid_path = self.pid_path(svc)?;
        let meta_path = self.meta_path(svc)?;
        if let Some(pid) = Self::read_pid(&pid_path)? {
            if !self.pid_is_alive(pid).await.unwrap_or(false) {
                Self::cleanup_stale(&pid_path, &meta_path);
            } else {
                let meta = Self::read_meta(&meta_path);
                match Self::verify_pid(svc, pid, meta.as_ref()) {
                    ProcVerify::Managed => return Ok(()),
                    ProcVerify::Stale(_) => Self::cleanup_stale(&pid_path, &meta_path),
                    ProcVerify::Unverifiable(reason) => {
                        return Err(bad_request(format!(
                            "refusing to start: existing pidfile points to a live process but identity cannot be verified: pid={pid} ({reason})"
                        )));
                    }
                }
            }
        }

        // Always append to logs (service restarts should preserve history).
        let stdout = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.stdout_path(svc)?)?;
        let stderr = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.stderr_path(svc)?)?;

        let mut cmd = std::process::Command::new(&svc.spec.command[0]);
        if svc.spec.command.len() > 1 {
            cmd.args(&svc.spec.command[1..]);
        }
        if !svc.spec.working_dir.trim().is_empty() {
            cmd.current_dir(&svc.spec.working_dir);
        }
        for (k, v) in &svc.spec.env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::from(stdout));
        cmd.stderr(Stdio::from(stderr));

        #[cfg(unix)]
        {
            // Put the child in its own process group so we can stop the full subtree.
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| {
            bad_request(format!(
                "spawn {:?}: {e}",
                svc.spec.command.first().cloned().unwrap_or_default()
            ))
        })?;

        let pid = child.id();
        Self::write_pid(&pid_path, pid)?;
        Self::write_started_at(&self.started_at_path(svc)?, Utc::now())?;

        // Capture proc identity for PID reuse safety. If /proc isn't available, we still write
        // metadata but stop/status will refuse to manage this process.
        let meta = ProcMetaV1 {
            version: 1,
            provider: "process".to_string(),
            service_id: svc.id.0.clone(),
            pid,
            pgid: Some(pid as i32),
            argv: svc.spec.command.clone(),
            proc_starttime_ticks: Self::proc_starttime_ticks(pid),
            created_at: Utc::now(),
        };
        if let Err(e) = Self::write_meta(&meta_path, &meta) {
            // Best-effort rollback (safe: we just spawned the process).
            let _ = self.kill_term(pid).await;
            return Err(e);
        }

        // Reap in the background to avoid zombies if the process exits.
        tokio::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || child.wait()).await;
        });

        Ok(())
    }

    async fn stop(&self, svc: &Service) -> Result<()> {
        let pid_path = self.pid_path(svc)?;
        let meta_path = self.meta_path(svc)?;
        let Some(pid) = Self::read_pid(&pid_path)? else {
            return Ok(());
        };

        // If it's already gone, clean up stale pidfile.
        if !self.pid_is_alive(pid).await.unwrap_or(false) {
            Self::cleanup_stale(&pid_path, &meta_path);
            return Ok(());
        }

        let meta = Self::read_meta(&meta_path);
        match Self::verify_pid(svc, pid, meta.as_ref()) {
            ProcVerify::Managed => {
                self.kill_term(pid).await?;
                Ok(())
            }
            ProcVerify::Stale(_) => {
                Self::cleanup_stale(&pid_path, &meta_path);
                Ok(())
            }
            ProcVerify::Unverifiable(reason) => Err(bad_request(format!(
                "refusing to stop: pid identity cannot be verified: pid={pid} ({reason})"
            ))),
        }
    }

    async fn restart(&self, svc: &Service) -> Result<()> {
        self.stop(svc).await?;
        self.start(svc).await
    }

    async fn status(&self, svc: &Service) -> Result<ServiceStatus> {
        let observed_at = Utc::now();
        let pid_path = self.pid_path(svc)?;
        let meta_path = self.meta_path(svc)?;
        let started_at = Self::read_started_at(&self.started_at_path(svc)?)
            .ok()
            .flatten();

        let mut st = ServiceStatus {
            service_id: svc.id.clone(),
            state: ServiceState::Stopped,
            message: String::new(),
            provider: self.id(),
            observed_at,
            started_at,
            pid: None,
            exit_code: None,
        };

        let Some(pid) = Self::read_pid(&pid_path)? else {
            st.state = ServiceState::Stopped;
            st.message = "not running".to_string();
            return Ok(st);
        };

        if !self.pid_is_alive(pid).await.unwrap_or(false) {
            Self::cleanup_stale(&pid_path, &meta_path);
            st.state = ServiceState::Stopped;
            st.message = "stale pidfile".to_string();
            return Ok(st);
        }

        let meta = Self::read_meta(&meta_path);
        match Self::verify_pid(svc, pid, meta.as_ref()) {
            ProcVerify::Managed => {
                st.state = ServiceState::Running;
                st.pid = Some(pid);
                st.message = "running".to_string();
                Ok(st)
            }
            ProcVerify::Stale(reason) => {
                Self::cleanup_stale(&pid_path, &meta_path);
                st.state = ServiceState::Stopped;
                st.message = format!("stale pidfile ({reason})");
                Ok(st)
            }
            ProcVerify::Unverifiable(reason) => {
                st.state = ServiceState::Unknown;
                st.pid = Some(pid);
                st.message = format!("pid identity cannot be verified ({reason})");
                Ok(st)
            }
        }
    }

    async fn logs(&self, svc: &Service, opts: LogsOptions) -> Result<Vec<LogEntry>> {
        // For now, we treat log lines as "now" time-stamped. If we later add timestamped
        // output wrappers, update this to parse timestamps.
        let limit = opts.limit;

        let mut out = Vec::new();
        let now = DateTime::<Utc>::from(SystemTime::now());

        let stdout = fs::read_to_string(self.stdout_path(svc)?).unwrap_or_default();
        for line in Self::slice_log_lines(&stdout, limit) {
            if line.trim().is_empty() {
                continue;
            }
            out.push(LogEntry {
                time: now,
                stream: "stdout".to_string(),
                message: line.to_string(),
            });
        }

        let stderr = fs::read_to_string(self.stderr_path(svc)?).unwrap_or_default();
        for line in Self::slice_log_lines(&stderr, limit) {
            if line.trim().is_empty() {
                continue;
            }
            out.push(LogEntry {
                time: now,
                stream: "stderr".to_string(),
                message: line.to_string(),
            });
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderId, Service, ServiceId, ServiceSpec};
    use std::collections::BTreeMap;

    fn svc() -> Service {
        Service {
            id: ServiceId("svc1".to_string()),
            spec: ServiceSpec {
                name: "demo".to_string(),
                description: String::new(),
                provider: ProviderId("process".to_string()),
                command: vec!["sleep".to_string(), "10".to_string()],
                working_dir: String::new(),
                env: BTreeMap::new(),
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
    fn slice_log_lines_applies_tail_limit() {
        let s = "a\nb\nc\nd\n";
        let out = ProcessProvider::slice_log_lines(s, Some(2));
        assert_eq!(out, vec!["c", "d"]);
    }

    #[test]
    fn proc_stat_starttime_from_str_parses_starttime() {
        // starttime is field 22 overall (token 19 after comm). We only care that we can parse it.
        let s = "123 (weird comm) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242";
        assert_eq!(
            ProcessProvider::proc_stat_starttime_from_str(s),
            Some(424242)
        );
    }

    #[test]
    fn verify_pid_with_obs_refuses_on_starttime_mismatch() {
        let svc = svc();
        let meta = ProcMetaV1 {
            version: 1,
            provider: "process".to_string(),
            service_id: svc.id.0.clone(),
            pid: 100,
            pgid: Some(100),
            argv: svc.spec.command.clone(),
            proc_starttime_ticks: Some(111),
            created_at: Utc::now(),
        };
        match ProcessProvider::verify_pid_with_obs(
            &svc,
            100,
            Some(&meta),
            Some(222),
            Some(svc.spec.command.clone()),
        ) {
            ProcVerify::Stale(reason) => assert!(reason.contains("starttime")),
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    #[test]
    fn verify_pid_with_obs_refuses_on_cmdline_mismatch() {
        let svc = svc();
        let meta = ProcMetaV1 {
            version: 1,
            provider: "process".to_string(),
            service_id: svc.id.0.clone(),
            pid: 100,
            pgid: Some(100),
            argv: svc.spec.command.clone(),
            proc_starttime_ticks: Some(111),
            created_at: Utc::now(),
        };
        match ProcessProvider::verify_pid_with_obs(
            &svc,
            100,
            Some(&meta),
            Some(111),
            Some(vec!["not".to_string(), "us".to_string()]),
        ) {
            ProcVerify::Stale(reason) => assert!(reason.contains("cmdline")),
            other => panic!("expected Stale, got {other:?}"),
        }
    }
}
