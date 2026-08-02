# silph

silph is a lightweight server monitoring stack akin to Beszel and Prometheus/Grafana.

silph comprises of two main components:

- **Collector**: runs on monitored hosts, exposing an endpoint that when called, will collect and produce metrics.
- **Server**: probes collectors, computes collected data, and stores them in a time series database for efficient storage. Also ships with a web dashboard UI to view that queries and displays metrics.

Design Goals:

- Written in Rust.
- Project should be able to be built with nix (flake.nix) along with a dev shell.
- Collector should be lightweight (low resource usage) and follow a pull model, only collecting/sending data when queried (akin to Prometheus' Node Exporter).
- Dashboard UI should be optional, and should be relatively lightweight with no extra fluff.
- Time series database should be used, but not hand-rolled. Use pre-existing framework/library. Strive for efficiency, both runtime query efficiency and also storage efficiency (encoding).
- Modularity: code should be easily auditable and expandable to add extra potential metrics.
- The response of the collector should be relatively uniform with a bunch of fields titled: `<metric_category>_<name>` along with a value. For example `memory_used` and `memory_free`.
- Minimal computation should be done by the collector, the data it returns can be pretty raw (raw counters) if needed, the idea is that the server will know how to use the data.
- There should be shared code between collector and server so that for each metric, the server knows how to process it. Avoid trying to duplicate logic such that changes to metrics need to be changed across multiple places in the codebase.

Metrics:

- As per modularity: more metrics should be able to be easily added in the future. For the first iteration, just focus on CPU usage, memory usage, disk usage,

Non-goals:

- Cross-platform: focus on supporting Linux hosts.

## Building

With nix:

```sh
nix build            # builds both silph-collector and silph-server
nix develop          # dev shell with the rust toolchain
```

Or plain cargo: `cargo build --release`. The dashboard is a default cargo
feature on silph-server; build with `--no-default-features` to omit it.

## Running

On each monitored host:

```sh
silph-collector --config collector.toml
```

On the monitoring server:

```sh
silph-server --config server.toml
```

See `examples/collector.toml` and `examples/server.toml` for annotated
configs. The dashboard is served at the server's listen address; the query API
lives under `/api/` (`/api/hosts`, `/api/metrics`, `/api/query`).

Sanity-check a collector by hand:

```sh
curl -H "Authorization: Bearer <token>" http://host:9100/metrics | jq
```

## Architecture

- `crates/silph-core` — the shared metric definitions. Each metric is one file
  implementing the `Metric` trait: a `collect` half (runs on the monitored
  host, returns raw values — counters are fine) and a `process` half (runs on
  the server, turns raw snapshots into stored series, e.g. CPU percent from
  jiffy counter deltas). Adding a metric means adding one file and one line in
  the `METRICS` registry; collector, server, API, and dashboard pick it up
  automatically.
- `crates/silph-collector` — axum on a current-thread tokio runtime; reads
  `/proc` directly (no procfs/sysinfo deps). Bearer-token auth.
- `crates/silph-server` — scrape loop per target, storage in
  [tsink](https://github.com/h2337/tsink) (embedded time-series engine:
  Gorilla/delta-of-delta compression, WAL, built-in retention and
  downsampling; version pinned since it is pre-1.0, and all tsink contact is
  isolated in `src/storage.rs`), JSON query API, and the optional dashboard
  (vanilla JS + vendored [uPlot](https://github.com/leeoniya/uPlot)).

## Security notes (v1)

- Collector scrapes are authenticated with a static bearer token.
- Transport is plain HTTP; run TLS through a reverse proxy or a private
  network (the server's HTTP client is built without a TLS stack).
- The server's API/dashboard has no auth: bind it to localhost or put it
  behind an authenticating reverse proxy.
