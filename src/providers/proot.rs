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
pub struct ProotDistroProvider {
    data_dir: PathBuf,
}

impl ProotDistroProvider {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    fn termux_bin_dirs() -> Vec<PathBuf> {
        vec![PathBuf::from("/data/data/com.termux/files/usr/bin")]
    }

    fn state_dir(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self
            .data_dir
            .clone()
            .join("providers")
            .join("proot-distro")
            .join(&svc.id.0))
    }

    fn pid_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("pid"))
    }

    fn stdout_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("stdout.log"))
    }

    fn stderr_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("stderr.log"))
    }

    fn started_at_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("started_at"))
    }

    fn meta_path(&self, svc: &Service) -> Result<PathBuf> {
        Ok(self.state_dir(svc)?.join("meta.json"))
    }

    fn ensure_dirs(&self, svc: &Service) -> Result<()> {
        fs::create_dir_all(self.state_dir(svc)?)?;
        Ok(())
    }

    fn distro(&self, svc: &Service) -> Result<String> {
        let Some(v) = svc.spec.runtime.get("distro") else {
            return Err(bad_request(
                "runtime.distro is required for proot-distro provider (e.g. \"ubuntu\")",
            ));
        };
        let Some(s) = v.as_str() else {
            return Err(bad_request("runtime.distro must be a string"));
        };
        let s = s.trim();
        if s.is_empty() {
            return Err(bad_request("runtime.distro must be non-empty"));
        }
        Ok(s.to_string())
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

    async fn pid_is_alive(&self, pid: u32) -> Result<bool> {
        #[cfg(unix)]
        {
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
            Err(bad_request(
                "proot-distro provider is only supported on unix",
            ))
        }
    }

    fn proot_distro_exe_for(&self, svc: &Service) -> Result<String> {
        if let Some(v) = svc.spec.runtime.get("proot_distro_path") {
            let Some(s) = v.as_str() else {
                return Err(bad_request("runtime.proot_distro_path must be a string"));
            };
            let s = s.trim();
            if s.is_empty() {
                return Err(bad_request("runtime.proot_distro_path must be non-empty"));
            }
            return Ok(s.to_string());
        }

        if find_in_path("proot-distro", &[]).is_some() {
            return Ok("proot-distro".to_string());
        }
        find_in_path("proot-distro", &Self::termux_bin_dirs())
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| bad_request("proot-distro not found"))
    }

    fn build_login_command(&self, svc: &Service) -> Result<(String, Vec<String>)> {
        let exe = self.proot_distro_exe_for(svc)?;
        let distro = self.distro(svc)?;

        // proot-distro login <distro> -- <command...>
        if svc.spec.command.is_empty() {
            return Err(bad_request("command is required"));
        }
        let mut args = vec!["login".to_string(), distro, "--".to_string()];
        args.extend(svc.spec.command.iter().cloned());
        Ok((exe, args))
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
        if let Ok(v) = std::env::var("SERVICE_MANAGER_PROCFS_ROOT") {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return PathBuf::from(v);
            }
        }
        PathBuf::from("/proc")
    }

    fn proc_stat_starttime_from_str(s: &str) -> Option<u64> {
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
            return ProcVerify::Unverifiable("missing meta.json".to_string());
        };

        if meta.pid != pid {
            return ProcVerify::Stale("pid mismatch".to_string());
        }
        if meta.provider != "proot-distro" {
            return ProcVerify::Stale(format!("provider mismatch: {}", meta.provider));
        }
        if meta.service_id != svc.id.0 {
            return ProcVerify::Stale("service_id mismatch".to_string());
        }

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
impl Provider for ProotDistroProvider {
    fn id(&self) -> ProviderId {
        ProviderId("proot-distro".to_string())
    }

    fn display_name(&self) -> String {
        "Termux proot-distro".to_string()
    }

    fn description(&self) -> String {
        "Spawn a service command inside a Termux proot-distro environment (login -- <cmd...>) and track it via PID/log files under the service-manager data dir."
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
        let found = find_in_path("proot-distro", &[]).is_some()
            || find_in_path("proot-distro", &Self::termux_bin_dirs()).is_some();
        if !found {
            return Ok(DetectResult {
                detected: false,
                details: "proot-distro not found".to_string(),
            });
        }
        Ok(DetectResult {
            detected: true,
            details: "ok".to_string(),
        })
    }

    async fn register(&self, svc: &Service) -> Result<()> {
        self.ensure_dirs(svc)?;
        // Validate required runtime options early.
        let _ = self.distro(svc)?;
        Ok(())
    }

    async fn unregister(&self, svc: &Service) -> Result<()> {
        let _ = self.stop(svc).await;
        let dir = self.state_dir(svc)?;
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    async fn start(&self, svc: &Service) -> Result<()> {
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

        let stdout = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.stdout_path(svc)?)?;
        let stderr = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.stderr_path(svc)?)?;

        let (exe, args) = self.build_login_command(svc)?;
        let mut argv = Vec::with_capacity(1 + args.len());
        argv.push(exe.clone());
        argv.extend(args.iter().cloned());

