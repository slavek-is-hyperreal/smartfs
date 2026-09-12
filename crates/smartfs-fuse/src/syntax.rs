//! Syntax validation and AST extraction pipeline for SmartFS.
//!
//! Enforces:
//! - Syntax check in `flush()` (throwaway parse returning EACCES on syntax error).
//! - Parsing offloaded to `tokio::task::spawn_blocking` (§3.6, §11.1).
//! - AST extraction for `cow_commit` in `release()`.

use sha2::{Digest, Sha256};
use smartfs_db::AstNodeInsert;
use smartfs_schema::error::{Result, SmartFsError};

/// @id: de8b3c4d-5e6f-4701-1829-3a4b5c6d7e8f
/// Determines the programming language / syntax format from a file name or path.
pub fn detect_syntax_language(filename: &str) -> Option<&'static str> {
    let ext = filename.rsplit('.').next()?;
    match ext.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "js" | "mjs" | "cjs" | "jsx" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "go" => Some("go"),
        "json" => Some("json"),
        _ => None,
    }
}

/// @id: ef9c4d5e-6f70-4812-293a-4b5c6d7e8f90
/// Validates source code syntax.
/// Returns `Ok(())` if valid or `Err(SmartFsError::SyntaxError)` with details if invalid.
pub fn validate_syntax(code: &str, lang: &str) -> Result<()> {
    if lang == "json" {
        return serde_json::from_str::<serde_json::Value>(code)
            .map(|_| ())
            .map_err(|e| SmartFsError::SyntaxError(format!("JSON syntax error: {e}")));
    }

    validate_balanced_delimiters(code, lang)
}

/// @id: f0ad5e6f-7081-4923-3a4b-5c6d7e8f90a1
/// Checks that parentheses, brackets, and braces are strictly balanced and properly nested,
/// ignoring characters inside comments and string literals.
pub fn validate_balanced_delimiters(code: &str, lang: &str) -> Result<()> {
    let mut stack: Vec<(char, usize, usize)> = Vec::new(); // (delimiter, line, col)
    let chars: Vec<char> = code.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut line = 1;
    let mut col = 1;

    let is_python = lang == "python";

    while i < len {
        let ch = chars[i];

        // Track line and column
        if ch == '\n' {
            line += 1;
            col = 1;
            i += 1;
            continue;
        }

        // 1. Line comments
        if (ch == '/' && i + 1 < len && chars[i + 1] == '/') || (is_python && ch == '#') {
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // 2. Block comments (/* ... */)
        if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
            i += 2;
            let mut closed = false;
            while i + 1 < len {
                if chars[i] == '\n' {
                    line += 1;
                    col = 1;
                }
                if chars[i] == '*' && chars[i + 1] == '/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            if !closed {
                return Err(SmartFsError::SyntaxError(format!(
                    "Unclosed block comment starting at line {line}"
                )));
            }
            continue;
        }

        // 3. String literals ("..." and '...')
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let quote_line = line;
            i += 1;
            col += 1;
            let mut closed = false;
            while i < len {
                let curr = chars[i];
                if curr == '\n' {
                    if is_python {
                        line += 1;
                        col = 1;
                    } else {
                        // Single-line strings in C/Rust/JS cannot span newlines unescaped
                        return Err(SmartFsError::SyntaxError(format!(
                            "Unclosed string literal at line {quote_line}"
                        )));
                    }
                }
                if curr == '\\' && i + 1 < len {
                    i += 2;
                    col += 2;
                    continue;
                }
                if curr == quote {
                    i += 1;
                    col += 1;
                    closed = true;
                    break;
                }
                i += 1;
                col += 1;
            }
            if !closed {
                return Err(SmartFsError::SyntaxError(format!(
                    "Unclosed string literal at line {quote_line}"
                )));
            }
            continue;
        }

        // 4. Delimiters
        match ch {
            '(' | '[' | '{' => {
                stack.push((ch, line, col));
            }
            ')' => {
                match stack.pop() {
                    Some(('(', _, _)) => {}
                    Some((mismatched, l, c)) => {
                        return Err(SmartFsError::SyntaxError(format!(
                            "Mismatched closing ')' at line {line}:{col}, expected matching '{mismatched}' from line {l}:{c}"
                        )));
                    }
                    None => {
                        return Err(SmartFsError::SyntaxError(format!(
                            "Unexpected closing ')' with no opening counterpart at line {line}:{col}"
                        )));
                    }
                }
            }
            ']' => {
                match stack.pop() {
                    Some(('[', _, _)) => {}
                    Some((mismatched, l, c)) => {
                        return Err(SmartFsError::SyntaxError(format!(
                            "Mismatched closing ']' at line {line}:{col}, expected matching '{mismatched}' from line {l}:{c}"
                        )));
                    }
                    None => {
                        return Err(SmartFsError::SyntaxError(format!(
                            "Unexpected closing ']' with no opening counterpart at line {line}:{col}"
                        )));
                    }
                }
            }
            '}' => {
                match stack.pop() {
                    Some(('{', _, _)) => {}
                    Some((mismatched, l, c)) => {
                        return Err(SmartFsError::SyntaxError(format!(
                            "Mismatched closing '}}' at line {line}:{col}, expected matching '{mismatched}' from line {l}:{c}"
                        )));
                    }
                    None => {
                        return Err(SmartFsError::SyntaxError(format!(
                            "Unexpected closing '}}' with no opening counterpart at line {line}:{col}"
                        )));
                    }
                }
            }
            _ => {}
        }

        i += 1;
        col += 1;
    }

    if let Some((unclosed, l, c)) = stack.pop() {
        return Err(SmartFsError::SyntaxError(format!(
            "Unclosed delimiter '{unclosed}' opened at line {l}:{c}"
        )));
    }

    Ok(())
}

