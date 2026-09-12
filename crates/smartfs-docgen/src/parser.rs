//! Source code parsing and AST item extraction for Rust source files.

use regex::Regex;
use uuid::Uuid;

use crate::types::SymbolKind;

/// @id: 471e8d82-f596-4c96-8402-d3adcb4c360d
/// Information extracted for an addressable Rust item in source code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedItem {
    /// The kind of item (Function, Struct, Enum, Trait, ImplBlock).
    pub kind: SymbolKind,
    /// Identifier or signature summary.
    pub name: String,
    /// 1-indexed line where the item definition begins.
    pub line: u32,
    /// Parsed UUID if `/// @id: <uuid>` was present in doc comments.
    pub id: Option<Uuid>,
    /// First non-empty, non-`@id:` documentation line.
    pub doc_summary: Option<String>,
    /// 0-indexed line index where doc comments begin (if any).
    pub doc_start_line: Option<usize>,
    /// 0-indexed line index where attributes begin (or item line if none).
    pub attr_start_line: usize,
    /// Leading whitespace indentation of the item / doc comments.
    pub indent: String,
}

/// @id: be01d327-5a7f-487c-95b2-c117b6da5b5d
/// Parses all addressable items from source file content lines.
pub fn parse_items(source: &str) -> Vec<ParsedItem> {
    let lines: Vec<&str> = source.lines().collect();
    let mut items = Vec::new();

    let re_fn = Regex::new(
        r#"^\s*pub(?:\([^)]+\))?\s+(?:(?:async|const|unsafe|extern(?:\s+"[^"]*")?)\s+)*fn\s+([a-zA-Z0-9_]+)"#,
    )
    .expect("valid regex");
    let re_struct = Regex::new(r"^\s*pub(?:\([^)]+\))?\s+struct\s+([a-zA-Z0-9_]+)")
        .expect("valid regex");
    let re_enum = Regex::new(r"^\s*pub(?:\([^)]+\))?\s+enum\s+([a-zA-Z0-9_]+)")
        .expect("valid regex");
    let re_trait = Regex::new(r"^\s*pub(?:\([^)]+\))?\s+(?:unsafe\s+)?trait\s+([a-zA-Z0-9_]+)")
        .expect("valid regex");
    let re_impl = Regex::new(r"^\s*(?:unsafe\s+)?impl\b").expect("valid regex");
    let re_id = Regex::new(r"///\s*@id:\s*([0-9a-fA-F-]{36})").expect("valid regex");

    let mut in_block_comment = false;
    let mut pending_cfg_test = false;
    let mut in_cfg_test = false;
    let mut cfg_test_depth: i32 = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }

        if trimmed.starts_with("/*") {
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            continue;
        }

        if in_cfg_test {
            for ch in line.chars() {
                if ch == '{' {
                    cfg_test_depth += 1;
                } else if ch == '}' {
                    cfg_test_depth -= 1;
                }
            }
            if cfg_test_depth <= 0 {
                in_cfg_test = false;
                cfg_test_depth = 0;
            }
            continue;
        }

        if trimmed.starts_with("#[cfg(test)]") {
            pending_cfg_test = true;
            continue;
        }

        if pending_cfg_test {
            if trimmed.starts_with("mod ") {
                in_cfg_test = true;
                pending_cfg_test = false;
                for ch in line.chars() {
                    if ch == '{' {
                        cfg_test_depth += 1;
                    } else if ch == '}' {
                        cfg_test_depth -= 1;
                    }
                }
                continue;
            } else if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with('#') {
                pending_cfg_test = false;
            }
        }

        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }

        let mut kind = None;
        let mut name = None;

        if let Some(caps) = re_fn.captures(line) {
            kind = Some(SymbolKind::Function);
            name = Some(caps[1].to_string());
        } else if let Some(caps) = re_struct.captures(line) {
            kind = Some(SymbolKind::Struct);
            name = Some(caps[1].to_string());
        } else if let Some(caps) = re_enum.captures(line) {
            kind = Some(SymbolKind::Enum);
            name = Some(caps[1].to_string());
        } else if let Some(caps) = re_trait.captures(line) {
            kind = Some(SymbolKind::Trait);
            name = Some(caps[1].to_string());
        } else if re_impl.is_match(line) {
            // Check that this is an impl block and not a parameter line
            let is_likely_impl_block = (trimmed.contains('{') || trimmed.contains("for") || trimmed.contains("where"))
                && !trimmed.ends_with(',')
                && !trimmed.ends_with(')');
            let next_has_brace = if !is_likely_impl_block && i + 1 < lines.len() {
                lines[i + 1].contains('{') || lines[i + 1].trim().starts_with("where")
            } else {
                false
            };

            if is_likely_impl_block || next_has_brace {
                kind = Some(SymbolKind::ImplBlock);
                name = Some(extract_impl_name(trimmed));
            }
        }

        if let (Some(kind), Some(name)) = (kind, name) {
            // Scan backwards to collect doc comments and attributes
            let mut k = i;
            let mut doc_lines: Vec<(usize, &str)> = Vec::new();
            let mut doc_start_line = None;
            let mut attr_start_line = i;
            let mut bracket_depth: i32 = 0;

            while k > 0 {
                k -= 1;
                let prev = lines[k].trim();

                if prev.is_empty() && bracket_depth <= 0 {
                    break;
                }

                for ch in prev.chars().rev() {
                    if ch == ']' {
                        bracket_depth += 1;
                    } else if ch == '[' {
                        bracket_depth -= 1;
                    }
                }

                if prev.starts_with("///") && bracket_depth <= 0 {
                    doc_lines.push((k, lines[k]));
                    doc_start_line = Some(k);
                    continue;
                }

                if (prev.starts_with("#[") || prev.starts_with("#![") || bracket_depth > 0)
                    && doc_start_line.is_none()
                {
                    attr_start_line = k;
                    continue;
                }

                if prev.starts_with("//") && !prev.starts_with("///") {
                    if doc_start_line.is_none() {
                        attr_start_line = k;
                    }
                    continue;
                }

                break;
            }

            doc_lines.reverse();

            // Extract UUID
            let mut id = None;
            for (_, doc_line) in &doc_lines {
                if let Some(caps) = re_id.captures(doc_line) {
                    if let Ok(parsed) = Uuid::parse_str(&caps[1]) {
                        id = Some(parsed);
                        break;
                    }
                }
            }

            // Extract first non-empty doc summary line that is not @id:
            let mut doc_summary = None;
            for (_, doc_line) in &doc_lines {
                let stripped = doc_line.trim();
                if let Some(content) = stripped.strip_prefix("///") {
                    let content = content.trim();
                    if !content.is_empty() && !content.starts_with("@id:") {
                        doc_summary = Some(content.to_string());
                        break;
                    }
                }
            }

            // Determine indentation
            let target_line = doc_start_line.unwrap_or(attr_start_line);
            let raw_target = lines[target_line];
            let indent: String = raw_target
                .chars()
                .take_while(|c| c.is_whitespace())
                .collect();

            items.push(ParsedItem {
                kind,
                name,
                line: (i + 1) as u32,
                id,
                doc_summary,
                doc_start_line,
                attr_start_line,
                indent,
            });
        }
    }

    items
}

/// Normalizes an `impl` statement line into a concise, readable symbol name.
fn extract_impl_name(line: &str) -> String {
    let s = line.trim();
    let s = s.strip_prefix("unsafe ").unwrap_or(s).trim();
    let s = s.strip_prefix("impl").unwrap_or(s).trim();

    // Remove leading generics if present on `impl<...>`
    let s = if s.starts_with('<') {
        if let Some(pos) = s.find('>') {
            s[pos + 1..].trim()
        } else {
            s
        }
    } else {
        s
    };

    // Remove trailing '{'
    let s = s.split('{').next().unwrap_or(s).trim();

    // Remove trailing 'where ...'
    let s = if let Some(idx) = s.find(" where") {
        s[..idx].trim()
    } else {
        s
    };

    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
