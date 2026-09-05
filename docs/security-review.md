# Security review and threat assessment

**Scope**: `cachic` at `v0.1.0-rc5` (commit `72a991f`), all crates, the container image, the compose
example and the Helm chart.
**Date**: 2026-09-05
**Method**: source audit of every request-handling path, plus executable proofs. Findings SR-01 to
SR-08 each have a test in `crates/cachic/tests/security_review.rs` that demonstrates the behaviour
against the current code. SR-09 to SR-11 are code-reading findings without a runtime proof, and are
labelled as such.
**Not covered**: no fuzzing campaign was run beyond the existing targets, no dependency source
review, no live-hardware testing. `cargo deny check advisories` is clean at this commit.

---

## Status

Recorded against `v0.1.0-rc5`. The three items the review marked "before any deployment where
untrusted clients can reach the cache" are fixed on `main`; the rest are open.

| ID | Status |
|---|---|
| SR-01 | **Fixed.** Admin API binds `127.0.0.1` by default (`ADMIN_BIND`). Bound wider, `/purge` and `/drain` refuse unless `ADMIN_TOKEN` is set, while health and metrics keep serving. Chart sets the wider bind, because kubelet probes the pod IP, and ships an optional `NetworkPolicy` |
| SR-02 | **Fixed.** The SNI path applies the matcher before resolving, refusing an unmatched name unless `PASSTHROUGH_UNKNOWN_HOSTS` is set, and counts refusals in `SniStats::not_allow_listed` |
| SR-07 (timeout) | **Fixed.** A `TokioTimer` is installed and `header_read_timeout` set to 15s |
| SR-03, SR-04, SR-05, SR-06, SR-08, SR-09, SR-10, SR-11, SR-07 (per-peer cap) | Open |

Each fixed finding's test now asserts the new behaviour instead of the old, with controls so it
cannot pass vacuously - SR-02's proof checks that an allow-listed name still splices and that
`passthrough` still opens an unmatched one, and SR-07's passes in ~15s rather than never.

---

## 1. Threat model

The deployment model is unusual and it drives everything below. cachic is installed by pointing a
LAN's DNS at it, so **clients do not opt in and are not authenticated**. There is no credential, no
client certificate and no ACL anywhere in the codebase. Reachability *is* authorisation.

### Actors

| Actor | Capability assumed |
|---|---|
| **A1 Hostile LAN client** | Full TCP access to ports 80, 443 and 9090. Can send arbitrary HTTP, arbitrary TLS, and arbitrary bytes. Cannot control DNS, cannot control any allow-listed CDN's content. This is the primary actor. |
| **A2 Curious LAN client** | As A1, but only reads. Wants other people's cached content. |
| **A3 Upstream list maintainer** | Can push to `uklans/cache-domains@master`, which cachic fetches and applies every 24h by default. |
| **A4 Hostile origin** | Controls an allow-listed CDN hostname's responses. Requires compromising a vendor CDN; low likelihood, high impact. |
| **A5 Co-tenant workload** | A pod in the same Kubernetes cluster, or a container on the same Docker network. |

### Assets

1. **Availability of the cache** — the whole LAN's downloads depend on it.
2. **Integrity of cached bytes** — clients hash-verify (Steam, PlayStation), so poisoned content is
   usually a failed install rather than code execution. Usually is not always: Windows Update and
   some launchers are more trusting.
3. **The operator's IP reputation and egress** — traffic relayed through cachic is attributed to
   the operator.
4. **Operational visibility** — the access log and metrics are how an operator sees abuse.

### Trust boundaries

```
   [ untrusted LAN ]                      [ cachic ]                  [ internet ]
                        :80   HTTP  ─────▶ matcher ─▶ key ─▶ store ─▶ guarded resolver ─▶ CDN
   any client, no ────▶ :443  SNI   ─────▶ (no matcher) ────────────▶ guarded resolver ─▶ any host
   authentication       :9090 admin ─────▶ (no auth by default) ────▶ purge / drain
                                                 ▲
                                                 └── cache-domains, fetched from GitHub every 24h
```

