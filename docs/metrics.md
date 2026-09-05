# Metrics

Everything on `/metrics` on the admin port, and — more usefully — which question each one answers.
Prometheus format; the bundled [dashboard](https://github.com/leftathome/cachic/blob/main/dashboards/cachic.json)
draws from these.

The service label is **`cdn_service`**, not `service`. In a Kubernetes monitoring stack the
ServiceMonitor pipeline attaches its own `service` label holding the Kubernetes Service name, and
on collision Prometheus keeps its own and renames the exporter's to `exported_service` — which
silently collapses every per-CDN query to one flat series named after the Service.

## Is it working, and is it worth it

| Metric | Labels | Reading it |
|---|---|---|
| `cachic_requests_total` | `cdn_service`, `status` | Request rate and hit ratio. `status` is the cache status — HIT, MISS, PARTIAL, BYPASS |
| `cachic_bytes_served_total` | `cdn_service`, `status` | What clients received |
| `cachic_bytes_fetched_total` | `cdn_service` | What the origin was asked for. Served divided by fetched is the cache's whole value proposition |
| `cachic_responses_total` | `cdn_service`, `code` | HTTP status codes. A different question from cache status: a burst of 502s does not move the HIT/MISS split |

## Why is it slow

| Metric | Labels | Reading it |
|---|---|---|
| `cachic_request_seconds` | `cdn_service`, `status` | End-to-end service time, histogram. What a client actually experiences |
| `cachic_upstream_seconds` | `cdn_service` | The origin leg only, histogram |

A cache hit never touches the origin, so these two answer different questions. Request latency
rising while upstream latency is flat points at the store or at writing to the client; both rising
together points at the origin.

The disk tier's own timings come from foyer: `foyer_hybrid_op_duration`,
`foyer_storage_op_duration`, `foyer_storage_disk_io_duration` and
`foyer_storage_entry_serde_duration`, each labelled by operation.

## Is something failing

| Metric | Labels | Reading it |
|---|---|---|
| `cachic_upstream_errors_total` | `cdn_service`, `kind` | `kind` is one of `timeout`, `connect`, `origin_4xx`, `origin_5xx`, `resolve` |
| `cachic_stale_responses_total` | `cdn_service` | Responses served from cache because the origin failed (FR-22). Climbing means clients are being covered for an outage they cannot see |
| `cachic_checksum_failures_total` | `cdn_service` | **Must be zero.** Non-zero is corruption on disk |
| `cachic_upstream_guard_refusals_total` | `reason` | Fetches the address guard refused |
| `cachic_generation_bumps_total` | `cdn_service` | Objects invalidated because their validators changed upstream |

## Is it saturated

| Metric | Labels | Reading it |
|---|---|---|
| `cachic_upstream_inflight` | — | Slice fetches in flight, all services |
| `cachic_upstream_inflight_service` | `cdn_service` | In flight for one service… |
| `cachic_upstream_limit_service` | `cdn_service` | …against that service's configured ceiling (FR-09). Sitting at the limit means that CDN is the bottleneck, not the cache |
| `cachic_client_connections` | — | Open client connections |
| `cachic_requests_in_flight` | — | Requests currently being served |

## How full is it

| Metric | Labels | Reading it |
|---|---|---|
| `cachic_index_objects` | — | Objects the index knows about |
| `cachic_index_bytes` | — | Bytes those objects account for |
| `cachic_store_capacity_bytes` | — | What the disk tier may use, *after* the free-space guard |
| `cachic_disk_available_bytes` | — | Free space on the backing filesystem |
| `cachic_disk_total_bytes` | — | Size of that filesystem |
| `cachic_disk_guard_engaged` | — | 1 while the guard is clamping the configured size, so the cache will not grow further |

## Is shutdown healthy

| Metric | Labels | Reading it |
|---|---|---|
| `cachic_draining` | — | 1 while draining |
| `cachic_requests_in_flight` | — | What the drain is waiting for. If it does not reach zero, a request is stuck and the pod will be killed rather than exiting cleanly |

## What to alert on

Ordered by how much it should wake someone:

1. `cachic_checksum_failures_total > 0` — corruption. Absolute; there is no acceptable rate.
2. `rate(foyer_storage_inner_op_total{op="channel_overflow"}[5m]) > 0` — the disk tier is
   silently dropping writes because fills are outrunning it. Clients see no error and the hit
   ratio simply never improves. Raise `CACHE_FLUSHERS` and `CACHE_BUFFER_POOL`.
3. `cachic_disk_guard_engaged == 1` for a sustained period — the cache has stopped growing.
4. A rising `cachic_upstream_errors_total` with a rising `cachic_stale_responses_total` — the
   origin is failing and the cache is covering. Useful before users notice.
5. `cachic_requests_in_flight` not falling while `cachic_draining == 1`.

Deliberately **not** exported: anything labelled per client address. On a LAN that is a
cardinality problem rather than a metric, and the access log already carries the client address
for that analysis.
