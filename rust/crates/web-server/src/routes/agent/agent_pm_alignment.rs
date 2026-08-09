use super::*;

fn pm_compact_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn contains_cjk_char(ch: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&ch)
        || ('\u{3400}'..='\u{4dbf}').contains(&ch)
        || ('\u{3040}'..='\u{30ff}').contains(&ch)
        || ('\u{ac00}'..='\u{d7af}').contains(&ch)
}

fn pm_is_url_dominant_text(text: &str) -> bool {
    let urls = extract_http_urls(text);
    if urls.is_empty() {
        return false;
    }
    let mut residue = text.to_string();
    for url in urls {
        residue = residue.replace(&url, " ");
    }
    let lexical_len = residue
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || contains_cjk_char(*ch))
        .count();
    lexical_len < 8
}

fn pm_is_claim_noise_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("prompt:")
        || lower.contains("runtime error")
        || lower.contains("runtime recovery failed")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("tool '")
        || lower.contains("webfetch")
        || lower.contains("low_claim_evidence_alignment")
        || lower.contains("insufficient_claim_evidence_url_triads")
        || lower.contains("missing_tool_retrieval")
        || lower.contains("contract_invalid:")
}

fn pm_clean_claim_text(raw: &str) -> Option<String> {
    let mut text = raw.trim();
    if text.is_empty() {
        return None;
    }
    text = text
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim_start_matches("• ");
    let cleaned = pm_compact_whitespace(text.trim_matches(|ch: char| {
        ch == '{' || ch == '}' || ch == '[' || ch == ']' || ch == '(' || ch == ')'
    }));
    if cleaned.is_empty() || pm_is_claim_noise_text(&cleaned) || pm_is_url_dominant_text(&cleaned) {
        return None;
    }
    if normalize_http_url_candidate(&cleaned).is_some() {
        return None;
    }
    Some(cleaned.chars().take(240).collect())
}

fn pm_clean_evidence_text(raw: &str, fallback: &str) -> String {
    let cleaned = pm_compact_whitespace(raw);
    if cleaned.is_empty() || pm_is_claim_noise_text(&cleaned) {
        return fallback.chars().take(320).collect();
    }
    cleaned.chars().take(320).collect()
}

