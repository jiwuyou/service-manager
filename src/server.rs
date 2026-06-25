use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, RwLock},
};

#[path = "repair_hook.rs"]
mod repair_hook;

use chrono::{DateTime, SecondsFormat, Utc};
use repair_hook::run_repair_hook;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::{
    api,
    error::{AppError, Result},
    model::{
        Action, AuditEvent, Capability, DetectResult, GroupActionFailure, GroupActionResult,
        GroupActionSkip, LogsOptions, Provider, ProviderId, ProviderInfo, Service, ServiceGroup,
        ServiceId, ServiceSpec, ServiceStatus,
    },
    openhouse_registry, providers,
    service_registry,
    store::JsonStore,
};

pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:20087";
pub const ENV_AUTH_TOKEN: &str = "SERVICE_MANAGER_TOKEN";
pub const ENV_SERVICE_REGISTRY_DIR: &str = "OPENHOUSEAI_SERVICE_MANAGER_SERVICES_DIR";
const GROUP_TAG_PREFIX: &str = "group:";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoreConfig {
    #[serde(rename = "type", default)]
    pub ty: String,
    #[serde(default)]
    pub path: String,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            ty: "json".to_string(),
            path: String::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub listen_addr: String,
    #[serde(default)]
    pub data_dir: String,
    #[serde(default)]
    pub service_registry_dir: String,
    #[serde(default)]
    pub openhouse_registry_source_dir: String,
    #[serde(default)]
    pub openhouse_registry_target_dir: String,
    #[serde(default)]
    pub auth_token: String,
    #[serde(default)]
    pub log_level: String,
    #[serde(default)]
    pub store: StoreConfig,
}

pub struct LoadedConfig {
    pub config: Config,
    pub path: PathBuf,
}

#[derive(Clone)]
pub struct ProviderRegistry {
    inner: Arc<RwLock<BTreeMap<String, Arc<dyn Provider>>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    #[allow(dead_code)]
    pub fn add(&self, p: Arc<dyn Provider>) -> Result<()> {
        let id = p.id();
        if id.0.trim().is_empty() {
            return Err(AppError::BadRequest("provider id is empty".to_string()));
        }
        let mut m = self.inner.write().expect("provider registry lock poisoned");
        if m.contains_key(&id.0) {
            return Err(AppError::BadRequest(format!(
                "provider already registered: {:?}",
                id.0
            )));
        }
        m.insert(id.0.clone(), p);
        Ok(())
    }

    pub fn get(&self, id: &ProviderId) -> Option<Arc<dyn Provider>> {
        let m = self.inner.read().expect("provider registry lock poisoned");
        m.get(&id.0).cloned()
    }

    pub fn list(&self) -> Vec<Arc<dyn Provider>> {
        let m = self.inner.read().expect("provider registry lock poisoned");
        m.values().cloned().collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct Engine {
    store: Arc<JsonStore>,
    registry: ProviderRegistry,
    now: fn() -> DateTime<Utc>,
}

impl Engine {
    pub fn new(store: Arc<JsonStore>, registry: ProviderRegistry) -> Self {
        Self {
            store,
            registry,
            now: || Utc::now(),
        }
    }

    #[allow(dead_code)]
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    pub fn list_services(&self) -> Result<Vec<Service>> {
        self.store.list_services()
    }

    pub fn get_service(&self, id: &ServiceId) -> Result<Service> {
        self.store.get_service(id)
    }

    pub fn list_groups(&self) -> Result<Vec<ServiceGroup>> {
        let mut groups: BTreeMap<String, Vec<Service>> = BTreeMap::new();
        for svc in self.store.list_services()? {
            for group in service_groups(&svc) {
                groups.entry(group).or_default().push(svc.clone());
            }
        }

        Ok(groups
            .into_iter()
            .map(|(name, services)| ServiceGroup {
                name,
                service_ids: services.iter().map(|svc| svc.id.clone()).collect(),
                services,
            })
            .collect())
    }

    pub async fn group_action(&self, group: &str, action: Action) -> Result<GroupActionResult> {
        let group = group.trim();
        if group.is_empty() {
            return Err(AppError::BadRequest("group name is required".to_string()));
        }
        if !matches!(action, Action::Start | Action::Stop | Action::Restart) {
            return Err(AppError::BadRequest(
                "group action must be start, stop, or restart".to_string(),
            ));
        }

        let Some(svc_group) = self.list_groups()?.into_iter().find(|g| g.name == group) else {
            return Err(AppError::NotFound);
        };

        let mut result = GroupActionResult {
            group: group.to_string(),
            action,
            total: svc_group.services.len(),
            succeeded: Vec::new(),
            skipped: Vec::new(),
            failed: Vec::new(),
        };

        for svc in svc_group.services {
            let required = match action {
                Action::Start => Capability::Start,
                Action::Stop => Capability::Stop,
                Action::Restart => Capability::Restart,
                _ => unreachable!("validated group action"),
            };
            let Some(provider) = self.registry.get(&svc.spec.provider) else {
                result.failed.push(GroupActionFailure {
                    service_id: svc.id,
                    message: format!("provider not found: {:?}", svc.spec.provider.0),
                });
                continue;
            };
            if !provider.capabilities().contains(&required) {
                result.skipped.push(GroupActionSkip {
                    service_id: svc.id,
                    reason: "provider does not support this action".to_string(),
                });
                continue;
            }

            let id = svc.id.clone();
            let outcome = match action {
                Action::Start => self.start(&id).await,
                Action::Stop => self.stop(&id).await,
                Action::Restart => self.restart(&id).await,
                _ => unreachable!("validated group action"),
            };
            match outcome {
                Ok(()) => result.succeeded.push(id),
                Err(e) => result.failed.push(GroupActionFailure {
                    service_id: id,
                    message: e.to_string(),
                }),
            }
        }

        Ok(result)
    }

    pub async fn create_service(&self, mut spec: ServiceSpec) -> Result<Service> {
        spec.validate()?;
        if self.registry.get(&spec.provider).is_none() {
            return Err(AppError::ProviderNotFound(spec.provider.0.clone()));
        }

        let now = (self.now)();
        let svc = Service {
            id: ServiceId::new(new_id(16)?),
            spec,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        };
        self.store.create_service(svc.clone())?;

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: now,
            action: Action::Create,
            service_id: Some(svc.id.clone()),
            provider: Some(svc.spec.provider.clone()),
            actor: "api".to_string(),
            details: String::new(),
        });

        Ok(svc)
    }

