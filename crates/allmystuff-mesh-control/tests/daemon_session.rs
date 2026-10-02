//! Opt-in actual-daemon check, launched only by mesh-control-isolated.py.
//! There is no default endpoint, daemon discovery or process launch here.

use std::path::PathBuf;
use std::time::Duration;

use allmystuff_mesh_control::contract::{verify_wire_status, StatusEvidence};
use allmystuff_mesh_control::{
    ControlClient, Endpoint, EventContract, EventRegistration, Request, StatusProbe,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

const DEADLINE: Duration = Duration::from_secs(120);

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("isolated harness must supply {name}"))
}

async fn status(client: &ControlClient, contract: EventContract) -> StatusEvidence {
    loop {
        match client.probe_status().await {
            StatusProbe::Answered(response) => {
                return verify_wire_status(&response, contract)
                    .expect("owned daemon must answer the selected wire Status");
            }
            StatusProbe::Absent | StatusProbe::Unavailable(_) => {
                // Only this explicitly launched private endpoint is retried.
                // The outer deadline bounds startup and the owned restart.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn channel(client: &ControlClient, registration: &EventRegistration, network: &str) {
    let response = client
        .subscribe_channel(registration, network, "isolated-session-check")
        .await
        .expect("owned channel command must complete");
    match registration.contract() {
        EventContract::LegacyV0_3_21 => {
            assert!(response.ok, "private legacy network subscription refused");
            assert_eq!(
                response
                    .data
                    .as_ref()
                    .and_then(|data| data.get("subscribed")),
                Some(&Value::Bool(true))
            );
            let released = client
                .request_for_registration(
                    registration,
                    &Request::ChannelUnsubscribe {
                        client_id: registration.client_id(),
                        network: network.to_owned(),
                        channel: "isolated-session-check".to_owned(),
                    },
                )
                .await
                .expect("release owned legacy channel");
            assert!(released.ok, "private legacy channel release refused");
            assert!(
                released
                    .data
                    .as_ref()
                    .and_then(|data| data.get("unsubscribed"))
                    == Some(&Value::Bool(true)),
                "private legacy channel release must be acknowledged"
            );
        }
        EventContract::CandidateV1Db7818e => {
            // Infrastructure-only has no joined networks. The exact refusal
            // proves C passed the daemon's authority check, which precedes its
            // network lookup. This is not successful channel installation.
            assert!(!response.ok);
            let expected = format!("unknown network: {network}");
            assert!(
                response.error.as_deref() == Some(expected.as_str()),
                "candidate must return the exact authenticated network refusal"
            );
        }
    }
}

#[test]
#[ignore = "requires checksum-pinned owned daemon and mesh-control-isolated.py custody"]
fn isolated_daemon_sessions() {
    let root = PathBuf::from(required("AMS_MESH_CONTROL_ROOT"));
    assert!(root.is_absolute(), "owned root must be absolute");
    let owner = required("AMS_MESH_CONTROL_OWNER");
    let custody: Value =
        serde_json::from_slice(&std::fs::read(root.join("custody.json")).unwrap()).unwrap();
    assert_eq!(
        custody.get("owner").and_then(Value::as_str),
        Some(owner.as_str())
    );
    assert_eq!(custody.get("root").and_then(Value::as_str), root.to_str());
    let contract = match required("AMS_MESH_CONTROL_CONTRACT").as_str() {
        "legacy" => EventContract::LegacyV0_3_21,
        "candidate" => EventContract::CandidateV1Db7818e,
        _ => panic!("harness must select a reviewed wire contract"),
    };
    let endpoint_text = required("AMS_MESH_CONTROL_ENDPOINT");
    #[cfg(unix)]
    let endpoint = {
        let path = PathBuf::from(&endpoint_text);
        assert_eq!(path, root.join("state").join("control.sock"));
        Endpoint::Path(path)
    };
    #[cfg(not(unix))]
    let endpoint = {
        assert_eq!(endpoint_text, format!("ams-mc-{owner}"));
        Endpoint::Name(endpoint_text)
    };
    let network = required("AMS_MESH_CONTROL_NETWORK");
    assert_eq!(network, format!("ams-mc-{owner}"));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(DEADLINE, async {
                let client = ControlClient::new(endpoint);
                let first_status = status(&client, contract).await;
                match contract {
                    EventContract::LegacyV0_3_21 => {
                        assert_eq!(first_status.joined_networks, vec![network.clone()]);
                    }
                    EventContract::CandidateV1Db7818e => {
                        assert!(first_status.joined_networks.is_empty());
                        let advert = first_status.realtime.as_ref().unwrap();
                        assert!(!advert.supported);
                        assert!(advert.encodings.is_empty());
                    }
                }

                let (tx_a, mut rx_a) = mpsc::channel(16);
                let first = client
                    .subscribe_events_for_contract(contract, tx_a)
                    .await
                    .unwrap();
                let old = first.registration();
                assert!(old.is_active());
                channel(&client, &old, &network).await;

                let (tx_b, mut rx_b) = mpsc::channel(16);
                let second = client
                    .subscribe_events_for_contract(contract, tx_b)
                    .await
                    .unwrap();
                let current = second.registration();
                assert_ne!(old.generation(), current.generation());
                while rx_a.recv().await.is_some() {}
                drop(first);
                assert!(client.validate_registration(&old).is_err());
                assert!(client
                    .subscribe_channel(&old, &network, "stale-check")
                    .await
                    .is_err());
                assert!(current.is_active());
                channel(&client, &current, &network).await;
                second.close().await;
                while rx_b.recv().await.is_some() {}
                assert!(client.validate_registration(&current).is_err());

                let (tx_c, mut rx_c) = mpsc::channel(16);
                let before_restart = client
                    .subscribe_events_for_contract(contract, tx_c)
                    .await
                    .unwrap();
                let retired = before_restart.registration();
                let ready_path = root.join("restart-ready");
                let writing_path = root.join("restart-ready-writing");
                let mut ready = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&writing_path)
                    .unwrap();
                use std::io::Write;
                ready.write_all(owner.as_bytes()).unwrap();
                ready.flush().unwrap();
                drop(ready);
                // Publish the complete marker atomically; the controller must
                // never observe a newly created but still empty marker file.
                std::fs::rename(writing_path, ready_path).unwrap();
                // Harness stops and awaits only its owned daemon, then launches
                // the same checksum-pinned binary with the same private state.
                while rx_c.recv().await.is_some() {}
                assert!(client.validate_registration(&retired).is_err());

                let after_status = status(&client, contract).await;
                assert_eq!(first_status.device_id, after_status.device_id);
                let (tx_d, mut rx_d) = mpsc::channel(16);
                let after_restart = client
                    .subscribe_events_for_contract(contract, tx_d)
                    .await
                    .unwrap();
                let renewed = after_restart.registration();
                assert_ne!(retired.generation(), renewed.generation());
                drop(before_restart);
                assert!(renewed.is_active());
                assert!(client
                    .subscribe_channel(&retired, &network, "stale-check")
                    .await
                    .is_err());
                channel(&client, &renewed, &network).await;
                after_restart.close().await;
                while rx_d.recv().await.is_some() {}
                assert!(client.validate_registration(&renewed).is_err());

                println!(
                    "MESH_CONTROL_ISOLATED {}",
                    json!({
                        "contract": match contract {
                            EventContract::LegacyV0_3_21 => "legacy",
                            EventContract::CandidateV1Db7818e => "candidate",
                        },
                        "sessions": 4,
                        "channel_checks": 3,
                        "channel_result": if contract == EventContract::LegacyV0_3_21 {
                            "subscribed_and_released"
                        } else {
                            "authenticated_unknown_network_refusal"
                        },
                        "renewal": true,
                        "owned_process_restart": true,
                        "stable_private_identity": true,
                        "stale_refusal": true,
                        "awaited_local_close": true,
                    })
                );
            })
            .await
            .expect("isolated daemon lifecycle deadline");
        });
}
