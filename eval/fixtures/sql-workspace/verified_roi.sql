-- verified=true; dialect=trino
WITH revenue AS (
    SELECT dt, SUM(total_price_usd) AS revenue
    FROM iceberg.finance.valid_paid_orders
    WHERE payment_status = 'paid' AND refund_status = 'none'
    GROUP BY dt
), cost AS (
    SELECT dt, SUM(cost) AS cost
    FROM (
        SELECT dt, bid_price_usd AS cost
        FROM iceberg.marketing.channel_attribution_data
        WHERE flag IS NULL OR flag NOT IN ('reject', 'rejected')
        UNION ALL
        SELECT dt, cost_usd AS cost
        FROM iceberg.marketing.attribution_tracker
        WHERE notify_status = 1
    ) t
    GROUP BY dt
)
SELECT r.dt, revenue, cost, revenue / NULLIF(cost, 0) AS roi
FROM revenue r
JOIN cost c ON r.dt = c.dt
ORDER BY r.dt;
