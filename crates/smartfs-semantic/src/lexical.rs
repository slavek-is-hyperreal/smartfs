//! Lexical graph nodes, relations, WordNet import, and TF-IDF centroid labeling.

use serde::{Deserialize, Serialize};
use smartfs_schema::error::{Result, SmartFsError};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

/// @id: 28f13b94-8192-4d2c-905b-43a9b5f102c7
/// A lexical node representing a lemma with optional part of speech and language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalNode {
    pub id: Uuid,
    pub lemma: String,
    pub pos: Option<String>,
    pub language: String,
    pub external_ref: Option<String>,
}

/// @id: b79e1208-410a-48d6-84fa-9f82d1c05a11
/// Relation type between lexical nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalRelation {
    Synonym,
    Hypernym,
    Hyponym,
    Meronym,
}

impl std::str::FromStr for LexicalRelation {
    type Err = SmartFsError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "synonym" => Ok(LexicalRelation::Synonym),
            "hypernym" => Ok(LexicalRelation::Hypernym),
            "hyponym" => Ok(LexicalRelation::Hyponym),
            "meronym" => Ok(LexicalRelation::Meronym),
            other => Err(SmartFsError::SyntaxError(format!(
                "Unknown lexical relation '{other}'"
            ))),
        }
    }
}

impl LexicalRelation {
    /// @id: d0124a91-3b4e-4f12-8901-23456789abcd
    /// Returns the static lowercase string representation of this relation.
    pub fn as_str(&self) -> &'static str {
        match self {
            LexicalRelation::Synonym => "synonym",
            LexicalRelation::Hypernym => "hypernym",
            LexicalRelation::Hyponym => "hyponym",
            LexicalRelation::Meronym => "meronym",
        }
    }
}

/// @id: e90b41c7-2d88-4a67-b501-8374d6c90e15
/// Link associating a lexical word node with a concept centroid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordCentroidLink {
    pub lexical_node_id: Uuid,
    pub centroid_id: Uuid,
    pub weight: f64,
}

/// @id: 5a8e2390-3481-4f10-98a7-bcde12894567
/// Input structure for importing a single node in WordNet format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordnetNodeImport {
    pub lemma: String,
    pub pos: Option<String>,
    pub external_ref: Option<String>,
}

/// @id: 6b9f3401-4592-5021-a9b8-cdef23905678
/// Input structure for importing an edge between lemmas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordnetEdgeImport {
    pub from: String,
    pub to: String,
    pub relation: String,
}

/// @id: 7ca04512-5603-6132-bac9-def034016789
/// Standardized intermediate JSON format for WordNet import.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WordnetImport {
    pub language: String,
    pub nodes: Vec<WordnetNodeImport>,
    pub edges: Vec<WordnetEdgeImport>,
}

/// @id: 1f0a9b82-3c74-4e12-8a90-df45b678c123
/// Breaks down identifier names (camelCase, PascalCase, snake_case, kebab-case) into individual lowercase word tokens.
pub fn tokenize_identifier(name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();
    let n = chars.len();

    for i in 0..n {
        let c = chars[i];
        if c == '_' || c == '-' || c == '.' || c == '/' || c == ':' || c == ' ' {
            if !current.is_empty() {
                tokens.push(current.to_lowercase());
                current.clear();
            }
        } else if c.is_uppercase() {
            let prev_lower = i > 0 && chars[i - 1].is_lowercase();
            let next_lower = i + 1 < n && chars[i + 1].is_lowercase();
            if (prev_lower || (next_lower && current.len() > 1)) && !current.is_empty() {
                tokens.push(current.to_lowercase());
                current.clear();
            }
            current.push(c);
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        tokens.push(current.to_lowercase());
    }

    tokens.into_iter().filter(|t| !t.is_empty()).collect()
}

/// @id: e8f2c6a1-4d97-4b03-a5e6-2f9d7c1b8a63
/// Imports WordNet lexical nodes and edges from a standardized JSON file.
///
/// Idempotent: uses UPSERT on `lexical_nodes` and `ON CONFLICT DO NOTHING` on `lexical_edges`.
pub async fn import_wordnet(db: &PgPool, path: &Path, language: &str) -> Result<usize> {
    let file = std::fs::File::open(path)?;
    let doc: WordnetImport = serde_json::from_reader(file)
        .map_err(|e| SmartFsError::SyntaxError(format!("Invalid WordNet JSON format: {e}")))?;

    let lang = if doc.language.is_empty() {
        language
    } else {
        &doc.language
    };

    let mut lemma_to_id: HashMap<String, Uuid> = HashMap::new();
    let mut inserted_count = 0usize;

    for node in &doc.nodes {
        let node_id = Uuid::new_v4();
        let row = sqlx::query(
            r#"
            INSERT INTO lexical_nodes (id, lemma, pos, language, external_ref)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (lemma, pos, language) DO UPDATE SET
                external_ref = COALESCE(EXCLUDED.external_ref, lexical_nodes.external_ref)
            RETURNING id
            "#,
        )
        .bind(node_id)
        .bind(&node.lemma)
        .bind(&node.pos)
        .bind(lang)
        .bind(&node.external_ref)
        .fetch_one(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("import_wordnet node insert error: {e}")))?;

        let returned_id: Uuid = row.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?;
        lemma_to_id.insert(node.lemma.clone(), returned_id);
        inserted_count += 1;
    }

    for edge in &doc.edges {
        let from_id = if let Some(&id) = lemma_to_id.get(&edge.from) {
            id
        } else {
            let row = sqlx::query("SELECT id FROM lexical_nodes WHERE lemma = $1 AND language = $2 LIMIT 1")
                .bind(&edge.from)
                .bind(lang)
                .fetch_optional(db)
                .await
                .map_err(|e| SmartFsError::Db(e.to_string()))?;
            match row {
                Some(r) => r.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?,
                None => continue,
            }
        };

        let to_id = if let Some(&id) = lemma_to_id.get(&edge.to) {
            id
        } else {
            let row = sqlx::query("SELECT id FROM lexical_nodes WHERE lemma = $1 AND language = $2 LIMIT 1")
                .bind(&edge.to)
                .bind(lang)
                .fetch_optional(db)
                .await
                .map_err(|e| SmartFsError::Db(e.to_string()))?;
            match row {
                Some(r) => r.try_get("id").map_err(|e| SmartFsError::Db(e.to_string()))?,
                None => continue,
            }
        };

        let relation = edge.relation.parse::<LexicalRelation>()?;
        let res = sqlx::query(
            r#"
            INSERT INTO lexical_edges (from_id, to_id, relation)
            VALUES ($1, $2, $3)
            ON CONFLICT (from_id, to_id, relation) DO NOTHING
            "#,
        )
        .bind(from_id)
        .bind(to_id)
        .bind(relation.as_str())
        .execute(db)
        .await
        .map_err(|e| SmartFsError::Db(format!("import_wordnet edge insert error: {e}")))?;

        inserted_count += res.rows_affected() as usize;
    }

    Ok(inserted_count)
}

