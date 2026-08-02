# silph

silph is a lightweight server monitoring stack, akin to Beszel or
Prometheus/Grafana, made of two components:

- **silph-collector** runs on each monitored host and exposes a `/metrics`
  endpoint that collects raw metrics (CPU, memory, disk) on demand.
- **silph-server** periodically scrapes collectors, processes the raw data,
  stores it in an embedded time-series database, and serves a query API and
  an optional web dashboard.

Goals: a lightweight pull-model collector, efficient time-series storage,
an optional minimal dashboard, and metrics that are easy to add — one file
each, shared between collector and server. Linux only.

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
configs. The dashboard is served at the server's listen address; the query
API lives under `/api/`.

Scrapes are authenticated with a static bearer token, but transport is
plain HTTP and the dashboard/API has no auth — bind to localhost or put a
reverse proxy in front for TLS/auth.

## NixOS

The flake ships a NixOS module. Add silph as an input, import
`silph.nixosModules.default`, and configure either or both services:

```nix
{
  inputs.silph.url = "github:claby2/silph";
}
```

```nix
# On each monitored host:
services.silph.collector = {
  enable = true;
  settings = { /* same shape as examples/collector.toml */ };
};

# On the monitoring server:
services.silph.server = {
  enable = true;
  settings = { /* same shape as examples/server.toml */ };
};
```

`settings` maps 1:1 to the TOML config (see `examples/`), so anything the
binaries accept can be set without module support; any string value can be
replaced by `{ _secret = "/path"; }`, which loads it from a file at service
start (e.g. an agenix/sops-nix path), keeping it out of the Nix store. The
server's database lives in `/var/lib/silph` by default (override with
`settings.data_dir`); both services run as hardened systemd units under
dynamic users. Remember to open the collector's `listen` port in the
firewall.
