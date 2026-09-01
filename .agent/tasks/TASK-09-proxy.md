# TASK-09: Proxy - server, router, headers, range parsing

## Context
Milestone: M1 | Requirements: FR-01, FR-05, FR-06, FR-07, FR-11, FR-15

The HTTP surface game clients actually see. Quirky clients (HTTP/1.0, odd headers) are called out
as a risk in plan section 10, so this module needs real traffic fixtures early.

## Implementation Plan
### Phase 1: Server
- [ ] hyper 1.x on a configurable port, accepting any `Host`
- [ ] HTTP/1.1 and HTTP/1.0, keep-alive, thousands of concurrent connections

### Phase 2: Router
- [ ] `GET`/`HEAD` to the cache path; everything else proxied uncached
- [ ] `HEAD` answered from the object index when known
- [ ] `GET /lancache-heartbeat` -> 204 with `X-LanCache-Processed-By` and the CORS headers
      prefill tools and LANCache Manager expect

### Phase 3: Headers
- [ ] Forward client headers (notably `User-Agent`); strip hop-by-hop
- [ ] Preserve upstream `Content-Type`, `ETag`, `Last-Modified`, `Cache-Control` as received
- [ ] Add `X-Cache: HIT|MISS|PARTIAL|BYPASS` and `X-LanCache-Processed-By`

### Phase 4: Ranges
- [ ] Single-range parsing, property-tested
- [ ] Multi-range answered with the full object (permitted by RFC 9110)
- [ ] Correct `416`, zero-length objects, objects without `Content-Length`

## Technical Decisions
- The heartbeat endpoint is an ecosystem contract - prefill tools probe it to decide whether a
  cache is present. Its exact shape is not ours to redesign.
- Range parsing is the most-fuzzed surface in the codebase (TASK-21); write it to be fuzzed.

## Dependencies
- Requires: TASK-07
- Blocks: TASK-12

## Completion Checklist
- [ ] Property tests on range parsing
- [ ] Fixture tests with captured client requests (Steam, WU, Blizzard)
- [ ] Heartbeat verified by an actual prefill tool
- [ ] Header pass-through and stripping tested both directions