fn parse_claim_alignment_json_line(line: &str) -> Option<PmClaimEvidenceDto> {
    let trimmed = line.trim().trim_end_matches(',');
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let claim = value
        .get("claim")
        .or_else(|| value.get("结论"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let Some(claim) = pm_clean_claim_text(claim) else {
        return None;
    };
    let evidence_excerpt = value
        .get("evidence")
        .or_else(|| value.get("evidenceExcerpt"))
        .or_else(|| value.get("证据"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let mut urls = Vec::new();
    if let Some(url) = value
        .get("url")
        .or_else(|| value.get("sourceUrl"))
        .or_else(|| value.get("链接"))
        .and_then(|v| v.as_str())
    {
        urls.extend(extract_http_urls(url));
    }
    if let Some(items) = value.get("urls").and_then(|v| v.as_array()) {
        for item in items {
            if let Some(url) = item.as_str() {
                urls.extend(extract_http_urls(url));
            }
        }
    }
    urls.sort();
    urls.dedup();
    let evidence_text = pm_clean_evidence_text(
        if evidence_excerpt.is_empty() {
            &claim
        } else {
            &evidence_excerpt
        },
        &claim,
    );
    Some(PmClaimEvidenceDto {
        claim,
        evidence_excerpt: evidence_text,
        cited: !urls.is_empty(),
        urls,
    })
}

fn parse_claim_alignment_inline_line(line: &str) -> Option<PmClaimEvidenceDto> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parts = trimmed
        .trim_start_matches("- ")
        .trim_start_matches("* ")
        .trim_start_matches("• ")
        .split('|')
        .map(str::trim)
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    let mut claim = String::new();
    let mut evidence = String::new();
    let mut urls: Vec<String> = Vec::new();
    for part in &parts {
        let lower = part.to_ascii_lowercase();
        if lower.starts_with("claim:")
            || part.starts_with("结论:")
            || part.starts_with("观点:")
            || part.starts_with("事实:")
        {
            claim = part
                .split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
        } else if lower.starts_with("evidence:")
            || part.starts_with("证据:")
            || part.starts_with("依据:")
        {
            evidence = part
                .split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
        } else if lower.starts_with("url:")
            || lower.starts_with("source:")
            || part.starts_with("链接:")
        {
            let rhs = part
                .split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default();
            urls.extend(extract_http_urls(&rhs));
        }
    }
    if claim.is_empty() {
        let likely_claim = parts.first().copied().unwrap_or("");
        if likely_claim.contains("http://") || likely_claim.contains("https://") {
            return None;
        }
        claim = likely_claim.to_string();
    }
    let Some(claim) = pm_clean_claim_text(&claim) else {
        return None;
    };
    if urls.is_empty() {
        urls.extend(extract_http_urls(trimmed));
    }
    urls.sort();
    urls.dedup();
    let evidence_excerpt = if evidence.is_empty() {
        pm_clean_evidence_text(trimmed, &claim)
    } else {
        pm_clean_evidence_text(&evidence, &claim)
    };
    Some(PmClaimEvidenceDto {
        claim,
        evidence_excerpt,
        cited: !urls.is_empty(),
        urls,
    })
}

fn extract_claim_alignment_table(text: &str) -> Vec<PmClaimEvidenceDto> {
    let mut rows = Vec::new();
    let mut in_alignment_section = false;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.contains("claim-evidence alignment")
            || lower.contains("claim evidence alignment")
            || line.contains("结论-证据对齐")
            || line.contains("结论证据对齐")
        {
            in_alignment_section = true;
            continue;
        }
        if !in_alignment_section {
            continue;
        }
        if line.starts_with('#') {
            break;
        }
        if !line.contains('|') {
            continue;
        }
        let cells = parse_markdown_table_cells(line);
        if cells.len() < 3 || is_markdown_table_separator(&cells) {
            continue;
        }
        let header0 = cells[0].to_ascii_lowercase();
        if header0.contains("claim") || cells[0].contains("结论") {
            continue;
        }
        let Some(claim) = pm_clean_claim_text(cells[0].trim()) else {
            continue;
        };
        let evidence = cells.get(1).map_or("", String::as_str).trim();
        let mut urls = cells
            .get(2)
            .map(|c| extract_http_urls(c))
            .unwrap_or_default();
        if urls.is_empty() {
            urls.extend(extract_http_urls(line));
        }
        urls.sort();
        urls.dedup();
        rows.push(PmClaimEvidenceDto {
            claim: claim.clone(),
            evidence_excerpt: pm_clean_evidence_text(evidence, &claim),
            cited: !urls.is_empty(),
            urls,
        });
        if rows.len() >= 24 {
            break;
        }
    }
    rows
}

pub(super) fn extract_claim_alignment(text: &str) -> Vec<PmClaimEvidenceDto> {
    let mut out = Vec::new();

    // 1) Structured JSON lines produced by machine-readable prompts.
    for line in text.lines().map(str::trim) {
        if let Some(row) = parse_claim_alignment_json_line(line) {
            out.push(row);
        }
    }

    // 2) Markdown table rows from explicit alignment section.
    out.extend(extract_claim_alignment_table(text));

    // 3) Inline claim/evidence/url lines.
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let is_bullet = line.starts_with("- ") || line.starts_with("* ") || line.starts_with("• ");
        let is_claim_line = line.contains("FACT")
            || line.contains("HYPOTHESIS")
            || line.contains("RECOMMENDATION")
            || line.to_ascii_lowercase().contains("claim:")
            || (is_bullet && !extract_http_urls(line).is_empty());
        if !is_claim_line {
            continue;
        }
        if let Some(row) = parse_claim_alignment_inline_line(line) {
            out.push(row);
        }
    }

    // 4) Fallback: bullet lines with URL only.
    if out.is_empty() {
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let is_bullet =
                line.starts_with("- ") || line.starts_with("* ") || line.starts_with("• ");
            let is_claim_line = line.contains("FACT")
                || line.contains("HYPOTHESIS")
                || line.contains("RECOMMENDATION")
                || (is_bullet && !extract_http_urls(line).is_empty());
            if !is_claim_line {
                continue;
            }
            let urls = extract_http_urls(line);
            let Some(claim) = pm_clean_claim_text(line) else {
                continue;
            };
            out.push(PmClaimEvidenceDto {
                evidence_excerpt: pm_clean_evidence_text(line, &claim),
                claim,
                cited: !urls.is_empty(),
                urls,
            });
            if out.len() >= 16 {
                break;
            }
        }
    }

    let mut seen = std::collections::BTreeSet::<String>::new();
    let mut deduped = Vec::new();
    for row in out {
        let key = format!(
            "{}|{}",
            normalize_claim_key(&row.claim),
            row.urls.first().map_or("", String::as_str)
        );
        if key.trim_matches('|').is_empty() || !seen.insert(key) {
            continue;
        }
        deduped.push(row);
        if deduped.len() >= 24 {
            break;
        }
    }
    deduped
}

