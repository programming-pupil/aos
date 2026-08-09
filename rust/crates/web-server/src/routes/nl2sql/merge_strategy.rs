//! Thin web adapter for NL2SQL merge strategies.

pub(crate) use nl2sql_domain::merge_strategy::{
    fill_null_columns, join_key_has_null, join_key_str, join_key_values, json_value_key,
    merge_rows_fn,
};

use super::max_cross_ds_rows;

pub(crate) fn hash_join(
    inputs: &[crate::nl2sql::MergeInput],
    intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
    on: &[String],
    is_left_join: bool,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    nl2sql_domain::merge_strategy::hash_join(
        inputs,
        intermediate,
        on,
        is_left_join,
        max_cross_ds_rows(),
    )
}

pub(crate) fn union_all(
    inputs: &[crate::nl2sql::MergeInput],
    intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    nl2sql_domain::merge_strategy::union_all(inputs, intermediate, max_cross_ds_rows())
}

pub(crate) fn union_distinct(
    inputs: &[crate::nl2sql::MergeInput],
    intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    nl2sql_domain::merge_strategy::union_distinct(inputs, intermediate, max_cross_ds_rows())
}

pub(crate) fn right_join(
    inputs: &[crate::nl2sql::MergeInput],
    intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
    on: &[String],
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    nl2sql_domain::merge_strategy::right_join(inputs, intermediate, on, max_cross_ds_rows())
}

pub(crate) fn full_outer_join(
    inputs: &[crate::nl2sql::MergeInput],
    intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
    on: &[String],
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    nl2sql_domain::merge_strategy::full_outer_join(inputs, intermediate, on, max_cross_ds_rows())
}

pub(crate) fn cross_join(
    inputs: &[crate::nl2sql::MergeInput],
    intermediate: &std::collections::HashMap<String, crate::nl2sql::StepResult>,
) -> anyhow::Result<(Vec<String>, Vec<serde_json::Value>)> {
    nl2sql_domain::merge_strategy::cross_join(inputs, intermediate, max_cross_ds_rows())
}
