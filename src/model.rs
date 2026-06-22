use std::{collections::BTreeMap, fmt, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceId(pub String);

impl ServiceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl ProviderId {
    #[allow(dead_code)]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Update,
    Delete,
    Register,
    Unregister,
    Start,
    Stop,
    Restart,
    Repair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceState {
    Unknown,
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartMode {
    #[serde(rename = "no")]
    No,
    OnFailure,
    Always,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartPolicy {
    #[serde(default)]
    pub mode: Option<RestartMode>,
    #[serde(default)]
    pub max_retries: i32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            mode: Some(RestartMode::No),
            max_retries: 0,
        }
    }
}

impl RestartPolicy {
    pub fn validate(&mut self) -> Result<()> {
        if self.mode.is_none() {
            self.mode = Some(RestartMode::No);
        }
        if self.max_retries < 0 {
            return Err(AppError::BadRequest(
                "restart.max_retries must be >= 0".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthCheckType {
    Http,
    Tcp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationDef(#[serde(with = "duration_serde")] pub Duration);

mod duration_serde {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(d: &Duration, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Match Go's intent ("5s", "1m0s") loosely via humantime formatting.
        s.serialize_str(&humantime::format_duration(*d).to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum In {
            Str(String),
            Ns(i64),
            Null,
        }

        match In::deserialize(deserializer)? {
            In::Null => Ok(Duration::from_secs(0)),
            In::Str(s) => {
                let s = s.trim();
                if s.is_empty() {
                    return Ok(Duration::from_secs(0));
                }
                humantime::parse_duration(s).map_err(serde::de::Error::custom)
            }
            In::Ns(n) => {
                if n < 0 {
                    return Err(serde::de::Error::custom("duration must be >= 0"));
                }
                Ok(Duration::from_nanos(n as u64))
            }
        }
    }
}

impl Default for DurationDef {
    fn default() -> Self {
        Self(Duration::from_secs(0))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheck {
    #[serde(rename = "type")]
    pub ty: HealthCheckType,
    #[serde(default)]
    pub interval: DurationDef,
    #[serde(default)]
    pub timeout: DurationDef,

    // HTTP
    #[serde(default)]
    pub url: String,

    // TCP
    #[serde(default)]
    pub address: String,
}

impl HealthCheck {
    pub fn validate(&mut self) -> Result<()> {
        match self.ty {
            HealthCheckType::Http => {
                if self.url.trim().is_empty() {
                    return Err(AppError::BadRequest(
                        "url is required for http healthcheck".to_string(),
                    ));
                }
            }
            HealthCheckType::Tcp => {
                if self.address.trim().is_empty() {
                    return Err(AppError::BadRequest(
                        "address is required for tcp healthcheck".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub provider: ProviderId,
    #[serde(default)]
    pub command: Vec<String>, // argv; command[0] is executable
    #[serde(default)]
    pub working_dir: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    // Provider-specific options. Schema intentionally loose.
    #[serde(default)]
    pub runtime: BTreeMap<String, serde_json::Value>,

    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default)]
    pub health: Vec<HealthCheck>,

    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ServiceSpec {
    pub fn validate(&mut self) -> Result<()> {
        self.name = self.name.trim().to_string();
        self.description = self.description.trim().to_string();
        self.working_dir = self.working_dir.trim().to_string();

        if self.name.is_empty() {
            return Err(AppError::BadRequest("name is required".to_string()));
        }
        if !valid_service_name(&self.name) {
            return Err(AppError::BadRequest(
                "name must match ^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$".to_string(),
            ));
        }
        if self.provider.0.trim().is_empty() {
            return Err(AppError::BadRequest("provider is required".to_string()));
        }
        for (i, part) in self.command.iter().enumerate() {
            if part.trim().is_empty() {
                return Err(AppError::BadRequest(format!("command[{i}] is empty")));
            }
        }

        self.restart.validate()?;
        for i in 0..self.health.len() {
            self.health[i].validate().map_err(|e| match e {
                AppError::BadRequest(msg) => AppError::BadRequest(format!("health[{i}]: {msg}")),
                other => other,
            })?;
        }
        Ok(())
    }
}

fn valid_service_name(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    let mut len = 1usize;
    for c in chars {
        len += 1;
        if len > 64 {
            return false;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
            return false;
        }
    }
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    pub id: ServiceId,
    pub spec: ServiceSpec,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub service_id: ServiceId,
    pub state: ServiceState,
    #[serde(default)]
    pub message: String,
    pub provider: ProviderId,
    pub observed_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceGroup {
    pub name: String,
    pub service_ids: Vec<ServiceId>,
    pub services: Vec<Service>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupActionResult {
    pub group: String,
    pub action: Action,
    pub total: usize,
    pub succeeded: Vec<ServiceId>,
    pub skipped: Vec<GroupActionSkip>,
    pub failed: Vec<GroupActionFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupActionSkip {
    pub service_id: ServiceId,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupActionFailure {
    pub service_id: ServiceId,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub time: DateTime<Utc>,
    #[serde(default)]
    pub stream: String, // stdout|stderr|system|unknown
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: String,
    pub time: DateTime<Utc>,
    pub action: Action,
    #[serde(default)]
    pub service_id: Option<ServiceId>,
    #[serde(default)]
    pub provider: Option<ProviderId>,
    #[serde(default)]
    pub actor: String, // e.g. "api"
    #[serde(default)]
    pub details: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogsOptions {
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability {
    Register,
    Unregister,
    Start,
    Stop,
    Restart,
    Status,
    Logs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: ProviderId,
    pub display_name: String,
    pub description: String,
    pub capabilities: Vec<Capability>,
    pub detected: bool,
    #[serde(default)]
    pub detect_error: String,
    #[serde(default)]
    pub detect_details: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectResult {
    pub detected: bool,
    pub details: String,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> String;
    fn description(&self) -> String;
    fn capabilities(&self) -> Vec<Capability>;

    async fn detect(&self) -> Result<DetectResult>;

    async fn register(&self, svc: &Service) -> Result<()>;
    async fn unregister(&self, svc: &Service) -> Result<()>;

    async fn start(&self, svc: &Service) -> Result<()>;
    async fn stop(&self, svc: &Service) -> Result<()>;
    async fn restart(&self, svc: &Service) -> Result<()>;
    async fn status(&self, svc: &Service) -> Result<ServiceStatus>;
    async fn logs(&self, svc: &Service, opts: LogsOptions) -> Result<Vec<LogEntry>>;
}
