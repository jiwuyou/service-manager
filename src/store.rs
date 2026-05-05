use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    error::{AppError, Result},
    model::{AuditEvent, Service, ServiceId},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonDisk {
    version: i32,
    #[serde(default)]
    services: BTreeMap<String, Service>,
    #[serde(default)]
    audit: Vec<AuditEvent>,
}

impl Default for JsonDisk {
    fn default() -> Self {
        Self {
            version: 1,
            services: BTreeMap::new(),
            audit: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct JsonStore {
    path: PathBuf,
    disk: Mutex<JsonDisk>,
}

impl JsonStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(AppError::BadRequest("store path is empty".to_string()));
        }
        let store = Self {
            path,
            disk: Mutex::new(JsonDisk::default()),
        };
        store.load()?;
        Ok(store)
    }

    pub fn list_services(&self) -> Result<Vec<Service>> {
        let disk = self.disk.lock().expect("store mutex poisoned");
        let mut out: Vec<Service> = disk
            .services
            .values()
            .filter(|s| s.deleted_at.is_none())
            .cloned()
            .collect();
        out.sort_by_key(|s| s.created_at);
        Ok(out)
    }

    pub fn get_service(&self, id: &ServiceId) -> Result<Service> {
        let disk = self.disk.lock().expect("store mutex poisoned");
        let Some(svc) = disk.services.get(&id.0) else {
            return Err(AppError::NotFound);
        };
        if svc.deleted_at.is_some() {
            return Err(AppError::NotFound);
        }
        Ok(svc.clone())
    }

    pub fn create_service(&self, svc: Service) -> Result<()> {
        if svc.id.0.trim().is_empty() {
            return Err(AppError::BadRequest("service id is empty".to_string()));
        }
        let key = svc.id.0.clone();
        self.stage_and_commit(move |staged| {
            if staged.services.contains_key(&key) {
                return Err(AppError::BadRequest(format!(
                    "service already exists: {:?}",
                    key
                )));
            }
            staged.services.insert(key, svc);
            Ok(())
        })
    }

    pub fn update_service(&self, mut svc: Service) -> Result<()> {
        if svc.id.0.trim().is_empty() {
            return Err(AppError::BadRequest("service id is empty".to_string()));
        }
        let key = svc.id.0.clone();
        self.stage_and_commit(move |staged| {
            let Some(existing) = staged.services.get(&key).cloned() else {
                return Err(AppError::NotFound);
            };
            if existing.deleted_at.is_some() {
                return Err(AppError::NotFound);
            }
            svc.created_at = existing.created_at;
            staged.services.insert(key, svc);
            Ok(())
        })
    }

    pub fn delete_service(&self, id: &ServiceId) -> Result<()> {
        if id.0.trim().is_empty() {
            return Err(AppError::BadRequest("service id is empty".to_string()));
        }
        let key = id.0.clone();
        self.stage_and_commit(move |staged| {
            let Some(mut existing) = staged.services.get(&key).cloned() else {
                return Err(AppError::NotFound);
            };
            if existing.deleted_at.is_some() {
                return Err(AppError::NotFound);
            }
            let now = Utc::now();
            existing.deleted_at = Some(now);
            existing.updated_at = now;
            staged.services.insert(key, existing);
            Ok(())
        })
    }

    pub fn list_audit_events(&self, limit: Option<usize>) -> Result<Vec<AuditEvent>> {
        let disk = self.disk.lock().expect("store mutex poisoned");
        match limit {
            None | Some(0) => Ok(disk.audit.clone()),
            Some(n) if n >= disk.audit.len() => Ok(disk.audit.clone()),
            Some(n) => Ok(disk.audit[disk.audit.len() - n..].to_vec()),
        }
    }

    pub fn append_audit_event(&self, evt: AuditEvent) -> Result<()> {
        self.stage_and_commit(move |staged| {
            staged.audit.push(evt);
            Ok(())
        })
    }

    pub fn export(&self) -> Result<Vec<u8>> {
        let disk = self.disk.lock().expect("store mutex poisoned");
        Ok(serde_json::to_vec_pretty(&*disk)?)
    }

    pub fn import(&self, data: &[u8]) -> Result<()> {
        let d: JsonDisk = serde_json::from_slice(data)?;
        let mut disk = self.disk.lock().expect("store mutex poisoned");
        self.flush_locked(&d)?;
        *disk = d;
        Ok(())
    }

    fn load(&self) -> Result<()> {
        let b = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(AppError::Io(e)),
        };
        if b.iter().all(|c| c.is_ascii_whitespace()) {
            return Ok(());
        }
        let d: JsonDisk = serde_json::from_slice(&b)?;
        let mut disk = self.disk.lock().expect("store mutex poisoned");
        *disk = d;
        Ok(())
    }

    fn flush_locked(&self, disk: &JsonDisk) -> Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| AppError::BadRequest("store path has no parent dir".to_string()))?;
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }

        let b = serde_json::to_vec_pretty(disk)?;
        let tmp = tmp_path(&self.path);

        // Note: permissions are best-effort across platforms.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut opts = fs::OpenOptions::new();
            opts.create(true).write(true).truncate(true);
            let mut f = opts.open(&tmp)?;
            use std::io::Write;
            f.write_all(&b)?;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        #[cfg(not(unix))]
        {
            fs::write(&tmp, &b)?;
        }

        if let Err(e) = fs::rename(&tmp, &self.path) {
            // Windows doesn't allow renaming over existing files.
            let _ = fs::remove_file(&self.path);
            fs::rename(&tmp, &self.path).map_err(|_| AppError::Io(e))?;
        }
        Ok(())
    }

    // Apply a mutation to a staged copy of the current state, flush that staged state to disk,
    // then commit it in-memory. This prevents in-memory state from diverging when flush fails.
    fn stage_and_commit<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(&mut JsonDisk) -> Result<()>,
    {
        let mut disk = self.disk.lock().expect("store mutex poisoned");
        let mut staged = disk.clone();
        f(&mut staged)?;
        self.flush_locked(&staged)?;
        *disk = staged;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_os_string();
    p.push(".tmp");
    PathBuf::from(p)
}
