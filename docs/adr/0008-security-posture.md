# 0008. Security posture

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: FR-03, FR-64, NFR-10, N2

## Decision

1. **No TLS termination, ever.** Port 443 is SNI pass-through only: read the ClientHello, resolve
   the SNI host, splice bytes. No MITM certificate is required on any client. This is N2 in the
   PRD and is not reopened.
2. **A dedicated resolver, always.** Upstreams resolve through `UPSTREAM_DNS`, never the system
   resolver, including on error paths. This is not primarily a security control - it exists
   because the proxy is deployed behind a DNS server that lies about CDN hostnames, and using that
   resolver loops traffic back into the cache. In Kubernetes the pod resolver may forward to
   exactly that server.
3. **Allow-listed upstreams by default.** Unmatched hosts return 404. `passthrough` mode exists but
   is off by default: without it the cache is an open proxy on the LAN.
4. **Refuse private and loopback upstream targets** unless explicitly configured (FR-64). This
   applies to the SNI path as well as the HTTP path; an SNI splice to a private address is the same
   open-relay hazard.
5. **Non-root**, with `CAP_NET_BIND_SERVICE` for privileged ports, read-only root filesystem in the
   container, and no secrets required to run.
6. **Redirects are not followed automatically.** Following one silently would cache content under a
   key that does not describe it.
7. **The admin API is local or cluster-only by default**, on a separate port from the data plane,
   with an optional bearer token that is never defaulted to a fixed value.

## Consequences

The spike sets `redirect::Policy::none()` on the upstream client, which is the point at which this
becomes real rather than aspirational.

Items 3 and 4 need tests that prove the negative - a resolver returning RFC1918 must be refused,
and the system resolver must be demonstrably never consulted. Those are TASK-10 deliverables and
are the kind of property that silently regresses without a test.

No secrets are required to operate the cache. Registry and signing credentials exist only in CI
and come from 1Password or Vault; nothing unencrypted is committed.

## What would overturn this

Nothing in items 1-4; they are product boundaries rather than implementation choices. Item 7's
default could reasonably become "token required" if the admin API grows anything more destructive
than purge.
