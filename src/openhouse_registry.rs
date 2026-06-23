use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::{AppError, Result},
    model::ServiceSpec,
};

pub const DEFAULT_TARGET_DIR: &str = "/data/data/com.termux/files/home/.config/openhouseai";
pub const ENV_SOURCE_DIR: &str = "OPENHOUSEAI_REGISTRY_SOURCE_DIR";
pub const ENV_TARGET_DIR: &str = "OPENHOUSEAI_REGISTRY_TARGET_DIR";

const COMPONENTS_DIR: &str = "components.d";
const SERVICES_DIR: &str = "service-manager/services.d";
const AI_DOCS_DIR: &str = "ai-docs";
const STATE_FILE: &str = "registry-state.json";
const STATE_VERSION: u32 = 1;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct RegistryConfig {
    pub source_dir: PathBuf,
    pub target_dir: PathBuf,
}

impl RegistryConfig {
    pub fn new(source_dir: impl Into<PathBuf>, target_dir: impl Into<PathBuf>) -> Self {
        Self {
            source_dir: source_dir.into(),
            target_dir: target_dir.into(),
        }
    }

    pub fn components_dir(&self) -> PathBuf {
        self.source_dir.join(COMPONENTS_DIR)
    }

    fn services_dir(&self) -> PathBuf {
        self.source_dir.join(SERVICES_DIR)
    }

