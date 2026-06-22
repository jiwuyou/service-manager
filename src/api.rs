use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    assets, auth,
    error::{AppError, Result},
    model::{Action, LogsOptions, Service, ServiceId, ServiceSpec, ServiceStatus},
    server::{AppState, health_payload},
};

pub fn router(state: AppState) -> Router {
    let protected: Router<AppState> = Router::new()
        .route("/providers", get(providers_list))
        .route("/groups", get(groups_list))
        .route("/groups/:name/status", get(group_status))
        .route("/groups/:name/start", post(group_start))
        .route("/groups/:name/stop", post(group_stop))
        .route("/groups/:name/restart", post(group_restart))
        .route("/services", get(services_list).post(services_create))
        .route("/services/statuses", get(services_statuses))
        .route(
            "/services/:id",
            get(services_get)
                .put(services_update)
                .delete(services_delete),
        )
        .route("/services/:id/start", post(service_start))
        .route("/services/:id/stop", post(service_stop))
        .route("/services/:id/restart", post(service_restart))
        .route("/services/:id/repair", post(service_repair))
        .route("/services/:id/register", post(service_register))
        .route("/services/:id/unregister", post(service_unregister))
        .route("/services/:id/status", get(service_status))
        .route("/services/:id/logs", get(service_logs))
        .route("/audit", get(audit_list))
        .route("/export", get(export_store))
        .route("/import", post(import_store))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_bearer_token,
        ));

    Router::<AppState>::new()
        .route("/", get(root))
        .route("/index.html", get(root))
        .route("/app.js", get(asset_app_js))
        .route("/styles.css", get(asset_styles_css))
        .route("/api/v1/health", get(health))
        .nest("/api/v1", protected)
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
        .with_state(state)
}

async fn root() -> Response {
    assets::response("/").expect("index asset is embedded")
}

async fn asset_app_js() -> Response {
    assets::response("/app.js").expect("app.js asset is embedded")
}

async fn asset_styles_css() -> Response {
    assets::response("/styles.css").expect("styles.css asset is embedded")
}

async fn health() -> impl IntoResponse {
    Json(health_payload())
}

async fn providers_list(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    let out = state.engine.list_providers().await?;
    Ok(Json(serde_json::to_value(out)?))
}

async fn services_list(
    State(state): State<AppState>,
    Query(q): Query<ServicesQuery>,
) -> Result<Json<serde_json::Value>> {
    let filters = ServiceFilters::from_query(&q);
    let svcs = filter_services(state.engine.list_services()?, &filters);
    Ok(Json(serde_json::to_value(svcs)?))
}

async fn groups_list(State(state): State<AppState>) -> Result<Json<serde_json::Value>> {
    let groups = state.engine.list_groups()?;
    Ok(Json(serde_json::to_value(groups)?))
}

async fn group_status(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let group = name.trim();
    if group.is_empty() {
        return Err(AppError::BadRequest("group name is required".to_string()));
    }

    let Some(svc_group) = state.engine.list_groups()?.into_iter().find(|g| g.name == group) else {
        return Err(AppError::NotFound);
    };
    let out = collect_service_statuses(&state, svc_group.services).await;
    Ok(Json(serde_json::to_value(out)?))
}

async fn group_start(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let out = state.engine.group_action(&name, Action::Start).await?;
    Ok(Json(serde_json::to_value(out)?))
}

async fn group_stop(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let out = state.engine.group_action(&name, Action::Stop).await?;
    Ok(Json(serde_json::to_value(out)?))
}

async fn group_restart(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let out = state.engine.group_action(&name, Action::Restart).await?;
    Ok(Json(serde_json::to_value(out)?))
}

async fn services_create(State(state): State<AppState>, body: Bytes) -> Result<impl IntoResponse> {
    let spec: ServiceSpec = parse_json_body(&body)?;
    let svc = state.engine.create_service(spec).await?;
    Ok((StatusCode::CREATED, Json(serde_json::to_value(svc)?)))
}

async fn services_statuses(
    State(state): State<AppState>,
    Query(q): Query<ServicesQuery>,
) -> Result<Json<serde_json::Value>> {
    let filters = ServiceFilters::from_query(&q);
    let svcs = filter_services(state.engine.list_services()?, &filters);
    let out = collect_service_statuses(&state, svcs).await;
    Ok(Json(serde_json::to_value(out)?))
}

async fn services_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let svc = state.engine.get_service(&ServiceId(id))?;
    Ok(Json(serde_json::to_value(svc)?))
}

