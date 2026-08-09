import { Card, Skeleton } from 'antd';

/**
 * Full-page loading skeleton for page-level loading states.
 */
export function PageSkeleton({ rows = 6 }: { rows?: number }) {
  return (
    <div style={{ padding: 24 }}>
      {/* Header */}
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'center',
          marginBottom: 24,
        }}
      >
        <Skeleton.Input active style={{ width: 200, height: 32 }} />
        <Skeleton.Button active style={{ width: 100 }} />
      </div>
      {/* Table rows */}
      <Card>
        <Skeleton active paragraph={{ rows }} />
      </Card>
    </div>
  );
}

/**
 * Table row skeleton for list pages.
 */
export function TableSkeleton({
  columns = 5,
  rows = 8,
}: {
  columns?: number;
  rows?: number;
}) {
  return (
    <div style={{ padding: 24 }}>
      <Card>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>
          {/* Header row */}
          <div style={{ display: 'flex', gap: 16 }}>
            {Array.from({ length: columns }).map((_, i) => (
              <Skeleton.Input
                key={i}
                active
                style={{
                  flex: 1,
                  height: 16,
                  minWidth: (100 / columns) * 1.5,
                }}
              />
            ))}
          </div>
          {/* Data rows */}
          {Array.from({ length: rows }).map((_, rowIdx) => (
            <div key={rowIdx} style={{ display: 'flex', gap: 16, alignItems: 'center' }}>
              {Array.from({ length: columns }).map((_, colIdx) => (
                <Skeleton.Input
                  key={colIdx}
                  active
                  style={{
                    flex: 1,
                    height: 14,
                    minWidth: (100 / columns) * 1.2,
                  }}
                />
              ))}
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}

/**
 * Stat card skeleton for dashboard.
 */
export function StatCardSkeleton() {
  return (
    <Card>
      <Skeleton active avatar={false} paragraph={{ rows: 1 }} />
    </Card>
  );
}