        let mut cmd = std::process::Command::new(&exe);
        cmd.args(args.iter());
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
            cmd.process_group(0);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| bad_request(format!("spawn proot-distro: {e}")))?;
        let pid = child.id();
        Self::write_pid(&pid_path, pid)?;
        Self::write_started_at(&self.started_at_path(svc)?, Utc::now())?;
        let meta = ProcMetaV1 {
            version: 1,
            provider: "proot-distro".to_string(),
            service_id: svc.id.0.clone(),
            pid,
            pgid: Some(pid as i32),
            argv,
            proc_starttime_ticks: Self::proc_starttime_ticks(pid),
            created_at: Utc::now(),
        };
        if let Err(e) = Self::write_meta(&meta_path, &meta) {
            let _ = self.kill_term(pid).await;
            return Err(e);
        }

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
        let Some(pid) = Self::read_pid(&pid_path)? else {
            return Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Stopped,
                message: "not running".to_string(),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: None,
                exit_code: None,
            });
        };

        if !self.pid_is_alive(pid).await.unwrap_or(false) {
            Self::cleanup_stale(&pid_path, &meta_path);
            return Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Stopped,
                message: "stale pidfile".to_string(),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: None,
                exit_code: None,
            });
        }

        let meta = Self::read_meta(&meta_path);
        match Self::verify_pid(svc, pid, meta.as_ref()) {
            ProcVerify::Managed => Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Running,
                message: "running".to_string(),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: Some(pid),
                exit_code: None,
            }),
            ProcVerify::Stale(reason) => {
                Self::cleanup_stale(&pid_path, &meta_path);
                Ok(ServiceStatus {
                    service_id: svc.id.clone(),
                    state: ServiceState::Stopped,
                    message: format!("stale pidfile ({reason})"),
                    provider: self.id(),
                    observed_at,
                    started_at: None,
                    pid: None,
                    exit_code: None,
                })
            }
            ProcVerify::Unverifiable(reason) => Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Unknown,
                message: format!("pid identity cannot be verified ({reason})"),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid: Some(pid),
                exit_code: None,
            }),
        }
    }

    async fn logs(&self, svc: &Service, opts: LogsOptions) -> Result<Vec<LogEntry>> {
        let now = DateTime::<Utc>::from(SystemTime::now());
        let limit = opts.limit;
        let mut out = Vec::new();

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
    use crate::model::{ProviderId, ServiceId, ServiceSpec};
    use std::collections::BTreeMap;

    fn svc() -> Service {
        Service {
            id: ServiceId("svc1".to_string()),
            spec: ServiceSpec {
                name: "demo".to_string(),
                description: String::new(),
                provider: ProviderId("proot-distro".to_string()),
                command: vec!["echo".to_string(), "hi".to_string()],
                working_dir: String::new(),
                env: BTreeMap::new(),
                runtime: BTreeMap::from_iter([
                    (
                        "distro".to_string(),
                        serde_json::Value::String("ubuntu".to_string()),
                    ),
                    (
                        "proot_distro_path".to_string(),
                        serde_json::Value::String("/bin/proot-distro".to_string()),
                    ),
                ]),
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
    fn build_login_command_prefixes_proot_distro_login_and_separator() {
        let p = ProotDistroProvider::new(std::env::temp_dir());
        let svc = svc();
        // This test doesn't assert the executable path (depends on PATH), only args shape.
        let (exe, args) = p
            .build_login_command(&svc)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(exe, "/bin/proot-distro");
        assert_eq!(args[0], "login");
        assert_eq!(args[2], "--");
        assert!(args.ends_with(&["echo".to_string(), "hi".to_string()]));
    }

    #[test]
    fn proc_stat_starttime_from_str_parses_starttime() {
        let s = "123 (weird comm) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242";
        assert_eq!(
            ProotDistroProvider::proc_stat_starttime_from_str(s),
            Some(424242)
        );
    }

    #[test]
    fn verify_pid_with_obs_refuses_on_starttime_mismatch() {
        let p = ProotDistroProvider::new(std::env::temp_dir());
        let svc = svc();
        let (exe, args) = p.build_login_command(&svc).unwrap();
        let mut argv = vec![exe];
        argv.extend(args);

        let meta = ProcMetaV1 {
            version: 1,
            provider: "proot-distro".to_string(),
            service_id: svc.id.0.clone(),
            pid: 100,
            pgid: Some(100),
            argv: argv.clone(),
            proc_starttime_ticks: Some(111),
            created_at: Utc::now(),
        };

        match ProotDistroProvider::verify_pid_with_obs(
            &svc,
            100,
            Some(&meta),
            Some(222),
            Some(argv),
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
            provider: "proot-distro".to_string(),
            service_id: svc.id.0.clone(),
            pid: 100,
            pgid: Some(100),
            argv: vec!["proot-distro".to_string()],
            proc_starttime_ticks: Some(111),
            created_at: Utc::now(),
        };

        match ProotDistroProvider::verify_pid_with_obs(
            &svc,
            100,
            Some(&meta),
            Some(111),
            Some(vec!["different".to_string()]),
        ) {
            ProcVerify::Stale(reason) => assert!(reason.contains("cmdline")),
            other => panic!("expected Stale, got {other:?}"),
        }
    }
}
