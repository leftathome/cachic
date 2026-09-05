# SOP: Measuring cachic in a container, and against monolithic

**Category**: development
**Created**: 2026-09-05
**Last Updated**: 2026-09-05

---

## Context

**When to use**: any throughput, latency or memory number that will be published, and any
comparison against `lancachenet/monolithic`.

**Problem it solves**: the in-tree harnesses (`bench`, `soak`) link the proxy into their own
process. That is right for correctness work and useless for two things — comparing cachic against
another implementation, and measuring a container from outside it. It also conflates the memory of
the proxy, the origin and the clients, which is how an early reading of RSS was wrong.

**Prerequisites**: the `cachic-tools` tarball (or `cargo build --release -p cachic-testkit
--example loadtest --example origin`), a container runtime, and root for the network namespace.

---

## The setup

### 1. Put the origin in its own network namespace

Both proxies want to bind `:80`. nginx binds the wildcard `0.0.0.0:80`, which conflicts with any
specific bind on the same port, so the origin cannot simply sit on another loopback address.

```sh
ip netns add originns
ip link add veth-o type veth peer name veth-i
ip link set veth-i netns originns
ip addr add 10.200.0.1/24 dev veth-o && ip link set veth-o up
ip netns exec originns ip addr add 10.200.0.2/24 dev veth-i
ip netns exec originns ip link set veth-i up
ip netns exec originns ip link set lo up

ip netns exec originns ./origin --address 10.200.0.2 --http-port 80 --dns-port 53 &
```

`origin` serves generated objects at `/o/<name>/<size>` and answers every DNS name with its own
address, which is what lets both proxies resolve a real CDN hostname to it.

### 2. Run the engine under test

```sh
# cachic
docker run -d --name cachic --network host -v /var/tmp/cc:/data/cache \
  -e HTTP_PORT=8080 -e HTTPS_PORT=8443 -e ADMIN_PORT=9091 \
  -e UPSTREAM_DNS=10.200.0.2 -e ALLOW_PRIVATE_UPSTREAMS=true \
  -e CACHE_MEM_SIZE=2g -e CACHE_DISK_SIZE=8g -e MIN_FREE_DISK=100m \
  ghcr.io/leftathome/cachic:<tag>

# monolithic, for the comparison
docker run -d --name mono --network host \
  -v /var/tmp/mono-cache:/data/cache -v /var/tmp/mono-logs:/data/logs \
  -e UPSTREAM_DNS=10.200.0.2 -e CACHE_DISK_SIZE=8g -e CACHE_MEM_SIZE=2000m \
  -e CACHE_MAX_AGE=3560d -e CACHE_SLICE_SIZE=1m lancachenet/monolithic:latest
```

`ALLOW_PRIVATE_UPSTREAMS=true` is required: the origin is on an RFC1918 address and the FR-64
guard refuses those by default.

### 3. Drive load and read the result

```sh
./loadtest --target http://127.0.0.1:8080 --clients 32 --seconds 60 \
           --objects 24 --object-mib 256
# 23934 requests, 5.21 Gbps (621 MiB/s), TTFB p50 11.08 ms / p99 28.28 ms
```

Sample RSS from outside while it runs; the peak under load is the number that matters, not idle:

```sh
PID=$(docker inspect -f '{{.State.Pid}}' cachic)
while :; do awk '/VmRSS/{print $2/1024" MiB"}' /proc/$PID/status; sleep 3; done
```

---

## Rules for a number worth publishing

- **Say whether it is cold or warm.** They are different claims. A cold cachic compared against a
  differently configured monolithic is how "cachic runs at 62% of nginx" was produced; matched and
  warm, the two are within noise. Run each engine cold, then warm, and report both.
- **Alternate engines, do not run one to completion then the other.** Page-cache state and thermal
  conditions drift, and running them in blocks attributes that drift to the engine.
- **State the working set against the RAM tier.** cachic is at or above parity until the working
  set is several times `CACHE_MEM_SIZE`; the gap only appears past that. A benchmark that fits the
  tier is measuring a different thing.
- **State the hardware, and treat absolutes as local.** These numbers came from WSL2 on a
  virtualised disk with clients on the same box. The ratios travel; the absolutes do not.
- **Take the best of several runs, not the mean.** Throughput noise is one-sided: interference only
  ever makes you slower, so a mean encodes whatever else the machine was doing.

---

## Where to look when a number is surprising

`/metrics` on the admin port carries foyer's own per-stage timings, which is usually enough to
localise a slowdown without a profiler:

```sh
curl -s http://127.0.0.1:9091/metrics | grep -E 'foyer_(hybrid|storage)_.*_(sum|count)'
```

`foyer_hybrid_op_duration` is the whole store operation, `foyer_storage_op_duration` the disk tier,
`foyer_storage_disk_io_duration` the read or write itself, and
`foyer_storage_entry_serde_duration` the decode and checksum. Dividing `_sum` by `_count` per label
gives a mean per stage, which is how the disk read was identified as 83% of a disk-tier hit.

See `docs/metrics.md` for cachic's own series.

---

## Traps

**`127.0.0.1:53` is blackholed under WSL2.** Something intercepts it; a DNS server binds
successfully and never receives a query. Any other loopback address works, which is why the setup
above uses a namespace rather than `127.0.0.2`.

**Podman here cannot apply `--memory`.** `could not find cgroup mount in /proc/self/cgroup`, so an
OOM kill cannot be reproduced locally and `container_memory_working_set_bytes` cannot be read. RSS
from `/proc/<pid>/status` is available and is what the sizing table uses; note that working set
also includes page cache, so the two are not interchangeable.

**A `--report-secs` longer than `--seconds` used to overshoot.** Fixed, but if you see a 5-second
run take 99 seconds, that is what happened.

**Clean up the cache directories between runs.** An 8g disk tier is an 8 GiB file, and two engines
plus repeated runs will fill the disk.

---

## Related

- `docs/sizing.md` — the t-shirt table these measurements produced
- `docs/benchmarks/rc2-dev/README.md` — the numbers themselves, with hardware
- `.agent/tasks/TASK-34-disk-tier-read-amplification.md` — the open question this setup found
- `docs/rc-test-plan.md` section A — the parity protocol for real hardware