fn parse_markdown_table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn is_markdown_table_separator(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let compact = cell.replace(' ', "");
            !compact.is_empty() && compact.chars().all(|ch| ch == '-' || ch == ':')
        })
}

pub(super) fn extract_conflict_matrix(text: &str) -> Vec<PmConflictRowDto> {
    let mut rows = Vec::new();
    let mut in_conflict_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("conflict matrix") || trimmed.contains("冲突矩阵") {
            in_conflict_section = true;
            continue;
        }
        if !in_conflict_section {
            continue;
        }
        if trimmed.starts_with('#') {
            break;
        }
        if trimmed.contains('|') {
            let cells = parse_markdown_table_cells(trimmed);
            if cells.len() < 6 || is_markdown_table_separator(&cells) {
                continue;
            }
            // Skip header row.
            let header = cells[0].to_ascii_lowercase();
            if header.contains("topic") || cells[0].contains("主题") {
                continue;
            }
            rows.push(PmConflictRowDto {
                topic: cells[0].clone(),
                source_a: cells[1].clone(),
                claim_a: cells[2].clone(),
                source_b: cells[3].clone(),
                claim_b: cells[4].clone(),
                verdict: cells[5].clone(),
            });
            if rows.len() >= 12 {
                break;
            }
            continue;
        }
        // Bullet fallback format:
        // - Topic: X | A: srcA claim ... | B: srcB claim ... | Verdict: ...
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("• ") {
            let segments = trimmed
                .trim_start_matches("- ")
                .trim_start_matches("* ")
                .trim_start_matches("• ")
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>();
            let mut topic = String::new();
            let mut source_a = String::new();
            let mut claim_a = String::new();
            let mut source_b = String::new();
            let mut claim_b = String::new();
            let mut verdict = String::new();
            for seg in &segments {
                let lower = seg.to_ascii_lowercase();
                if lower.starts_with("topic:") || seg.starts_with("主题:") {
                    topic = seg
                        .split_once(':')
                        .map(|(_, v)| v.trim().to_string())
                        .unwrap_or_default();
                } else if lower.starts_with("a:") || lower.starts_with("source a:") {
                    let body = seg
                        .split_once(':')
                        .map(|(_, v)| v.trim().to_string())
                        .unwrap_or_default();
                    source_a = body.chars().take(48).collect();
                    claim_a = body;
                } else if lower.starts_with("b:") || lower.starts_with("source b:") {
                    let body = seg
                        .split_once(':')
                        .map(|(_, v)| v.trim().to_string())
                        .unwrap_or_default();
                    source_b = body.chars().take(48).collect();
                    claim_b = body;
                } else if lower.starts_with("verdict:") || seg.starts_with("裁决:") {
                    verdict = seg
                        .split_once(':')
                        .map(|(_, v)| v.trim().to_string())
                        .unwrap_or_default();
                }
            }
            if !claim_a.is_empty() || !claim_b.is_empty() || !verdict.is_empty() {
                rows.push(PmConflictRowDto {
                    topic,
                    source_a,
                    claim_a,
                    source_b,
                    claim_b,
                    verdict,
                });
                if rows.len() >= 12 {
                    break;
                }
            }
        }
    }
    rows
}

