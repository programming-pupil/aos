pub fn extract_first_json_object(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut end_idx: Option<usize> = None;
    for (offset, ch) in text[start..].char_indices() {
        if in_str {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                in_str = false;
            }
            continue;
        }
        if ch == '"' {
            in_str = true;
            continue;
        }
        if ch == '{' {
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                end_idx = Some(start + offset + ch.len_utf8());
                break;
            }
        }
    }
    let end = end_idx?;
    Some(text[start..end].to_string())
}

fn normalize_jsonish_text(input: &str) -> String {
    input
        .replace(['“', '”'], "\"")
        .replace(['‘', '’'], "'")
        .replace('：', ":")
}

fn strip_trailing_commas_in_jsonish(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut idx = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    while idx < chars.len() {
        let ch = chars[idx];
        if in_str {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            idx = idx.saturating_add(1);
            continue;
        }
        if ch == '"' {
            in_str = true;
            out.push(ch);
            idx = idx.saturating_add(1);
            continue;
        }
        if ch == ',' {
            let mut lookahead = idx.saturating_add(1);
            while lookahead < chars.len() && chars[lookahead].is_whitespace() {
                lookahead = lookahead.saturating_add(1);
            }
            if lookahead < chars.len() && matches!(chars[lookahead], '}' | ']') {
                idx = idx.saturating_add(1);
                continue;
            }
        }
        out.push(ch);
        idx = idx.saturating_add(1);
    }
    out
}

pub fn parse_json_object_relaxed(object: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(object) {
        return Some(value);
    }
    let normalized = normalize_jsonish_text(object);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&normalized) {
        return Some(value);
    }
    let stripped = strip_trailing_commas_in_jsonish(&normalized);
    serde_json::from_str::<serde_json::Value>(&stripped).ok()
}

fn extract_json_from_fenced_code_blocks(text: &str, name_upper: &str) -> Option<serde_json::Value> {
    let mut in_block = false;
    let mut block = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_block {
                let block_upper = block.to_ascii_uppercase();
                let looks_like_json = block.trim_start().starts_with('{');
                if block_upper.contains(name_upper) || looks_like_json {
                    if let Some(object) = extract_first_json_object(&block) {
                        if let Some(parsed) = parse_json_object_relaxed(&object) {
                            return Some(parsed);
                        }
                    }
                }
                block.clear();
                in_block = false;
            } else {
                in_block = true;
                block.clear();
            }
            continue;
        }
        if in_block {
            block.push_str(line);
            block.push('\n');
        }
    }
    None
}

pub fn extract_named_json_object(text: &str, name: &str) -> Option<serde_json::Value> {
    let name_upper = name.to_ascii_uppercase();
    for line in text.lines() {
        if !line.to_ascii_uppercase().contains(&name_upper) {
            continue;
        }
        if let Some(object) = extract_first_json_object(line) {
            if let Some(value) = parse_json_object_relaxed(&object) {
                return Some(value);
            }
        }
    }
    let text_upper = text.to_ascii_uppercase();
    if let Some(idx) = text_upper.find(&name_upper) {
        if let Some(object) = extract_first_json_object(&text[idx..]) {
            if let Some(value) = parse_json_object_relaxed(&object) {
                return Some(value);
            }
        }
    }
    extract_json_from_fenced_code_blocks(text, &name_upper)
}
