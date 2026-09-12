//! Tool dispatch and business logic execution layer for SmartFS MCP server.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use smartfs_db::{FileVersionRecord, FulltextHit, InodeRecord, PgPool};
use smartfs_schema::error::{Result, SmartFsError};
use smartfs_store::BlobStore;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::tokens::{DestructiveAction, TokenManager};
use crate::types::{
    AstDiff, CentroidSummary, ConfirmationRequired, DestructiveResult, FileContentResult,
    PluginSchema, PluginSummary,
};

/// @id: deb83d63-df4c-4af5-b476-2e4bd0bd2451
/// Primary tool execution engine for SmartFS MCP.
#[derive(Clone)]
pub struct ToolHandler {
    pool: PgPool,
    store: Arc<dyn BlobStore>,
    tokens: Arc<Mutex<TokenManager>>,
    plugins_dir: Option<PathBuf>,
}

/// @id: f57b71d8-5653-4346-bbc1-e9cb7e6d98da
impl ToolHandler {
    /// @id: 51036957-cdeb-4de3-b5e5-08f728482e69
    /// Creates a new `ToolHandler` with database pool, blob store, and optional plugins directory.
    pub fn new(pool: PgPool, store: Arc<dyn BlobStore>, plugins_dir: Option<PathBuf>) -> Self {
        Self {
            pool,
            store,
            tokens: Arc::new(Mutex::new(TokenManager::with_default_ttl())),
            plugins_dir,
        }
    }

    /// @id: 64d7ab67-d678-4a77-aebb-2893ea56c943
    /// Creates a new `ToolHandler` with a custom token TTL (for testing).
    pub fn with_token_ttl(
        pool: PgPool,
        store: Arc<dyn BlobStore>,
        plugins_dir: Option<PathBuf>,
        ttl: Duration,
    ) -> Self {
        Self {
            pool,
            store,
            tokens: Arc::new(Mutex::new(TokenManager::new(ttl))),
            plugins_dir,
        }
    }

    /// @id: 793a37cf-199b-48fe-b3fd-0da23c5536ec
    /// Access the underlying database connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// @id: 11d7d0c2-7707-4f46-9035-afe310eccf36
    /// Access the underlying blob store.
    pub fn store(&self) -> &dyn BlobStore {
        &*self.store
    }

