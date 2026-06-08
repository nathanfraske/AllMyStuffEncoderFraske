//! `allmystuff` — the CLI half of the app.
//!
//! A bare `allmystuff` opens the desktop app (`allmystuff-gui`). The
//! subcommands are for headless boxes, scripts, and CI: scan the machine,
//! show what it would expose on the mesh, or self-update.
//!
//! ```text
//! allmystuff                 # open the desktop app
//! allmystuff scan            # pretty inventory of this machine
//! allmystuff scan --json     # the same, as JSON
//! allmystuff capabilities    # what this machine would expose on the mesh
//! allmystuff update          # update to the latest release
//! ```

mod gui_launch;

use std::process::ExitCode;

use allmystuff_graph::{Catalog, MeshNode, NodeId, Relationship};

fn main() -> ExitCode {
    // Apply any update staged on the previous run before doing anything
    // else — same "stage now, apply on next launch" model as MyOwnMesh.
    allmystuff_updater::apply_pending_if_any();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        // No subcommand → open the desktop app.
        return gui_launch::launch();
    };

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
        "update" => run_update(&args[1..]),
        other => {
            eprintln!("allmystuff: unknown command `{other}`\n");
            print_help();
            ExitCode::FAILURE
        }
    }
}

/// `allmystuff update [check|apply|status|enable|disable] [--json]` —
/// self-update, mirroring `myownmesh update`. A bare `update` fetches the
/// latest release and updates both binaries in one shot.
fn run_update(args: &[String]) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let sub = args
        .first()
        .map(String::as_str)
        .filter(|s| !s.starts_with('-'));

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("allmystuff: couldn't start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result: Result<(), String> = rt.block_on(async {
        match sub {
            None => {
                let outcome = allmystuff_updater::update_now()
                    .await
                    .map_err(|e| e.to_string())?;
                match outcome {
                    allmystuff_updater::UpdateNowOutcome::PackageManager => println!(
                        "Installed by a package manager — use it to update (brew/apt/winget)."
                    ),
                    allmystuff_updater::UpdateNowOutcome::UpToDate { current, .. } => {
                        println!("Already up to date (v{current}).")
                    }
                    allmystuff_updater::UpdateNowOutcome::Updated { to, components } => println!(
                        "Updated to v{to} ({}). Restart to run the new version.",
                        components.join(" + ")
                    ),
                }
                Ok(())
            }
            Some("check") => {
                let outcome = allmystuff_updater::check_now(true)
                    .await
                    .map_err(|e| e.to_string())?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&outcome).unwrap_or_default()
                    );
                } else {
                    println!("{outcome:?}");
                }
                Ok(())
            }
            Some("apply") => {
                match allmystuff_updater::apply_now().map_err(|e| e.to_string())? {
                    Some(v) => println!("Applied v{v}. Restart to run it."),
                    None => println!("No staged update to apply."),
                }
                Ok(())
            }
            Some("status") => {
                let st = allmystuff_updater::status().map_err(|e| e.to_string())?;
                if json {
                    println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default());
                } else {
                    println!("version    {}", st.current_version);
                    println!("install    {:?}", st.install_kind);
                    println!(
                        "auto       {} ({}, every {}h)",
                        st.enabled, st.auto_apply, st.check_interval_hours
                    );
                    println!("channel    {}", st.channel);
                    println!("feed       {}", st.release_url);
                    if let Some(v) = st.staged_version {
                        println!("staged     v{v} (applies on next launch)");
                    }
                }
                Ok(())
            }
            Some("enable") | Some("disable") => {
                let on = sub == Some("enable");
                allmystuff_updater::set_enabled(on).map_err(|e| e.to_string())?;
                println!("Automatic updates {}.", if on { "on" } else { "off" });
                Ok(())
            }
            Some(other) => Err(format!("unknown update subcommand `{other}`")),
        }
    });

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("allmystuff update: {e}");
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
    let caps = allmystuff_bridge::capabilities_from_inventory(&inv, &me);

    // The presence advert this node would broadcast to peers (see
    // `allmystuff_protocol::NodeProfile`).
    let summary = allmystuff_bridge::node_summary(&inv);
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

    With no command, opens the desktop app (allmystuff-gui).

COMMANDS:
    scan            Pretty inventory of this machine
    scan --json     Inventory as JSON
    capabilities    Capabilities this machine would expose on the mesh graph
    update          Update to the latest release (check | apply | status | enable | disable)
    version         Print version
    help            Show this help",
        ver = env!("CARGO_PKG_VERSION")
    );
}
