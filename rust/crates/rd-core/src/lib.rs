//! Pure repository analysis primitives for AOS Code Studio.
//!
//! Keep this crate independent from HTTP, database, auth, and runtime state so
//! language detection and repository indexing can evolve without bloating the
//! `web-server` route layer.

pub mod command_safety;
pub mod context_planner;
pub mod context_profile;
pub mod diff;
pub mod review;
pub mod runtime_tools;
pub mod text;

pub use context_profile::{
    contains_any, is_deep_review_prompt, is_overview_prompt, RdContextBudget, RdContextProfile,
};

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use walkdir::WalkDir;

pub const MAX_FILE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageStat {
    pub language: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryDetection {
    pub primary_language: Option<String>,
    pub languages: Vec<LanguageStat>,
    pub stack: Vec<String>,
    pub package_manager: Option<String>,
    pub detected_test_command: Option<String>,
    pub detected_build_command: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RepositorySymbol {
    pub file_path: String,
    pub language: Option<String>,
    pub symbol_name: String,
    pub symbol_kind: String,
    pub signature: Option<String>,
    pub line_number: u64,
}

#[derive(Debug, Clone)]
pub struct RepositoryImport {
    pub file_path: String,
    pub language: Option<String>,
    pub import_path: String,
    pub import_kind: String,
    pub line_number: u64,
}

pub fn count_repository_files(root: &Path) -> i32 {
    let count = collect_flat_tree(root, 10_000)
        .into_iter()
        .filter(|rel| root.join(rel).is_file())
        .count();
    i32::try_from(count).unwrap_or(i32::MAX)
}

pub fn collect_repository_symbols(root: &Path, limit: usize) -> Vec<RepositorySymbol> {
    let mut symbols = Vec::new();
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !should_skip_path(entry.path()));
    for entry in walker.filter_map(Result::ok) {
        if symbols.len() >= limit {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Some(language) = language_for_path(path) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for (idx, line) in content.lines().enumerate() {
            if symbols.len() >= limit {
                break;
            }
            if let Some((symbol_kind, symbol_name)) = detect_symbol(line) {
                symbols.push(RepositorySymbol {
                    file_path: rel.clone(),
                    language: Some(language.clone()),
                    symbol_name,
                    symbol_kind,
                    signature: Some(truncate_text(line.trim(), 500)),
                    line_number: u64::try_from(idx + 1).unwrap_or(0),
                });
            }
        }
    }
    symbols
}

pub fn collect_repository_imports(root: &Path, limit: usize) -> Vec<RepositoryImport> {
    let mut imports = Vec::new();
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !should_skip_path(entry.path()));
    for entry in walker.filter_map(Result::ok) {
        if imports.len() >= limit {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.len() > MAX_FILE_BYTES {
            continue;
        }
        let Some(language) = language_for_path(path) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let mut go_import_block = false;
        for (idx, line) in content.lines().enumerate() {
            if imports.len() >= limit {
                break;
            }
            let detected = if language == "go" {
                detect_go_import(line, &mut go_import_block)
            } else {
                detect_import_for_language(&language, line)
            };
            if let Some((import_kind, import_path)) = detected {
                imports.push(RepositoryImport {
                    file_path: rel.clone(),
                    language: Some(language.clone()),
                    import_path,
                    import_kind,
                    line_number: u64::try_from(idx + 1).unwrap_or(0),
                });
            }
        }
    }
    imports
}

pub fn detect_repository_profile(root: &Path) -> RepositoryDetection {
    let languages = detect_repository_languages(root, 20);
    let primary_language = languages.first().map(|item| item.language.clone());
    let mut stack = BTreeSet::new();
    let mut package_manager = None;
    let mut detected_test_command = None;
    let mut detected_build_command = None;

    if path_exists(root, "Cargo.toml") {
        stack.insert("Rust".to_string());
        detected_test_command.get_or_insert_with(|| "cargo test --workspace".to_string());
        detected_build_command.get_or_insert_with(|| "cargo build --workspace".to_string());
    }

    if path_exists(root, "go.mod") {
        stack.insert("Go".to_string());
        detected_test_command.get_or_insert_with(|| "go test ./...".to_string());
        detected_build_command.get_or_insert_with(|| "go build ./...".to_string());
    }

    if path_exists(root, "pom.xml") {
        stack.insert("Java".to_string());
        stack.insert("Maven".to_string());
        if repo_file_contains(root, "pom.xml", "spring-boot") {
            stack.insert("Spring Boot".to_string());
        }
        let mvn = if path_exists(root, "mvnw") {
            "./mvnw"
        } else {
            "mvn"
        };
        detected_test_command.get_or_insert_with(|| format!("{mvn} test"));
        detected_build_command.get_or_insert_with(|| format!("{mvn} package -DskipTests"));
    }

    if path_exists(root, "build.gradle")
        || path_exists(root, "build.gradle.kts")
        || path_exists(root, "settings.gradle")
        || path_exists(root, "settings.gradle.kts")
    {
        stack.insert("Java".to_string());
        stack.insert("Gradle".to_string());
        if repo_file_contains(root, "build.gradle", "org.springframework.boot")
            || repo_file_contains(root, "build.gradle.kts", "org.springframework.boot")
        {
            stack.insert("Spring Boot".to_string());
        }
        let gradle = if path_exists(root, "gradlew") {
            "./gradlew"
        } else {
            "gradle"
        };
        detected_test_command.get_or_insert_with(|| format!("{gradle} test"));
        detected_build_command.get_or_insert_with(|| format!("{gradle} build -x test"));
    }

    if path_exists(root, "pyproject.toml")
        || path_exists(root, "requirements.txt")
        || path_exists(root, "pytest.ini")
    {
        stack.insert("Python".to_string());
        if path_exists(root, "pytest.ini") || repo_file_contains(root, "pyproject.toml", "pytest") {
            detected_test_command.get_or_insert_with(|| "python -m pytest".to_string());
        }
    }

    if let Some(package) = read_package_json(root) {
        stack.insert("Node.js".to_string());
        let manager = detect_package_manager(root);
        package_manager = Some(manager.clone());
        for framework in detect_package_frameworks(&package) {
            stack.insert(framework);
        }
        if let Some(command) = package_script_command(&package, &manager, "test") {
            detected_test_command = Some(command);
        }
        if let Some(command) = package_script_command(&package, &manager, "build") {
            detected_build_command = Some(command);
        }
    }

    RepositoryDetection {
        primary_language,
        languages,
        stack: stack.into_iter().collect(),
        package_manager,
        detected_test_command,
        detected_build_command,
    }
}

pub fn language_for_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "rs" => "rust",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            "vue" => "vue",
            "svelte" => "svelte",
            "html" | "htm" => "html",
            "css" => "css",
            "scss" => "scss",
            "less" => "less",
            "json" => "json",
            "md" => "markdown",
            "py" => "python",
            "go" => "go",
            "java" => "java",
            "sql" => "sql",
            "toml" => "toml",
            "yaml" | "yml" => "yaml",
            _ => return None,
        }
        .to_string(),
    )
}