/// @id: a4c91032-6804-4df1-87a2-19e48c205b31
/// Links a lexical word node to a concept centroid with a given weight.
pub async fn link_word_to_centroid(
    db: &PgPool,
    lexical_node_id: Uuid,
    centroid_id: Uuid,
    weight: f64,
) -> Result<()> {
    // Find table corresponding to this centroid
    for link_table in [
        "word_centroid_links_1536",
        "word_centroid_links_1024_qwen",
        "word_centroid_links_768",
        "word_centroid_links_384",
    ] {
        let centroid_table = link_table.replace("word_centroid_links_", "concept_centroids_");
        let exists: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS(SELECT 1 FROM {centroid_table} WHERE id = $1)"
        ))
        .bind(centroid_id)
        .fetch_one(db)
        .await
        .unwrap_or(false);

        if exists {
            let sql = format!(
                r#"
                INSERT INTO {link_table} (lexical_node_id, centroid_id, weight)
                VALUES ($1, $2, $3)
                ON CONFLICT (lexical_node_id, centroid_id) DO UPDATE SET
                    weight = EXCLUDED.weight
                "#
            );
            sqlx::query(&sql)
                .bind(lexical_node_id)
                .bind(centroid_id)
                .bind(weight)
                .execute(db)
                .await
                .map_err(|e| SmartFsError::Db(format!("link_word_to_centroid error: {e}")))?;
            return Ok(());
        }
    }

    Err(SmartFsError::NotFound(format!(
        "Centroid {centroid_id} not found in any centroid table"
    )))
}