The three inbound arrows are the boundaries that matter. Only the first one is fully guarded.

---

## 2. Findings

| ID | Severity | Finding | Proof | Status |
|---|---|---|---|---|
| SR-01 | **Critical** | Admin API is unauthenticated by default and binds `0.0.0.0`; `/purge` and `/drain` are exposed | test | **fixed** |
| SR-02 | **Critical** | SNI pass-through is an open TCP relay — FR-64's allow-list half is unimplemented | test | **fixed** |
| SR-03 | **High** | SNI connections are subject to no limit of any kind | test | open |
| SR-07 | **High** | No header-read timeout, and the connection limit has no per-peer component | test | timeout **fixed**, per-peer cap open |
| SR-09 | **High** | Upstream response bodies are buffered unbounded during the probe | code | open |
| SR-04 | Medium | lancache-format access-log lines are forgeable from a client-supplied header | test | open |
| SR-05 | Medium | The domain list is refreshed unpinned and unverified, and wildcards are unvalidated | test | open |
| SR-06 | Medium | Distinct upstream URLs collapse to one cache key (poisoning primitive, inherited) | test | open |
| SR-10 | Medium | `Vary` and `Cache-Control: private/no-store` are ignored; credentials are forwarded | code | open |
| SR-08 | Low | IPv4-embedded IPv6 forms bypass the address guard | test | open |
| SR-11 | Low | Upstream error text is reflected to the client; `Host` chooses the upstream port | code | open |

---

### SR-01 — Critical — The admin API is unauthenticated by default, on every interface

`main.rs:112` binds the admin listener to `SocketAddr::from(([0, 0, 0, 0], config.admin_port))`.
`ADMIN_TOKEN` defaults to `""` (`config/mod.rs:180`), and `AuthToken::new("")` deliberately maps an
empty token to "no authentication" (`admin/api.rs:43`), so `permits()` returns `true` for every
request. `/purge` and `/drain` are therefore open to anyone who can reach port 9090.

`POST /purge?all=true` empties the entire cache. `POST /drain` fails readiness, which in Kubernetes
removes the pod from its Service endpoints. Both are single unauthenticated requests. Because they
are `POST` rather than `GET`, they are also reachable by CSRF from a page any LAN user visits — a
cross-origin form POST needs no preflight, and the query string carries the parameters.

The code's own justification is incorrect. `AuthToken`'s docstring says an absent token "is only
safe because it is bound to loopback or a cluster network by default", `admin/mod.rs` says purge and
drain "must not be reachable by every client on the LAN", and `docs/quickstart.md` tells operators
"Metrics are on `127.0.0.1:9090`". None of that is true of the process, which binds all interfaces.
There is no configuration option to change the bind address.

This is a deviation from an accepted decision, not an unconsidered gap. ADR-0008 item 7 states "The
admin API is **local or cluster-only by default**, on a separate port from the data plane". The port
separation was implemented and is enforced by configuration validation; the bind address was not.
ADR-0008's "What would overturn this" also anticipates the fix: "Item 7's default could reasonably
become 'token required' if the admin API grows anything more destructive than purge." `/drain` has
since been added, and purging the whole cache is itself destructive.

What actually confines it varies by deployment, and only one path is safe:

| Deployment | Exposure |
|---|---|
| `deploy/compose` | **Confined.** Published as `127.0.0.1:9090:9090`. Still reachable from other containers on the same Docker network (A5). |
| Compose with `--network host` | **Exposed to the LAN.** |
| Published release binaries (FR-73) | **Exposed to the LAN.** Nothing confines it and no option exists. |
| Helm, default | Reachable from every pod in the cluster via the admin ClusterIP Service. No `NetworkPolicy` template ships. |
| Helm with `hostNetwork: true` (offered in `values.yaml:38` for clusters without a load balancer) | **Exposed to the LAN.** |