pub fn should_skip_path(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(part) = component else {
            return false;
        };
        matches!(
            part.to_string_lossy().as_ref(),
            ".git"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".next"
                | ".turbo"
                | ".cache"
                | "vendor"
                | ".aosd-agents"
                | ".sandbox-tmp"
                | ".sandbox-home"
        )
    })
}

pub fn detect_symbol(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('#')
        || trimmed.starts_with('*')
    {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    if [
        "if ", "for ", "while ", "switch ", "catch ", "return ", "throw ", "new ", "else ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return None;
    }
    static SYMBOL_PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    let patterns = SYMBOL_PATTERNS.get_or_init(|| {
        [
            (
                r"^(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\b",
                "function",
            ),
            (
                r"^(?:export\s+(?:default\s+)?)?(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)\b",
                "function",
            ),
            (
                r"^(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*(?::[^=]+)?=",
                "binding",
            ),
            (r"^def\s+([A-Za-z_][A-Za-z0-9_]*)\b", "function"),
            (
                r"^func\s+(?:\([^)]+\)\s*)?([A-Za-z_][A-Za-z0-9_]*)\b",
                "function",
            ),
            (
                r"^(?:export\s+(?:default\s+)?)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)\b",
                "class",
            ),
            (
                r"^(?:pub\s+)?(?:struct|enum|trait)\s+([A-Za-z_][A-Za-z0-9_]*)\b",
                "type",
            ),
            (
                r"^(?:export\s+)?(?:interface|type)\s+([A-Za-z_$][A-Za-z0-9_$]*)\b",
                "type",
            ),
            (
                r"^(?:(?:public|private|protected|abstract|final|static|sealed|non-sealed|strictfp)\s+)*class\s+([A-Za-z_$][A-Za-z0-9_$]*)\b",
                "class",
            ),
            (
                r"^(?:(?:public|private|protected|abstract|final|static|sealed|non-sealed|strictfp)\s+)*(?:interface|enum|record)\s+([A-Za-z_$][A-Za-z0-9_$]*)\b",
                "type",
            ),
            (
                r"^(?:(?:public|private|protected|static|final|abstract|synchronized|native|strictfp|default)\s+)+(?:<[^>]+>\s+)?[A-Za-z_$][A-Za-z0-9_$.]*(?:\s*<[^;{}()=]+>)?(?:\s*\[\s*\])*\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
                "function",
            ),
            (
                r"^(?:<[^>]+>\s+)?(?:void|boolean|byte|short|int|long|float|double|char|[A-Z][A-Za-z0-9_$.]*(?:\s*<[^;{}()=]+>)?)(?:\s*\[\s*\])*\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*\(",
                "function",
            ),
        ]
        .into_iter()
        .map(|(pattern, kind)| {
            (
                regex::Regex::new(pattern).expect("symbol regex should compile"),
                kind,
            )
        })
        .collect()
    });
    for (regex, kind) in patterns {
        if let Some(captures) = regex.captures(trimmed) {
            if let Some(name) = captures.get(1).map(|value| value.as_str()) {
                return Some(((*kind).to_string(), name.to_string()));
            }
        }
    }
    None
}

pub fn detect_import(line: &str) -> Option<(String, String)> {
    detect_import_for_language("unknown", line)
}

pub fn detect_import_for_language(language: &str, line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('#')
        || trimmed.starts_with('*')
    {
        return None;
    }
    static IMPORT_PATTERNS: OnceLock<Vec<(regex::Regex, &'static str, &'static [&'static str])>> =
        OnceLock::new();
    let patterns = IMPORT_PATTERNS.get_or_init(|| {
        [
            (
                r#"^import\s+(?:type\s+)?(?:[^'"]+\s+from\s+)?['"]([^'"]+)['"]"#,
                "import",
                &["typescript", "javascript", "vue", "svelte", "unknown"][..],
            ),
            (
                r#"^export\s+[^'"]+\s+from\s+['"]([^'"]+)['"]"#,
                "export",
                &["typescript", "javascript", "vue", "svelte", "unknown"][..],
            ),
            (
                r#"^(?:const|let|var)\s+[A-Za-z_$][A-Za-z0-9_$]*\s*=\s*require\(['"]([^'"]+)['"]\)"#,
                "require",
                &["typescript", "javascript", "vue", "svelte", "unknown"][..],
            ),
            (
                r"^import\s+(?:static\s+)?([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$*][A-Za-z0-9_$*]*)*);",
                "import",
                &["java", "unknown"][..],
            ),
            (r"^(?:pub\s+)?use\s+([^;]+);", "use", &["rust", "unknown"][..]),
            (
                r#"^import\s+(?:[._A-Za-z][A-Za-z0-9_]*\s+)?["]([^"]+)["]"#,
                "import",
                &["go", "unknown"][..],
            ),
            (
                r"^from\s+([A-Za-z0-9_\.]+)\s+import\b",
                "from",
                &["python", "unknown"][..],
            ),
            (
                r"^import\s+([A-Za-z0-9_\.]+)\b",
                "import",
                &["python", "unknown"][..],
            ),
            (
                r#"^@import\s+(?:url\()?['"]?([^'")\s;]+)"#,
                "import",
                &["css", "scss", "less", "unknown"][..],
            ),
        ]
        .into_iter()
        .map(|(pattern, kind, languages)| {
            (
                regex::Regex::new(pattern).expect("import regex should compile"),
                kind,
                languages,
            )
        })
        .collect()
    });
    for (regex, kind, languages) in patterns {
        if !languages.contains(&language) {
            continue;
        }
        if let Some(captures) = regex.captures(trimmed) {
            if let Some(path) = captures.get(1).map(|value| value.as_str().trim()) {
                if is_plausible_import_path(language, path) {
                    return Some(((*kind).to_string(), path.to_string()));
                }
            }
        }
    }
    None
}

fn detect_go_import(line: &str, in_import_block: &mut bool) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
        return None;
    }
    if trimmed == ")" {
        *in_import_block = false;
        return None;
    }
    if trimmed == "import (" {
        *in_import_block = true;
        return None;
    }
    if let Some(import) = detect_import_for_language("go", trimmed) {
        return Some(import);
    }
    if !*in_import_block {
        return None;
    }
    static GO_BLOCK_IMPORT_PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let regex = GO_BLOCK_IMPORT_PATTERN.get_or_init(|| {
        regex::Regex::new(r#"^(?:[._A-Za-z][A-Za-z0-9_]*\s+)?["]([^"]+)["]"#)
            .expect("go import block regex should compile")
    });
    let path = regex
        .captures(trimmed)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str().trim())?;
    if is_plausible_import_path("go", path) {
        Some(("import".to_string(), path.to_string()))
    } else {
        None
    }
}

pub fn is_plausible_import_path(language: &str, path: &str) -> bool {
    let value = path.trim();
    if value.is_empty() || value.len() > 500 {
        return false;
    }
    if value.contains('\\')
        || value.contains("\\n")
        || value.contains("\\r")
        || value.chars().any(char::is_control)
    {
        return false;
    }
    if !value
        .chars()
        .any(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '@')
    {
        return false;
    }
    if language != "rust"
        && value
            .chars()
            .any(|ch| matches!(ch, '{' | '}' | '[' | ']' | ',' | ';' | '"' | '\''))
    {
        return false;
    }
    if language == "java" {
        static JAVA_IMPORT_PATH: OnceLock<regex::Regex> = OnceLock::new();
        let regex = JAVA_IMPORT_PATH.get_or_init(|| {
            regex::Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$*][A-Za-z0-9_$*]*)+$")
                .expect("java import validation regex should compile")
        });
        return regex.is_match(value);
    }
    true
}