    fn ai_docs_dir(&self) -> PathBuf {
        self.source_dir.join(AI_DOCS_DIR)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryFileState {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryState {
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub source_path: String,
    pub target_path: String,
    pub status: String,
    #[serde(default)]
    pub files: Vec<RegistryFileState>,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentRecord {
    pub id: String,
    pub path: String,
    pub manifest: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryMutationResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<ComponentRecord>,
    pub state: RegistryState,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryApplyRequest {
    #[serde(default)]
    pub component: Option<Value>,
    #[serde(default)]
    pub components: Vec<Value>,
    #[serde(default)]
    pub services: Vec<ServiceRegistryEntry>,
    #[serde(default)]
    pub ai_docs: Vec<AiDocInput>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryApplyResult {
    pub components: Vec<String>,
    pub services: Vec<String>,
    pub ai_docs: Vec<String>,
    pub state: RegistryState,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ServiceRegistryEntry {
    Wrapped(ServiceRegistryWrappedEntry),
    Spec(ServiceSpec),
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRegistryWrappedEntry {
    #[serde(default, rename = "schemaVersion")]
    pub _schema_version: Option<u32>,
    #[serde(default)]
    pub id: Option<String>,
    pub service: ServiceSpec,
}

impl ServiceRegistryEntry {
    pub fn into_parts(self) -> Result<(String, ServiceSpec)> {
        let (id, mut service) = match self {
            ServiceRegistryEntry::Wrapped(wrapped) => {
                let service_id = wrapped
                    .id
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| wrapped.service.name.clone());
                (service_id, wrapped.service)
            }
            ServiceRegistryEntry::Spec(service) => (service.name.clone(), service),
        };
        let id = validate_registry_id("service id", &id)?;
        service.validate()?;
        Ok((id, service))
    }
}

#[derive(Clone, Debug)]
pub struct PreparedRegistryApply {
    pub components: Vec<(String, Value)>,
    pub services: Vec<(String, ServiceSpec)>,
    pub ai_docs: Vec<PreparedAiDoc>,
}

#[derive(Clone, Debug)]
pub struct PreparedAiDoc {
    pub relative_path: PathBuf,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiDocInput {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ServiceRegistryDocument {
    Batch(ServiceRegistryBatchDocument),
    Item(ServiceRegistryEntry),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServiceRegistryBatchDocument {
    #[serde(default)]
    _schema_version: Option<u32>,
    services: Vec<ServiceRegistryEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceRegistryFile<'a> {
    schema_version: u32,
    id: &'a str,
    service: &'a ServiceSpec,
}

pub fn prepare_apply_request(request: RegistryApplyRequest) -> Result<PreparedRegistryApply> {
    let mut component_ids = BTreeSet::new();
    let mut prepared_components = Vec::new();
    let mut components = request.components;
    if let Some(component) = request.component {
        components.push(component);
    }

    for manifest in components {
        let id = component_id_from_manifest(&manifest)?;
        validate_component_manifest(&id, &manifest)?;
        if !component_ids.insert(id.clone()) {
            return Err(AppError::BadRequest(format!(
                "duplicate component id in apply payload: {id:?}"
            )));
        }
        prepared_components.push((id, manifest));
    }

    let mut service_ids = BTreeSet::new();
    let mut prepared_services = Vec::new();
    for entry in request.services {
        let (id, service) = entry.into_parts()?;
        if !service_ids.insert(id.clone()) {
            return Err(AppError::BadRequest(format!(
                "duplicate service id in apply payload: {id:?}"
            )));
        }
        prepared_services.push((id, service));
    }

    let mut ai_doc_paths = BTreeSet::new();
    let mut prepared_ai_docs = Vec::new();
    for doc in request.ai_docs {
        let relative_path = validate_relative_path("ai doc path", &doc.path)?;
        let normalized = path_to_slash(&relative_path);
        if !ai_doc_paths.insert(normalized.clone()) {
            return Err(AppError::BadRequest(format!(
                "duplicate aiDocs path in apply payload: {normalized:?}"
            )));
        }
        prepared_ai_docs.push(PreparedAiDoc {
            relative_path,
            content: doc.content,
        });
    }

    Ok(PreparedRegistryApply {
        components: prepared_components,
        services: prepared_services,
        ai_docs: prepared_ai_docs,
    })
}

pub fn apply_prepared_registry(
    cfg: &RegistryConfig,
    prepared: &PreparedRegistryApply,
) -> Result<RegistryApplyResult> {
    ensure_source_layout(cfg)?;
    let staging_source = unique_sibling_path(&cfg.source_dir, "apply-staging");
    if let Some(parent) = staging_source.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&staging_source)?;
    copy_dir_contents(&cfg.source_dir, &staging_source)?;
    let staging_cfg = RegistryConfig::new(&staging_source, &cfg.target_dir);
    ensure_source_layout(&staging_cfg)?;

    let mut component_ids = Vec::new();
    for (id, manifest) in &prepared.components {
        put_component(&staging_cfg, id, manifest.clone())?;
        component_ids.push(id.clone());
    }

    let mut service_ids = Vec::new();
    for (id, service) in &prepared.services {
        write_service_registry(&staging_cfg, id, service)?;
        service_ids.push(id.clone());
    }

    let mut ai_doc_paths = Vec::new();
    for doc in &prepared.ai_docs {
        let path = write_prepared_ai_doc(&staging_cfg, doc)?;
        ai_doc_paths.push(path);
    }

    validate_registry_tree(&staging_source)?;
    let state = commit_staged_source_and_sync(cfg, &staging_source)?;
    Ok(RegistryApplyResult {
        components: component_ids,
        services: service_ids,
        ai_docs: ai_doc_paths,
        state,
    })
}

pub fn read_state(cfg: &RegistryConfig) -> Result<RegistryState> {
    let path = cfg.source_dir.join(STATE_FILE);
    match fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RegistryState {
            version: STATE_VERSION,
            generated_at: Utc::now(),
            source_path: cfg.source_dir.to_string_lossy().to_string(),
            target_path: cfg.target_dir.to_string_lossy().to_string(),
            status: "missing".to_string(),
            files: Vec::new(),
            errors: vec!["registry-state.json does not exist".to_string()],
        }),
        Err(e) => Err(AppError::Io(e)),
    }
}

pub fn list_components(cfg: &RegistryConfig) -> Result<Vec<ComponentRecord>> {
    let dir = cfg.components_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut paths = json_files_in_dir(&dir)?;
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let manifest = read_json_value(&path)?;
        let id = component_id_from_manifest(&manifest)?;
        validate_component_manifest(&id, &manifest)?;
        out.push(ComponentRecord {
            id,
            path: relative_to(&cfg.source_dir, &path),
            manifest,
        });
    }
    Ok(out)
}

pub fn get_component(cfg: &RegistryConfig, id: &str) -> Result<ComponentRecord> {
    let id = validate_registry_id("component id", id)?;
    let path = component_path(cfg, &id)?;
    match read_json_value(&path) {
        Ok(manifest) => {
            validate_component_manifest(&id, &manifest)?;
            Ok(ComponentRecord {
                id,
                path: relative_to(&cfg.source_dir, &path),
                manifest,
            })
        }
        Err(AppError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Err(AppError::NotFound),
        Err(e) => Err(e),
    }
}

pub fn put_component(cfg: &RegistryConfig, id: &str, manifest: Value) -> Result<ComponentRecord> {
    let id = validate_registry_id("component id", id)?;
    validate_component_manifest(&id, &manifest)?;
    let path = component_path(cfg, &id)?;
    write_atomic_json(&path, &manifest, 0o600)?;
    Ok(ComponentRecord {
        id,
        path: relative_to(&cfg.source_dir, &path),
        manifest,
    })
}

pub fn delete_component(cfg: &RegistryConfig, id: &str) -> Result<()> {
    let id = validate_registry_id("component id", id)?;
    let path = component_path(cfg, &id)?;
    match fs::remove_file(&path) {
        Ok(()) => {
            fsync_parent_best_effort(&path);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(AppError::NotFound),
        Err(e) => Err(AppError::Io(e)),
    }
}

#[cfg(test)]
fn apply_registry(
    cfg: &RegistryConfig,
    request: RegistryApplyRequest,
) -> Result<RegistryApplyResult> {
    let prepared = prepare_apply_request(request)?;
    apply_prepared_registry(cfg, &prepared)
}

pub fn sync_registry(cfg: &RegistryConfig) -> Result<RegistryState> {
    ensure_source_layout(cfg)?;
    validate_registry_tree(&cfg.source_dir)?;

    let files = collect_registry_files(&cfg.source_dir)?;
    let state = RegistryState {
        version: STATE_VERSION,
        generated_at: Utc::now(),
        source_path: cfg.source_dir.to_string_lossy().to_string(),
        target_path: cfg.target_dir.to_string_lossy().to_string(),
        status: "ok".to_string(),
        files,
        errors: Vec::new(),
    };

    if same_path(&cfg.source_dir, &cfg.target_dir) {
        write_atomic_json(&cfg.source_dir.join(STATE_FILE), &state, 0o600)?;
        return Ok(state);
    }

    let staging = unique_sibling_path(&cfg.target_dir, "sync-staging");
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&staging)?;
    copy_dir_contents(&cfg.source_dir, &staging)?;
    validate_registry_tree(&staging)?;
    replace_dir(&staging, &cfg.target_dir)?;
    write_atomic_json(&cfg.target_dir.join(STATE_FILE), &state, 0o600)?;
    write_atomic_json(&cfg.source_dir.join(STATE_FILE), &state, 0o600)?;
    Ok(state)
}

fn commit_staged_source_and_sync(
    cfg: &RegistryConfig,
    staging_source: &Path,
) -> Result<RegistryState> {
    if let Some(parent) = cfg.source_dir.parent() {
        fs::create_dir_all(parent)?;
    }

    if !cfg.source_dir.exists() {
        fs::rename(staging_source, &cfg.source_dir)?;
        fsync_parent_best_effort(&cfg.source_dir);
        return sync_registry(cfg);
    }

    let backup = unique_sibling_path(&cfg.source_dir, "apply-backup");
    fs::rename(&cfg.source_dir, &backup)?;
    match fs::rename(staging_source, &cfg.source_dir) {
        Ok(()) => {
            fsync_parent_best_effort(&cfg.source_dir);
            match sync_registry(cfg) {
                Ok(state) => {
                    let _ = remove_path_all(&backup);
                    Ok(state)
                }
                Err(err) => {
                    let failed = unique_sibling_path(&cfg.source_dir, "apply-failed");
                    let _ = fs::rename(&cfg.source_dir, &failed);
                    let _ = fs::rename(&backup, &cfg.source_dir);
                    let _ = sync_registry(cfg);
                    let _ = remove_path_all(&failed);
                    Err(err)
                }
            }
        }
        Err(err) => {
            let _ = fs::rename(&backup, &cfg.source_dir);
            Err(AppError::Io(err))
        }
    }
}

pub fn validate_component_manifest(expected_id: &str, manifest: &Value) -> Result<()> {
    let obj = manifest
        .as_object()
        .ok_or_else(|| AppError::BadRequest("component manifest must be a JSON object".to_string()))?;

    let actual_id = obj
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::BadRequest("component manifest id is required".to_string()))?;
    let expected_id = validate_registry_id("component id", expected_id)?;
    let actual_id = validate_registry_id("component id", actual_id)?;
    if actual_id != expected_id {
        return Err(AppError::BadRequest(format!(
            "component manifest id {:?} does not match path id {:?}",
            actual_id, expected_id
        )));
    }

    for layer in ["shellMenu", "smallphoneApp", "serviceManager", "ai"] {
        match obj.get(layer) {
            Some(Value::Object(_)) => {}
            Some(_) => {
                return Err(AppError::BadRequest(format!(
                    "component manifest {layer} must be an object"
                )));
            }
            None => {
                return Err(AppError::BadRequest(format!(
                    "component manifest missing required layer {layer}"
                )));
            }
        }
    }

    let mut paths = Vec::new();
    find_forbidden_component_keys(manifest, "$", &mut paths);
    if !paths.is_empty() {
        return Err(AppError::BadRequest(format!(
            "component manifest contains forbidden runtime keys: {}",
            paths.join(", ")
        )));
    }

    Ok(())
}

fn component_id_from_manifest(manifest: &Value) -> Result<String> {
    let id = manifest
        .as_object()
        .and_then(|obj| obj.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::BadRequest("component manifest id is required".to_string()))?;
    validate_registry_id("component id", id)
}

fn component_path(cfg: &RegistryConfig, id: &str) -> Result<PathBuf> {
    let id = validate_registry_id("component id", id)?;
    Ok(cfg.components_dir().join(format!("{id}.json")))
}

fn write_service_registry(cfg: &RegistryConfig, id: &str, service: &ServiceSpec) -> Result<()> {
    let id = validate_registry_id("service id", id)?;
    let doc = ServiceRegistryFile {
        schema_version: 1,
        id: &id,
        service,
    };
    write_atomic_json(&cfg.services_dir().join(format!("{id}.json")), &doc, 0o600)
}

fn write_prepared_ai_doc(cfg: &RegistryConfig, doc: &PreparedAiDoc) -> Result<String> {
    let path = cfg.ai_docs_dir().join(&doc.relative_path);
    write_atomic_bytes(&path, doc.content.as_bytes(), 0o600)?;
    Ok(path_to_slash(&doc.relative_path))
}

fn ensure_source_layout(cfg: &RegistryConfig) -> Result<()> {
    create_dir_all_mode(&cfg.source_dir, 0o700)?;
    create_dir_all_mode(&cfg.components_dir(), 0o700)?;
    create_dir_all_mode(&cfg.services_dir(), 0o700)?;
    create_dir_all_mode(&cfg.ai_docs_dir(), 0o700)?;
    Ok(())
}

fn validate_registry_tree(root: &Path) -> Result<()> {
    let components = root.join(COMPONENTS_DIR);
    if components.is_dir() {
        for path in json_files_in_dir(&components)? {
            let manifest = read_json_value(&path)?;
            let id = component_id_from_manifest(&manifest)?;
            validate_component_manifest(&id, &manifest).map_err(|e| match e {
                AppError::BadRequest(msg) => {
                    AppError::BadRequest(format!("{}: {msg}", path.display()))
                }
                other => other,
            })?;
        }
    }

    let services = root.join(SERVICES_DIR);
    if services.is_dir() {
        for path in json_files_in_dir(&services)? {
            let bytes = fs::read(&path)?;
            validate_service_registry_doc(&bytes).map_err(|e| match e {
                AppError::BadRequest(msg) => {
                    AppError::BadRequest(format!("{}: {msg}", path.display()))
                }
                other => other,
            })?;
        }
    }
    Ok(())
}

fn validate_service_registry_doc(bytes: &[u8]) -> Result<()> {
    let doc: ServiceRegistryDocument = serde_json::from_slice(bytes)?;
    let entries = match doc {
        ServiceRegistryDocument::Batch(batch) => batch.services,
        ServiceRegistryDocument::Item(item) => vec![item],
    };
    for entry in entries {
        let _ = entry.into_parts()?;
    }
    Ok(())
}

fn json_files_in_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_json_value(path: &Path) -> Result<Value> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(AppError::Json)
}

fn collect_registry_files(root: &Path) -> Result<Vec<RegistryFileState>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_files_recursive(root, root, &mut paths)?;
    paths.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = Vec::with_capacity(paths.len());
    for (rel, path) in paths {
        let bytes = fs::read(&path)?;
        out.push(RegistryFileState {
            path: rel,
            sha256: sha256_hex(&bytes),
            bytes: bytes.len() as u64,
        });
    }
    Ok(out)
}

fn collect_files_recursive(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_symlink() {
            return Err(AppError::BadRequest(format!(
                "registry tree must not contain symlink: {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            collect_files_recursive(root, &path, out)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let rel = relative_to(root, &path);
        if should_skip_registry_file(&rel) {
            continue;
        }
        out.push((rel, path));
    }
    Ok(())
}

fn should_skip_registry_file(rel: &str) -> bool {
    rel == STATE_FILE
        || rel.ends_with(".tmp")
        || rel.contains(".tmp.")
        || rel.contains(".sync-staging.")
        || rel.contains(".sync-backup.")
}

fn copy_dir_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if file_type.is_symlink() {
            return Err(AppError::BadRequest(format!(
                "registry tree must not contain symlink: {}",
                source_path.display()
            )));
        }
        if file_type.is_dir() {
            fs::create_dir_all(&destination_path)?;
            copy_dir_contents(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            let rel = relative_to(source, &source_path);
            if should_skip_registry_file(&rel) {
                continue;
            }
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn replace_dir(staging: &Path, target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    if !target.exists() {
        fs::rename(staging, target)?;
        fsync_parent_best_effort(target);
        return Ok(());
    }

    let backup = unique_sibling_path(target, "sync-backup");
    fs::rename(target, &backup)?;
    match fs::rename(staging, target) {
        Ok(()) => {
            fsync_parent_best_effort(target);
            let _ = remove_path_all(&backup);
            Ok(())
        }
        Err(e) => {
            let _ = fs::rename(&backup, target);
            Err(AppError::Io(e))
        }
    }
}

fn remove_path_all(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn write_atomic_json<T: Serialize>(path: &Path, value: &T, mode: u32) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    write_atomic_bytes(path, &bytes, mode)
}

fn write_atomic_bytes(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = unique_temp_file_path(path);
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    set_file_mode_best_effort(&tmp, mode);
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(path);
        fs::rename(&tmp, path).map_err(|_| AppError::Io(e))?;
    }
    fsync_parent_best_effort(path);
    Ok(())
}

fn create_dir_all_mode(path: &Path, mode: u32) -> Result<()> {
    fs::create_dir_all(path)?;
    set_dir_mode_best_effort(path, mode);
    Ok(())
}

fn set_file_mode_best_effort(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn set_dir_mode_best_effort(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

fn fsync_parent_best_effort(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

fn unique_temp_file_path(path: &Path) -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("registry-file");
    path.with_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        id
    ))
}

fn unique_sibling_path(path: &Path, tag: &str) -> PathBuf {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("openhouseai");
    path.with_file_name(format!(
        ".{file_name}.{tag}.{}.{}",
        std::process::id(),
        id
    ))
}

fn validate_registry_id(label: &str, raw: &str) -> Result<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(AppError::BadRequest(format!("{label} is required")));
    }
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(AppError::BadRequest(format!("{label} is required")));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(AppError::BadRequest(format!("invalid {label}: {value:?}")));
    }
    let mut len = 1usize;
    for ch in chars {
        len += 1;
        if len > 64 || !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
            return Err(AppError::BadRequest(format!("invalid {label}: {value:?}")));
        }
    }
    Ok(value.to_string())
}

fn validate_relative_path(label: &str, raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AppError::BadRequest(format!("{label} is required")));
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return Err(AppError::BadRequest(format!("{label} must be relative")));
    }
    let mut seen_normal = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => seen_normal = true,
            Component::CurDir => {}
            _ => {
                return Err(AppError::BadRequest(format!(
                    "{label} must not contain parent or prefix components"
                )));
            }
        }
    }
    if !seen_normal {
        return Err(AppError::BadRequest(format!("{label} is required")));
    }
    Ok(path)
}

