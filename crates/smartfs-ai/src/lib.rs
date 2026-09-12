//! smartfs-ai — Embedding Worker, Tree-sitter AST parsing, and ONNX inference.
//!
//! Exclusively owns:
//! - The async embedding pipeline: pending file versions -> embeddings in DB.
//! - Tree-sitter AST extraction (in `spawn_blocking`).
//! - CPU/ONNX embedding inference (Qwen3-Embedding-0.6B default, ADR-49).
//! - Supervisor worker loop with anti-starvation valve (ADR-42) and non-preemptible force mode (FIX-05, ADR-47).
//! - Order-independent recomputation of `is_current` (FIX-02, ADR-45).
//! - Populating `search_text` column on file versions (ADR-54).

pub mod activity;
pub mod ast;
pub mod engine;
pub mod worker;

pub use activity::ActivityMonitor;
pub use ast::{extract_ast_nodes, parse_ast_nodes_blocking};
pub use engine::{CpuEmbeddingEngine, EmbeddingEngine};
pub use worker::{
    embed_version, finish_embed, run_worker_supervisor, should_embed_despite_activity,
};