fn detect_repository_languages(root: &Path, limit: usize) -> Vec<LanguageStat> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let walker = WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| !should_skip_path(entry.path()));
    for entry in walker.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(language) = language_for_path(path) else {
            continue;
        };
        if !is_code_language(&language) {
            continue;
        }
        *counts.entry(language).or_default() += 1;
    }
    let mut languages = counts
        .into_iter()
        .map(|(language, file_count)| LanguageStat {
            language,
            file_count,
        })
        .collect::<Vec<_>>();
    languages.sort_by(|a, b| {
        b.file_count
            .cmp(&a.file_count)
            .then_with(|| a.language.cmp(&b.language))
    });
    languages.truncate(limit);
    languages
}

fn is_code_language(language: &str) -> bool {
    matches!(
        language,
        "rust"
            | "typescript"
            | "javascript"
            | "vue"
            | "svelte"
            | "java"
            | "python"
            | "go"
            | "sql"
            | "html"
            | "css"
            | "scss"
            | "less"
    )
}

fn path_exists(root: &Path, rel: &str) -> bool {
    root.join(rel).exists()
}

fn repo_file_contains(root: &Path, rel: &str, needle: &str) -> bool {
    std::fs::read_to_string(root.join(rel))
        .map(|content| content.contains(needle))
        .unwrap_or(false)
}