    pub fn upsert_registered_service(
        &self,
        id: ServiceId,
        mut spec: ServiceSpec,
    ) -> Result<Service> {
        spec.validate()?;
        if self.registry.get(&spec.provider).is_none() {
            return Err(AppError::ProviderNotFound(spec.provider.0.clone()));
        }

        let id = ServiceId(validate_registry_service_id(id.0)?);
        let now = (self.now)();
        let created_at = match self.store.get_service(&id) {
            Ok(existing) => {
                if existing.spec.provider != spec.provider {
                    return Err(AppError::BadRequest(format!(
                        "provider cannot be changed for registered service {:?}",
                        id.0
                    )));
                }
                existing.created_at
            }
            Err(AppError::NotFound) => now,
            Err(e) => return Err(e),
        };

        let svc = Service {
            id: id.clone(),
            spec,
            created_at,
            updated_at: now,
            deleted_at: None,
        };
        self.store.upsert_service(svc.clone())?;

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: now,
            action: Action::Register,
            service_id: Some(id),
            provider: Some(svc.spec.provider.clone()),
            actor: "registry".to_string(),
            details: "loaded from services.d".to_string(),
        });

        Ok(svc)
    }

    pub fn export_store_snapshot(&self) -> Result<Vec<u8>> {
        self.store.export()
    }

    pub fn restore_store_snapshot(&self, snapshot: &[u8]) -> Result<()> {
        self.store.import(snapshot)
    }

    pub async fn update_service(&self, id: &ServiceId, mut spec: ServiceSpec) -> Result<Service> {
        spec.validate()?;
        let existing = self.store.get_service(id)?;
        if existing.spec.provider != spec.provider {
            return Err(AppError::BadRequest(
                "provider cannot be changed for an existing service".to_string(),
            ));
        }
        let p = self
            .registry
            .get(&spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(spec.provider.0.clone()))?;

        let now = (self.now)();
        let mut updated = existing.clone();
        updated.spec = spec;
        updated.updated_at = now;
        self.store.update_service(updated.clone())?;

        if let Err(e) = p.register(&updated).await {
            // Best-effort rollback of stored spec.
            let _ = self.store.update_service(existing);
            return Err(e);
        }

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: now,
            action: Action::Update,
            service_id: Some(updated.id.clone()),
            provider: Some(updated.spec.provider.clone()),
            actor: "api".to_string(),
            details: String::new(),
        });

        Ok(updated)
    }

    pub async fn delete_service(&self, id: &ServiceId) -> Result<()> {
        let svc = self.store.get_service(id)?;
        let now = (self.now)();

        if let Some(p) = self.registry.get(&svc.spec.provider) {
            p.unregister(&svc).await?;
        }

        self.store.delete_service(id)?;
        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: now,
            action: Action::Delete,
            service_id: Some(id.clone()),
            provider: Some(svc.spec.provider),
            actor: "api".to_string(),
            details: String::new(),
        });
        Ok(())
    }

    pub async fn list_providers(&self) -> Result<Vec<ProviderInfo>> {
        let ps = self.registry.list();
        let mut out = Vec::with_capacity(ps.len());
        for p in ps {
            let mut info = ProviderInfo {
                id: p.id(),
                display_name: p.display_name(),
                description: p.description(),
                capabilities: p.capabilities(),
                detected: false,
                detect_error: String::new(),
                detect_details: String::new(),
            };
            match p.detect().await {
                Ok(DetectResult { detected, details }) => {
                    info.detected = detected;
                    info.detect_details = details;
                }
                Err(e) => {
                    info.detected = false;
                    info.detect_error = e.to_string();
                }
            }
            out.push(info);
        }
        Ok(out)
    }

    pub async fn start(&self, id: &ServiceId) -> Result<()> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;

        p.register(&svc).await?;
        p.start(&svc).await?;

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: (self.now)(),
            action: Action::Start,
            service_id: Some(id.clone()),
            provider: Some(svc.spec.provider),
            actor: "api".to_string(),
            details: String::new(),
        });
        Ok(())
    }

    pub async fn stop(&self, id: &ServiceId) -> Result<()> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;

        p.stop(&svc).await?;

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: (self.now)(),
            action: Action::Stop,
            service_id: Some(id.clone()),
            provider: Some(svc.spec.provider),
            actor: "api".to_string(),
            details: String::new(),
        });
        Ok(())
    }

    pub async fn restart(&self, id: &ServiceId) -> Result<()> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;

        p.register(&svc).await?;
        p.restart(&svc).await?;

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: (self.now)(),
            action: Action::Restart,
            service_id: Some(id.clone()),
            provider: Some(svc.spec.provider),
            actor: "api".to_string(),
            details: String::new(),
        });
        Ok(())
    }

    pub async fn repair(&self, id: &ServiceId) -> Result<()> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;

        let details = if svc.spec.repair_hook()?.is_some() {
            run_repair_hook(&svc).await?;
            "repair hook".to_string()
        } else {
            p.register(&svc).await?;
            p.restart(&svc).await?;
            "legacy register + restart".to_string()
        };

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: (self.now)(),
            action: Action::Repair,
            service_id: Some(id.clone()),
            provider: Some(svc.spec.provider),
            actor: "api".to_string(),
            details,
        });
        Ok(())
    }

    pub async fn register(&self, id: &ServiceId) -> Result<()> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;

        p.register(&svc).await?;

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: (self.now)(),
            action: Action::Register,
            service_id: Some(id.clone()),
            provider: Some(svc.spec.provider),
            actor: "api".to_string(),
            details: String::new(),
        });
        Ok(())
    }

    pub async fn unregister(&self, id: &ServiceId) -> Result<()> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;

        p.unregister(&svc).await?;

        let _ = self.store.append_audit_event(AuditEvent {
            id: new_id(16)?,
            time: (self.now)(),
            action: Action::Unregister,
            service_id: Some(id.clone()),
            provider: Some(svc.spec.provider),
            actor: "api".to_string(),
            details: String::new(),
        });
        Ok(())
    }

    pub async fn status(&self, id: &ServiceId) -> Result<ServiceStatus> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;
        let mut st = p.status(&svc).await?;

        if st.service_id.0.trim().is_empty() {
            st.service_id = svc.id;
        }
        if st.provider.0.trim().is_empty() {
            st.provider = svc.spec.provider;
        }
        if st.observed_at.timestamp() == 0 && st.observed_at.timestamp_subsec_nanos() == 0 {
            st.observed_at = (self.now)();
        }
        Ok(st)
    }

    pub async fn logs(
        &self,
        id: &ServiceId,
        opts: LogsOptions,
    ) -> Result<Vec<crate::model::LogEntry>> {
        let svc = self.store.get_service(id)?;
        let p = self
            .registry
            .get(&svc.spec.provider)
            .ok_or_else(|| AppError::ProviderNotFound(svc.spec.provider.0.clone()))?;
        p.logs(&svc, opts).await
    }

    pub fn list_audit_events(&self, limit: Option<usize>) -> Result<Vec<AuditEvent>> {
        self.store.list_audit_events(limit)
    }

    pub fn export(&self) -> Result<Vec<u8>> {
        self.store.export()
    }

    pub fn import(&self, data: &[u8]) -> Result<()> {
        self.store.import(data)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub engine: Arc<Engine>,
}

