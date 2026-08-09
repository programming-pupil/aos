-- Tighten only the untouched AOS default. Tenant-customized profiles remain
-- unchanged. The first attempt is already a parallel multi-dimension evidence
-- wave; the second is reserved for one fresh, targeted repair.
UPDATE pm_budget_profiles
SET pipeline_timeout_secs = 390,
    max_attempts = 2,
    retrieve_max_tool_calls = 6,
    source_slot_search_secs = 90,
    source_slot_browser_secs = 100,
    source_slot_api_fetch_secs = 75,
    retry_step_budget_secs = 60,
    retry_total_budget_secs = 150,
    updated_at = CURRENT_TIMESTAMP
WHERE profile_key = 'normal'
  AND pipeline_timeout_secs = 480
  AND max_attempts = 3
  AND retrieve_max_tool_calls = 8
  AND max_calls_per_source = 3
  AND source_slot_search_secs = 110
  AND source_slot_browser_secs = 120
  AND source_slot_api_fetch_secs = 90
  AND preflight_model_timeout_secs = 30
  AND preflight_probe_timeout_secs = 10
  AND preflight_overall_timeout_secs = 45
  AND retry_step_budget_secs = 75
  AND retry_total_budget_secs = 240;