fn read_package_json(root: &Path) -> Option<Value> {
    let path = root.join("package.json");
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn detect_package_manager(root: &Path) -> String {
    if path_exists(root, "pnpm-lock.yaml") {
        "pnpm".to_string()
    } else if path_exists(root, "yarn.lock") {
        "yarn".to_string()
    } else if path_exists(root, "bun.lockb") || path_exists(root, "bun.lock") {
        "bun".to_string()
    } else {
        "npm".to_string()
    }
}

fn package_script_command(package: &Value, manager: &str, script: &str) -> Option<String> {
    let script_body = package.get("scripts")?.get(script)?.as_str()?.trim();
    if script_body.is_empty()
        || script_body.contains("no test specified")
        || script_body.contains("exit 1")
    {
        return None;
    }
    Some(match (manager, script) {
        ("npm", "test") => "npm test".to_string(),
        ("npm", _) => format!("npm run {script}"),
        ("bun", _) => format!("bun run {script}"),
        _ => format!("{manager} {script}"),
    })
}

fn detect_package_frameworks(package: &Value) -> Vec<String> {
    let mut deps = BTreeSet::new();
    for section in ["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(map) = package.get(section).and_then(Value::as_object) {
            deps.extend(map.keys().map(|name| name.to_ascii_lowercase()));
        }
    }
    let mut frameworks = Vec::new();
    for (dep, label) in [
        ("next", "Next.js"),
        ("vite", "Vite"),
        ("react", "React"),
        ("vue", "Vue"),
        ("svelte", "Svelte"),
        ("typescript", "TypeScript"),
        ("vitest", "Vitest"),
        ("jest", "Jest"),
        ("playwright", "Playwright"),
        ("eslint", "ESLint"),
    ] {
        if deps.contains(dep) {
            frameworks.push(label.to_string());
        }
    }
    frameworks
}

