import { describe, expect, it } from 'vitest';
import type { ConversationMessage } from '@/types';
import {
  mapConversationMessagesToTurns,
  normalizeNl2sqlErrorMessage,
  parseSqlError,
} from './helpers';

function message(overrides: Partial<ConversationMessage>): ConversationMessage {
  return {
    query_id: 'event-1',
    data_source_id: null,
    question: '统计订单量',
    generated_sql: null,
    rows_returned: null,
    execution_ms: null,
    created_at: '2026-08-03 10:00:00',
    ...overrides,
  };
}

describe('mapConversationMessagesToTurns', () => {
  it('keeps a clarification answer visible without replaying the enriched backend question', () => {
    const messages = [
      message({
        message_type: 'clarification',
        query_id: 'clarify-prompt',
        clarification_question: '请补充统计范围',
      }),
      message({
        message_type: 'clarification',
        query_id: 'clarify-answer',
        clarification_question: '请补充统计范围',
        clarification_answer: '最近 7 天',
      }),
      message({
        message_type: 'query',
        query_id: 'query-1',
        data_source_id: 'ds-1',
        question: '统计订单量\n补充条件：最近 7 天',
        generated_sql: 'SELECT COUNT(*) FROM orders',
      }),
    ];

    const turns = mapConversationMessagesToTurns(messages);

    expect(turns.map((turn) => [turn.role, turn.question])).toEqual([
      ['assistant', '统计订单量'],
      ['user', '最近 7 天'],
      ['assistant', '统计订单量\n补充条件：最近 7 天'],
    ]);
    expect(turns[2].sql).toBe('SELECT COUNT(*) FROM orders');
  });

  it('keeps a genuine later user follow-up as a separate turn', () => {
    const messages = [
      message({
        message_type: 'clarification',
        query_id: 'clarify-answer',
        clarification_question: '请补充统计范围',
        clarification_answer: '最近 7 天',
      }),
      message({
        message_type: 'query',
        query_id: 'query-2',
        data_source_id: 'ds-1',
        question: '换成按周统计',
        generated_sql: 'SELECT week, COUNT(*) FROM orders GROUP BY week',
      }),
    ];

    const turns = mapConversationMessagesToTurns(messages);

    expect(turns.filter((turn) => turn.role === 'user').map((turn) => turn.question)).toEqual([
      '最近 7 天',
      '换成按周统计',
    ]);
  });
});

describe('NL2SQL error presentation', () => {
  it('localizes quota errors through the active translation function', () => {
    const translated = normalizeNl2sqlErrorMessage(
      '当前用于 NL2SQL 的 API Key 余额不足或已欠费',
      ((key: string) => key === 'nl2sql.apiKeyQuotaExceeded'
        ? 'Localized quota message'
        : key) as never,
    );

    expect(translated).toBe('Localized quota message');
  });

  it('classifies policy denial while preserving denied table and column details', () => {
    const error = parseSqlError(
      '[query_policy_denied] SQL was generated successfully (tables: secrets; columns: password_hash)',
    );

    expect(error.type).toBe('permission');
    expect(error.message).toContain('tables: secrets');
    expect(error.message).toContain('columns: password_hash');
  });
});
