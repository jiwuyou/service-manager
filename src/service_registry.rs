use std::{fs, path::Path};

use serde::Deserialize;
use tracing::info;

use crate::{
    error::Result,
    model::{ServiceId, ServiceSpec},
    server::Engine,
};

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RegistryDocument {
    Batch {
        #[serde(default, rename = "schemaVersion")]
        _schema_version: Option<u32>,
        services: Vec<RegistryItem>,
    },
    Item(RegistryItem),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RegistryItem {
    Wrapped {
        #[serde(default)]
        id: Option<String>,
        service: ServiceSpec,
    },
    Spec(ServiceSpec),
}

impl RegistryItem {
    fn into_parts(self) -> (String, ServiceSpec) {
        match self {
            RegistryItem::Wrapped { id, service } => {
                let service_id = id
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| service.name.clone());
                (service_id, service)
            }
            RegistryItem::Spec(service) => (service.name.clone(), service),
        }
    }
}

pub fn load_from_dir(engine: &Engine, dir: &Path) -> Result<usize> {
    if dir.as_os_str().is_empty() || !dir.is_dir() {
        return Ok(0);
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            entries.push(path);
        }
    }
    entries.sort();

    let mut loaded = 0usize;
    for path in entries {
        let bytes = fs::read(&path)?;
        let doc: RegistryDocument = serde_json::from_slice(&bytes)?;
        let items = match doc {
            RegistryDocument::Batch { services, .. } => services,
            RegistryDocument::Item(item) => vec![item],
        };
        for item in items {
            let (id, spec) = item.into_parts();
            engine.upsert_registered_service(ServiceId(id), spec)?;
            loaded += 1;
        }
        info!("loaded service registry file {}", path.display());
    }

    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_accepts_wrapped_service_with_stable_id() {
        let raw = br#"{
          "schemaVersion": 1,
          "id": "hermes-webui",
          "service": {
            "name": "hermes-webui",
            "provider": "process",
            "command": ["python", "bootstrap.py"],
            "tags": ["group:local-stack"]
          }
        }"#;

        let doc: RegistryDocument = serde_json::from_slice(raw).unwrap();
        let RegistryDocument::Item(item) = doc else {
            panic!("expected item document");
        };
        let (id, spec) = item.into_parts();
        assert_eq!(id, "hermes-webui");
        assert_eq!(spec.name, "hermes-webui");
        assert_eq!(spec.provider.0, "process");
    }

    #[test]
    fn registry_accepts_batch_services() {
        let raw = br#"{
          "schemaVersion": 1,
          "services": [
            {
              "id": "a",
              "service": {
                "name": "a",
                "provider": "process",
                "command": ["sleep", "1"]
              }
            },
            {
              "name": "b",
              "provider": "process",
              "command": ["sleep", "2"]
            }
          ]
        }"#;

        let doc: RegistryDocument = serde_json::from_slice(raw).unwrap();
        let RegistryDocument::Batch { services, .. } = doc else {
            panic!("expected batch document");
        };
        assert_eq!(services.len(), 2);
        let ids = services
            .into_iter()
            .map(|item| item.into_parts().0)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["a", "b"]);
    }
}
