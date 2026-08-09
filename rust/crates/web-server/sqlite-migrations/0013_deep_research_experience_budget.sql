-- Apply the current bounded deep-research budget only to the exact AOS default
-- installed by migration 0010. Tenant-customized profiles must remain intact.
UPDATE pm_budget_profiles
SET pipeline_timeout_secs = 360,
    source_slot_search_secs = 75,
    source_slot_browser_secs = 90,
    source_slot_api_fetch_secs = 60,
    retry_total_budget_secs = 120,
    updated_at = CURRENT_TIMESTAMP
WHERE profile_key = 'normal'
  AND pipeline_timeout_secs = 390
  AND max_attempts = 2
  AND retrieve_max_tool_calls = 6
  AND max_calls_per_source = 3
  AND source_slot_search_secs = 90
  AND source_slot_browser_secs = 100
  AND source_slot_api_fetch_secs = 75
  AND preflight_model_timeout_secs = 30
  AND preflight_probe_timeout_secs = 10
  AND preflight_overall_timeout_secs = 45
  AND retry_step_budget_secs = 60
  AND retry_total_budget_secs = 150;
