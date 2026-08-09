//! File-watching config reloader for MCP server hot-reload.
//!
//! Watches the five config files that `ConfigLoader::discover()` monitors, and
//! applies a diff of MCP server changes whenever any of them changes on disk.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use crate::config::{ConfigLoader, ScopedMcpServerConfig};

/// The result of reloading MCP server configs from disk.
#[derive(Debug, Clone)]
pub struct McpConfigDiff {
    /// Servers that were added (present in the new config, absent from the old).
    pub added: Vec<(String, ScopedMcpServerConfig)>,
    /// Servers that were removed (present in the old config, absent from the new).
    pub removed: Vec<String>,
    /// Servers that are in both configs but the config changed.
    pub changed: Vec<(String, ScopedMcpServerConfig)>,
}

/// Computes a diff between two MCP server collections.
#[must_use]
pub fn diff_mcp_config(
    old_servers: &BTreeMap<String, ScopedMcpServerConfig>,
    new_servers: &BTreeMap<String, ScopedMcpServerConfig>,
) -> McpConfigDiff {
    let mut removed = Vec::new();
    let mut added = Vec::new();
    let mut changed = Vec::new();

    for (name, old_config) in old_servers {
        if let Some(new_config) = new_servers.get(name) {
            if old_config != new_config {
                changed.push((name.clone(), new_config.clone()));
            }
        } else {
            removed.push(name.clone());
        }
    }

    for (name, new_config) in new_servers {
        if !old_servers.contains_key(name) {
            added.push((name.clone(), new_config.clone()));
        }
    }

    McpConfigDiff {
        added,
        removed,
        changed,
    }
}

/// The async result type returned by [`McpReloadableManager::reload_servers`].
type ReloadResult = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>>
            + Send,
    >,
>;

/// Capability trait for MCP server managers that can apply config diffs at runtime.
/// Both `McpServerManager` (stdio-only) and `McpServerSessionManager` (stdio + HTTP + SSE)
/// implement this via their respective adapter types.
pub trait McpReloadableManager: Send + Sync {
    /// Apply a config diff: add new servers, remove stale ones, update changed ones.
    fn reload_servers(
        &self,
        added: Vec<(String, ScopedMcpServerConfig)>,
        removed: Vec<String>,
        changed: Vec<(String, ScopedMcpServerConfig)>,
    ) -> ReloadResult;
}

/// Adapter that makes `McpServerManager` (stdio-only) reloadable.
pub struct ReloadableMcpServerManager(pub Arc<RwLock<crate::mcp_stdio::McpServerManager>>);

impl McpReloadableManager for ReloadableMcpServerManager {
    fn reload_servers(
        &self,
        added: Vec<(String, ScopedMcpServerConfig)>,
        removed: Vec<String>,
        changed: Vec<(String, ScopedMcpServerConfig)>,
    ) -> ReloadResult {
        let manager = Arc::clone(&self.0);
        Box::pin(async move {
            let mut guard = manager.write().await;
            crate::mcp_stdio::McpServerManager::reload_servers(&mut guard, added, removed, changed)
                .await?;
            Ok(())
        })
    }
}

/// Adapter that makes `McpServerSessionManager` (stdio + HTTP + SSE) reloadable.
pub struct ReloadableMcpServerSessionManager(pub Arc<RwLock<crate::mcp::McpServerSessionManager>>);

