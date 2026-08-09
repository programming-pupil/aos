WITH revenue AS (
    SELECT dt, SUM(total_price_usd) AS revenue
    FROM iceberg.finance.valid_paid_orders
    WHERE payment_status = 'paid' AND refund_status = 'none'
    GROUP BY 1
), ua AS (
    SELECT dt, SUM(bid_price_usd) AS ua_cost
    FROM iceberg.marketing.channel_attribution_data
    WHERE flag IS NULL OR flag NOT IN ('reject', 'rejected')
    GROUP BY 1
), ug AS (
    SELECT dt, SUM(cost_usd) AS ug_cost
    FROM iceberg.marketing.attribution_tracker
    WHERE notify_status = 1
    GROUP BY 1
)
SELECT
    revenue.dt,
    revenue,
    ua_cost,
    ug_cost,
    revenue / NULLIF(ua_cost + ug_cost, 0) AS roi
FROM revenue
LEFT JOIN ua USING (dt)
LEFT JOIN ug USING (dt);