fn service_groups(svc: &Service) -> Vec<String> {
    svc.spec
        .tags
        .iter()
        .filter_map(|tag| {
            let tag = tag.trim();
            let name = tag.strip_prefix(GROUP_TAG_PREFIX)?.trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect()
}

fn validate_registry_service_id(raw: String) -> Result<String> {
    let value = raw.trim().to_string();
    if value.is_empty() {
        return Err(AppError::BadRequest("service registry id is empty".to_string()));
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(AppError::BadRequest("service registry id is empty".to_string()));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(AppError::BadRequest(format!(
            "invalid service registry id {:?}",
            value
        )));
    }
    let mut len = 1usize;
    for ch in chars {
        len += 1;
        if len > 64 || !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
            return Err(AppError::BadRequest(format!(
                "invalid service registry id {:?}",
                value
            )));
        }
    }
    Ok(value)
}

pub async fn serve(cfg_path: Option<PathBuf>, bind: Option<String>) -> Result<()> {
    let loaded = load_config(cfg_path)?;
    let mut cfg = loaded.config;
    if let Some(b) = bind
        && !b.trim().is_empty()
    {
        cfg.listen_addr = b;
    }

    init_tracing(&cfg.log_level);

    if cfg.store.ty.trim().is_empty() {
        cfg.store.ty = "json".to_string();
    }
    if cfg.store.ty != "json" {
        return Err(AppError::BadRequest(format!(
            "unsupported store type {:?}",
            cfg.store.ty
        )));
    }

    let store = Arc::new(JsonStore::open(cfg.store.path.clone())?);
    let registry = ProviderRegistry::new();
    providers::register_defaults(&registry, PathBuf::from(&cfg.data_dir))?;
    let engine = Arc::new(Engine::new(store, registry));
    let loaded_services =
        service_registry::load_from_dir(&engine, Path::new(&cfg.service_registry_dir))?;
    if loaded_services > 0 {
        info!(
            "loaded {} registered services from {}",
            loaded_services, cfg.service_registry_dir
        );
    }
    let state = AppState {
        config: cfg.clone(),
        engine,
    };

    let app = api::router(state.clone());

    let listener = TcpListener::bind(&cfg.listen_addr)
        .await
        .map_err(|e| AppError::BadRequest(format!("bind {}: {e}", cfg.listen_addr)))?;
    info!("listening on {}", cfg.listen_addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|e| {
            warn!("server error: {e}");
            AppError::Internal
        })?;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn init_tracing(log_level: &str) {
    let filter = if log_level.trim().is_empty() {
        "info"
    } else {
        log_level
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .try_init();
}

pub fn load_config(path: Option<PathBuf>) -> Result<LoadedConfig> {
    let mut cfg = default_config()?;
    let cfg_path = match path {
        Some(p) => p,
        None => default_config_path()?,
    };

    match fs::read(&cfg_path) {
        Ok(b) => {
            if !b.iter().all(|c| c.is_ascii_whitespace()) {
                let file_cfg: ConfigFile = serde_json::from_slice(&b)?;
                file_cfg.apply(&mut cfg);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(AppError::Io(e)),
    }

    apply_defaults(&mut cfg)?;
    apply_env(&mut cfg);
    ensure_token(&mut cfg, &cfg_path)?;
    ensure_dirs(&cfg)?;
    Ok(LoadedConfig {
        config: cfg,
        path: cfg_path,
    })
}

pub fn default_config() -> Result<Config> {
    let cfg_dir = user_config_dir()?;
    let data_dir = cfg_dir.join("service-manager").join("data");
    let service_registry_dir = cfg_dir
        .join("openhouseai")
        .join("service-manager")
        .join("services.d");
    let openhouse_registry_source_dir = cfg_dir.join("openhouseai");
    Ok(Config {
        listen_addr: DEFAULT_LISTEN_ADDR.to_string(),
        data_dir: data_dir.to_string_lossy().to_string(),
        service_registry_dir: service_registry_dir.to_string_lossy().to_string(),
        openhouse_registry_source_dir: openhouse_registry_source_dir.to_string_lossy().to_string(),
        openhouse_registry_target_dir: openhouse_registry::DEFAULT_TARGET_DIR.to_string(),
        auth_token: String::new(),
        log_level: "info".to_string(),
        store: StoreConfig {
            ty: "json".to_string(),
            path: data_dir.join("store.json").to_string_lossy().to_string(),
        },
    })
}

pub fn default_config_path() -> Result<PathBuf> {
    let cfg_dir = user_config_dir()?;
    Ok(cfg_dir.join("service-manager").join("config.json"))
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    listen_addr: Option<String>,
    data_dir: Option<String>,
    service_registry_dir: Option<String>,
    openhouse_registry_source_dir: Option<String>,
    openhouse_registry_target_dir: Option<String>,
    auth_token: Option<String>,
    log_level: Option<String>,
    store: Option<StoreConfigFile>,
}

#[derive(Debug, Deserialize)]
struct StoreConfigFile {
    #[serde(rename = "type")]
    ty: Option<String>,
    path: Option<String>,
}

impl ConfigFile {
    fn apply(self, cfg: &mut Config) {
        if let Some(v) = self.listen_addr {
            cfg.listen_addr = v;
        }
        if let Some(v) = self.data_dir {
            cfg.data_dir = v;
        }
        if let Some(v) = self.service_registry_dir {
            cfg.service_registry_dir = v;
        }
        if let Some(v) = self.openhouse_registry_source_dir {
            cfg.openhouse_registry_source_dir = v;
        }
        if let Some(v) = self.openhouse_registry_target_dir {
            cfg.openhouse_registry_target_dir = v;
        }
        if let Some(v) = self.auth_token {
            cfg.auth_token = v;
        }
        if let Some(v) = self.log_level {
            cfg.log_level = v;
        }
        if let Some(s) = self.store {
            if let Some(v) = s.ty {
                cfg.store.ty = v;
            }
            if let Some(v) = s.path {
                cfg.store.path = v;
            }
        }
    }
}

fn apply_defaults(cfg: &mut Config) -> Result<()> {
    if cfg.listen_addr.trim().is_empty() {
        cfg.listen_addr = DEFAULT_LISTEN_ADDR.to_string();
    }
    if cfg.log_level.trim().is_empty() {
        cfg.log_level = "info".to_string();
    }
    if cfg.store.ty.trim().is_empty() {
        cfg.store.ty = "json".to_string();
    }

    if cfg.data_dir.trim().is_empty() {
        cfg.data_dir = default_config()?.data_dir;
    }
    if cfg.service_registry_dir.trim().is_empty() {
        cfg.service_registry_dir = default_config()?.service_registry_dir;
    }
    if cfg.openhouse_registry_source_dir.trim().is_empty() {
        cfg.openhouse_registry_source_dir = default_config()?.openhouse_registry_source_dir;
    }
    if cfg.openhouse_registry_target_dir.trim().is_empty() {
        cfg.openhouse_registry_target_dir = default_config()?.openhouse_registry_target_dir;
    }
    if cfg.store.path.trim().is_empty() {
        cfg.store.path = Path::new(&cfg.data_dir)
            .join("store.json")
            .to_string_lossy()
            .to_string();
    }
    Ok(())
}

fn apply_env(cfg: &mut Config) {
    if cfg.auth_token.trim().is_empty()
        && let Ok(tok) = env::var(ENV_AUTH_TOKEN)
    {
        let t = tok.trim().to_string();
        if !t.is_empty() {
            cfg.auth_token = t;
        }
    }
    if let Ok(dir) = env::var(ENV_SERVICE_REGISTRY_DIR) {
        let value = dir.trim().to_string();
        if !value.is_empty() {
            cfg.service_registry_dir = value;
        }
    }
    if let Ok(dir) = env::var(openhouse_registry::ENV_SOURCE_DIR) {
        let value = dir.trim().to_string();
        if !value.is_empty() {
            cfg.openhouse_registry_source_dir = value.clone();
            cfg.service_registry_dir = Path::new(&value)
                .join("service-manager")
                .join("services.d")
                .to_string_lossy()
                .to_string();
        }
    }
    if let Ok(dir) = env::var(openhouse_registry::ENV_TARGET_DIR) {
        let value = dir.trim().to_string();
        if !value.is_empty() {
            cfg.openhouse_registry_target_dir = value;
        }
    }
}

fn ensure_dirs(cfg: &Config) -> Result<()> {
    if cfg.data_dir.trim().is_empty() {
        return Err(AppError::BadRequest("config data_dir is empty".to_string()));
    }
    create_dir_all_700(Path::new(&cfg.data_dir))?;
    if !cfg.service_registry_dir.trim().is_empty() {
        create_dir_all_700(Path::new(&cfg.service_registry_dir))?;
    }
    if !cfg.openhouse_registry_source_dir.trim().is_empty() {
        create_dir_all_700(Path::new(&cfg.openhouse_registry_source_dir))?;
    }

    if !cfg.store.path.trim().is_empty()
        && let Some(parent) = Path::new(&cfg.store.path).parent()
    {
        create_dir_all_700(parent)?;
    }
    Ok(())
}

fn ensure_token(cfg: &mut Config, cfg_path: &Path) -> Result<()> {
    if !cfg.auth_token.trim().is_empty() {
        return Ok(());
    }

    let tok = random_token(32)?;
    cfg.auth_token = tok;

    // Best-effort persist.
    if cfg_path.as_os_str().is_empty() {
        return Ok(());
    }
    if let Some(parent) = cfg_path.parent() {
        create_dir_all_700(parent)?;
    }
    write_atomic_json(cfg_path, cfg)?;
    Ok(())
}

pub fn rotate_token(cfg_path: Option<PathBuf>) -> Result<(String, PathBuf)> {
    let mut loaded = load_config(cfg_path)?;
    loaded.config.auth_token = random_token(32)?;
    write_atomic_json(&loaded.path, &loaded.config)?;
    Ok((loaded.config.auth_token, loaded.path))
}

pub fn show_token(cfg_path: Option<PathBuf>) -> Result<(String, PathBuf)> {
    let loaded = load_config(cfg_path)?;
    Ok((loaded.config.auth_token, loaded.path))
}

pub fn config_paths(cfg_path: Option<PathBuf>) -> Result<(PathBuf, String, String)> {
    let loaded = load_config(cfg_path)?;
    Ok((
        loaded.path,
        loaded.config.data_dir,
        loaded.config.store.path,
    ))
}

fn user_config_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(v) = env::var_os("APPDATA") {
            return Ok(PathBuf::from(v));
        }
        if let Some(v) = env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(v).join("AppData").join("Roaming"));
        }
        return Err(AppError::BadRequest(
            "cannot determine user config dir".to_string(),
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let home =
            env::var_os("HOME").ok_or_else(|| AppError::BadRequest("HOME not set".to_string()))?;
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(v) = env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(v));
        }
        let home =
            env::var_os("HOME").ok_or_else(|| AppError::BadRequest("HOME not set".to_string()))?;
        Ok(PathBuf::from(home).join(".config"))
    }
}

fn create_dir_all_700(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn write_atomic_json<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    let b = serde_json::to_vec_pretty(v)?;
    let tmp = tmp_path(path);
    fs::write(&tmp, &b)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(path);
        fs::rename(&tmp, path).map_err(|_| AppError::Io(e))?;
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(".tmp");
    PathBuf::from(p)
}

fn random_token(n_bytes: usize) -> Result<String> {
    if n_bytes == 0 {
        return Err(AppError::BadRequest("token bytes must be > 0".to_string()));
    }
    let mut b = vec![0u8; n_bytes];
    getrandom::getrandom(&mut b).map_err(|_| AppError::Internal)?;
    Ok(hex_encode(&b))
}

fn new_id(n_bytes: usize) -> Result<String> {
    // 128-bit random hex string by default (n_bytes=16). Good enough for local use.
    random_token(n_bytes)
}

fn hex_encode(b: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(b.len() * 2);
    for &x in b {
        out.push(HEX[(x >> 4) as usize]);
        out.push(HEX[(x & 0x0f) as usize]);
    }
    String::from_utf8(out).expect("hex is valid utf8")
}

pub fn health_payload() -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "time": Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
    })
}

