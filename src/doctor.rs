use std::path::PathBuf;

use crate::{
    error::Result,
    providers,
    server::{ProviderRegistry, config_paths},
    store,
};

/// Run local diagnostics.
///
/// Integration note (Lead): wire `Command::Doctor` to call this (instead of the stub in main.rs).
pub async fn run(cfg_path: Option<PathBuf>) -> Result<()> {
    let (cfg, data_dir, store_path) = config_paths(cfg_path)?;
    println!("config: {}", cfg.display());
    println!("data_dir: {}", data_dir);
    println!("store: {}", store_path);

    let _ = store::JsonStore::open(store_path)?;
    println!("store: ok");

    let registry = ProviderRegistry::new();
    providers::register_defaults(&registry, PathBuf::from(&data_dir))?;
    println!("providers: {}", registry.list().len());

    for p in registry.list() {
        match p.detect().await {
            Ok(r) => {
                println!(
                    "provider {}: detected={} details={}",
                    p.id().0,
                    r.detected,
                    r.details
                );
            }
            Err(e) => {
                println!("provider {}: detect_error={e}", p.id().0);
            }
        }
    }

    Ok(())
}
