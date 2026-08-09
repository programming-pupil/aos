# Federated Trino workspace

- Catalog: `iceberg`
- Schemas: `finance`, `marketing`, `analyst`, `experiments`
- `finance.valid_paid_orders`: `dt`, `order_id`, `user_id`, `product_id`,
  `payment_status`, `refund_status`, `total_price_usd`, `country`.
- `marketing.channel_attribution_data`: `dt`, `user_id`, `channel`, `bid_price_usd`,
  `flag`, `receive_time`.
- `marketing.attribution_tracker`: `dt`, `user_id`, `channel`, `cost_usd`,
  `notify_status`.
- `analyst.dwd_game_event`: `dt`, `app_token`, `app_ver_code`, `country`,
  `device_brand`, `os_version`, `event_name`, `event_params`.
- `experiments.user_cohort`: `dt`, `user_id`, `experiment_id`, `group_id`.

Date partitions use `yyyyMMdd` strings. Business reporting timezone is UTC+8.