**Proof**: `sr01_purge_all_needs_no_credentials_by_default`,
`sr01_drain_needs_no_credentials_by_default`. The control test
`sr01_a_configured_token_does_defend_the_endpoint` confirms a configured token returns 401 and
preserves the object, so the first two are not passing vacuously.

**Recommendation**

1. Bind the admin listener to `127.0.0.1` by default, and add an `ADMIN_BIND` option for operators
   who need otherwise. This alone closes the finding for the bare-binary case.
2. Require a token for the mutating endpoints (`/purge`, `/drain`) whenever the bind address is not
   loopback — refuse to start rather than serving them unauthenticated.
3. Keep `/healthz`, `/readyz` and `/metrics` on the current terms; they are the reason the port is
   reachable at all.
4. Ship a `NetworkPolicy` template in the chart restricting the admin port to the monitoring
   namespace.

---

### SR-02 — Critical — SNI pass-through is an open TCP relay

FR-64 is a P0 requirement with two halves: "Refuses to proxy to private/loopback upstream addresses
unless configured, **and only proxies allow-listed hosts unless `passthrough` is on** (the cache is
an open proxy on the LAN otherwise)."

The HTTP path implements both. `proxy/server.rs` calls `matcher.service_for(&host)` and returns 404
for an unmatched `Host`, citing FR-64 in a comment.

The SNI path implements only the first. `sni/proxy.rs:185` resolves whatever name the ClientHello
carries and splices to it. There is no reference to `Matcher`, `service_for` or `DomainList`
anywhere under `crates/cachic/src/sni/` — verified by grep. `TASK-27-sni-passthrough.md` lists its
requirements as "FR-08, N2"; FR-64 is not among them, which is how the gap was introduced.

The module docstring reads "The address guard applies here exactly as it does to the HTTP path",
treating the address guard as the whole of FR-64. On the HTTP path there are two gates. Here there
is one.

ADR-0008 shows the omission precisely. Item 3 — "Allow-listed upstreams by default. Unmatched hosts
return 404. `passthrough` mode exists but is off by default: without it the cache is an open proxy
on the LAN" — is stated generally, with no HTTP qualifier. Item 4, the address guard, explicitly
says "This applies to the SNI path as well as the HTTP path; an SNI splice to a private address is
the same open-relay hazard." The SNI path was considered for item 4 and carried it. It was not
carried for item 3, and the identical reasoning applies: an SNI splice to a non-allow-listed *host*
is the same open-relay hazard as an HTTP proxy to one.

Consequences for A1:

- **Open relay to port 443 of any public host on the internet.** Destination port is fixed to
  `HTTPS_PORT`, which bounds it, but the destination host is entirely attacker-chosen.
- **Attribution laundering.** The origin sees the cache's address. Abuse reports reach the operator.
- **Egress filtering bypass.** On a venue network that blocks direct outbound but permits the cache,
  cachic is a hole to any host on 443.
- **Not restricted to TLS.** After the hello is replayed, `copy_bidirectional` copies bytes
  verbatim. Only the first record must look like a ClientHello.

**Proof**: `sr02_sni_relays_to_a_host_that_is_not_in_the_allow_list` drives a full splice to
`evil-relay-target.net`, a name the test first asserts is absent from the bundled list, and reads
the non-CDN origin's bytes back through the relay. (The mock origin is on loopback, so the test sets
`allow_private` exactly as `proxy_integration.rs` does. That flag is orthogonal: the missing gate is
the *name* check, whose absence is unconditional.)

**Recommendation**: apply the matcher to the SNI host before resolving, refusing an unmatched name
unless `PASSTHROUGH_UNKNOWN_HOSTS` is set — the same decision the HTTP path already makes, with the
same configuration switch. Count refusals in `SniStats` so the metric exists.

