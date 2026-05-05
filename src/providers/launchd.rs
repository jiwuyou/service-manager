use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;

use crate::{
    error::Result,
    model::{
        Capability, DetectResult, LogEntry, LogsOptions, Provider, ProviderId, Service,
        ServiceState, ServiceStatus,
    },
};

use super::{bad_request, lossy_stderr, lossy_stdout, run_command_output};

#[derive(Clone, Debug, Default)]
pub struct LaunchdProvider;

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl LaunchdProvider {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn label(&self, svc: &Service) -> Result<String> {
        if let Some(v) = svc.spec.runtime.get("label") {
            let Some(s) = v.as_str() else {
                return Err(bad_request("runtime.label must be a string"));
            };
            let s = s.trim();
            if s.is_empty() {
                return Err(bad_request("runtime.label must be non-empty"));
            }
            return Ok(s.to_string());
        }
        Ok(format!("com.service_manager.{}", svc.spec.name))
    }

    async fn default_domain(&self) -> Result<String> {
        // Default to the current user GUI domain.
        let out = run_command_output("id".to_string(), vec!["-u".to_string()]).await?;
        if !out.status.success() {
            return Err(bad_request(format!("id -u failed: {}", lossy_stderr(&out))));
        }
        let uid = lossy_stdout(&out);
        if uid.parse::<u32>().is_err() {
            return Err(bad_request(format!("unexpected id -u output: {uid:?}")));
        }
        Ok(format!("gui/{uid}"))
    }

    async fn domain(&self, svc: &Service) -> Result<String> {
        if let Some(v) = svc.spec.runtime.get("domain") {
            let Some(s) = v.as_str() else {
                return Err(bad_request(
                    "runtime.domain must be a string (e.g. \"gui/501\" or \"system\")",
                ));
            };
            let s = s.trim();
            if s.is_empty() {
                return Err(bad_request("runtime.domain must be non-empty"));
            }
            return Ok(s.to_string());
        }
        self.default_domain().await
    }

    fn plist_path(&self, svc: &Service, label: &str) -> Result<PathBuf> {
        if let Some(v) = svc.spec.runtime.get("plist_path") {
            let Some(s) = v.as_str() else {
                return Err(bad_request("runtime.plist_path must be a string"));
            };
            let s = s.trim();
            if s.is_empty() {
                return Err(bad_request("runtime.plist_path must be non-empty"));
            }
            return Ok(PathBuf::from(s));
        }

        let home = std::env::var_os("HOME").ok_or_else(|| bad_request("HOME not set"))?;
        Ok(PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{label}.plist")))
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    fn plist_contents(&self, svc: &Service, label: &str) -> Result<String> {
        if svc.spec.command.is_empty() {
            return Err(bad_request("command is required for launchd provider"));
        }

        let mut out = String::new();
        out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        out.push('\n');
        out.push_str(
            r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#,
        );
        out.push('\n');
        out.push_str(r#"<plist version="1.0">"#);
        out.push('\n');
        out.push_str("<dict>\n");

        out.push_str("  <key>Label</key>\n");
        out.push_str("  <string>");
        out.push_str(&Self::xml_escape(label));
        out.push_str("</string>\n");

        out.push_str("  <key>ProgramArguments</key>\n");
        out.push_str("  <array>\n");
        for part in &svc.spec.command {
            out.push_str("    <string>");
            out.push_str(&Self::xml_escape(part));
            out.push_str("</string>\n");
        }
        out.push_str("  </array>\n");

        if !svc.spec.working_dir.trim().is_empty() {
            out.push_str("  <key>WorkingDirectory</key>\n");
            out.push_str("  <string>");
            out.push_str(&Self::xml_escape(&svc.spec.working_dir));
            out.push_str("</string>\n");
        }

        if !svc.spec.env.is_empty() {
            out.push_str("  <key>EnvironmentVariables</key>\n");
            out.push_str("  <dict>\n");
            for (k, v) in &svc.spec.env {
                out.push_str("    <key>");
                out.push_str(&Self::xml_escape(k));
                out.push_str("</key>\n");
                out.push_str("    <string>");
                out.push_str(&Self::xml_escape(v));
                out.push_str("</string>\n");
            }
            out.push_str("  </dict>\n");
        }

        out.push_str("  <key>RunAtLoad</key>\n");
        out.push_str("  <true/>\n");

        out.push_str("  <key>KeepAlive</key>\n");
        // Best-effort: KeepAlive true approximates RestartMode::Always.
        out.push_str("  <true/>\n");

        out.push_str("</dict>\n</plist>\n");
        Ok(out)
    }

    pub(crate) fn build_bootstrap_args(domain: &str, plist: &Path) -> Vec<String> {
        vec![
            "bootstrap".to_string(),
            domain.to_string(),
            plist.to_string_lossy().to_string(),
        ]
    }

    pub(crate) fn build_bootout_args(domain: &str, plist: &Path) -> Vec<String> {
        vec![
            "bootout".to_string(),
            domain.to_string(),
            plist.to_string_lossy().to_string(),
        ]
    }

    pub(crate) fn build_kickstart_args(domain: &str, label: &str) -> Vec<String> {
        vec![
            "kickstart".to_string(),
            "-k".to_string(),
            format!("{domain}/{label}"),
        ]
    }

    pub(crate) fn build_kill_args(domain: &str, label: &str) -> Vec<String> {
        vec![
            "kill".to_string(),
            "SIGTERM".to_string(),
            format!("{domain}/{label}"),
        ]
    }

    pub(crate) fn build_print_args(domain: &str, label: &str) -> Vec<String> {
        vec!["print".to_string(), format!("{domain}/{label}")]
    }

    fn parse_print_for_pid(stdout: &str) -> Option<u32> {
        // Heuristic parsing; output format is not stable across macOS releases.
        for line in stdout.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("pid = ")
                && let Ok(p) = rest.trim().parse::<u32>()
                && p != 0
            {
                return Some(p);
            }
        }
        None
    }

    fn parse_print_state(stdout: &str) -> ServiceState {
        let s = stdout.to_ascii_lowercase();
        if s.contains("state = running") {
            return ServiceState::Running;
        }
        if s.contains("state = waiting") || s.contains("state = stopped") {
            return ServiceState::Stopped;
        }
        ServiceState::Unknown
    }
}

#[async_trait]
impl Provider for LaunchdProvider {
    fn id(&self) -> ProviderId {
        ProviderId("launchd".to_string())
    }

    fn display_name(&self) -> String {
        "launchd".to_string()
    }

    fn description(&self) -> String {
        "Manage a macOS LaunchAgent via launchctl (bootstrap/bootout/kickstart/print).".to_string()
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Register,
            Capability::Unregister,
            Capability::Start,
            Capability::Stop,
            Capability::Restart,
            Capability::Status,
        ]
    }

    async fn detect(&self) -> Result<DetectResult> {
        #[cfg(not(target_os = "macos"))]
        {
            return Ok(DetectResult {
                detected: false,
                details: "not macOS".to_string(),
            });
        }

        #[cfg(target_os = "macos")]
        {
            if find_in_path("launchctl", &[]).is_none() {
                return Ok(DetectResult {
                    detected: false,
                    details: "launchctl not found".to_string(),
                });
            }
            Ok(DetectResult {
                detected: true,
                details: "ok".to_string(),
            })
        }
    }

    async fn register(&self, svc: &Service) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = svc;
            return Err(bad_request("launchd provider is only available on macOS"));
        }

        #[cfg(target_os = "macos")]
        {
            if find_in_path("launchctl", &[]).is_none() {
                return Err(bad_request("launchctl not found"));
            }

            let label = self.label(svc)?;
            let domain = self.domain(svc).await?;
            let plist = self.plist_path(svc, &label)?;

            if let Some(parent) = plist.parent() {
                fs::create_dir_all(parent)?;
            }

            let contents = self.plist_contents(svc, &label)?;
            fs::write(&plist, contents)?;

            // Best-effort bootout first to make register idempotent.
            let _ = run_command_output(
                "launchctl".to_string(),
                Self::build_bootout_args(&domain, &plist),
            )
            .await;

            let out = run_command_output(
                "launchctl".to_string(),
                Self::build_bootstrap_args(&domain, &plist),
            )
            .await?;
            if !out.status.success() {
                return Err(bad_request(format!(
                    "launchctl bootstrap failed: {}",
                    lossy_stderr(&out)
                )));
            }
            Ok(())
        }
    }

    async fn unregister(&self, svc: &Service) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = svc;
            return Err(bad_request("launchd provider is only available on macOS"));
        }

        #[cfg(target_os = "macos")]
        {
            if find_in_path("launchctl", &[]).is_none() {
                return Err(bad_request("launchctl not found"));
            }

            let label = self.label(svc)?;
            let domain = self.domain(svc).await?;
            let plist = self.plist_path(svc, &label)?;

            let _ = run_command_output(
                "launchctl".to_string(),
                Self::build_bootout_args(&domain, &plist),
            )
            .await;
            let _ = fs::remove_file(&plist);
            Ok(())
        }
    }

    async fn start(&self, svc: &Service) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = svc;
            return Err(bad_request("launchd provider is only available on macOS"));
        }

        #[cfg(target_os = "macos")]
        {
            if find_in_path("launchctl", &[]).is_none() {
                return Err(bad_request("launchctl not found"));
            }
            let label = self.label(svc)?;
            let domain = self.domain(svc).await?;
            let out = run_command_output(
                "launchctl".to_string(),
                Self::build_kickstart_args(&domain, &label),
            )
            .await?;
            if !out.status.success() {
                return Err(bad_request(format!(
                    "launchctl kickstart failed: {}",
                    lossy_stderr(&out)
                )));
            }
            Ok(())
        }
    }

    async fn stop(&self, svc: &Service) -> Result<()> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = svc;
            return Err(bad_request("launchd provider is only available on macOS"));
        }

        #[cfg(target_os = "macos")]
        {
            if find_in_path("launchctl", &[]).is_none() {
                return Err(bad_request("launchctl not found"));
            }
            let label = self.label(svc)?;
            let domain = self.domain(svc).await?;
            let out = run_command_output(
                "launchctl".to_string(),
                Self::build_kill_args(&domain, &label),
            )
            .await?;
            if !out.status.success() {
                return Err(bad_request(format!(
                    "launchctl kill failed: {}",
                    lossy_stderr(&out)
                )));
            }
            Ok(())
        }
    }

    async fn restart(&self, svc: &Service) -> Result<()> {
        self.stop(svc).await?;
        self.start(svc).await
    }

    async fn status(&self, svc: &Service) -> Result<ServiceStatus> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = svc;
            return Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: ServiceState::Unknown,
                message: "not macOS".to_string(),
                provider: self.id(),
                observed_at: Utc::now(),
                started_at: None,
                pid: None,
                exit_code: None,
            });
        }

        #[cfg(target_os = "macos")]
        {
            let observed_at = Utc::now();
            if find_in_path("launchctl", &[]).is_none() {
                return Ok(ServiceStatus {
                    service_id: svc.id.clone(),
                    state: ServiceState::Unknown,
                    message: "launchctl not found".to_string(),
                    provider: self.id(),
                    observed_at,
                    started_at: None,
                    pid: None,
                    exit_code: None,
                });
            }

            let label = self.label(svc)?;
            let domain = self.domain(svc).await?;
            let out = run_command_output(
                "launchctl".to_string(),
                Self::build_print_args(&domain, &label),
            )
            .await?;

            if !out.status.success() {
                return Ok(ServiceStatus {
                    service_id: svc.id.clone(),
                    state: ServiceState::Unknown,
                    message: format!("launchctl print failed: {}", lossy_stderr(&out)),
                    provider: self.id(),
                    observed_at,
                    started_at: None,
                    pid: None,
                    exit_code: None,
                });
            }

            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let pid = Self::parse_print_for_pid(&stdout);
            let state = Self::parse_print_state(&stdout);
            Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state,
                message: "ok".to_string(),
                provider: self.id(),
                observed_at,
                started_at: None,
                pid,
                exit_code: None,
            })
        }
    }

    async fn logs(&self, _svc: &Service, _opts: LogsOptions) -> Result<Vec<LogEntry>> {
        // `log show` parsing is heavy and slow; we can add this later once the provider contract
        // is tightened around log streaming semantics.
        Ok(Vec::new())
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
                provider: ProviderId("launchd".to_string()),
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
    fn command_builders_include_expected_targets() {
        let p = LaunchdProvider::new();
        let label = p.label(&svc()).unwrap();
        let domain = "gui/501";
        let plist = PathBuf::from("/tmp/demo.plist");

        let bs = LaunchdProvider::build_bootstrap_args(domain, &plist);
        assert_eq!(bs[0], "bootstrap");
        assert_eq!(bs[1], domain);

        let ks = LaunchdProvider::build_kickstart_args(domain, &label);
        assert_eq!(ks[0], "kickstart");
        assert!(ks[2].starts_with("gui/501/"));
    }

    #[test]
    fn xml_escape_escapes_angle_and_ampersand() {
        let s = LaunchdProvider::xml_escape("a&b<c>");
        assert_eq!(s, "a&amp;b&lt;c&gt;");
    }
}
