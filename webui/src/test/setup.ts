// Vitest setup: provide a minimal in-memory `localStorage` for the node test
// environment. Some app modules (e.g. the auth store) read `localStorage` at
// import time, so it must exist before any module graph is evaluated.
class MemoryStorage {
  private store = new Map<string, string>();

  get length(): number {
    return this.store.size;
  }

  clear(): void {
    this.store.clear();
  }

  getItem(key: string): string | null {
    return this.store.has(key) ? (this.store.get(key) as string) : null;
  }

  key(index: number): string | null {
    return Array.from(this.store.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.store.delete(key);
  }

  setItem(key: string, value: string): void {
    this.store.set(key, String(value));
  }
}

const g = globalThis as unknown as {
  localStorage?: Storage;
  sessionStorage?: Storage;
};

if (!g.localStorage) {
  g.localStorage = new MemoryStorage() as unknown as Storage;
}
if (!g.sessionStorage) {
  g.sessionStorage = new MemoryStorage() as unknown as Storage;
}
