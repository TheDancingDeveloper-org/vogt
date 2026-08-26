//! Model discovery and cache boundary.
//!
//! This crate intentionally does not download weights. A future backend can
//! implement `ModelCache` against a local directory or a content-addressed
//! store without making the HTTP layer aware of model management.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub object: &'static str,
    pub created: u64,
    pub owned_by: String,
}

impl ModelInfo {
    pub fn new(id: impl Into<String>, owned_by: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            object: "model",
            created: 0,
            owned_by: owned_by.into(),
        }
    }
}

/// Catalog/cache seam used by `/v1/models` and future model loading.
pub trait ModelCache: Send + Sync {
    fn list(&self) -> Vec<ModelInfo>;
    fn contains(&self, model: &str) -> bool {
        self.list().iter().any(|entry| entry.id == model)
    }
    fn root(&self) -> &Path;
}

#[derive(Debug, Clone)]
pub struct ConfiguredModelCache {
    root: PathBuf,
    models: Vec<ModelInfo>,
}

impl ConfiguredModelCache {
    pub fn new(root: impl Into<PathBuf>, models: Vec<ModelInfo>) -> Self {
        Self {
            root: root.into(),
            models,
        }
    }
}

impl ModelCache for ConfiguredModelCache {
    fn list(&self) -> Vec<ModelInfo> {
        self.models.clone()
    }

    fn root(&self) -> &Path {
        &self.root
    }
}