fn find_forbidden_component_keys(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if matches!(key.as_str(), "command" | "shell" | "script" | "args") {
                    out.push(child_path.clone());
                }
                find_forbidden_component_keys(child, &child_path, out);
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                find_forbidden_component_keys(child, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(path_to_slash)
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

fn path_to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    const H0: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() + 8) % 64 != 0 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let mut h = H0;
    let mut w = [0u32; 64];
    for chunk in padded.chunks_exact(64) {
        for (i, word) in w.iter_mut().take(16).enumerate() {
            let j = i * 4;
            *word = u32::from_be_bytes([chunk[j], chunk[j + 1], chunk[j + 2], chunk[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_registry() -> RegistryConfig {
        let uniq = format!(
            "service-manager-registry-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let base = std::env::temp_dir().join(uniq);
        RegistryConfig::new(base.join("source"), base.join("target"))
    }

    fn manifest(id: &str) -> Value {
        serde_json::json!({
            "schemaVersion": 1,
            "id": id,
            "title": "Demo",
            "shellMenu": {},
            "smallphoneApp": {},
            "serviceManager": {},
            "ai": {}
        })
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn component_manifest_rejects_runtime_keys() {
        let bad = serde_json::json!({
            "id": "demo",
            "shellMenu": {},
            "smallphoneApp": {},
            "serviceManager": {"script": "run.sh"},
            "ai": {}
        });
        let err = validate_component_manifest("demo", &bad).unwrap_err();
        assert!(err.to_string().contains("forbidden runtime keys"));
    }

    #[test]
    fn sync_writes_target_and_state() {
        let cfg = temp_registry();
        let component = put_component(&cfg, "demo", manifest("demo")).unwrap();
        assert_eq!(component.id, "demo");

        let state = sync_registry(&cfg).unwrap();
        assert_eq!(state.status, "ok");
        assert!(
            state
                .files
                .iter()
                .any(|file| file.path == "components.d/demo.json")
        );
        assert!(cfg.target_dir.join("components.d/demo.json").is_file());
        assert!(cfg.target_dir.join(STATE_FILE).is_file());
    }

    #[test]
    fn apply_writes_service_registry_with_command_allowed() {
        let cfg = temp_registry();
        let req = RegistryApplyRequest {
            component: Some(manifest("demo")),
            components: Vec::new(),
            services: vec![ServiceRegistryEntry::Wrapped(ServiceRegistryWrappedEntry {
                _schema_version: None,
                id: Some("demo-service".to_string()),
                service: serde_json::from_value(serde_json::json!({
                    "name": "demo-service",
                    "provider": "process",
                    "command": ["sleep", "1"]
                }))
                .unwrap(),
            })],
            ai_docs: vec![AiDocInput {
                path: "demo/openhouse.ai.md".to_string(),
                content: "# Demo".to_string(),
            }],
        };

        let out = apply_registry(&cfg, req).unwrap();
        assert_eq!(out.components, vec!["demo"]);
        assert_eq!(out.services, vec!["demo-service"]);
        assert_eq!(out.ai_docs, vec!["demo/openhouse.ai.md"]);
        assert!(cfg.target_dir.join("service-manager/services.d/demo-service.json").is_file());
        assert!(cfg.target_dir.join("ai-docs/demo/openhouse.ai.md").is_file());
    }
}
