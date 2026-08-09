-- Reduce the legacy default research budget without overwriting tenant-customized
-- profiles. New tenants receive the same values from tenant_bootstrap.
UPDATE pm_budget_profiles
SET pipeline_timeout_secs = 540,
    source_slot_search_secs = 90,
    source_slot_browser_secs = 120,
    source_slot_api_fetch_secs = 90,
    preflight_overall_timeout_secs = 45,
    retry_step_budget_secs = 75,
    retry_total_budget_secs = 240,
    updated_at = CURRENT_TIMESTAMP
WHERE profile_key = 'normal'
  AND pipeline_timeout_secs = 1800
  AND max_attempts = 4
  AND retrieve_max_tool_calls = 12
  AND max_calls_per_source = 3
  AND source_slot_search_secs = 300
  AND source_slot_browser_secs = 300
  AND source_slot_api_fetch_secs = 300
  AND preflight_model_timeout_secs = 30
  AND preflight_probe_timeout_secs = 10
  AND preflight_overall_timeout_secs = 120
  AND retry_step_budget_secs = 90
  AND retry_total_budget_secs = 420;
