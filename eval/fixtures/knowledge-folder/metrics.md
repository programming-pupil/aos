# Business metrics

## eCPM

`eCPM = ad_revenue / ad_impressions * 1000`. Revenue is normalized to USD before
aggregation. For legacy SDK events, `event_params['ecpm']` is micros and must be divided by
1000; MAX mediation events are already in the reporting unit.

## ROI

`ROI = revenue / (ua_cost + ug_cost)`. Revenue comes from valid paid orders. Rejected
attribution callbacks are excluded from UA cost, and UG cost only includes successful notify
records.

## New users and retention

New users are first-device activations in UTC+8. D1 retention uses those new users as the
denominator and users active on the following calendar day as the numerator.
