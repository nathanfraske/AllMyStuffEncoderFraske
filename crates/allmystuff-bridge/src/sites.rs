//! The sites seam: turn discovered listening services into the [`SiteAdvert`]s
//! a node publishes, and the small pure decisions the reverse proxy leans on
//! (the host's port allow-list, the client's local-port mapping).
//!
//! Exposure is **opt-in**: the scan finds every listening port, but a node
//! only ever advertises the ones whose ids the owner explicitly chose. The
//! advertised set is therefore both what peers see *and* the host's
//! allow-list — the proxy refuses to dial any port that isn't on it, so a
//! peer can never pivot to an unadvertised local service.

use std::collections::BTreeSet;

use allmystuff_inventory::Inventory;
use allmystuff_protocol::SiteAdvert;

/// Build the [`SiteAdvert`]s a node should publish, given its scan and the
/// set of listening-service ids (`tcp:8080`) the owner chose to expose.
/// Opt-in by construction: a discovered service not in `exposed` is simply
/// omitted, so a freshly-started dev server never auto-broadcasts and the
/// presence advert only changes when the owner changes their selection.
pub fn sites_from_inventory(inv: &Inventory, exposed: &BTreeSet<String>) -> Vec<SiteAdvert> {
    inv.listening
        .iter()
        .filter(|svc| exposed.contains(&svc.id))
        .map(|svc| SiteAdvert {
            id: svc.id.clone(),
            label: svc.name.clone(),
            port: svc.port,
            scheme: svc.scheme.clone(),
            loopback: svc.loopback,
        })
        .collect()
}

/// The host's allow-list check: may a proxy connection target `port`? Only
/// when `port` appears in the node's *currently advertised* sites. This is
/// the load-bearing control — the client names the port in its `Open`, but
/// the host trusts only its own advert, so a peer can't reach a local
/// service the owner never exposed (a database, an admin panel, SSH).
pub fn port_is_advertised(advertised: &[SiteAdvert], port: u16) -> bool {
    advertised.iter().any(|s| s.port == port)
}

/// Pick the local port to bind when mapping a remote site here. "Direct" —
/// the same number, so `localhost:8080` mirrors the remote's `:8080` — when
/// it's a non-privileged port we haven't already mapped; otherwise "remapped"
/// to the first free port in a high, stable range above the ephemeral churn.
///
/// Pure: `taken` is the set of ports this client has already bound. The OS
/// is the real arbiter (a bind can still lose a race to an unrelated
/// process), so the caller binds the returned port and, on failure, retries
/// with it added to `taken`.
pub fn allocate_local_port(preferred: u16, taken: &BTreeSet<u16>) -> u16 {
    // Privileged ports (<1024) can't be bound without root, so a remote
    // `:443` always remaps into the high range rather than failing forever.
    if preferred >= 1024 && !taken.contains(&preferred) {
        return preferred;
    }
    const BASE: u16 = 47_000;
    (BASE..u16::MAX)
        .find(|p| !taken.contains(p))
        .unwrap_or(BASE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use allmystuff_inventory::{ListeningService, ServiceKind};

    fn svc(id: &str, port: u16, kind: ServiceKind, loopback: bool) -> ListeningService {
        ListeningService {
            id: id.into(),
            name: kind.label().into(),
            port,
            kind,
            scheme: kind.scheme().into(),
            loopback,
            process: String::new(),
        }
    }

    fn inv_with(listening: Vec<ListeningService>) -> Inventory {
        let mut inv = allmystuff_inventory::scan();
        inv.listening = listening;
        inv
    }

    #[test]
    fn advertises_only_the_opted_in_subset() {
        let inv = inv_with(vec![
            svc("tcp:8080", 8080, ServiceKind::Http, true),
            svc("tcp:5432", 5432, ServiceKind::Postgres, true),
            svc("tcp:22", 22, ServiceKind::Ssh, false),
        ]);
        // Only the web app is exposed; the database and SSH stay private.
        let exposed: BTreeSet<String> = ["tcp:8080".to_string()].into_iter().collect();
        let adverts = sites_from_inventory(&inv, &exposed);
        assert_eq!(adverts.len(), 1);
        assert_eq!(adverts[0].port, 8080);
        assert_eq!(adverts[0].label, "HTTP");
        assert_eq!(adverts[0].scheme, "http");
        assert!(adverts[0].loopback);
        assert!(adverts[0].is_web());

        // Nothing opted in → nothing advertised (the default).
        assert!(sites_from_inventory(&inv, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn host_only_proxies_advertised_ports() {
        let advertised = vec![SiteAdvert {
            id: "tcp:8080".into(),
            label: "HTTP".into(),
            port: 8080,
            scheme: "http".into(),
            loopback: true,
        }];
        assert!(port_is_advertised(&advertised, 8080));
        // The classic pivot attempt — a peer asking for SSH / Postgres / the
        // daemon socket — is refused because it was never advertised.
        assert!(!port_is_advertised(&advertised, 22));
        assert!(!port_is_advertised(&advertised, 5432));
        assert!(!port_is_advertised(&[], 8080));
    }

    #[test]
    fn local_port_is_direct_then_remapped() {
        let mut taken = BTreeSet::new();
        // First map of :8080 lands on :8080 (direct).
        assert_eq!(allocate_local_port(8080, &taken), 8080);
        taken.insert(8080u16);
        // A second site that also wants :8080 remaps into the high range.
        let remapped = allocate_local_port(8080, &taken);
        assert!(remapped >= 47_000 && remapped != 8080);
        // A privileged remote port (<1024) never binds directly.
        assert!(allocate_local_port(443, &taken) >= 47_000);
    }
}
