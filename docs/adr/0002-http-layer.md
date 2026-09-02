# 0002. HTTP layer: hyper rather than Pingora

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: M0 (TASK-05), plan decision 0.3

## Context

The plan set hyper as the default and said Pingora wins only if `ProxyHttp::request_filter`
short-circuiting plus Pingora's upstream connectors give a cleaner slice orchestrator, and if its
Linux-first stance is acceptable.

## Decision

Use hyper 1.x for the server and reqwest (rustls) for upstream fetching. Do not adopt Pingora.

## Consequences

The deciding argument is structural rather than a benchmark. Pingora's proxy model is one upstream
response per downstream request. Our orchestrator is the opposite shape: one downstream request
fans out to N upstream slice fetches, which are then reassembled in order, and some of those
fetches are shared with other downstream requests that are not this connection's concern. Fitting
that into `ProxyHttp` means working around the framework rather than with it.

`pingora-cache` does not help: its storage is a test-grade `MemCache` and its APIs are documented
as experimental. We would be bringing a cache framework and then not using its cache.

hyper cost us nothing in the spike. The one real integration constraint found: foyer's
`get_or_fetch` future is `!Sync`, so response bodies that await it must be `UnsyncBoxBody` rather
than `BoxBody`. hyper does not require `Sync` bodies, so this is a type-signature detail, but it
is the kind of thing that would have been a fight inside a framework.

M0 did not build a Pingora prototype. Per TASK-05, that was the plan: if the comparison cannot be
made on paper, the answer is to stay on hyper.

## What would overturn this

Evidence that hyper's HTTP/1.x server mishandles real game-client traffic in ways that are
expensive to fix (the plan lists HTTP/1.0 and quirky headers as a risk), combined with Pingora
handling those cases out of the box. Capturing real client traffic into fixtures during M1 is what
would surface this.
