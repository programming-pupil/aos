//! Text matching helpers used by NL2SQL routing and domain selection.

pub fn normalize_domain_match_text(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c))
        .flat_map(char::to_lowercase)
        .collect()
}

pub fn question_mentions_domain(question: &str, domain_name: &str) -> bool {
    let q_raw = question.trim().to_ascii_lowercase();
    let d_raw = domain_name.trim().to_ascii_lowercase();
    if d_raw.is_empty() {
        return false;
    }
    if q_raw.contains(&d_raw) {
        return true;
    }
    let q_norm = normalize_domain_match_text(question);
    let d_norm = normalize_domain_match_text(domain_name);
    !d_norm.is_empty() && q_norm.contains(&d_norm)
}
