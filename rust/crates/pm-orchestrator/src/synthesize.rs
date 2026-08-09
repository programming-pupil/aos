pub fn default_final_sections(user_lang_hint: Option<&str>) -> [&'static str; 4] {
    match user_lang_hint
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "zh" | "zh-cn" | "zh-hans" => ["已证实", "待验证", "风险项", "建议动作"],
        _ => [
            "Confirmed",
            "Need Verification",
            "Risks",
            "Recommended Actions",
        ],
    }
}