---

### SR-03 — High — SNI connections are subject to no limit

`SniProxy::bind` spawns a task per accepted socket (`sni/proxy.rs:88`) with no limiter. The HTTP
path takes a `ConnectionPermit` and refuses at `ConnectionLimit` (10,000 by default); the SNI path
has no equivalent. Each parked connection holds a task, a socket and a hello buffer that may grow to
`MAX_CLIENT_HELLO` (16 KiB + 5).

The 10-second `HELLO_TIMEOUT` bounds how long a *silent* connection lives, which is a genuine
mitigation and is why this is High rather than Critical. It does not bound *how many* arrive, and it
does not apply once a splice is established.

**Proof**: `sr03_sni_accepts_connections_without_any_ceiling` — 64 concurrent connections, all
accepted, no rejection path to count them.

**Recommendation**: share the existing `ConnectionLimit` (or a second instance) with the SNI
listener, and export `cachic_sni_connections_rejected_total`.

---

### SR-07 — High — No header-read timeout, and no per-peer connection accounting

Two defects that compose into a cheap, total denial of service.

**No timeout.** `proxy/server.rs:150` calls `http1::Builder::new().serve_connection(io, service)`.
In hyper 1.11.1 `h1_header_read_timeout` defaults to `Dur::Default(Some(30s))`, but
`common/time.rs:70` returns `None` and logs `timeout 'header_read_timeout' has default, but no timer
set` when the builder has no `Timer` — and this builder has none. **The default timeout is inert.**
A connection that sends a partial request line is never closed.

**No per-peer accounting.** `ConnectionLimit::try_acquire` takes no address. The 10,000 ceiling is
global, so one host reaching it denies service to every other client. Rejection happens at accept,
before any per-client consideration.

Together: one machine opens 10,000 sockets, writes one byte on each, and every subsequent client is
refused at accept — indefinitely, at negligible cost to the attacker. The `Server::bind` accept loop
also has no accept-rate limiting.

**Proof**: `sr07_a_half_sent_request_is_never_timed_out` holds a half-sent request for 35 seconds —
past hyper's default — and confirms the server has not closed it.
`sr07_one_peer_can_consume_every_connection_slot` shows the limiter has no per-peer component.

**Recommendation**

1. Install a timer and set an explicit header-read timeout:
   `http1::Builder::new().timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(15))`.
   This is a two-line fix and it is the important one.
2. Add an idle keep-alive timeout.
3. Add a per-source-IP connection cap (a small fraction of the global ceiling), so one host cannot
   consume the whole budget. Configurable, since NAT and multi-user hosts are legitimate.

---

### SR-09 — High — Upstream response bodies are buffered without bound

`upstream/client.rs:243` calls `response.bytes().await`, which buffers the entire body into memory.
Nothing caps it — not `Content-Length`, not a byte ceiling.

This is reachable on the ordinary path, not an exotic one. The probe
(`orchestrator/mod.rs:291`) issues a ranged `fetch_range`; when the origin **ignores** `Range` and
answers `200` with the whole object, the probe buffers all of it and only then concludes
`no_ranges = true`. Range-ignoring origins are an expected, documented CDN behaviour — FR-13 and
FR-32 exist for them, and `fetch_stream` exists to stream them — but `fetch_stream` is only used
*after* `no_ranges` is known, which requires the probe that just allocated the object.

A1 requests several distinct large objects from a range-ignoring allow-listed origin. Each probe
allocates its whole object concurrently, bounded only by `UPSTREAM_MAX_INFLIGHT` (256). The 120s
`request_timeout` caps a single body at whatever the link delivers in two minutes — a few GiB on a
fast connection — which is already well past the default `CACHE_MEM_SIZE` of 2 GiB and past the
`CACHE_MEM_SIZE + ~700 MiB` sizing rule in `docs/sizing.md`. The compose example sets no container
memory limit.