pub fn install_service(cfg_path: Option<PathBuf>, bind: Option<String>) -> Result<()> {
    let cfg_path = cfg_path.map(absolutize_path).transpose()?;
    let loaded = load_config(cfg_path)?;
    let cfg_path = loaded.path;

    let exe = env::current_exe().map_err(AppError::Io)?;
    let bind = bind.and_then(|b| {
        let b = b.trim().to_string();
        (!b.is_empty()).then_some(b)
    });

    // Termux runit.
    if let Some(prefix) = env::var_os("PREFIX") {
        let var_service = PathBuf::from(prefix).join("var").join("service");
        if var_service.is_dir() {
            install_termux_runit(&var_service, &exe, &cfg_path, bind.as_deref())?;
            eprintln!(
                "installed Termux runit service at {}",
                var_service.display()
            );
            return Ok(());
        }
    }

    // macOS launchd user agent.
    #[cfg(target_os = "macos")]
    {
        install_launchd_user(&exe, &cfg_path, bind.as_deref())?;
        eprintln!("installed launchd agent (user) com.service-manager");
        return Ok(());
    }

    // Linux/systemd user unit (best effort on other UNIXes that have systemctl).
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        install_systemd_user(&exe, &cfg_path, bind.as_deref())?;
        eprintln!("installed systemd user unit service-manager.service");
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(AppError::BadRequest(
        "install-service is only supported on Termux (runit), Linux systemd user services, or macOS launchd user agents"
            .to_string(),
    ))
}

