ALTER TABLE rd_agent_profiles ADD COLUMN source TEXT NOT NULL DEFAULT 'custom';
ALTER TABLE rd_agent_profiles ADD COLUMN source_item_id TEXT DEFAULT NULL;

DROP INDEX idx__rd_agent_profiles__uk_rd_agent_profiles_name;

UPDATE rd_agent_profiles
SET source = 'aos',
    source_item_id = CASE name
        WHEN 'Rust Review Agent' THEN 'rust-review-agent'
        WHEN 'React Frontend Fix Agent' THEN 'react-frontend-fix-agent'
        WHEN 'Security Review Agent' THEN 'security-review-agent'
        WHEN 'Test Repair Agent' THEN 'test-repair-agent'
        WHEN 'Architecture Agent' THEN 'architecture-agent'
        WHEN 'Coding Agent' THEN 'coding-agent'
        WHEN 'Review Agent' THEN 'review-agent'
        WHEN 'Test Agent' THEN 'test-agent'
        WHEN 'PR Agent' THEN 'pr-agent'
        WHEN 'Java Spring Agent' THEN 'java-spring-agent'
        WHEN 'Python Service Agent' THEN 'python-agent'
        WHEN 'SQL Migration Review Agent' THEN 'sql-migration-review-agent'
        WHEN 'DevOps CI Agent' THEN 'devops-ci-agent'
        WHEN 'Performance Agent' THEN 'performance-agent'
        WHEN 'Accessibility UX Agent' THEN 'accessibility-ux-agent'
        WHEN 'Legacy Refactor Agent' THEN 'legacy-refactor-agent'
    END
WHERE source_item_id IS NULL
  AND name IN (
      'Rust Review Agent',
      'React Frontend Fix Agent',
      'Security Review Agent',
      'Test Repair Agent',
      'Architecture Agent',
      'Coding Agent',
      'Review Agent',
      'Test Agent',
      'PR Agent',
      'Java Spring Agent',
      'Python Service Agent',
      'SQL Migration Review Agent',
      'DevOps CI Agent',
      'Performance Agent',
      'Accessibility UX Agent',
      'Legacy Refactor Agent'
  )
  AND id = (
      SELECT MIN(existing.id)
      FROM rd_agent_profiles existing
      WHERE existing.tenant_id = rd_agent_profiles.tenant_id
        AND existing.name = rd_agent_profiles.name
  );

CREATE UNIQUE INDEX idx_rd_agent_profiles_market_source
ON rd_agent_profiles (tenant_id, source, source_item_id)
WHERE source_item_id IS NOT NULL;

CREATE UNIQUE INDEX idx_rd_agent_profiles_name_source
ON rd_agent_profiles (tenant_id, name, source);
