# TASK-27: SNI pass-through on 443

## Context
Milestone: M4 | Requirements: FR-08, N2

Replaces `sniproxy` in the lancache deployment model. Clients resolve a CDN name to us and then
speak TLS; we must splice them to the real origin without decrypting anything.

## Implementation Plan
- [ ] Peek the TLS ClientHello with `tls-parser`, extract SNI, without consuming the bytes
- [ ] Resolve the SNI host through the dedicated resolver (never the system one)
- [ ] `tokio::io::copy_bidirectional` to splice, with timeouts on both halves
- [ ] Apply the private-address guard to the resolved target (FR-64)
- [ ] Handle no-SNI and malformed ClientHello by closing, not by guessing
- [ ] Connection metrics for the 443 path, separate from the cached path

## Technical Decisions
- No caching, no decryption, no MITM certificate - this is explicitly N2 in the PRD and is not
  reopened.
- The guard applies here too. An SNI splice to a private address is the same open-relay hazard as
  an HTTP proxy to one.

## Dependencies
- Requires: TASK-10
- Blocks: v1.0

## Completion Checklist
- [ ] Real TLS client reaches a real origin through the splice
- [ ] Malformed and absent SNI handled without panic
- [ ] Guard tested against a private-resolving SNI host
- [ ] Throughput does not regress the cached path