pub fn uninstall_service(cfg_path: Option<PathBuf>) -> Result<()> {
    let _cfg_path = match cfg_path {
        Some(p) => absolutize_path(p)?,
        None => default_config_path()?,
    };

    // Termux runit.
    if let Some(prefix) = env::var_os("PREFIX") {
        let var_service = PathBuf::from(prefix).join("var").join("service");
        if var_service.is_dir() {
            uninstall_termux_runit(&var_service)?;
            eprintln!(
                "uninstalled Termux runit service at {}",
                var_service.display()
            );
            return Ok(());
        }
    }

    #[cfg(target_os = "macos")]
    {
        uninstall_launchd_user()?;
        eprintln!("uninstalled launchd agent (user) com.service-manager");
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        uninstall_systemd_user()?;
        eprintln!("uninstalled systemd user unit service-manager.service");
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(AppError::BadRequest(format!(
        "uninstall-service is unsupported on this platform (config path was {})",
        _cfg_path.display()
    )))
}

fn absolutize_path(p: PathBuf) -> Result<PathBuf> {
    if p.is_absolute() {
        return Ok(p);
    }
    let cwd = env::current_dir().map_err(AppError::Io)?;
    Ok(cwd.join(p))
}

fn service_args(exe: &Path, cfg_path: &Path, bind: Option<&str>) -> Vec<String> {
    let mut args = vec![
        exe.to_string_lossy().to_string(),
        "serve".to_string(),
        "--config".to_string(),
        cfg_path.to_string_lossy().to_string(),
    ];
    if let Some(b) = bind {
        args.push("--bind".to_string());
        args.push(b.to_string());
    }
    args
}

fn sh_quote(s: &str) -> String {
    // POSIX shell single-quote escaping: close/open around embedded single quotes.
    if s.is_empty() {
        return "''".to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\"'\"'");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn systemd_escape_arg(s: &str) -> String {
    // systemd accepts double-quoted arguments in ExecStart. Escape " and \.
    let needs_quotes = s
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\\');
    if !needs_quotes {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn write_text_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o755));
        }
    }
    fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