async fn services_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<serde_json::Value>> {
    let spec: ServiceSpec = parse_json_body(&body)?;
    let svc = state.engine.update_service(&ServiceId(id), spec).await?;
    Ok(Json(serde_json::to_value(svc)?))
}

async fn services_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.engine.delete_service(&ServiceId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn service_start(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.engine.start(&ServiceId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn service_stop(State(state): State<AppState>, Path(id): Path<String>) -> Result<StatusCode> {
    state.engine.stop(&ServiceId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn service_restart(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.engine.restart(&ServiceId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn service_repair(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.engine.repair(&ServiceId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn service_register(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.engine.register(&ServiceId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn service_unregister(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    state.engine.unregister(&ServiceId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn service_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>> {
    let st = state.engine.status(&ServiceId(id)).await?;
    Ok(Json(serde_json::to_value(st)?))
}

#[derive(Debug, Deserialize)]
struct LogsQuery {
    since: Option<String>,
    until: Option<String>,
    limit: Option<String>,
}

async fn service_logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<LogsQuery>,
) -> Result<Json<serde_json::Value>> {
    let opts = LogsOptions {
        since: parse_rfc3339_opt(q.since.as_deref())?,
        until: parse_rfc3339_opt(q.until.as_deref())?,
        limit: parse_usize_opt(q.limit.as_deref())?,
    };
    let logs = state.engine.logs(&ServiceId(id), opts).await?;
    Ok(Json(serde_json::to_value(logs)?))
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    limit: Option<String>,
}

async fn audit_list(
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<serde_json::Value>> {
    let limit = parse_usize_opt(q.limit.as_deref())?;
    let evts = state.engine.list_audit_events(limit)?;
    Ok(Json(serde_json::to_value(evts)?))
}

async fn export_store(State(state): State<AppState>) -> Result<Response> {
    let b = state.engine.export()?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        b,
    )
        .into_response())
}

async fn import_store(State(state): State<AppState>, body: Bytes) -> Result<StatusCode> {
    state.engine.import(&body)?;
    Ok(StatusCode::NO_CONTENT)
}

fn parse_json_body<T: serde::de::DeserializeOwned>(b: &[u8]) -> Result<T> {
    if b.is_empty() {
        return Err(AppError::BadRequest("empty JSON body".to_string()));
    }
    serde_json::from_slice::<T>(b).map_err(|e| AppError::BadRequest(e.to_string()))
}

fn parse_rfc3339_opt(s: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    let Some(s) = s else { return Ok(None) };
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let dt = DateTime::parse_from_rfc3339(s)
        .map_err(|_| AppError::BadRequest("invalid timestamp; must be RFC3339".to_string()))?;
    Ok(Some(dt.with_timezone(&Utc)))
}

fn parse_usize_opt(s: Option<&str>) -> Result<Option<usize>> {
    let Some(s) = s else { return Ok(None) };
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let n: usize = s
        .parse()
        .map_err(|_| AppError::BadRequest("invalid limit".to_string()))?;
    Ok(Some(n))
}

#[derive(Debug, Deserialize, Default)]
struct ServicesQuery {
    tag: Option<String>,
    tags: Option<String>,
    group: Option<String>,
    groups: Option<String>,
}

#[derive(Debug, Default)]
struct ServiceFilters {
    tags: Vec<String>,
    groups: Vec<String>,
}

impl ServiceFilters {
    fn from_query(q: &ServicesQuery) -> Self {
        let mut filters = Self::default();
        collect_selector_values(q.tag.as_deref(), &mut filters.tags);
        collect_selector_values(q.tags.as_deref(), &mut filters.tags);
        collect_selector_values(q.group.as_deref(), &mut filters.groups);
        collect_selector_values(q.groups.as_deref(), &mut filters.groups);
        filters
    }

    fn matches(&self, svc: &Service) -> bool {
        self.tags
            .iter()
            .all(|want| svc.spec.tags.iter().any(|tag| tag.trim() == want))
            && self.groups.iter().all(|want| {
                svc.spec.tags.iter().any(|tag| {
                    tag.trim()
                        .strip_prefix("group:")
                        .map(|name| name.trim() == want)
                        .unwrap_or(false)
                })
            })
    }
}

#[derive(Debug, Serialize)]
struct ServiceStatusItem {
    service: Service,
    #[serde(default)]
    status: Option<ServiceStatus>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    error: String,
}

fn collect_selector_values(raw: Option<&str>, out: &mut Vec<String>) {
    let Some(raw) = raw else {
        return;
    };
    for part in raw.split(',') {
        let value = part.trim();
        if value.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == value) {
            out.push(value.to_string());
        }
    }
}

fn filter_services(services: Vec<Service>, filters: &ServiceFilters) -> Vec<Service> {
    if filters.tags.is_empty() && filters.groups.is_empty() {
        return services;
    }
    services
        .into_iter()
        .filter(|svc| filters.matches(svc))
        .collect()
}

async fn collect_service_statuses(
    state: &AppState,
    services: Vec<Service>,
) -> Vec<ServiceStatusItem> {
    let mut out = Vec::with_capacity(services.len());
    for svc in services {
        match state.engine.status(&svc.id).await {
            Ok(status) => out.push(ServiceStatusItem {
                service: svc,
                status: Some(status),
                error: String::new(),
            }),
            Err(e) => out.push(ServiceStatusItem {
                service: svc,
                status: None,
                error: e.to_string(),
            }),
        }
    }
    out
}

// Silence unused imports if we later want to add headers for web assets.
#[allow(dead_code)]
fn _set_json_headers(_h: &mut HeaderMap) {}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };

    use axum::{
        body::Body,
        http::{
            Request, StatusCode,
            header::{AUTHORIZATION, CONTENT_TYPE},
        },
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::{
        model::{
            Capability, DetectResult, LogEntry, LogsOptions, Provider, ProviderId, Service,
            ServiceState, ServiceStatus,
        },
        server::{AppState, Config, Engine, ProviderRegistry, StoreConfig},
        store::JsonStore,
    };

    struct FakeProvider {
        id: ProviderId,
        state: Mutex<BTreeMap<String, ServiceState>>,
    }

    impl FakeProvider {
        fn new(id: &str) -> Self {
            Self {
                id: ProviderId(id.to_string()),
                state: Mutex::new(BTreeMap::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for FakeProvider {
        fn id(&self) -> ProviderId {
            self.id.clone()
        }

        fn display_name(&self) -> String {
            "Fake Provider".to_string()
        }

        fn description(&self) -> String {
            "Test-only provider".to_string()
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

        async fn detect(&self) -> crate::error::Result<DetectResult> {
            Ok(DetectResult {
                detected: true,
                details: String::new(),
            })
        }

        async fn register(&self, _svc: &Service) -> crate::error::Result<()> {
            Ok(())
        }

        async fn unregister(&self, _svc: &Service) -> crate::error::Result<()> {
            Ok(())
        }

        async fn start(&self, svc: &Service) -> crate::error::Result<()> {
            let mut m = self.state.lock().unwrap();
            m.insert(svc.id.0.clone(), ServiceState::Running);
            Ok(())
        }

        async fn stop(&self, svc: &Service) -> crate::error::Result<()> {
            let mut m = self.state.lock().unwrap();
            m.insert(svc.id.0.clone(), ServiceState::Stopped);
            Ok(())
        }

        async fn restart(&self, svc: &Service) -> crate::error::Result<()> {
            let mut m = self.state.lock().unwrap();
            m.insert(svc.id.0.clone(), ServiceState::Running);
            Ok(())
        }

        async fn status(&self, svc: &Service) -> crate::error::Result<ServiceStatus> {
            let m = self.state.lock().unwrap();
            let st = m.get(&svc.id.0).copied().unwrap_or(ServiceState::Stopped);
            Ok(ServiceStatus {
                service_id: svc.id.clone(),
                state: st,
                message: String::new(),
                provider: svc.spec.provider.clone(),
                observed_at: chrono::Utc::now(),
                started_at: None,
                pid: None,
                exit_code: None,
            })
        }

        async fn logs(
            &self,
            _svc: &Service,
            _opts: LogsOptions,
        ) -> crate::error::Result<Vec<LogEntry>> {
            Ok(vec![LogEntry {
                time: chrono::Utc::now(),
                stream: "system".to_string(),
                message: "hello".to_string(),
            }])
        }
    }

    fn test_state() -> (AppState, String) {
        let token = "test-token-123".to_string();

        let uniq = format!(
            "service-manager-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let base = std::env::temp_dir().join(uniq);
        let _ = fs::create_dir_all(&base);
        let store_path: PathBuf = base.join("store.json");

        let cfg = Config {
            listen_addr: "127.0.0.1:0".to_string(),
            data_dir: base.to_string_lossy().to_string(),
            service_registry_dir: base.join("services.d").to_string_lossy().to_string(),
            auth_token: token.clone(),
            log_level: "info".to_string(),
            store: StoreConfig {
                ty: "json".to_string(),
                path: store_path.to_string_lossy().to_string(),
            },
        };

        let store = std::sync::Arc::new(JsonStore::open(store_path).unwrap());
        let registry = ProviderRegistry::new();
        registry
            .add(std::sync::Arc::new(FakeProvider::new("fake")))
            .unwrap();
        let engine = std::sync::Arc::new(Engine::new(store, registry));
        (
            AppState {
                config: cfg,
                engine,
            },
            token,
        )
    }

    #[tokio::test]
    async fn health_is_unprotected() {
        let (state, _tok) = test_state();
        let app = super::router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn auth_is_required() {
        let (state, _tok) = test_state();
        let app = super::router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/services")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(res.headers().get("www-authenticate").is_some());
    }

    #[tokio::test]
    async fn service_lifecycle() {
        let (state, tok) = test_state();
        let app = super::router(state);

        // Create
        let spec = serde_json::json!({
            "name": "demo",
            "description": "d",
            "provider": "fake",
            "command": ["echo", "hi"],
            "working_dir": "",
            "env": {},
            "runtime": {},
            "restart": {"mode": "no", "max_retries": 0},
            "health": [],
            "enabled": true,
            "tags": ["test"]
        });
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/services")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(spec.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let id = v["id"].as_str().unwrap().to_string();

        // Start
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/services/{id}/start"))
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        // Status
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/services/{id}/status"))
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let st: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(st["state"], "running");

        // Delete
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/services/{id}"))
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        // Get (now 404)
        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/services/{id}"))
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn group_lifecycle() {
        let (state, tok) = test_state();
        let app = super::router(state);

        for name in ["demo-a", "demo-b"] {
            let spec = serde_json::json!({
                "name": name,
                "provider": "fake",
                "command": ["echo", name],
                "restart": {"mode": "no", "max_retries": 0},
                "tags": ["group:local-stack"]
            });
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/services")
                        .header(AUTHORIZATION, format!("Bearer {tok}"))
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(spec.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::CREATED);
        }

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/groups")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let groups: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(groups.as_array().unwrap().len(), 1);
        assert_eq!(groups[0]["name"], "local-stack");
        assert_eq!(groups[0]["service_ids"].as_array().unwrap().len(), 2);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/groups/local-stack/start")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["group"], "local-stack");
        assert_eq!(result["action"], "start");
        assert_eq!(result["total"], 2);
        assert_eq!(result["succeeded"].as_array().unwrap().len(), 2);
        assert_eq!(result["failed"].as_array().unwrap().len(), 0);

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/groups/missing/start")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn service_filters_and_bulk_statuses() {
        let (state, tok) = test_state();
        let app = super::router(state);

        let mut ids = Vec::new();
        for (name, tags) in [
            ("phone-ai", vec!["smallphoneai", "group:phone-control"]),
            ("phone-app", vec!["smallphone", "group:phone-control"]),
            ("other", vec!["other", "group:other"]),
        ] {
            let spec = serde_json::json!({
                "name": name,
                "provider": "fake",
                "command": ["echo", name],
                "restart": {"mode": "no", "max_retries": 0},
                "tags": tags
            });
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/services")
                        .header(AUTHORIZATION, format!("Bearer {tok}"))
                        .header(CONTENT_TYPE, "application/json")
                        .body(Body::from(spec.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::CREATED);
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            let svc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            ids.push(svc["id"].as_str().unwrap().to_string());
        }

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/services/{}/start", ids[0]))
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/services?tag=smallphoneai")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let services: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(services.as_array().unwrap().len(), 1);
        assert_eq!(services[0]["spec"]["name"], "phone-ai");

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/services?group=phone-control")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let services: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(services.as_array().unwrap().len(), 2);

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/services/statuses?group=phone-control")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let statuses: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(statuses.as_array().unwrap().len(), 2);
        assert!(
            statuses
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["status"]["state"] == "running")
        );

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/groups/phone-control/status")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let statuses: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(statuses.as_array().unwrap().len(), 2);

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/groups/missing/status")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn validation_and_provider_not_found() {
        let (state, tok) = test_state();
        let app = super::router(state);

        // Invalid name
        let bad = serde_json::json!({
            "name": "!!!",
            "provider": "fake"
        });
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/services")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(bad.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        // Provider not found
        let bad = serde_json::json!({
            "name": "ok-name",
            "provider": "missing"
        });
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/services")
                    .header(AUTHORIZATION, format!("Bearer {tok}"))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(bad.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "provider_not_found");
    }
}
