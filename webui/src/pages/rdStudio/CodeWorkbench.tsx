import type { ReactNode } from 'react';

export function CodeWorkbench({
  collapsed,
  sidebar,
  inspector,
  bottom,
  children,
}: {
  collapsed: boolean;
  sidebar: ReactNode;
  inspector: ReactNode;
  bottom: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className={`rd-studio-main-grid${collapsed ? ' rd-studio-main-grid-collapsed' : ''}`}>
      <aside className="rd-studio-side-panel" style={{ display: 'flex', flexDirection: 'column', gap: 10, minWidth: 0 }}>
        {sidebar}
      </aside>
      <section className="rd-studio-center-panel">
        <main className="rd-studio-workspace" style={{ display: 'flex', flexDirection: 'column', gap: 10, minWidth: 0 }}>
          {children}
        </main>
        <aside className="rd-studio-inspector">
          {inspector}
        </aside>
        <section className="rd-studio-bottom-panel">
          {bottom}
        </section>
      </section>
    </div>
  );
}
