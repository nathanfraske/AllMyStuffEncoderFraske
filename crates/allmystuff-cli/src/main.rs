//! `allmystuff` — the headless face of the app.
//!
//! No window, no mesh daemon required: it scans the machine it runs on and
//! shows you what AllMyStuff sees — the raw inventory, or the routable
//! capabilities that inventory becomes on the graph. Handy on a headless
//! box, in CI, or just to sanity-check the scanner.
//!
//! ```text
//! allmystuff                 # pretty inventory of this machine
//! allmystuff scan --json     # the same, as JSON
//! allmystuff capabilities    # what this machine would expose on the mesh
//! ```

mod bridge;

use std::process::ExitCode;

use allmystuff_graph::{Catalog, MeshNode, NodeId, Relationship};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("scan");

    match cmd {
        "-h" | "--help" | "help" => {
            print_help();
            ExitCode::SUCCESS
        }
        "-V" | "--version" | "version" => {
            println!("allmystuff {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "scan" => {
            let json = args.iter().any(|a| a == "--json");
            run_scan(json)
        }
        "capabilities" | "caps" | "graph" => run_capabilities(),
        other => {
            eprintln!("allmystuff: unknown command `{other}`\n");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn run_scan(json: bool) -> ExitCode {
    let inv = allmystuff_inventory::scan();
    if json {
        match serde_json::to_string_pretty(&inv) {
            Ok(s) => {
                println!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("allmystuff: failed to serialize inventory: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        print!("{}", allmystuff_inventory::report::render(&inv));
        ExitCode::SUCCESS
    }
}

fn run_capabilities() -> ExitCode {
    let inv = allmystuff_inventory::scan();
    let me = NodeId::this();
    let caps = bridge::capabilities_from_inventory(&inv, &me);

    // The presence advert this node would broadcast to peers (see
    // `allmystuff_protocol::NodeProfile`).
    let summary = bridge::node_summary(&inv);
    println!(
        "Presence advert  ·  {}  ·  {}  ·  {} devices\n",
        summary.os, summary.cpu, summary.device_count
    );

    println!(
        "This machine would expose {} capabilities on the mesh:\n",
        caps.len()
    );
    for c in &caps {
        let arrow = match c.flow {
            allmystuff_graph::Flow::Source => "→ out",
            allmystuff_graph::Flow::Sink => "in  →",
            allmystuff_graph::Flow::Duplex => "↔",
        };
        println!(
            "  {:<7} {:<8} {arrow:<6}  {}  [{}]",
            c.media.label(),
            c.origin,
            c.label,
            c.id
        );
    }

    // Tiny live demo of the model on real data: stand up a one-node
    // catalog and show that two of your own devices connect freely while
    // the same wire to a *shared* person is gated until you grant it.
    let mut cat = Catalog::new();
    cat.nodes.push(MeshNode::this(&inv.host.hostname));
    cat.capabilities = caps.clone();

    println!("\nThe graph in action (on this machine's real devices):");
    let mic = cat
        .capabilities
        .iter()
        .find(|c| c.origin == "microphone")
        .map(|c| c.id.clone());
    let sys = cat
        .capabilities
        .iter()
        .find(|c| c.origin == "system")
        .map(|c| c.id.clone());
    match (mic, sys) {
        (Some(mic), Some(sys)) => match cat.propose_route(&mic, &sys) {
            Ok(r) => println!("  ✓ {} → {}  ({})", r.from, r.to, r.media.label()),
            Err(e) => println!("  · couldn't wire mic → system audio: {e}"),
        },
        _ => println!(
            "  · no microphone detected here, so there's nothing to route — \
             plug one in and re-run, or try this on your laptop."
        ),
    }

    // Show authorization gating with a synthetic shared peer.
    cat.nodes.push(MeshNode {
        id: "friend".into(),
        label: "A friend's laptop".into(),
        kind: allmystuff_graph::NodeKind::Machine,
        relationship: Relationship::Shared(allmystuff_graph::Share {
            person: allmystuff_graph::Person {
                id: "person:friend".into(),
                name: "your friend".into(),
            },
            grants: vec![],
        }),
        online: false,
    });
    cat.capabilities.push(allmystuff_graph::Capability::new(
        "friend",
        "friend:screen-in",
        "Their screen",
        allmystuff_graph::MediaKind::Display,
        allmystuff_graph::Flow::Sink,
        "display",
    ));
    if let Err(e) = cat.propose_route(&format!("{}:screen", me).into(), &"friend:screen-in".into())
    {
        println!("  ✓ casting your screen to a friend is blocked until you allow it:");
        println!("      {e}");
    }

    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "allmystuff {ver} — map all your stuff.

USAGE:
    allmystuff [COMMAND]

COMMANDS:
    scan            Pretty inventory of this machine (default)
    scan --json     Inventory as JSON
    capabilities    Capabilities this machine would expose on the mesh graph
    version         Print version
    help            Show this help

The desktop app (Tauri + Svelte) adds the graph, mesh joining, and the
share flow on top of exactly these scans. See the README.",
        ver = env!("CARGO_PKG_VERSION")
    );
}
