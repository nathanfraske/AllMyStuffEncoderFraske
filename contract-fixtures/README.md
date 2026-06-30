# KVM ↔ AllMyStuff wire-contract fixtures

These JSON files are the **cross-language contract** between AllMyStuff's Rust
wire types and the NanoKVM mesh bridge's hand-written Go structs. The bridge
mirrors the same shapes in Go; if the two drift, peers **silently** drop the
KVM (a JSON parse error on the receiving end, never a visible failure).

The files are generated — **never edit them by hand**. The single source of
truth is the Rust types in `allmystuff-protocol` and `allmystuff-session`.
Regenerate after any protocol change:

```sh
cargo run -p allmystuff-session --example dump_kvm_fixtures -- contract-fixtures
```

The NanoKVM repo commits a copy under `server/service/mesh/testdata/` and a Go
test round-trips each fixture against its Go structs, pinned to this protocol
version (`allmystuff_protocol::PROTOCOL_VERSION`). Keep the two copies in sync:
when you regenerate here, copy the files into the NanoKVM repo and run its
`go test ./service/mesh/...`.

| Fixture | Type | Channel / socket |
|---|---|---|
| `node_profile_kvm[_claimable]` | `NodeProfile` (KVM) | `allmystuff/presence/v1` |
| `control_kvm_attach` / `control_kvm_detach` | `ControlMessage::Kvm` | `allmystuff/control/v1` |
| `control_ownership_claim` / `_claimed` / `_fleetkey` | `ControlMessage::Ownership` | `allmystuff/control/v1` |
| `control_route_offer_site` / `control_route_accept` | `ControlMessage::Route` | `allmystuff/control/v1` |
| `site_frame_open` / `_data` / `_close` | `SiteFrame` / `SiteEvent` | `allmystuff/media/v1` |
| `capability_screen` / `capability_control` | `Capability` | presence |
| `site_advert`, `inventory_summary` | building blocks | presence |
| `client_id`, `req_*`, `response_ok`, `server_out_channel_inbound` | daemon control socket | `~/.myownmesh/daemon.sock` |
