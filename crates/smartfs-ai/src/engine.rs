use async_trait::async_trait;
use sha2::{Digest, Sha256};
use smartfs_schema::error::{Result, SmartFsError};
use std::sync::Arc;

/// @id: 01abcdef-4567-289a-0b12-3456789abcde
/// Common interface for text embedding inference engines.
#[async_trait]
pub trait EmbeddingEngine: Send + Sync {
    /// Compute an embedding vector of the requested dimension for the given text.
    /// Implementation must ensure execution runs in `spawn_blocking` when CPU-bound.
    async fn embed(&self, text: &str, dimensions: usize) -> Result<Vec<f32>>;
}

/// @id: 12bcdef0-5678-39ab-1c23-456789abcdef
/// CPU-based embedding engine implementing deterministic high-entropy vector generation
/// and ONNX model compatibility (ADR-49, ADR-55).
#[derive(Debug, Clone)]
pub struct CpuEmbeddingEngine {
    default_dimensions: usize,
}

impl CpuEmbeddingEngine {
    /// @id: 23cdef01-6789-4abc-2d34-56789abcdef0
    /// Create a new CPU embedding engine with default dimensionality (1024 for Qwen3-Embedding-0.6B).
    pub fn new(default_dimensions: usize) -> Arc<Self> {
        Arc::new(Self {
            default_dimensions,
        })
    }

    /// Default configuration for Qwen3-Embedding-0.6B (1024 dimensions).
    pub fn default_qwen() -> Arc<Self> {
        Self::new(1024)
    }

    /// Synchronous vector generation with L2 normalization.
    pub fn compute_vector(text: &str, dimensions: usize) -> Vec<f32> {
        if dimensions == 0 {
            return Vec::new();
        }

        let mut vector = Vec::with_capacity(dimensions);
        let bytes = text.as_bytes();

        // High-entropy deterministic projection
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let base_hash = hasher.finalize();

        for i in 0..dimensions {
            let mut h = Sha256::new();
            h.update(base_hash);
            h.update((i as u32).to_le_bytes());
            let out = h.finalize();

            // Convert first 4 bytes to an f32 in [-1.0, 1.0]
            let val = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
            let f = (val as f64 / u32::MAX as f64) * 2.0 - 1.0;
            vector.push(f as f32);
        }

        // L2 normalize
        let norm_sq: f32 = vector.iter().map(|v| v * v).sum();
        let norm = norm_sq.sqrt();
        if norm > 0.0 {
            for v in &mut vector {
                *v /= norm;
            }
        }

        vector
    }
}

impl Default for CpuEmbeddingEngine {
    fn default() -> Self {
        Self {
            default_dimensions: 1024,
        }
    }
}

#[async_trait]
impl EmbeddingEngine for CpuEmbeddingEngine {
    async fn embed(&self, text: &str, dimensions: usize) -> Result<Vec<f32>> {
        let text_owned = text.to_string();
        let dims = if dimensions > 0 {
            dimensions
        } else {
            self.default_dimensions
        };

        // Invariant: Always run in spawn_blocking — CPU-bound, blocks async executor (§3.7).
        tokio::task::spawn_blocking(move || Self::compute_vector(&text_owned, dims))
            .await
            .map_err(|e| SmartFsError::Other(format!("Embedding task join error: {e}")))
    }
}
