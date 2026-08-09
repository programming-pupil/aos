-- Move untouched AOS research defaults to the bounded 5-8 minute profile.
-- Exact matching preserves tenant profiles that an administrator customized.
UPDATE pm_budget_profiles
SET pipeline_timeout_secs = 480,
    max_attempts = 3,
    retrieve_max_tool_calls = 8,
    source_slot_search_secs = 110,
    updated_at = CURRENT_TIMESTAMP
WHERE profile_key = 'normal'
  AND pipeline_timeout_secs = 540
  AND max_attempts = 4
  AND retrieve_max_tool_calls = 12
  AND max_calls_per_source = 3
  AND source_slot_search_secs = 90
  AND source_slot_browser_secs = 120
  AND source_slot_api_fetch_secs = 90
  AND preflight_model_timeout_secs = 30
  AND preflight_probe_timeout_secs = 10
  AND preflight_overall_timeout_secs = 45
  AND retry_step_budget_secs = 75
  AND retry_total_budget_secs = 240;