    /// @id: 7124053b-f08b-413a-ac34-97dec2104237
    /// Dispatches a tool call by name and arguments JSON, returning the JSON result.
    pub async fn dispatch_tool_call(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        match name {
            "search_semantic" => {
                let query_vector: Option<Vec<f32>> = arguments
                    .get("query_vector")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let model_id: Option<Uuid> = arguments
                    .get("model_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok());
                let limit: Option<usize> = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                let type_filter: Option<String> = arguments
                    .get("type_filter")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let hits = self
                    .search_semantic(query_vector, model_id, limit, type_filter)
                    .await?;
                Ok(serde_json::to_value(hits).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "search_functions" => {
                let query_vector: Option<Vec<f32>> = arguments
                    .get("query_vector")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let language: Option<String> = arguments
                    .get("language")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let kind: Option<String> = arguments
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let limit: Option<usize> = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                let hits = self
                    .search_functions(query_vector, language, kind, limit)
                    .await?;
                Ok(serde_json::to_value(hits).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "get_file_history" => {
                let path = arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'path'".to_string()))?;
                let history = self.get_file_history(path.to_string()).await?;
                Ok(serde_json::to_value(history).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "query_by_metadata" => {
                let filter = arguments
                    .get("filter")
                    .cloned()
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'filter'".to_string()))?;
                let versions = self.query_by_metadata(filter).await?;
                Ok(serde_json::to_value(versions).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "find_by_hash" => {
                let content_hash = arguments
                    .get("content_hash")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'content_hash'".to_string()))?;
                let version = self.find_by_hash(content_hash.to_string()).await?;
                Ok(serde_json::to_value(version).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "get_file_content" => {
                let path = arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'path'".to_string()))?;
                let version = arguments
                    .get("version")
                    .and_then(|v| v.as_i64())
                    .map(|n| n as i32);
                let content = self.get_file_content(path.to_string(), version).await?;
                Ok(serde_json::to_value(content).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "find_broken_files" => {
                let broken = self.find_broken_files().await?;
                Ok(serde_json::to_value(broken).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "diff_functions" => {
                let path = arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'path'".to_string()))?;
                let v1 = arguments
                    .get("v1")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'v1'".to_string()))? as i32;
                let v2 = arguments
                    .get("v2")
                    .and_then(|v| v.as_i64())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'v2'".to_string()))? as i32;

                let diff = self.diff_functions(path.to_string(), v1, v2).await?;
                Ok(serde_json::to_value(diff).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "search_by_concept" => {
                let query_vector: Option<Vec<f32>> = arguments
                    .get("query_vector")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let plugin_type: Option<String> = arguments
                    .get("plugin_type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let model_id: Option<Uuid> = arguments
                    .get("model_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok());
                let limit: Option<usize> = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                let res = self
                    .search_by_concept(query_vector, plugin_type, model_id, limit)
                    .await?;
                Ok(res)
            }
            "search_fulltext" => {
                let query = arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'query'".to_string()))?;
                let plugin_type: Option<String> = arguments
                    .get("plugin_type")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let limit: Option<i64> = arguments.get("limit").and_then(|v| v.as_i64());

                let hits = self.search_fulltext(query.to_string(), plugin_type, limit).await?;
                Ok(serde_json::to_value(hits).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "get_actor_activity" => {
                let actor_id = arguments
                    .get("actor_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'actor_id'".to_string()))?;
                let since: Option<DateTime<Utc>> = arguments
                    .get("since")
                    .and_then(|v| v.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));
                let limit: Option<usize> = arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                let activity = self.get_actor_activity(actor_id.to_string(), since, limit).await?;
                Ok(serde_json::to_value(activity).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "describe_plugin_type" => {
                let plugin_type = arguments
                    .get("plugin_type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'plugin_type'".to_string()))?;
                let schema = self.describe_plugin_type(plugin_type.to_string()).await?;
                Ok(serde_json::to_value(schema).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "list_plugin_types" => {
                let list = self.list_plugin_types().await?;
                Ok(serde_json::to_value(list).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            "delete_file" => {
                let path = arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'path'".to_string()))?;
                let token: Option<String> = arguments
                    .get("confirmation_token")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let res = self.delete_file(path.to_string(), token).await?;
                Ok(res)
            }
            "overwrite_file" => {
                let path = arguments
                    .get("path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'path'".to_string()))?;
                let content = arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| SmartFsError::SyntaxError("Missing required parameter 'content'".to_string()))?;
                let token: Option<String> = arguments
                    .get("confirmation_token")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let res = self.overwrite_file(path.to_string(), content.to_string(), token).await?;
                Ok(res)
            }
            other => Err(SmartFsError::NotFound(format!("Method '{other}' not found"))),
        }
    }

    /// @id: a474a6a6-cdf4-4628-9441-65f26421dc1d
    /// Resolves a path like "/foo/bar.rs" to its `InodeRecord`.
    pub async fn resolve_path(&self, path: &str) -> Result<InodeRecord> {
        let root = smartfs_db::inode_lookup_by_ino(&self.pool, 1)
            .await?
            .ok_or_else(|| SmartFsError::NotFound("Root inode not found".to_string()))?;

        let clean_path = path.trim().trim_matches('/');
        if clean_path.is_empty() {
            return Ok(root);
        }

        let mut current = root;
        for segment in clean_path.split('/') {
            if segment.is_empty() {
                continue;
            }
            let child = smartfs_db::inode_lookup(&self.pool, Some(current.id), segment)
                .await?
                .ok_or_else(|| {
                    SmartFsError::NotFound(format!("Path '{path}' not found (missing component '{segment}')"))
                })?;
            current = child;
        }

        Ok(current)
    }

    /// @id: f828fa60-9ec5-4f0f-909c-db15755ed698
    /// Recursively lists all non-directory file inodes across the workspace.
    pub async fn collect_all_file_inodes(&self) -> Result<Vec<InodeRecord>> {
        let mut files = Vec::new();
        let root = match smartfs_db::inode_lookup_by_ino(&self.pool, 1).await? {
            Some(r) => r,
            None => return Ok(files),
        };

        let mut queue = vec![root];
        while let Some(inode) = queue.pop() {
            if inode.is_dir {
                let children = smartfs_db::inode_list_children(&self.pool, Some(inode.id)).await?;
                for child in children {
                    queue.push(child);
                }
            } else {
                files.push(inode);
            }
        }

        Ok(files)
    }

    // --- Tool Implementation Methods ---

    /// @id: f9f66a59-e654-462b-9018-763263b34928
    /// `search_semantic`: cosine search across general embedding tables.
    pub async fn search_semantic(
        &self,
        query_vector: Option<Vec<f32>>,
        model_id: Option<Uuid>,
        limit: Option<usize>,
        type_filter: Option<String>,
    ) -> Result<Vec<smartfs_semantic::ConceptSearchHit>> {
        let q_vec = match query_vector {
            Some(v) => v,
            None => return Ok(Vec::new()),
        };

        let m_id = match model_id {
            Some(m) => m,
            None => smartfs_db::get_default_model_id(&self.pool).await?,
        };

        let lim = limit.unwrap_or(10);
        let p_type = type_filter.unwrap_or_else(|| "generic".to_string());

        smartfs_semantic::search_by_concept(&self.pool, &q_vec, &p_type, m_id, lim).await
    }

    /// @id: 62de7a4f-173e-4ce8-9e4f-8944d5f307ae
    /// `search_functions`: cosine search on function-level AST embeddings.
    pub async fn search_functions(
        &self,
        query_vector: Option<Vec<f32>>,
        language: Option<String>,
        _kind: Option<String>,
        limit: Option<usize>,
    ) -> Result<Vec<smartfs_semantic::ConceptSearchHit>> {
        let q_vec = match query_vector {
            Some(v) => v,
            None => return Ok(Vec::new()),
        };

        // For AST 1536-dim search, look up model or fall back to default
        let m_id = match smartfs_db::get_model_id_by_name(&self.pool, "text-embedding-3-large").await? {
            Some(id) => id,
            None => smartfs_db::get_default_model_id(&self.pool).await?,
        };

        let lang = language.unwrap_or_else(|| "rust".to_string());
        let lim = limit.unwrap_or(10);

        smartfs_semantic::search_by_concept(&self.pool, &q_vec, &lang, m_id, lim).await
    }

    /// @id: cad2807b-c50b-4a04-b515-b5d70eb18086
    /// `get_file_history`: resolves path to inode, returns versions with `parent_version_id` chain.
    pub async fn get_file_history(&self, path: String) -> Result<Vec<FileVersionRecord>> {
        let inode = self.resolve_path(&path).await?;
        smartfs_db::version_history(&self.pool, inode.id).await
    }

    /// @id: 4d9ab12f-382d-4856-af9b-e850b92de5d9
    /// `query_by_metadata`: structured query on `special_data` JSONB.
    pub async fn query_by_metadata(&self, filter: serde_json::Value) -> Result<Vec<FileVersionRecord>> {
        let file_inodes = self.collect_all_file_inodes().await?;
        let mut matches = Vec::new();

        for inode in file_inodes {
            let versions = smartfs_db::version_history(&self.pool, inode.id).await?;
            for v in versions {
                if json_contains(&v.special_data, &filter) {
                    matches.push(v);
                }
            }
        }

        Ok(matches)
    }

    /// @id: 1c44b494-f647-4385-96cf-0f288b88bc47
    /// `find_by_hash`: find an existing file version record by content hash.
    pub async fn find_by_hash(&self, content_hash: String) -> Result<Option<FileVersionRecord>> {
        smartfs_db::version_find_by_hash(&self.pool, &content_hash).await
    }

    /// @id: b2437593-8b43-4b50-ba56-fc3f8eaff7a8
    /// `get_file_content`: retrieves raw bytes / text from store for a given path and version.
    pub async fn get_file_content(
        &self,
        path: String,
        version: Option<i32>,
    ) -> Result<FileContentResult> {
        let inode = self.resolve_path(&path).await?;
        let version_record = smartfs_db::version_get(&self.pool, inode.id, version)
            .await?
            .ok_or_else(|| {
                SmartFsError::NotFound(format!("Version {version:?} of '{path}' not found"))
            })?;

        let blob_id = version_record.blob_id.unwrap_or_default();
        let bytes = self
            .store
            .get(blob_id, version_record.external_path.as_deref())
            .await?;

        let text = String::from_utf8(bytes.clone()).ok();
        let bytes_base64 = if text.is_none() {
            Some(hex_encode(&bytes))
        } else {
            None
        };

        Ok(FileContentResult {
            path,
            version: version_record.version_number,
            size: version_record.size,
            content_hash: version_record.content_hash,
            text,
            bytes_base64,
        })
    }

    /// @id: 30105cc3-8470-4127-a084-f30a4092a74d
    /// `find_broken_files`: finds versions where status is 'syntax_error'.
    pub async fn find_broken_files(&self) -> Result<Vec<FileVersionRecord>> {
        let file_inodes = self.collect_all_file_inodes().await?;
        let mut broken = Vec::new();

        for inode in file_inodes {
            let versions = smartfs_db::version_history(&self.pool, inode.id).await?;
            for v in versions {
                if v.status == "syntax_error" {
                    broken.push(v);
                }
            }
        }

        Ok(broken)
    }

    /// @id: 741abc83-4d7e-4297-8bc4-f18a7fd6084d
    /// `diff_functions`: AST diff between two versions of a file.
    pub async fn diff_functions(&self, path: String, v1: i32, v2: i32) -> Result<AstDiff> {
        let inode = self.resolve_path(&path).await?;
        let rec1 = smartfs_db::version_get(&self.pool, inode.id, Some(v1))
            .await?
            .ok_or_else(|| SmartFsError::NotFound(format!("Version {v1} not found for '{path}'")))?;
        let rec2 = smartfs_db::version_get(&self.pool, inode.id, Some(v2))
            .await?
            .ok_or_else(|| SmartFsError::NotFound(format!("Version {v2} not found for '{path}'")))?;

        let nodes1 = smartfs_db::get_ast_nodes(&self.pool, rec1.id).await?;
        let nodes2 = smartfs_db::get_ast_nodes(&self.pool, rec2.id).await?;

        let mut map1: HashMap<(String, String), &smartfs_db::AstNodeRecord> = HashMap::new();
        for n in &nodes1 {
            map1.insert((n.kind.clone(), n.name.clone()), n);
        }

        let mut map2: HashMap<(String, String), &smartfs_db::AstNodeRecord> = HashMap::new();
        for n in &nodes2 {
            map2.insert((n.kind.clone(), n.name.clone()), n);
        }

        let mut added = Vec::new();
        let mut changed = Vec::new();
        let mut removed = Vec::new();

        for (k, n2) in &map2 {
            match map1.get(k) {
                None => added.push((*n2).clone()),
                Some(n1) => {
                    if n1.content_hash != n2.content_hash || n1.source != n2.source {
                        changed.push((*n2).clone());
                    }
                }
            }
        }

        for (k, n1) in &map1 {
            if !map2.contains_key(k) {
                removed.push((*n1).clone());
            }
        }

        Ok(AstDiff {
            added,
            changed,
            removed,
        })
    }

    /// @id: 63c8f929-a967-47e9-92b0-794eab8aac30
    /// `search_by_concept`: delegates to `smartfs_semantic::search_by_concept` if vector given,
    /// or returns list of active centroids sorted by `member_count` (ADR-50/53).
    pub async fn search_by_concept(
        &self,
        query_vector: Option<Vec<f32>>,
        plugin_type: Option<String>,
        model_id: Option<Uuid>,
        limit: Option<usize>,
    ) -> Result<serde_json::Value> {
        if let Some(vec) = query_vector {
            let m_id = match model_id {
                Some(m) => m,
                None => smartfs_db::get_default_model_id(&self.pool).await?,
            };
            let p_type = plugin_type.unwrap_or_else(|| "generic".to_string());
            let lim = limit.unwrap_or(10);
            let hits = smartfs_semantic::search_by_concept(&self.pool, &vec, &p_type, m_id, lim).await?;
            Ok(serde_json::to_value(hits).map_err(|e| SmartFsError::Other(e.to_string()))?)
        } else {
            // When query_vector is absent: returns list of active centroids
            let combos = smartfs_semantic::fetch_calibrated_combinations(&self.pool)
                .await
                .unwrap_or_default();

            let target_plugin = plugin_type.as_deref();
            let mut summaries: Vec<CentroidSummary> = Vec::new();

            for (p_type, m_id) in combos {
                if let Some(target) = target_plugin {
                    if p_type != target {
                        continue;
                    }
                }
                if let Ok(cfg) = smartfs_semantic::load_consolidation_config(&self.pool, &p_type, m_id).await {
                    summaries.push(CentroidSummary {
                        id: m_id,
                        label: Some(p_type.clone()),
                        member_count: cfg.backlog_threshold,
                        sample_names: vec![p_type],
                    });
                }
            }

            summaries.sort_by_key(|a| std::cmp::Reverse(a.member_count));
            if let Some(lim) = limit {
                summaries.truncate(lim);
            }

            Ok(serde_json::to_value(summaries).map_err(|e| SmartFsError::Other(e.to_string()))?)
        }
    }

    /// @id: 8cde9539-3c24-4bcb-8492-ab19be66b533
    /// `search_fulltext`: delegates to `smartfs_db::search_fulltext_bm25` (ADR-54).
    pub async fn search_fulltext(
        &self,
        query: String,
        plugin_type: Option<String>,
        limit: Option<i64>,
    ) -> Result<Vec<FulltextHit>> {
        smartfs_db::search_fulltext_bm25(
            &self.pool,
            &query,
            plugin_type.as_deref(),
            limit.unwrap_or(20),
        )
        .await
    }

    /// @id: 80656d24-8778-4f54-b3aa-0d8756ca2f01
    /// `get_actor_activity`: queries file versions authored by a specific agent (ADR-56).
    pub async fn get_actor_activity(
        &self,
        actor_id: String,
        since: Option<DateTime<Utc>>,
        limit: Option<usize>,
    ) -> Result<Vec<FileVersionRecord>> {
        let file_inodes = self.collect_all_file_inodes().await?;
        let mut matches = Vec::new();

        for inode in file_inodes {
            let versions = smartfs_db::version_history(&self.pool, inode.id).await?;
            for v in versions {
                let matches_actor = v
                    .special_data
                    .get("agent")
                    .and_then(|a| a.get("actor_id"))
                    .and_then(|id| id.as_str())
                    == Some(&actor_id);

                if matches_actor {
                    if let Some(since_dt) = since {
                        if v.created_at < since_dt {
                            continue;
                        }
                    }
                    matches.push(v);
                }
            }
        }

        matches.sort_by_key(|a| std::cmp::Reverse(a.created_at));
        if let Some(lim) = limit {
            matches.truncate(lim);
        }

        Ok(matches)
    }

    /// @id: fc912c04-9578-4b19-bcd4-31ee12a69ff1
    /// `describe_plugin_type`: describes the schema of a plugin type (ADR-57).
    pub async fn describe_plugin_type(&self, plugin_type: String) -> Result<PluginSchema> {
        if let Some(ref dir) = self.plugins_dir {
            let path = dir.join(format!("{plugin_type}.json"));
            if path.exists() {
                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                    if let Ok(schema) = serde_json::from_str::<PluginSchema>(&content) {
                        return Ok(schema);
                    }
                }
            }
        }

        // Built-in standard plugin specifications from Architecture v4.5 §10
        match plugin_type.as_str() {
            "rust" => Ok(PluginSchema {
                plugin_type: "rust".to_string(),
                description: "Rust source code file containing compiled, memory-safe system-level code.".to_string(),
                match_extensions: vec![".rs".to_string()],
                schema: serde_json::json!({
                    "language": { "type": "string", "description": "Programming language identifier as detected by tree-sitter." },
                    "node_count": { "type": "integer", "description": "Total number of top-level AST nodes extracted from this file." },
                    "has_syntax_errors": { "type": "boolean", "description": "Whether tree-sitter detected any syntax errors during parsing." },
                    "has_unsafe": { "type": "boolean", "description": "Whether this file contains any unsafe blocks." }
                }),
                ast: true,
                embedding: Some(serde_json::json!({
                    "model": "text-embedding-3-large",
                    "dimensions": 1536,
                    "description": "Semantic embedding of full source code, optimised for code similarity search."
                })),
            }),
            "png" => Ok(PluginSchema {
                plugin_type: "png".to_string(),
                description: "PNG image file containing raster graphics data with optional metadata.".to_string(),
                match_extensions: vec![".png".to_string()],
                schema: serde_json::json!({
                    "width": { "type": "integer", "description": "Image width in pixels." },
                    "height": { "type": "integer", "description": "Image height in pixels." },
                    "color_type": { "type": "string", "description": "Colour mode, e.g. RGBA, RGB, Greyscale." },
                    "has_alpha": { "type": "boolean", "description": "Whether image contains a transparency channel." },
                    "text_metadata": { "type": "object", "description": "Key-value pairs from PNG tEXt chunks, e.g. Software, Author." }
                }),
                ast: false,
                embedding: None,
            }),
            "generic" => Ok(PluginSchema {
                plugin_type: "generic".to_string(),
                description: "Generic untyped file processed with default file-level embeddings.".to_string(),
                match_extensions: vec![],
                schema: serde_json::json!({}),
                ast: false,
                embedding: None,
            }),
            other => Err(SmartFsError::NotFound(format!("Plugin type '{other}' not found"))),
        }
    }

    /// @id: 40695b0e-e3a2-4b9e-bede-7b7c432f33f7
    /// `list_plugin_types`: lists all registered plugin summaries (ADR-57).
    pub async fn list_plugin_types(&self) -> Result<Vec<PluginSummary>> {
        let mut summaries = vec![
            PluginSummary {
                plugin_type: "rust".to_string(),
                description: "Rust source code file containing compiled, memory-safe system-level code.".to_string(),
                match_extensions: vec![".rs".to_string()],
            },
            PluginSummary {
                plugin_type: "png".to_string(),
                description: "PNG image file containing raster graphics data with optional metadata.".to_string(),
                match_extensions: vec![".png".to_string()],
            },
            PluginSummary {
                plugin_type: "generic".to_string(),
                description: "Generic untyped file processed with default file-level embeddings.".to_string(),
                match_extensions: vec![],
            },
        ];

        // Also check plugins directory if present
        if let Some(ref dir) = self.plugins_dir {
            if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) == Some("json") {
                        if let Ok(content) = tokio::fs::read_to_string(&path).await {
                            if let Ok(schema) = serde_json::from_str::<PluginSchema>(&content) {
                                if !summaries.iter().any(|s| s.plugin_type == schema.plugin_type) {
                                    summaries.push(PluginSummary {
                                        plugin_type: schema.plugin_type,
                                        description: schema.description,
                                        match_extensions: schema.match_extensions,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(summaries)
    }

    /// @id: a3fa4687-0c1f-4b86-b906-a40f00581393
    /// `delete_file`: destructive deletion guarded by confirmation token (ADR-48).
    pub async fn delete_file(
        &self,
        path: String,
        confirmation_token: Option<String>,
    ) -> Result<serde_json::Value> {
        match confirmation_token {
            None => {
                let token = self
                    .tokens
                    .lock()
                    .await
                    .create_token(DestructiveAction::DeleteFile { path: path.clone() });
                let resp = ConfirmationRequired {
                    status: "confirmation_required".to_string(),
                    confirmation_token: token,
                    action: "delete_file".to_string(),
                    path: path.clone(),
                    message: format!(
                        "Destructive operation 'delete_file' on '{path}' requires confirmation. Re-call with confirmation_token."
                    ),
                };
                Ok(serde_json::to_value(resp).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            Some(token) => {
                let _action = self
                    .tokens
                    .lock()
                    .await
                    .consume_token(&token, "delete_file", &path)
                    .ok_or_else(|| {
                        SmartFsError::Conflict("Invalid or expired confirmation token".to_string())
                    })?;

                let inode = self.resolve_path(&path).await?;
                smartfs_db::inode_delete(&self.pool, inode.id).await?;

                let result = DestructiveResult {
                    status: "success".to_string(),
                    action: "delete_file".to_string(),
                    path,
                    message: "File deleted successfully".to_string(),
                    version_id: None,
                };
                Ok(serde_json::to_value(result).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
        }
    }

    /// @id: b06aaf5b-0dcc-4d99-b006-a6d9a6a62fc7
    /// `overwrite_file`: destructive file overwrite guarded by confirmation token (ADR-48).
    pub async fn overwrite_file(
        &self,
        path: String,
        content: String,
        confirmation_token: Option<String>,
    ) -> Result<serde_json::Value> {
        match confirmation_token {
            None => {
                let token = self.tokens.lock().await.create_token(
                    DestructiveAction::OverwriteFile {
                        path: path.clone(),
                        content,
                    },
                );
                let resp = ConfirmationRequired {
                    status: "confirmation_required".to_string(),
                    confirmation_token: token,
                    action: "overwrite_file".to_string(),
                    path: path.clone(),
                    message: format!(
                        "Destructive operation 'overwrite_file' on '{path}' requires confirmation. Re-call with confirmation_token."
                    ),
                };
                Ok(serde_json::to_value(resp).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
            Some(token) => {
                let action = self
                    .tokens
                    .lock()
                    .await
                    .consume_token(&token, "overwrite_file", &path)
                    .ok_or_else(|| {
                        SmartFsError::Conflict("Invalid or expired confirmation token".to_string())
                    })?;

                let write_content = match action {
                    DestructiveAction::OverwriteFile { content, .. } => content,
                    _ => content,
                };

                let inode = self.resolve_path(&path).await?;
                let blob_id = Uuid::new_v4();
                let bytes = write_content.as_bytes();
                let content_hash = compute_sha256_hex(bytes);

                self.store.put(blob_id, bytes).await?;

                let version_id = smartfs_db::cow_commit(
                    &self.pool,
                    inode.id,
                    Some(blob_id),
                    &content_hash,
                    bytes.len() as i64,
                    None,
                    None,
                    None,
                    None,
                    &[],
                )
                .await?;

                let result = DestructiveResult {
                    status: "success".to_string(),
                    action: "overwrite_file".to_string(),
                    path,
                    message: "File overwritten successfully".to_string(),
                    version_id: Some(version_id),
                };
                Ok(serde_json::to_value(result).map_err(|e| SmartFsError::Other(e.to_string()))?)
            }
        }
    }
}

/// Recursive matcher to verify `filter` is a subset of `data` (JSONB matching).
fn json_contains(data: &serde_json::Value, filter: &serde_json::Value) -> bool {
    match (data, filter) {
        (serde_json::Value::Object(data_map), serde_json::Value::Object(filter_map)) => {
            for (k, f_val) in filter_map {
                match data_map.get(k) {
                    Some(d_val) => {
                        if !json_contains(d_val, f_val) {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            true
        }
        (d, f) => d == f,
    }
}

/// Helper to encode byte slice as hex string.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Self-contained SHA-256 implementation (FIPS 180-4) to avoid unnecessary external crate dependencies.
#[allow(clippy::all)]
fn compute_sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0x00);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, part) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([part[0], part[1], part[2], part[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_val = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_val
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_val = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_val);
    }

    let mut out = String::with_capacity(64);
    for val in h {
        use std::fmt::Write;
        let _ = write!(out, "{val:08x}");
    }
    out
}
