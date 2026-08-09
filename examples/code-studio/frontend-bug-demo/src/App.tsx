import React from 'react';
import { createRoot } from 'react-dom/client';
import { calculateRoi, formatPercent } from './metrics';
import './styles.css';

const dailyRows = [
  { country: 'ID', channel: 'TikTok', costUsd: 9100, revenueUsd: 8554 },
  { country: 'ID', channel: 'Facebook', costUsd: 5300, revenueUsd: 6042 },
  { country: 'ID', channel: 'Organic', costUsd: 0, revenueUsd: 2195 }
];

function App() {
  const totalCost = dailyRows.reduce((sum, row) => sum + row.costUsd, 0);
  const totalRevenue = dailyRows.reduce((sum, row) => sum + row.revenueUsd, 0);
  const roi = calculateRoi(totalRevenue, totalCost);

  return (
    <main className="shell">
      <section className="summary">
        <span className="eyebrow">AOS Code Studio demo</span>
        <h1>Indonesia ROI monitor</h1>
        <p>Open Preview, inspect the console error, then ask AOS to fix it with a candidate Diff.</p>
        <strong className="roi">{formatPercent(roi)}</strong>
      </section>

      <section className="table" aria-label="Channel ROI">
        {dailyRows.map((row) => (
          <article key={row.channel} className="row">
            <span>{row.channel}</span>
            <span>{formatPercent(calculateRoi(row.revenueUsd, row.costUsd))}</span>
          </article>
        ))}
      </section>
    </main>
  );
}

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