#[cfg(unix)]
fn write_executable_file(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o755));
    }
    fs::write(path, contents)?;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
    Ok(())
}

fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program).args(args).output().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            AppError::BadRequest(format!("{program} not found in PATH"))
        } else {
            AppError::Io(e)
        }
    })?;
    if out.status.success() {
        return Ok(());
    }
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut msg = format!("command failed (exit {code}): {program} {}", args.join(" "));
    if !stderr.is_empty() {
        msg.push_str(&format!("\nstderr: {stderr}"));
    }
    if !stdout.is_empty() {
        msg.push_str(&format!("\nstdout: {stdout}"));
    }
    if program == "systemctl"
        && (stderr.contains("Failed to connect to bus")
            || stderr.contains("DBUS_SESSION_BUS_ADDRESS")
            || stderr.contains("XDG_RUNTIME_DIR"))
    {
        msg.push_str(
            "\n\nHint: systemd user services require a working user session bus. Try running from a normal login session, or ensure $XDG_RUNTIME_DIR and $DBUS_SESSION_BUS_ADDRESS are set, or enable lingering (loginctl enable-linger $USER) on headless machines.",
        );
    }
    Err(AppError::BadRequest(msg))
}

fn install_termux_runit(
    var_service_dir: &Path,
    exe: &Path,
    cfg_path: &Path,
    bind: Option<&str>,
) -> Result<()> {
    let svc_dir = var_service_dir.join("service-manager");
    fs::create_dir_all(&svc_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&svc_dir, fs::Permissions::from_mode(0o700));
    }

    let run_path = svc_dir.join("run");
    let args = service_args(exe, cfg_path, bind);
    let script = termux_runit_run_script(&args);
    #[cfg(unix)]
    write_executable_file(&run_path, &script)?;
    #[cfg(not(unix))]
    {
        let _ = (run_path, script);
        return Err(AppError::BadRequest(
            "Termux runit install requires a Unix-like platform".to_string(),
        ));
    }

    // Ensure enabled by default.
    let down_path = svc_dir.join("down");
    let _ = fs::remove_file(down_path);

    // Best-effort start if sv exists.
    let _ = Command::new("sv").args(["up", "service-manager"]).output();

    Ok(())
}

