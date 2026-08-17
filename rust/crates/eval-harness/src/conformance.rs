//! Traceability half of the semantic-kernel conformance gate.
//!
//! Source references are not behavior verification. This module validates the
//! case inventory and emits executable Cargo test commands; the repository
//! script runs those commands and is the only gate allowed to claim behavior
//! verification.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

const DATASET_RELATIVE_PATH: &str = "../../../eval/datasets/semantic-kernel-conformance.json";
const MATRIX_RELATIVE_PATH: &str = "../../../docs/AOS_SEMANTIC_KERNEL_CONFORMANCE_MATRIX.zh-CN.md";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConformanceCase {
    pub id: String,
    pub section: String,
    pub production: String,
    pub test: String,
    pub trigger: String,
    pub expected: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorCommand {
    pub case_id: String,
    pub package: String,
    pub test_filter: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ConformanceDataset {
    schema_version: String,
    spec: String,
    cases: Vec<ConformanceCase>,
}

#[derive(Debug, Error)]
pub enum ConformanceError {
    #[error("cannot read semantic-kernel conformance dataset: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot parse semantic-kernel conformance dataset: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid conformance dataset: {0}")]
    Invalid(String),
}

fn dataset_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(DATASET_RELATIVE_PATH)
}

fn repo_path(relative_path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative_path)
}

fn table_ids(markdown: &str, start_heading: &str, end_heading: Option<&str>) -> BTreeSet<String> {
    let section = markdown
        .split_once(start_heading)
        .map_or("", |(_, remainder)| remainder);
    let section = end_heading
        .and_then(|heading| section.split_once(heading).map(|(body, _)| body))
        .unwrap_or(section);
    section
        .lines()
        .filter_map(|line| {
            let id = line.split('|').nth(1)?.trim();
            let valid = id.contains('-')
                && id.chars().any(|character| character.is_ascii_uppercase())
                && id.chars().all(|character| {
                    character.is_ascii_uppercase() || character.is_ascii_digit() || character == '-'
                });
            valid.then(|| id.to_string())
        })
        .collect()
}

pub fn load_semantic_kernel_conformance_dataset() -> Result<Vec<ConformanceCase>, ConformanceError>
{
    let dataset: ConformanceDataset = serde_json::from_str(&fs::read_to_string(dataset_path())?)?;
    if dataset.schema_version != "semantic-kernel-conformance-v1" {
        return Err(ConformanceError::Invalid(format!(
            "unexpected schema version {}",
            dataset.schema_version
        )));
    }
    if dataset.spec != "docs/AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md" {
        return Err(ConformanceError::Invalid(format!(
            "dataset points at unexpected spec {}",
            dataset.spec
        )));
    }
    let mut ids = BTreeSet::new();
    for case in &dataset.cases {
        if case.id.trim().is_empty()
            || case.section.trim().is_empty()
            || case.production.trim().is_empty()
            || case.test.trim().is_empty()
            || case.trigger.trim().is_empty()
            || case.expected.trim().is_empty()
        {
            return Err(ConformanceError::Invalid(format!(
                "case {} has an empty required field",
                case.id
            )));
        }
        if !ids.insert(case.id.clone()) {
            return Err(ConformanceError::Invalid(format!(
                "duplicate case id {}",
                case.id
            )));
        }
    }
    Ok(dataset.cases)
}

fn source_and_symbol(reference: &str) -> Result<(&str, &str), ConformanceError> {
    let (path, qualified_symbol) = reference.split_once("::").ok_or_else(|| {
        ConformanceError::Invalid(format!("reference {reference} must be path::symbol"))
    })?;
    let symbol = qualified_symbol
        .rsplit("::")
        .next()
        .ok_or_else(|| ConformanceError::Invalid(format!("reference {reference} has no symbol")))?;
    Ok((path, symbol))
}

fn assert_reference_exists(reference: &str) -> Result<(), ConformanceError> {
    let (relative_path, symbol) = source_and_symbol(reference)?;
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(relative_path);
    let content = fs::read_to_string(&path)?;
    if !content.contains(symbol) {
        return Err(ConformanceError::Invalid(format!(
            "symbol {symbol} not found in {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn assert_semantic_kernel_traceability_dataset(
) -> Result<Vec<ConformanceCase>, ConformanceError> {
    let cases = load_semantic_kernel_conformance_dataset()?;
    if cases.len() != 31 {
        return Err(ConformanceError::Invalid(format!(
            "expected 31 P0 cases, found {}",
            cases.len()
        )));
    }
    for case in &cases {
        assert_reference_exists(&case.production)?;
        assert_reference_exists(&case.test)?;
    }
    let dataset_ids = cases
        .iter()
        .map(|case| case.id.clone())
        .collect::<BTreeSet<_>>();
    let spec = fs::read_to_string(repo_path("docs/AOS_SEMANTIC_KERNEL_REFACTOR.zh-CN.md"))?;
    let spec_ids = table_ids(&spec, "## 20. P0 开发任务清单", Some("## 21."));
    if spec_ids != dataset_ids {
        return Err(ConformanceError::Invalid(format!(
            "P0 spec/dataset ids differ: spec={spec_ids:?}, dataset={dataset_ids:?}"
        )));
    }
    let matrix =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(MATRIX_RELATIVE_PATH))?;
    let matrix_ids = table_ids(&matrix, "| ID |", Some("## 删除与保留"));
    if matrix_ids != dataset_ids {
        return Err(ConformanceError::Invalid(format!(
            "conformance matrix/dataset ids differ: matrix={matrix_ids:?}, dataset={dataset_ids:?}"
        )));
    }
    Ok(cases)
}

pub fn semantic_kernel_behavior_commands() -> Result<Vec<BehaviorCommand>, ConformanceError> {
    let cases = assert_semantic_kernel_traceability_dataset()?;
    cases
        .into_iter()
        .map(|case| {
            let (path, symbol) = source_and_symbol(&case.test)?;
            let package = path
                .strip_prefix("rust/crates/")
                .and_then(|value| value.split('/').next())
                .ok_or_else(|| {
                    ConformanceError::Invalid(format!(
                        "behavior test {} is not inside a Rust crate",
                        case.test
                    ))
                })?;
            Ok(BehaviorCommand {
                case_id: case.id,
                package: package.to_string(),
                test_filter: symbol.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{assert_semantic_kernel_traceability_dataset, semantic_kernel_behavior_commands};

    #[test]
    fn every_p0_case_has_live_traceability_references() {
        let cases = assert_semantic_kernel_traceability_dataset().expect("conformance dataset");
        assert_eq!(cases.len(), 31);
        assert!(cases.iter().all(|case| case.expected.len() > 12));
    }

    #[test]
    fn every_p0_case_produces_an_executable_behavior_command() {
        let commands = semantic_kernel_behavior_commands().expect("behavior commands");
        assert_eq!(commands.len(), 31);
        assert!(commands
            .iter()
            .all(|command| !command.package.is_empty() && !command.test_filter.is_empty()));
    }
}
