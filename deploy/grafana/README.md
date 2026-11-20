# Grafana dashboards

`vogt-request-latency.json` (WI-2697): p50/p95 per route and the ten slowest
routes, for `vogt_engine_http_request_seconds` (engine, `ENGINE_METRICS_ADDR`)
and `vogt_core_http_request_seconds` (core, `VOGT_METRICS_ADDR`). Datasource
uid `prometheus` (Node B). Import it, or provision it beside the
`vogt-session-launch` dashboard.

Both histograms are labelled `route` (a template, never an id), `method` and
`status`, timed to response start. The engine labels its `/api/vogt` proxy hop
with the core's template (`/api/vogt/backlog`), read from the core's
`X-Vogt-Route` response header.

## Acceptance thresholds (GUI-lag initiative WI-2690)

Over a 1 h window on the engine histogram (what the PWA feels):

| Surface | Route(s) | p95 |
|---|---|---|
| Inbox | `/api/vogt/inbox` | ≤ 300 ms |
| Backlog | `/api/vogt/backlog` | ≤ 300 ms |
| Coverage | `/api/vogt/coverage` | ≤ 300 ms |
| Rail | `/api/sessions` (GET) | ≤ 100 ms |

```promql
histogram_quantile(0.95, sum by (le, route) (rate(
  vogt_engine_http_request_seconds_bucket{method="GET",
    route=~"/api/vogt/(inbox|backlog|coverage)"}[1h]))) > 0.3
histogram_quantile(0.95, sum by (le) (rate(
  vogt_engine_http_request_seconds_bucket{method="GET",route="/api/sessions"}[1h]))) > 0.1
```

A result from either query is a breach.