/// @id: 01be6f70-8192-4a34-4b5c-6d7e8f90a1b2
/// Extracts high-level AST constructs (functions, structs, classes, traits, impls) from code.
pub fn extract_ast_nodes(code: &str, lang: &str) -> Vec<AstNodeInsert> {
    let mut nodes = Vec::new();
    let lines: Vec<&str> = code.lines().collect();
    if lines.is_empty() {
        return nodes;
    }

    let is_rust = lang == "rust";
    let is_python = lang == "python";
    let is_js = lang == "javascript" || lang == "typescript";
    let is_go = lang == "go";

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();

        if is_rust {
            if let Some(name) = extract_decl_name(line, "fn ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("function", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "struct ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("struct", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "enum ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("enum", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "trait ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("trait", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "impl ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("impl", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            }
        }

        if is_python {
            if let Some(name) = extract_decl_name(line, "def ") {
                let (end, src) = extract_indent_block(&lines, i);
                nodes.push(make_ast_node("function", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "class ") {
                let (end, src) = extract_indent_block(&lines, i);
                nodes.push(make_ast_node("class", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            }
        }

        if is_js {
            if let Some(name) = extract_decl_name(line, "function ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("function", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "class ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("class", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            }
        }

        if is_go {
            if let Some(name) = extract_decl_name(line, "func ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("function", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            } else if let Some(name) = extract_decl_name(line, "type ") {
                let (end, src) = extract_brace_block(&lines, i);
                nodes.push(make_ast_node("type", name, i + 1, end + 1, src));
                i = end + 1;
                continue;
            }
        }

        i += 1;
    }

    nodes
}

/// @id: 12cf7081-92a3-4b45-5c6d-7e8f90a1b2c3
/// Runs syntax validation and AST extraction offloaded to `tokio::task::spawn_blocking`.
///
/// Returns `Err(SmartFsError::SyntaxError)` if syntax validation fails.
/// Returns `Ok(Vec<AstNodeInsert>)` with extracted nodes on success.
pub async fn validate_and_extract_ast_blocking(
    code: String,
    filename: String,
) -> Result<Vec<AstNodeInsert>> {
    tokio::task::spawn_blocking(move || {
        let lang = match detect_syntax_language(&filename) {
            Some(l) => l,
            None => return Ok(Vec::new()),
        };

        validate_syntax(&code, lang)?;
        let nodes = extract_ast_nodes(&code, lang);
        Ok(nodes)
    })
    .await
    .map_err(|e| SmartFsError::Other(format!("Syntax validation task join error: {e}")))?
}

fn extract_decl_name(line: &str, keyword: &str) -> Option<String> {
    let pos = line.find(keyword)?;
    let remainder = line[pos + keyword.len()..].trim();
    let ident: String = remainder
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if !ident.is_empty() {
        Some(ident)
    } else {
        None
    }
}

fn extract_brace_block(lines: &[&str], start_idx: usize) -> (usize, String) {
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

fn extract_indent_block(lines: &[&str], start_idx: usize) -> (usize, String) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_syntax_language() {
        assert_eq!(detect_syntax_language("main.rs"), Some("rust"));
        assert_eq!(detect_syntax_language("script.py"), Some("python"));
        assert_eq!(detect_syntax_language("index.js"), Some("javascript"));
        assert_eq!(detect_syntax_language("app.tsx"), Some("typescript"));
        assert_eq!(detect_syntax_language("server.go"), Some("go"));
        assert_eq!(detect_syntax_language("config.json"), Some("json"));
        assert_eq!(detect_syntax_language("notes.md"), None);
    }

    #[test]
    fn test_validate_valid_code() {
        let rust_code = r#"
            fn add(a: i32, b: i32) -> i32 {
                a + b
            }
        "#;
        assert!(validate_syntax(rust_code, "rust").is_ok());

        let python_code = r#"
            def add(a, b):
                return a + b
        "#;
        assert!(validate_syntax(python_code, "python").is_ok());

        let json_code = r#"{"name": "smartfs", "version": 6}"#;
        assert!(validate_syntax(json_code, "json").is_ok());
    }

    #[test]
    fn test_validate_syntax_error_broken_tokens() {
        // Unclosed paren before open brace: the exact example from §11.2
        let broken = "fn broken( {";
        let res = validate_syntax(broken, "rust");
        assert!(res.is_err(), "Expected syntax error on 'fn broken( {{'");

        // Unbalanced closing bracket
        let broken2 = "fn broken() { let x = [1, 2); }";
        let res2 = validate_syntax(broken2, "rust");
        assert!(res2.is_err());

        // Invalid JSON
        let bad_json = r#"{"key": unquoted_value}"#;
        let res3 = validate_syntax(bad_json, "json");
        assert!(res3.is_err());
    }

    #[test]
    fn test_extract_ast_nodes() {
        let rust_code = r#"
            fn calculate_sum(x: i32, y: i32) -> i32 {
                x + y
            }

            struct MyPoint {
                x: f64,
                y: f64,
            }
        "#;
        let nodes = extract_ast_nodes(rust_code, "rust");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].kind, "function");
        assert_eq!(nodes[0].name, "calculate_sum");
        assert_eq!(nodes[1].kind, "struct");
        assert_eq!(nodes[1].name, "MyPoint");
    }

    #[tokio::test]
    async fn test_validate_and_extract_ast_blocking() {
        let code = "fn test_async() -> bool { true }".to_string();
        let filename = "test.rs".to_string();
        let nodes = validate_and_extract_ast_blocking(code, filename)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "test_async");

        // Non-code file should pass cleanly with empty nodes
        let doc = "# Documentation\nAll good.".to_string();
        let doc_name = "README.md".to_string();
        let doc_nodes = validate_and_extract_ast_blocking(doc, doc_name)
            .await
            .unwrap();
        assert!(doc_nodes.is_empty());
    }
}
