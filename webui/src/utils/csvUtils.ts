/**
 * CSV Export/Import utilities for management tabs.
 * Uses native browser APIs — no external CSV library needed.
 */

export function exportToCsv<T extends Record<string, unknown>>(
  rows: T[],
  filename: string,
): void {
  if (rows.length === 0) return;
  const headers = Object.keys(rows[0]);
  const escape = (v: unknown): string => {
    const s = String(v ?? '');
    if (s.includes(',') || s.includes('"') || s.includes('\n')) {
      return `"${s.replace(/"/g, '""')}"`;
    }
    return s;
  };
  const csv = [
    headers.join(','),
    ...rows.map(row => headers.map(h => escape(row[h])).join(',')),
  ].join('\n');

  const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename.endsWith('.csv') ? filename : `${filename}.csv`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

export function parseCsv(text: string): string[][] {
  const rows: string[][] = [];
  let pos = 0;
  const len = text.length;

  const skipWhitespace = () => {
    while (pos < len && /\s/.test(text[pos])) pos++;
  };

  const readField = (): string => {
    if (pos >= len) return '';
    if (text[pos] === '"') {
      pos++;
      let field = '';
      while (pos < len) {
        if (text[pos] === '"') {
          if (pos + 1 < len && text[pos + 1] === '"') {
            field += '"';
            pos += 2;
          } else {
            pos++;
            break;
          }
        } else {
          field += text[pos];
          pos++;
        }
      }
      return field;
    }
    let field = '';
    while (pos < len && text[pos] !== ',' && text[pos] !== '\n' && text[pos] !== '\r') {
      field += text[pos];
      pos++;
    }
    return field;
  };

  const readRow = (): string[] | null => {
    if (pos >= len) return null;
    const fields: string[] = [];
    fields.push(readField());
    skipWhitespace();
    while (pos < len && text[pos] === ',') {
      pos++;
      skipWhitespace();
      fields.push(readField());
      skipWhitespace();
    }
    if (pos < len && text[pos] === '\r') pos++;
    if (pos < len && text[pos] === '\n') pos++;
    return fields;
  };

  skipWhitespace();
  while (pos < len) {
    const row = readRow();
    if (row) rows.push(row);
    if (pos >= len) break;
  }
  return rows;
}

export function importCsvFile<T extends Record<string, string>>(
  file: File,
  columns: (keyof T)[],
): Promise<T[]> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = (e) => {
      try {
        const text = e.target?.result as string;
        const rows = parseCsv(text);
        if (rows.length < 2) {
          resolve([]);
          return;
        }
        const headers = rows[0].map(h => h.trim().toLowerCase());
        const colMap = new Map(headers.map((h, i) => [h, i]));
        const result: T[] = [];
        for (let r = 1; r < rows.length; r++) {
          const row = rows[r];
          if (row.length === 0 || row.every(c => c.trim() === '')) continue;
          const obj: Record<string, string> = {};
          for (const col of columns) {
            const idx = colMap.get(String(col).toLowerCase());
            obj[String(col)] = idx !== undefined ? (row[idx] ?? '').trim() : '';
          }
          result.push(obj as T);
        }
        resolve(result);
      } catch (err) {
        reject(err);
      }
    };
    reader.onerror = () => reject(new Error('Failed to read file'));
    reader.readAsText(file);
  });
}