**No runtime proof was produced** — demonstrating it means allocating multiple gigabytes in CI. The
code path is unambiguous on reading, but it should be confirmed against `mockcdn` with
`RangeBehaviour::Ignore` and a large object before being treated as settled.

**Recommendation**: bound the probe. Stream the response and abort once more than one slice-size
(plus a margin) has arrived without a `206`, since at that point `no_ranges` is already established
and the bytes are being discarded anyway. Reject a `Content-Length` above a configurable ceiling
before reading the body.

---

### SR-04 — Medium — The lancache access log is forgeable from a client-supplied header

`AccessEvent::to_lancache` (`telemetry/logs.rs:84`) interpolates `user_agent` and `path` into a
positional, quoted, single-line format with no escaping, and the subscriber for that format is
configured `.without_time().with_target(false).with_level(false)` — bare lines with no framing.

HTTP header values may legally contain `"`. hyper rejects only CR and LF, so a client can close the
`User-Agent` field, supply its own trailing fields, and emit further whole records.

The audience for this format is third-party dashboards — LANCache Manager, DeveLanCacheUI,
lancache-ui — that parse it positionally into a database. A1 can therefore fabricate entries
attributing traffic to another machine, inflate or hide byte counts, and feed arbitrary text to a
downstream parser that was not written with hostile input in mind.

The JSON format (the default, and the supported one) is not affected: `tracing`'s JSON layer escapes
correctly.

**Proof**: `sr04_a_user_agent_can_forge_fields_in_the_lancache_log` first asserts hyper accepts a
quote in a header value, then shows the forged record surviving verbatim into the emitted line.

