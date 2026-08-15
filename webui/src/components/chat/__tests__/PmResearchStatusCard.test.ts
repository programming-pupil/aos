import { describe, expect, it } from 'vitest';
import { extractPmRequirementStateView } from '../PmResearchStatusCard';

describe('PM requirement state projection', () => {
  it('extracts the durable state from a persisted stage event', () => {
    const view = extractPmRequirementStateView([], [{
      key: 'event-7',
      label: '需求状态',
      status: 'completed',
      attempt: 1,
      durationMs: 12,
      detail: 'updated',
      rawDetail: {
        requirementState: {
          readiness: 'brief',
          problemFrame: { statement: '降低首日流失', confirmed: false },
          jobs: [{ statement: '定位高流失人群' }],
          desiredOutcomes: [{ statement: '形成可验证的留存方案' }],
          openQuestions: [{ question: '先确认主要用户群？' }],
        },
      },
    }]);

    expect(view).toEqual({
      problemFrame: { statement: '降低首日流失', confirmed: false },
      jobs: ['定位高流失人群'],
      outcomes: ['形成可验证的留存方案'],
      openQuestions: ['先确认主要用户群？'],
      readiness: 'brief',
      confirmed: false,
    });
  });

  it('does not render a state panel when history has no durable state event', () => {
    expect(extractPmRequirementStateView([], [])).toBeNull();
  });
});