pub fn collect_flat_tree(root: &Path, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    collect_flat_tree_inner(root, root, limit, &mut out);
    out
}

fn collect_flat_tree_inner(root: &Path, dir: &Path, limit: usize, out: &mut Vec<String>) {
    if out.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if out.len() >= limit || should_skip_path(&entry.path()) {
            continue;
        }
        let path = entry.path();
        if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
        if path.is_dir() {
            collect_flat_tree_inner(root, &path, limit, out);
        }
    }
}

fn truncate_text(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    value.chars().take(max).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_symbol_extracts_common_language_symbols() {
        assert_eq!(
            detect_symbol("pub async fn run_task() -> Result<()>"),
            Some(("function".to_string(), "run_task".to_string()))
        );
        assert_eq!(
            detect_symbol("export class AgentRunner {"),
            Some(("class".to_string(), "AgentRunner".to_string()))
        );
        assert_eq!(
            detect_symbol("def build_prompt(value):"),
            Some(("function".to_string(), "build_prompt".to_string()))
        );
        assert_eq!(
            detect_symbol("public class UserService {"),
            Some(("class".to_string(), "UserService".to_string()))
        );
        assert_eq!(
            detect_symbol("sealed interface PaymentCommand permits CreatePayment {"),
            Some(("type".to_string(), "PaymentCommand".to_string()))
        );
        assert_eq!(
            detect_symbol("public record UserDto(String id, String name) {}"),
            Some(("type".to_string(), "UserDto".to_string()))
        );
        assert_eq!(
            detect_symbol("private static Optional<UserDto> buildUserDto(User user) {"),
            Some(("function".to_string(), "buildUserDto".to_string()))
        );
        assert_eq!(
            detect_symbol("CompletableFuture<List<UserDto>> loadUsers() {"),
            Some(("function".to_string(), "loadUsers".to_string()))
        );
        assert_eq!(
            detect_symbol("export default function CampaignCard() {"),
            Some(("function".to_string(), "CampaignCard".to_string()))
        );
        assert_eq!(
            detect_symbol("const CampaignCard: React.FC<Props> = ({ item }) => {"),
            Some(("binding".to_string(), "CampaignCard".to_string()))
        );
        assert_eq!(
            detect_symbol("export const CampaignCard = memo(function CampaignCardInner() {"),
            Some(("binding".to_string(), "CampaignCard".to_string()))
        );
        assert_eq!(
            detect_symbol(
                "export const Input = forwardRef<HTMLInputElement, Props>((props, ref) => {"
            ),
            Some(("binding".to_string(), "Input".to_string()))
        );
    }

    #[test]
    fn detect_symbol_ignores_comments() {
        assert_eq!(detect_symbol("// fn fake() {}"), None);
        assert_eq!(detect_symbol("/* public class Fake {} */"), None);
        assert_eq!(detect_symbol("# def fake(): pass"), None);
        assert_eq!(detect_symbol("return buildUserDto(user);"), None);
        assert_eq!(detect_symbol("if (ready) {"), None);
    }

    #[test]
    fn detect_import_extracts_common_language_imports() {
        assert_eq!(
            detect_import("import React from 'react';"),
            Some(("import".to_string(), "react".to_string()))
        );
        assert_eq!(
            detect_import("export { Button } from \"@/components/Button\";"),
            Some(("export".to_string(), "@/components/Button".to_string()))
        );
        assert_eq!(
            detect_import("const fs = require('node:fs');"),
            Some(("require".to_string(), "node:fs".to_string()))
        );
        assert_eq!(
            detect_import("import java.util.List;"),
            Some(("import".to_string(), "java.util.List".to_string()))
        );
        assert_eq!(
            detect_import("use crate::routes::rd;"),
            Some(("use".to_string(), "crate::routes::rd".to_string()))
        );
        assert_eq!(
            detect_import("from pathlib import Path"),
            Some(("from".to_string(), "pathlib".to_string()))
        );
    }

    #[test]
    fn implausible_import_paths_are_rejected() {
        for value in [r"],\n", r"{\n", "}", "{", "]", "],", ";\n"] {
            assert!(!is_plausible_import_path("java", value), "{value}");
            assert!(!is_plausible_import_path("unknown", value), "{value}");
        }
        assert!(!is_plausible_import_path("java", "java.util.List;"));
        assert!(is_plausible_import_path("java", "java.util.List"));
        assert!(is_plausible_import_path("java", "java.util.*"));
    }

    #[test]
    fn detect_repository_profile_extracts_frontend_commands() {
        let root = temp_repo("frontend");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{
              "scripts": { "test": "vitest run", "build": "vite build" },
              "dependencies": { "react": "^19.0.0", "vite": "^6.0.0" },
              "devDependencies": { "typescript": "^5.0.0", "vitest": "^3.0.0" }
            }"#,
        )
        .unwrap();
        std::fs::write(root.join("src/App.tsx"), "export const App = () => null;").unwrap();

        let detection = detect_repository_profile(&root);
        assert_eq!(detection.package_manager.as_deref(), Some("pnpm"));
        assert_eq!(
            detection.detected_test_command.as_deref(),
            Some("pnpm test")
        );
        assert_eq!(
            detection.detected_build_command.as_deref(),
            Some("pnpm build")
        );
        assert!(detection.stack.contains(&"React".to_string()));
        assert!(detection.stack.contains(&"Vite".to_string()));
        assert!(detection.stack.contains(&"TypeScript".to_string()));
        assert_eq!(detection.primary_language.as_deref(), Some("typescript"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detect_repository_profile_extracts_java_commands() {
        let root = temp_repo("java");
        std::fs::create_dir_all(root.join("src/main/java/com/acme")).unwrap();
        std::fs::write(root.join("mvnw"), "").unwrap();
        std::fs::write(
            root.join("pom.xml"),
            r#"<project><dependencies><dependency><artifactId>spring-boot-starter-web</artifactId></dependency></dependencies></project>"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/main/java/com/acme/UserService.java"),
            "public class UserService { public String name() { return \"a\"; } }",
        )
        .unwrap();

        let detection = detect_repository_profile(&root);
        assert_eq!(
            detection.detected_test_command.as_deref(),
            Some("./mvnw test")
        );
        assert_eq!(
            detection.detected_build_command.as_deref(),
            Some("./mvnw package -DskipTests")
        );
        assert!(detection.stack.contains(&"Java".to_string()));
        assert!(detection.stack.contains(&"Maven".to_string()));
        assert!(detection.stack.contains(&"Spring Boot".to_string()));
        assert_eq!(detection.primary_language.as_deref(), Some("java"));

        std::fs::remove_dir_all(root).unwrap();
    }

    fn temp_repo(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("aos-rd-{name}-{nanos}"));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
