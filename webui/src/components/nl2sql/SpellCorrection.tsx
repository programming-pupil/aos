// ── NL2SQL Spell Correction & Semantic Validation ────────────────────────────────
// Provides Levenshtein-based spelling suggestions for table/column names and
// a "semantic unreachable" warning when the NL input cannot be answered with
// the current schema (low confidence or no matching tables/columns).

import React from 'react';
import { Alert, Tag, Typography } from 'antd';
import { useTranslation } from 'react-i18next';
import { distance as levenshtein } from 'fastest-levenshtein';
import type { QueryUnderstandingResponse } from '@/types';

const { Text } = Typography;

interface SchemaColumn {
  table_name: string;
  column_name: string;
  description?: string;
}

interface SpellCorrectionProps {
  question: string;
  schemaColumns: SchemaColumn[];
  threshold?: number; // max Levenshtein distance for a suggestion (default: 3)
}

interface Suggestion {
  original: string;
  corrected: string;
  table: string;
  column: string;
  distance: number;
}

function findSuggestions(question: string, schemaColumns: SchemaColumn[], threshold: number): Suggestion[] {
  const suggestions: Suggestion[] = [];
  const words = question.toLowerCase().split(/\s+/);

  for (const word of words) {
    if (word.length < 3) continue; // skip short words

    let bestMatch: Suggestion | null = null;

    for (const col of schemaColumns) {
      const tableName = col.table_name.toLowerCase();
      const colName = col.column_name.toLowerCase();

      // Check column name
      const colDist = levenshtein(word, colName);
      if (colDist <= threshold && colDist < (bestMatch?.distance ?? Infinity)) {
        bestMatch = { original: word, corrected: colName, table: col.table_name, column: colName, distance: colDist };
      }

      // Check table name
      const tableDist = levenshtein(word, tableName);
      if (tableDist <= threshold && tableDist < (bestMatch?.distance ?? Infinity)) {
        bestMatch = { original: word, corrected: tableName, table: col.table_name, column: colName, distance: tableDist };
      }
    }

    if (bestMatch) suggestions.push(bestMatch);
  }

  return suggestions;
}

export function SpellCorrection({ question, schemaColumns, threshold = 3 }: SpellCorrectionProps) {
  const { t } = useTranslation();
  const suggestions = findSuggestions(question, schemaColumns, threshold);

  if (suggestions.length === 0) return null;

  return (
    <Alert
      type="warning"
      showIcon
      icon={<Text style={{ fontSize: 14 }}>{t('nl2sql.spellCorrection.icon')}</Text>}
      message={
        <div>
          <Text strong style={{ fontSize: 12 }}>{t('nl2sql.spellCorrection.title')}</Text>
          <div style={{ marginTop: 4 }}>
            {suggestions.map((s, i) => (
              <Tag key={i} color="orange" style={{ marginRight: 4, marginBottom: 4 }}>
                {s.original}
                {' → '}
                {s.corrected}
                <Text type="secondary" style={{ fontSize: 10, marginLeft: 4 }}>
                  ({t('nl2sql.spellCorrection.table')}: {s.table})
                </Text>
              </Tag>
            ))}
          </div>
        </div>
      }
      style={{ marginBottom: 8, fontSize: 12 }}
    />
  );
}

interface SemanticUnreachableProps {
  qu: QueryUnderstandingResponse;
}

export function SemanticUnreachable({ qu }: SemanticUnreachableProps) {
  const { t } = useTranslation();

  // If confidence is very low (< 0.3) and no entities were extracted, the question
  // may not be answerable with the current schema.
  const isUnreachable = qu.confidence < 0.3 && !qu.entities.subject && qu.entities.filters.length === 0;

  if (!isUnreachable) return null;

  return (
    <Alert
      type="info"
      showIcon
      message={
        <div>
          <Text strong style={{ fontSize: 12 }}>{t('nl2sql.semanticUnreachable.title')}</Text>
          <Text style={{ fontSize: 12, display: 'block', marginTop: 4, color: 'inherit' }}>
            {t('nl2sql.semanticUnreachable.hint')}
          </Text>
        </div>
      }
      style={{ marginBottom: 8, fontSize: 12 }}
    />
  );
}
