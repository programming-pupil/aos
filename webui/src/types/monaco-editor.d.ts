// Type stub for monaco-editor (available through @monaco-editor/react bundle)
declare module 'monaco-editor' {
  export interface IStandaloneCodeEditor extends Record<string, unknown> {
    getValue(): string;
    setValue(value: string): void;
    getModel(): unknown;
    getPosition(): unknown;
  }

  export const editor: {
    setModelMarkers(
      model: unknown,
      ownerId: string,
      markers: Array<{
        startLineNumber: number;
        endLineNumber: number;
        startColumn: number;
        endColumn: number;
        message: string;
        severity: number;
      }>
    ): void;
    createModel(value: string, language?: string, uri?: unknown): unknown;
    getModel(uri: unknown): unknown | null;
    createDiffEditor(container: HTMLElement | null, options?: unknown): unknown;
  };
}
