WITH ad_events AS (
    SELECT
        dt,
        app_token,
        app_ver_code,
        country,
        CASE
            WHEN event_params['mediation'] = 'max'
                THEN CAST(event_params['ecpm'] AS DOUBLE)
            ELSE CAST(event_params['ecpm'] AS DOUBLE) / 1000
        END AS ecpm,
        CAST(event_params['revenue'] AS DOUBLE) AS revenue
    FROM iceberg.analyst.dwd_game_event
    WHERE event_name = 'ad_show'
), daily AS (
    SELECT
        dt,
        app_token,
        app_ver_code,
        country,
        COUNT(*) AS impressions,
        SUM(revenue) AS revenue,
        AVG(ecpm) AS sdk_avg_ecpm
    FROM ad_events
    GROUP BY 1, 2, 3, 4
)
SELECT
    dt,
    app_token,
    app_ver_code,
    country,
    impressions,
    revenue,
    revenue / NULLIF(impressions, 0) * 1000 AS report_ecpm,
    sdk_avg_ecpm
FROM daily
ORDER BY dt, impressions DESC;
