use sha2::{Digest, Sha256};
use smartfs_db::AstNodeInsert;

/// @id: ef89abcd-2345-0678-e9ab-cdef01234567
/// Synchronously parse code and extract AST nodes (functions, structs, classes, impls).
/// Note: Must be invoked inside `spawn_blocking` when called in an async context (§3.7).
pub fn extract_ast_nodes(code: &str, file_type: &str) -> Vec<AstNodeInsert> {
    let mut nodes = Vec::new();
    let lines: Vec<&str> = code.lines().collect();

    if lines.is_empty() {
        return nodes;
    }

    let is_rust = file_type == "rust" || file_type == "rs";
    let is_python = file_type == "python" || file_type == "py";

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();

        // 1. Rust functions, structs, enums, traits, impls
        if is_rust {
            if let Some(name) = extract_decl_name(line, "fn ") {
                let (end, src) = extract_block(&lines, i);
                nodes.push(make_ast_node("function", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "struct ") {
                let (end, src) = extract_block(&lines, i);
                nodes.push(make_ast_node("struct", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "enum ") {
                let (end, src) = extract_block(&lines, i);
                nodes.push(make_ast_node("enum", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "trait ") {
                let (end, src) = extract_block(&lines, i);
                nodes.push(make_ast_node("trait", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "impl ") {
                let (end, src) = extract_block(&lines, i);
                nodes.push(make_ast_node("impl", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            }
        }

        // 2. Python def, class
        if is_python {
            if let Some(name) = extract_python_name(line, "def ") {
                let (end, src) = extract_python_block(&lines, i);
                nodes.push(make_ast_node("function", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_python_name(line, "class ") {
                let (end, src) = extract_python_block(&lines, i);
                nodes.push(make_ast_node("class", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            }
        }

        // Generic fallback for functions across other languages
        if let Some(name) = extract_decl_name(line, "function ") {
            let (end, src) = extract_block(&lines, i);
            nodes.push(make_ast_node("function", name, i + 1, end + 1, src));
            i = end + 1;
            continue;
        }

        i += 1;
    }

    nodes
}

/// @id: f09abcde-3456-1789-fa01-23456789abcd
/// Parse AST nodes offloading execution to `tokio::task::spawn_blocking` (Invariant: C/CPU parser blocks thread).
pub async fn parse_ast_nodes_blocking(code: String, file_type: String) -> Vec<AstNodeInsert> {
    tokio::task::spawn_blocking(move || extract_ast_nodes(&code, &file_type))
        .await
        .unwrap_or_default()
}

fn extract_decl_name(line: &str, keyword: &str) -> Option<String> {
    if let Some(pos) = line.find(keyword) {
        let remainder = &line[pos + keyword.len()..].trim();
        let ident: String = remainder
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !ident.is_empty() {
            return Some(ident);
        }
    }
    None
}

fn extract_python_name(line: &str, keyword: &str) -> Option<String> {
    if line.starts_with(keyword) || line.contains(&format!(" {keyword}")) {
        extract_decl_name(line, keyword)
    } else {
        None
    }
}

fn extract_block(lines: &[&str], start_idx: usize) -> (usize, String) {
    let mut brace_depth = 0;
    let mut found_open = false;
    let mut end_idx = start_idx;

    for (idx, line) in lines.iter().enumerate().skip(start_idx) {
        end_idx = idx;
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                found_open = true;
            } else if ch == '}' {
                brace_depth -= 1;
            }
        }
        if found_open && brace_depth <= 0 {
            break;
        }
    }

    let src = lines[start_idx..=end_idx].join("\n");
    (end_idx, src)
}

fn extract_python_block(lines: &[&str], start_idx: usize) -> (usize, String) {
    let start_indent = get_indent(lines[start_idx]);
    let mut end_idx = start_idx;

    for (idx, line) in lines.iter().enumerate().skip(start_idx + 1) {
        if line.trim().is_empty() {
            continue;
        }
        let indent = get_indent(line);
        if indent <= start_indent {
            break;
        }
        end_idx = idx;
    }

    let src = lines[start_idx..=end_idx].join("\n");
    (end_idx, src)
}

fn get_indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

fn make_ast_node(
    kind: &str,
    name: String,
    start_line: usize,
    end_line: usize,
    source: String,
) -> AstNodeInsert {
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    let content_hash = hex::encode(hasher.finalize());

    AstNodeInsert {
        kind: kind.to_string(),
        name,
        start_line: start_line as i32,
        end_line: end_line as i32,
        source,
        content_hash,
    }
}