/// @id: 3c8e41a9-9023-4e89-a2f1-6789b0123456
/// Labels a centroid by applying TF-IDF over member identifier names.
///
/// Algorithm:
/// 1. Tokenize AST node names (for 1536) or file names (for other dimensions).
/// 2. Compute internal Term Frequency (TF) within the centroid.
/// 3. Compute Document Frequency (DF) across all centroids of the same `plugin_type`.
/// 4. Choose term with the highest TF-IDF score (breaking ties with first-token frequency).
/// 5. Cache result in `concept_centroids_*.label`.
pub async fn label_centroid_from_members(db: &PgPool, centroid_id: Uuid) -> Result<Option<String>> {
    // Locate centroid
    let mut target_family: Option<(&'static str, &'static str, String)> = None;

    for (c_tbl, m_tbl, is_ast) in [
        ("concept_centroids_1536", "centroid_members_1536", true),
        ("concept_centroids_1024_qwen", "centroid_members_1024_qwen", false),
        ("concept_centroids_768", "centroid_members_768", false),
        ("concept_centroids_384", "centroid_members_384", false),
    ] {
        let sql = format!("SELECT plugin_type FROM {c_tbl} WHERE id = $1");
        if let Some(row) = sqlx::query(&sql).bind(centroid_id).fetch_optional(db).await.map_err(|e| SmartFsError::Db(e.to_string()))? {
            let p_type: String = row.try_get("plugin_type").map_err(|e| SmartFsError::Db(e.to_string()))?;
            let _ = is_ast;
            target_family = Some((c_tbl, m_tbl, p_type));
            break;
        }
    }

    let (c_table, m_table, plugin_type) = match target_family {
        Some(f) => f,
        None => return Err(SmartFsError::NotFound(format!("Centroid {centroid_id} not found"))),
    };

    // Fetch names of members in this centroid
    let names: Vec<String> = if c_table.contains("1536") {
        sqlx::query_scalar(&format!(
            r#"
            SELECT an.name
            FROM {m_table} cm
            JOIN ast_nodes an ON cm.ast_node_id = an.id
            WHERE cm.centroid_id = $1
            "#
        ))
        .bind(centroid_id)
        .fetch_all(db)
        .await
        .map_err(|e| SmartFsError::Db(e.to_string()))?
    } else {
        sqlx::query_scalar(&format!(
            r#"
            SELECT ir.name
            FROM {m_table} cm
            JOIN file_versions fv ON cm.version_id = fv.id
            JOIN inode_registry ir ON fv.inode_id = ir.id
            WHERE cm.centroid_id = $1
            "#
        ))
        .bind(centroid_id)
        .fetch_all(db)
        .await
        .map_err(|e| SmartFsError::Db(e.to_string()))?
    };

    if names.is_empty() {
        return Ok(None);
    }

    // Tokenize and calculate TF
    let mut tf: HashMap<String, usize> = HashMap::new();
    let mut first_token_count: HashMap<String, usize> = HashMap::new();
    let mut total_tokens = 0usize;

    for name in &names {
        let tokens = tokenize_identifier(name);
        if let Some(first) = tokens.first() {
            *first_token_count.entry(first.clone()).or_insert(0) += 1;
        }
        for token in tokens {
            *tf.entry(token).or_insert(0) += 1;
            total_tokens += 1;
        }
    }

    if total_tokens == 0 {
        return Ok(None);
    }

    // Count total centroids for this plugin_type
    let total_centroids: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM {c_table} WHERE plugin_type = $1 AND is_active = TRUE"
    ))
    .bind(&plugin_type)
    .fetch_one(db)
    .await
    .unwrap_or(1);

    let n_centroids = total_centroids.max(1) as f64;

    // Calculate TF-IDF for each token
    let mut best_term: Option<String> = None;
    let mut best_score = -1.0f64;
    let mut best_first_count = 0usize;

    for (term, count) in &tf {
        let tf_score = *count as f64 / total_tokens as f64;
        // Document frequency: how many centroids contain this term
        // Using sample heuristic or search
        let df_pattern = format!("%{term}%");
        let df_count: i64 = if c_table.contains("1536") {
            sqlx::query_scalar(&format!(
                r#"
                SELECT COUNT(DISTINCT cm.centroid_id)
                FROM {m_table} cm
                JOIN ast_nodes an ON cm.ast_node_id = an.id
                JOIN {c_table} cc ON cm.centroid_id = cc.id
                WHERE cc.plugin_type = $1 AND cc.is_active = TRUE
                  AND an.name ILIKE $2
                "#
            ))
            .bind(&plugin_type)
            .bind(&df_pattern)
            .fetch_one(db)
            .await
            .unwrap_or(1)
        } else {
            sqlx::query_scalar(&format!(
                r#"
                SELECT COUNT(DISTINCT cm.centroid_id)
                FROM {m_table} cm
                JOIN file_versions fv ON cm.version_id = fv.id
                JOIN inode_registry ir ON fv.inode_id = ir.id
                JOIN {c_table} cc ON cm.centroid_id = cc.id
                WHERE cc.plugin_type = $1 AND cc.is_active = TRUE
                  AND ir.name ILIKE $2
                "#
            ))
            .bind(&plugin_type)
            .bind(&df_pattern)
            .fetch_one(db)
            .await
            .unwrap_or(1)
        };

        let df_f = (df_count.max(1)) as f64;
        let idf_score = ((1.0 + n_centroids) / (1.0 + df_f)).ln() + 1.0;
        let tfidf = tf_score * idf_score;
        let first_cnt = first_token_count.get(term).copied().unwrap_or(0);

        if tfidf > best_score || ((tfidf - best_score).abs() < 1e-6 && first_cnt > best_first_count) {
            best_score = tfidf;
            best_term = Some(term.clone());
            best_first_count = first_cnt;
        }
    }

    if let Some(ref label) = best_term {
        let update_sql = format!("UPDATE {c_table} SET label = $1 WHERE id = $2");
        sqlx::query(&update_sql)
            .bind(label)
            .bind(centroid_id)
            .execute(db)
            .await
            .map_err(|e| SmartFsError::Db(format!("Failed to update centroid label: {e}")))?;
    }

    Ok(best_term)
}