**Recommendation**: when rendering the lancache format, strip or percent-escape `"`, `\`, CR, LF and
C0 controls from `path` and `user_agent`. The format is positional and cannot be changed, but the
values within it can be sanitised without moving any field.

---

### SR-05 — Medium — The domain list is refreshed unpinned and unverified, and wildcards are unvalidated

`services/refresh.rs` fetches `cache_domains.json` and every file it names from
`raw.githubusercontent.com/uklans/cache-domains/**master**` every 24 hours by default, and applies
the result if it merely parses and is non-empty (`refresh.rs:73`). There is no commit pin, no
signature, no size bound, and no limit on how much the list may change between refreshes.

`Pattern::parse` (`services/domains.rs:46`) applies no structural validation: `*.com` becomes
`Suffix("com")` and matches every `.com` host in existence. Nothing rejects a wildcard whose parent
is a public suffix, a bare TLD, or `*` itself.

This matters more than an allow-list widening normally would, because `include_host` is `false` by
default — the cache key excludes the hostname, which is the entire point of a LAN cache. One
attacker-controlled hostname inside a service is therefore enough to write **every key in that
service**. A single added line in the upstream repository (A3), or one careless operator addition,
converts cachic into an open proxy whose cache any client can write.

The bundled list is currently sound: all 16 wildcards have vendor-controlled parents at three or
more labels (`*.cdn.blizzard.com`, `*.gs2.ww.prod.dl.playstation.net.edgesuite.net`, and so on), and
no entry names a multi-tenant CDN parent an attacker could obtain a hostname under. That is a
property of today's data, not something the code enforces.

**Proof**: `sr05_a_wildcard_may_name_a_public_suffix` and
`sr05_a_bare_star_and_a_single_label_are_both_accepted`.

**Recommendation**

1. Reject a wildcard whose parent is a public suffix or a single label. A bundled PSL snapshot is
   the thorough version; refusing fewer than three labels catches the dangerous cases and is
   defensible on its own.
2. Pin the refresh to a tag or commit, or verify a signature. At minimum, refuse a refresh that
   changes the pattern count by more than a configurable fraction, and log a diff of added patterns
   at `warn`.
3. Consider defaulting `CACHE_DOMAINS_REFRESH` to `0` (disabled), since the bundled snapshot already
   makes a fresh install work and an unattended allow-list update is a standing supply-chain path.

---

### SR-06 — Medium — Distinct upstream URLs collapse to one cache key

The cache key is a lossy normalisation of a request target that is sent upstream verbatim.
`proxy/server.rs:379` builds the fetch URL as `format!("{scheme}://{host}{target}")` from the raw
target, while `key::normalise` percent-decodes, collapses `//`, `.` and `..`, and drops the query
string. Any origin that distinguishes two targets cachic merges will serve different bytes that land
under one key.

The query string is the sharp edge. Dropping it from the key is correct and necessary — CDN auth
tokens live there and keeping them would make every request a miss — but it is still sent to the
origin. An origin that varies its response on a query parameter lets a client choose the bytes
stored at another client's key. The same holds for `%2F` against origins that do not decode it
(object stores generally do not) and for dot-segments against origins that do not collapse them.

This is inherited from monolithic rather than introduced here — nginx keys on `$uri` and proxies
`$request_uri` — so it is parity, not a regression. It is reported because it is the most direct
poisoning primitive in the design and it is not written down anywhere.

Mitigating factors: exploitation needs an allow-listed origin that serves attacker-chosen bytes for
a URL whose normalised path collides with a victim's object, which the bundled list does not
obviously provide. Steam and PlayStation clients hash-verify and will reject poisoned content, so
the realistic outcome is a failed install rather than code execution. The generation mechanism
(`orchestrator/validators.rs`) is sound and will invalidate an object whose validators change, which
also means an attacker who *can* poison will usually cause cache thrash rather than a stable poison.

**Proof**: `sr06_distinct_targets_collapse_to_one_object_id` shows seven distinct targets colliding
with one victim key; `sr06_any_host_in_a_service_can_write_every_key_in_it` shows the host exclusion
that gives SR-05 its reach.

**Recommendation**: document this in `docs/known-limitations.md` as an accepted design consequence
with its preconditions stated — that is the honest resolution, and it is what makes SR-05's
wildcard validation obviously load-bearing rather than merely tidy. If a stronger position is wanted
later, per-service `keep_query` already exists as the mechanism.

---

### SR-10 — Medium — `Vary` and `Cache-Control` are ignored, and credentials are forwarded upstream

`forwarded_request_headers` (`proxy/headers.rs:95`) forwards **everything** the client sent except
hop-by-hop headers, `Host` and `Range`. That includes `Authorization`, `Cookie`, `Accept-Encoding`,
`If-None-Match` and `If-Modified-Since`. (`Proxy-Authorization` is stripped, as hop-by-hop.)

Meanwhile:

- **`Vary` is never read.** It is not in `PRESERVED_ENTITY` and appears nowhere in the codebase. An
  origin that says `Vary: Accept-Encoding` is ignored, and the cache key has no header component.
- **`Cache-Control: no-store` and `private` are never honoured.** `cache-control` is preserved and
  replayed to clients, but nothing consults it before storing.

So a request carrying credentials, or one whose response the origin marked private or declared to
vary, is cached under a key with no credential and no header component, and then served to every
other client. That is cache deception: A1 or A2 can retrieve a response generated for someone else's
request. Most allow-listed hosts serve static content, which is what keeps this Medium, but the list
includes hosts with authenticated download paths.

`Accept-Encoding` is the more likely nuisance: alternating requests with and without it can make an
origin alternate encodings under one key. Because a gzip response usually carries a different ETag,
the generation mechanism will invalidate the object each time — turning it into a cheap cache-thrash
denial of service rather than a silent corruption.

**No runtime proof was produced** for this one; it rests on the absence of `Vary` handling, which is
established by grep, and on the forwarding rule, which is explicit in the source.

**Recommendation**

1. Do not cache a response carrying `Cache-Control: private` or `no-store`; serve it through as a
   `BYPASS`.
2. Treat a response with a `Vary` other than the trivial cases as uncacheable, or fold the named
   request headers into the cache key. Refusing to cache is the smaller change and the safer default.
3. Strip `Authorization` and `Cookie` from forwarded requests by default, with a per-service opt-in
   for any service that genuinely needs them. A cache that shares one key namespace across all
   clients has no business forwarding per-client credentials.

---

### SR-08 — Low — IPv4-embedded IPv6 forms bypass the address guard

`check_v6` (`upstream/guard.rs:88`) decodes only IPv4-*mapped* addresses via `to_ipv4_mapped`, which
covers `::ffff:0:0/96` and nothing else. The deprecated IPv4-*compatible* form (`::a.b.c.d`), NAT64's
well-known prefix (`64:ff9b::/96`) and 6to4 (`2002::/16`) each embed an IPv4 address that is never
judged by IPv4 rules.

`::192.168.1.1`, `::127.0.0.1`, `64:ff9b::192.168.1.1` and `2002:c0a8:0101::1` all pass the guard.

Reachability depends on the host's IPv6 stack and on a NAT64 or 6to4 path existing, so this is
defence in depth rather than a live bypass on a typical deployment. It is reported because it is the
same class of trick the guard already defends against — its own comment says an IPv4-mapped address
"must be judged by its IPv4 rules, or `::ffff:192.168.1.1` walks straight through the guard".

**Proof**: `sr08_ipv4_embedded_ipv6_forms_bypass_the_address_guard`, which uses the mapped form as a
control.

**Recommendation**: in `check_v6`, extract and re-check the embedded IPv4 address for
`::/96` (excluding `::` and `::1`, already handled), `64:ff9b::/96` and `2002::/16`. Also consider
refusing `2001::/32` (Teredo). Roughly ten lines, with the existing test style.

---

### SR-11 — Low — Reflected error detail, and `Host` selects the upstream port

Two minor items, grouped.

**Error reflection.** `proxy/server.rs:233` returns `502` with the body
`format!("upstream error: {e}\n")`. That renders resolver refusals — including the "the cache is an
open proxy on the LAN" text and the refusal category — and `ShortSlice` errors carrying internal
URLs and slice indices. It is a modest oracle for how the cache behaves toward a given host. The
content-type is `text/plain`, so there is no XSS.

**Upstream port from `Host`.** `Matcher::normalise_host` strips the port for matching, but the fetch
URL is built from the raw header, so `Host: cdn.blizzard.com:22` matches the service and directs the
fetch to port 22 of a real CDN host. The address guard still applies, so this reaches public hosts
only, and the response is not usefully returned — but connect-timing differences make it a slow port
scanner sourced from the operator's address.

**Recommendation**: return a generic `502` body and log the detail; rebuild the upstream URL from
the normalised host, keeping the port only when it is 80 or 443 (or the configured upstream port).

---

## 3. What the design gets right

Stating this precisely matters, because it is what the remaining findings should be weighed against.

- **The upstream resolver is the strongest part of the codebase.** `hickory-resolver` is built
  without its `system-config` feature, so the constructors that read `/etc/resolv.conf` do not
  compile — the guarantee is structural rather than a review catch. `use_hosts_file` is
  `ResolveHosts::Never`. `GuardedResolver` is wired into reqwest's `dns_resolver`, so the addresses
  that were guarded are the addresses dialled, closing the resolve-then-reresolve TOCTOU that
  shipped in rc1. The comment explaining why is accurate and load-bearing.
- **The address guard is thorough** on the forms it covers: RFC 1918, loopback, link-local
  (including `169.254.169.254`), CGNAT, documentation and benchmarking ranges, IPv6 ULA and
  link-local, and IPv4-mapped IPv6. It is applied on every fetch, not only the first, and to literal
  addresses as well as resolved names. SR-08 is a gap at the edges of a good guard.
- **Redirects are deliberately not followed**, with the cache-poisoning reason stated in the module
  docstring. This closes a vector that catches many caching proxies.
- **The ClientHello parser is properly defensive** — every length bounds-checked, total, returns
  `None` rather than panicking, with a hard `MAX_CLIENT_HELLO` ceiling.
- **The validator and generation mechanism is correct.** Weak ETags are rejected for a byte-range
  cache, an ETag appearing or disappearing is a mismatch, and generation is part of the slice key so
  invalidation is atomic with no window in which a response could mix versions.
- **The host matcher is sound**: exact-then-suffix-walk with longest match winning, no substring
  matching, ports and trailing dots normalised, IPv6 literals handled.
- **Slices are checksummed on decode**, so a corrupt slice fails to load rather than being served.
- **Container hardening is good**: distroless non-root, `read_only: true`,
  `no-new-privileges:true`, only `NET_BIND_SERVICE`, `runAsNonRoot` and `readOnlyRootFilesystem` in
  the chart, SBOM via `cargo auditable`.
- **Range parsing is safe** — single ranges only, multi-range refused rather than expanded, no
  arithmetic that can overflow.
- **`cargo deny check advisories` is clean** at this commit.

---

## 4. Prioritised remediation

**Before any deployment where untrusted clients can reach the cache**

1. **SR-01** — bind admin to loopback by default; add `ADMIN_BIND`; require a token when it is not
   loopback. *Small change, removes a one-request cache wipe.*
2. **SR-02** — apply the matcher to the SNI host. *Closes a P0 requirement gap and an open relay.*
3. **SR-07** — install a hyper `Timer` and set `header_read_timeout`. *Two lines, removes a trivial
   total DoS.*

**Next**

4. **SR-09** — bound the probe's buffered body.
5. **SR-03** — put the SNI listener under a connection limit.
6. **SR-07 (part 2)** — per-source-IP connection cap.
7. **SR-05** — reject public-suffix wildcards; pin or bound the refresh.

**Then**

8. **SR-10** — honour `Cache-Control: private/no-store`, refuse to cache on `Vary`, stop forwarding
   `Authorization` and `Cookie` by default.
9. **SR-04** — sanitise `path` and `user_agent` in the lancache log format.
10. **SR-08** — extend `check_v6` to the IPv4-embedded forms.
11. **SR-11** — generic 502 bodies; rebuild the upstream URL from the normalised host.
12. **SR-06** — document in `docs/known-limitations.md`.

**Worth considering beyond the findings**

- There is no per-client rate limiting or quota anywhere. A1 can fill the disk with junk from
  allow-listed origins and evict everyone else's content; `CACHE_MAX_AGE` defaults to 3560 days, so
  nothing ages out on its own. FR-46's free-space guard bounds disk usage but not who gets to use
  it. A per-client byte or request budget would be the general answer to several findings at once.
- FR-31 (never cancel an in-flight fill) is correct for the intended workload but is an
  amplification primitive against a hostile one: a client can start fills and disconnect
  immediately, repeatedly, pinning the upstream budget with work nobody will read. Worth an explicit
  decision rather than leaving it implicit.
- The `PASSTHROUGH_UNKNOWN_HOSTS` path currently returns `501 Not Implemented`. When TASK-18
  implements it, it becomes the single most dangerous switch in the configuration — it is what
  FR-64 names as the thing that makes cachic an open proxy. It should be hard to turn on by
  accident and should log loudly at startup.

---

## 5. Reproducing

```sh
cargo test --test security_review
```

Twelve tests, all passing against `72a991f`. Each documents behaviour that exists today; when a
finding is fixed its test should start failing, and should be replaced by one asserting the new
behaviour. `sr07_a_half_sent_request_is_never_timed_out` waits 35 seconds by design — a shorter wait
would prove only that the connection was open, not that nothing will ever close it.