impl McpReloadableManager for ReloadableMcpServerSessionManager {
    fn reload_servers(
        &self,
        added: Vec<(String, ScopedMcpServerConfig)>,
        removed: Vec<String>,
        changed: Vec<(String, ScopedMcpServerConfig)>,
    ) -> ReloadResult {
        use crate::config::McpServerConfig;
        let manager = Arc::clone(&self.0);
        Box::pin(async move {
            let mut guard = manager.write().await;
            for name in removed {
                let _ = guard.remove_session(&name).await;
            }
            for (name, config) in added {
                match &config.config {
                    McpServerConfig::Stdio(s) => {
                        let _ = guard.add_stdio_server(&name, &s.command);
                    }
                    McpServerConfig::Http(s) | McpServerConfig::Sse(s) => {
                        let _ = guard
                            .add_http_session(&name, &s.url, s.headers.clone())
                            .await;
                    }
                    _ => {}
                }
            }
            for (name, config) in changed {
                let _ = guard.remove_session(&name).await;
                match &config.config {
                    McpServerConfig::Stdio(s) => {
                        let _ = guard.add_stdio_server(&name, &s.command);
                    }
                    McpServerConfig::Http(s) | McpServerConfig::Sse(s) => {
                        let _ = guard
                            .add_http_session(&name, &s.url, s.headers.clone())
                            .await;
                    }
                    _ => {}
                }
            }
            Ok(())
        })
    }
}

/// A config file watcher that watches config files and applies MCP server diffs
/// to any manager that implements `McpReloadableManager`.
pub struct ConfigWatcher<M: McpReloadableManager> {
    loader: ConfigLoader,
    manager: Arc<RwLock<M>>,
    last_config: std::sync::RwLock<Option<BTreeMap<String, ScopedMcpServerConfig>>>,
}

impl<M: McpReloadableManager> ConfigWatcher<M> {
    /// Create a new watcher. Every time a config file changes, the diff is applied
    /// to the shared manager.
    pub fn new(loader: ConfigLoader, manager: Arc<RwLock<M>>) -> Self {
        Self {
            loader,
            manager,
            last_config: std::sync::RwLock::new(None),
        }
    }

    /// Start watching config files and applying diffs.
    /// Returns when the sender is dropped or a fatal watcher error occurs.
    pub async fn start(&self) {
        let paths: Vec<PathBuf> = self.loader.discover().into_iter().map(|e| e.path).collect();
        if paths.is_empty() {
            return;
        }

        let (notify_tx, mut notify_rx) = mpsc::channel(64);
        let loader_for_thread = self.loader.clone();

        let mut watcher = match RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = notify_tx.blocking_send(event);
                }
            },
            Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("mcp: failed to create file watcher: {e}");
                return;
            }
        };

        for path in &paths {
            let watch_path = if path.exists() {
                path.clone()
            } else if let Some(parent) = path.parent() {
                parent.to_path_buf()
            } else {
                path.clone()
            };
            if let Err(e) = watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
                tracing::warn!("mcp: failed to watch {watch_path:?}: {e}");
            }
        }

        tracing::debug!("mcp: watching {paths:?} for config changes");

        while let Some(event) = notify_rx.recv().await {
            self.handle_event(&loader_for_thread, event).await;
        }
    }

    async fn handle_event(&self, loader: &ConfigLoader, event: Event) {
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }

        let config_path_set: BTreeSet<PathBuf> =
            loader.discover().into_iter().map(|e| e.path).collect();

        let is_relevant = event.paths.iter().any(|p| config_path_set.contains(p));
        if !is_relevant {
            return;
        }

        let new_config = match loader.load() {
            Ok(cfg) => cfg.mcp().servers().clone(),
            Err(e) => {
                tracing::warn!("mcp: failed to reload config after file change: {e}");
                return;
            }
        };

        let diff = {
            let guard = self
                .last_config
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(old) = &*guard {
                diff_mcp_config(old, &new_config)
            } else {
                drop(guard);
                *self
                    .last_config
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(new_config);
                return;
            }
        };

        *self
            .last_config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(new_config);

        if diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty() {
            return;
        }

        tracing::debug!(
            "mcp: applying hot-reload — {} added, {} removed, {} changed",
            diff.added.len(),
            diff.removed.len(),
            diff.changed.len(),
        );

        let guard = self.manager.read().await;
        match guard
            .reload_servers(diff.added, diff.removed, diff.changed)
            .await
        {
            Ok(()) => {
                tracing::debug!("mcp: hot-reload applied successfully");
            }
            Err(e) => {
                tracing::error!("mcp: hot-reload failed: {e}");
            }
        }
    }
}
