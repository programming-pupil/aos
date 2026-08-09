import dayjs from 'dayjs';

function encodeCsvCell(val: unknown): string {
  const str = String(val ?? '');
  const trimmed = str.trim();
  const excelSafe = /^[=+\-@\t\r]/.test(trimmed) ? ' ' + str : str;
  const normalized = excelSafe.replace(/\r\n|\n|\r/g, '\r\n');
  if (normalized.includes(',') || normalized.includes('"') || normalized.includes('\r\n')) {
    return '"' + normalized.replace(/"/g, '""') + '"';
  }
  return normalized;
}

export function downloadCSV(columns: string[], rows: Record<string, unknown>[], filename?: string) {
  const header = columns.map(encodeCsvCell).join(',');
  const body = rows.map((row) =>
    columns.map((col) => encodeCsvCell(row[col])).join(',')
  );
  const csv = [header, ...body].join('\r\n');
  const blob = new Blob([csv], { type: 'text/csv' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename ?? `nl2sql_result_${dayjs().format('YYYYMMDD_HHmmss')}.csv`;
  a.click();
  URL.revokeObjectURL(url);
}

function excelCellValue(value: unknown): string | number | boolean | Date | null {
  if (value == null) return null;
  if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
    return value;
  }
  if (value instanceof Date) return value;
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export async function downloadExcel(
  columns: string[],
  rows: Record<string, unknown>[],
  filename?: string,
) {
  const { default: writeExcelFile } = await import('write-excel-file/browser');
  const sheetData = [
    columns.map((column) => ({ value: column, fontWeight: 'bold' as const })),
    ...rows.map((row) => columns.map((column) => excelCellValue(row[column]))),
  ];
  const columnWidths = columns.map((column) => ({
    width: Math.min(
      80,
      Math.max(column.length, ...rows.slice(0, 100).map((row) => String(row[column] ?? '').length)) + 2,
    ),
  }));

  await writeExcelFile(sheetData, {
    sheet: 'Query Results',
    columns: columnWidths,
  }).toFile(filename ?? `nl2sql_result_${dayjs().format('YYYYMMDD_HHmmss')}.xlsx`);
}

export function downloadJSON(columns: string[], rows: Record<string, unknown>[], filename?: string) {
  const data = rows.map((row) => {
    const mapped: Record<string, unknown> = {};
    columns.forEach((col) => { mapped[col] = row[col] ?? null; });
    return mapped;
  });
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename ?? `nl2sql_result_${dayjs().format('YYYYMMDD_HHmmss')}.json`;
  a.click();
  URL.revokeObjectURL(url);
}
