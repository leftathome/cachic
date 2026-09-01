# TASK-10: Upstream client, resolver and guards

## Context
Milestone: M1 | Requirements: FR-03, FR-04, FR-09, FR-64

The dedicated resolver is the single most important non-obvious behaviour in the system: the
proxy exists because DNS lies about CDN hostnames, so the proxy must not use that DNS.

## Implementation Plan
### Phase 1: Resolver
- [ ] `hickory-resolver` configured from `UPSTREAM_DNS`, IPv4 and IPv6
- [ ] Never fall back to the system resolver, including on error paths
- [ ] Cache resolutions with sane TTL handling

### Phase 2: Client
- [ ] Connection pool per host, HTTP/1.1
- [ ] Timeouts: connect, read, overall
- [ ] No automatic redirects - handle explicitly so the cache key stays correct
- [ ] Upstream scheme per service: same-as-client by default, `https` selectable, system roots

### Phase 3: Guards and limits
- [ ] Refuse upstream targets resolving to private/loopback ranges unless configured (FR-64)
- [ ] Per-service upstream concurrency limits and global connection limits with backpressure
- [ ] Single retry on transient failure

## Technical Decisions
- The private-address guard defaults to on. Without it, and without an allow-list, the cache is an
  open proxy on the LAN.
- Redirects are handled by us because following one silently would cache content under the wrong
  key.

## Dependencies
- Requires: TASK-07
- Blocks: TASK-12

## Completion Checklist
- [ ] Test proving the system resolver is never consulted
- [ ] Private-address guard tested with a resolver returning RFC1918
- [ ] Timeout and retry behaviour tested against a flaky `mockcdn`
- [ ] Limits exert backpressure rather than queueing without bound
