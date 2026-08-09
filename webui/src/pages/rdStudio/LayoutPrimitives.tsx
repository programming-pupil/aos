import { useState } from 'react';
import type { ReactNode } from 'react';
import { Card } from 'antd';

export function SectionCard({ title, children, extra }: { title: ReactNode; children: ReactNode; extra?: ReactNode }) {
  return (
    <Card
      size="small"
      title={title}
      extra={extra}
      style={{ background: 'rgba(15, 23, 42, 0.72)', borderColor: 'rgba(148, 163, 184, 0.18)' }}
      styles={{ header: { color: '#e5eefc', borderBottomColor: 'rgba(148, 163, 184, 0.18)' }, body: { color: '#dbe7ff' } }}
    >
      {children}
    </Card>
  );
}

export function CollapsiblePhase({
  title,
  children,
  extra,
  defaultOpen = false,
}: {
  title: ReactNode;
  children: ReactNode;
  extra?: ReactNode;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);

  function toggleOpen() {
    setOpen((value) => !value);
  }

  return (
    <section className={`rd-task-phase${open ? ' rd-task-phase-open' : ''}`}>
      <div
        className="rd-task-phase-header"
        role="button"
        tabIndex={0}
        aria-expanded={open}
        onClick={toggleOpen}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            toggleOpen();
          }
        }}
      >
        <span className="rd-task-phase-toggle" aria-hidden="true">
          &gt;
        </span>
        <span className="rd-task-phase-title">{title}</span>
        {extra ? (
          <span
            className="rd-task-phase-extra"
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => event.stopPropagation()}
          >
            {extra}
          </span>
        ) : null}
      </div>
      {open ? <div className="rd-task-phase-body">{children}</div> : null}
    </section>
  );
}