fn uninstall_termux_runit(var_service_dir: &Path) -> Result<()> {
    let svc_dir = var_service_dir.join("service-manager");
    if !svc_dir.exists() {
        return Ok(());
    }

    let _ = Command::new("sv")
        .args(["down", "service-manager"])
        .output();
    // Remove service directory/symlink.
    if svc_dir.is_dir() {
        fs::remove_dir_all(&svc_dir)?;
    } else {
        let _ = fs::remove_file(&svc_dir);
    }
    Ok(())
}

fn termux_runit_run_script(args: &[String]) -> String {
    // termux-services expects an executable `run` that execs the daemon in foreground.
    // Use Termux's canonical shell path in the shebang.
    let mut cmd = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            cmd.push(' ');
        }
        cmd.push_str(&sh_quote(a));
    }
    format!(
        "#!/data/data/com.termux/files/usr/bin/sh\n\
exec 2>&1\n\
exec {cmd} 2>&1\n"
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_systemd_user(exe: &Path, cfg_path: &Path, bind: Option<&str>) -> Result<()> {
    let home = user_home_dir()?;
    let unit_dir = home.join(".config").join("systemd").join("user");
    let unit_path = unit_dir.join("service-manager.service");
    let unit = systemd_user_unit_contents(exe, cfg_path, bind);
    write_text_file(&unit_path, &unit)?;

    run_cmd("systemctl", &["--user", "daemon-reload"])?;
    run_cmd(
        "systemctl",
        &["--user", "enable", "--now", "service-manager.service"],
    )?;
    // Kick again in case the unit was already enabled but stopped.
    let _ = run_cmd("systemctl", &["--user", "start", "service-manager.service"]);
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn uninstall_systemd_user() -> Result<()> {
    // Best-effort stop/disable; ignore failures for missing/offline systemd.
    let _ = run_cmd(
        "systemctl",
        &["--user", "disable", "--now", "service-manager.service"],
    );
    let home = user_home_dir()?;
    let unit_path = home
        .join(".config")
        .join("systemd")
        .join("user")
        .join("service-manager.service");
    let _ = fs::remove_file(&unit_path);
    let _ = run_cmd("systemctl", &["--user", "daemon-reload"]);
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn systemd_user_unit_contents(exe: &Path, cfg_path: &Path, bind: Option<&str>) -> String {
    let args = service_args(exe, cfg_path, bind);
    let exec = args
        .iter()
        .map(|a| systemd_escape_arg(a))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\n\
Description=service-manager (local-only service manager)\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={exec}\n\
Restart=on-failure\n\
RestartSec=3\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    )
}

#[cfg(target_os = "macos")]
fn install_launchd_user(exe: &Path, cfg_path: &Path, bind: Option<&str>) -> Result<()> {
    let home = user_home_dir()?;
    let agents_dir = home.join("Library").join("LaunchAgents");
    let plist_path = agents_dir.join("com.service-manager.plist");
    let plist = launchd_agent_plist_contents(exe, cfg_path, bind);
    write_text_file(&plist_path, &plist)?;

    let uid = unsafe { libc::geteuid() };
    let domain = format!("gui/{uid}");
    let plist_str = plist_path.to_string_lossy().to_string();

    // bootstrap can error if already loaded; treat that as idempotent.
    match Command::new("launchctl")
        .args(["bootstrap", &domain, &plist_str])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
            if !stderr.contains("already") {
                return Err(AppError::BadRequest(format!(
                    "launchctl bootstrap failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(AppError::BadRequest(
                "launchctl not found in PATH".to_string(),
            ));
        }
        Err(e) => return Err(AppError::Io(e)),
    }

    let target = format!("{domain}/com.service-manager");
    let _ = Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .output();
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd_user() -> Result<()> {
    let home = user_home_dir()?;
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join("com.service-manager.plist");
    let uid = unsafe { libc::geteuid() };
    let domain = format!("gui/{uid}");
    let plist_str = plist_path.to_string_lossy().to_string();

    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_str])
        .output();
    let _ = fs::remove_file(&plist_path);
    Ok(())
}

#[cfg(target_os = "macos")]
fn launchd_agent_plist_contents(exe: &Path, cfg_path: &Path, bind: Option<&str>) -> String {
    let args = service_args(exe, cfg_path, bind);
    let args_xml = args
        .iter()
        .map(|a| format!("      <string>{}</string>\n", xml_escape(a)))
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
  <dict>\n\
    <key>Label</key>\n\
    <string>com.service-manager</string>\n\
    <key>ProgramArguments</key>\n\
    <array>\n\
{args_xml}    </array>\n\
    <key>RunAtLoad</key>\n\
    <true/>\n\
    <key>KeepAlive</key>\n\
    <true/>\n\
  </dict>\n\
</plist>\n"
    )
}

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn user_home_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(v) = env::var_os("USERPROFILE") {
            return Ok(PathBuf::from(v));
        }
        return Err(AppError::BadRequest(
            "cannot determine home dir".to_string(),
        ));
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home =
            env::var_os("HOME").ok_or_else(|| AppError::BadRequest("HOME not set".to_string()))?;
        Ok(PathBuf::from(home))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn generates_systemd_user_unit() {
        let exe = Path::new("/opt/bin/service-manager");
        let cfg = Path::new("/home/alice/.config/service-manager/config.json");
        let unit = systemd_user_unit_contents(exe, cfg, Some("127.0.0.1:9999"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("/opt/bin/service-manager"));
        assert!(unit.contains("serve"));
        assert!(unit.contains("--config"));
        assert!(unit.contains("/home/alice/.config/service-manager/config.json"));
        assert!(unit.contains("--bind"));
        assert!(unit.contains("127.0.0.1:9999"));
    }

    #[test]
    fn generates_termux_runit_script() {
        let args = vec![
            "/data/data/com.termux/files/usr/bin/service-manager".to_string(),
            "serve".to_string(),
            "--config".to_string(),
            "/data/data/com.termux/files/home/.config/service-manager/config.json".to_string(),
        ];
        let script = termux_runit_run_script(&args);
        assert!(script.starts_with("#!/data/data/com.termux/files/usr/bin/sh\n"));
        assert!(script.contains("exec "));
        assert!(script.contains("serve"));
        assert!(script.contains("--config"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn generates_launchd_plist() {
        let exe = Path::new("/usr/local/bin/service-manager");
        let cfg = Path::new("/Users/alice/Library/Application Support/service-manager/config.json");
        let plist = launchd_agent_plist_contents(exe, cfg, None);
        assert!(plist.contains("<string>com.service-manager</string>"));
        assert!(plist.contains("/usr/local/bin/service-manager"));
        assert!(plist.contains("serve"));
        assert!(plist.contains("--config"));
    }
}