fn classify_conflict_relation(verdict: &str) -> &'static str {
    let lower = verdict.to_ascii_lowercase();
    if lower.contains("agree")
        || lower.contains("consistent")
        || lower.contains("support")
        || verdict.contains("一致")
        || verdict.contains("支持")
    {
        return "corroborates";
    }
    if lower.contains("conflict")
        || lower.contains("disagree")
        || lower.contains("contradict")
        || lower.contains("refute")
        || verdict.contains("矛盾")
        || verdict.contains("冲突")
    {
        return "contradicts";
    }
    "unresolved"
}

fn collect_claim_match_urls(
    claim_alignment: &[PmClaimEvidenceDto],
    claim_text: &str,
) -> Vec<String> {
    let normalized = normalize_claim_key(claim_text);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for row in claim_alignment {
        let row_claim = normalize_claim_key(&row.claim);
        if row_claim.is_empty() {
            continue;
        }
        if row_claim.contains(&normalized)
            || normalized.contains(&row_claim)
            || row
                .evidence_excerpt
                .to_ascii_lowercase()
                .contains(&normalized)
        {
            out.extend(row.urls.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

pub(super) fn build_pm_conflict_graph(
    conflict_matrix: &[PmConflictRowDto],
    claim_alignment: &[PmClaimEvidenceDto],
) -> PmConflictGraphDto {
    let mut edges = Vec::new();
    let mut topics = std::collections::BTreeSet::new();
    let mut confidence_sum = 0.0f64;
    let mut unresolved = 0usize;
    let mut adjudicated = 0usize;

    for row in conflict_matrix.iter().take(16) {
        let topic = if row.topic.trim().is_empty() {
            "general".to_string()
        } else {
            row.topic.clone()
        };
        topics.insert(topic.clone());
        let relation = classify_conflict_relation(&row.verdict).to_string();
        let mut urls = collect_claim_match_urls(claim_alignment, &row.claim_a);
        urls.extend(collect_claim_match_urls(claim_alignment, &row.claim_b));
        urls.sort();
        urls.dedup();
        let mut confidence: f64 = if relation == "unresolved" { 0.45 } else { 0.72 };
        if !row.source_a.trim().is_empty() && !row.source_b.trim().is_empty() {
            confidence += 0.08;
        }
        if urls.len() >= 2 {
            confidence += 0.10;
        }
        confidence = confidence.clamp(0.0, 0.95);
        if relation == "unresolved" {
            unresolved += 1;
        } else {
            adjudicated += 1;
        }
        confidence_sum += confidence;
        edges.push(PmConflictEdgeDto {
            topic,
            source_left: row.source_a.clone(),
            source_right: row.source_b.clone(),
            relation,
            verdict: row.verdict.clone(),
            confidence,
            urls,
        });
    }

    let edge_count = edges.len();
    let avg_confidence = if edge_count == 0 {
        0.0
    } else {
        (confidence_sum / edge_count as f64).clamp(0.0, 1.0)
    };
    PmConflictGraphDto {
        topic_count: topics.len(),
        edge_count,
        adjudicated_count: adjudicated,
        unresolved_count: unresolved,
        avg_confidence,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_claim_alignment_skips_url_only_noise_rows() {
        let text = r#"
Claim-Evidence Alignment
| Claim | Evidence Excerpt | URL |
| --- | --- | --- |
| {(https://dataportal.com/reports/digital-2025-indonesia\nPrompt:)} | {(https://dataportal.com/reports/digital-2025-indonesia\nPrompt:)} | {(https://dataportal.com/reports/digital-2025-indonesia\nPrompt:)} |
| 印尼移动游戏收入持续增长 | DataReportal + IMARC show upward trend | https://dataportal.com/reports/digital-2025-indonesia |
"#;
        let rows = extract_claim_alignment(text);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].claim, "印尼移动游戏收入持续增长");
        assert_eq!(
            rows[0].urls,
            vec!["https://dataportal.com/reports/digital-2025-indonesia".to_string()]
        );
    }

    #[test]
    fn natural_action_bullets_are_not_misclassified_as_factual_claims() {
        let text = r#"
- 支持暂停、恢复和取消。
- 为副作用操作增加幂等键。
- 官方文档说明队列与 worker 可以独立扩缩容：https://docs.example.com/runtime
"#;
        let rows = extract_claim_alignment(text);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].cited);
        assert_eq!(rows[0].urls, vec!["https://docs.example.com/runtime"]);
    }
}
