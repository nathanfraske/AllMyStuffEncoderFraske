//! Raw-wire upstream diagnostic; only mesh03_quiet_eof.py launches it.
//! No library session implementation, installed endpoint, peer or media flow.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::prelude::*;
#[cfg(unix)]
use interprocess::local_socket::GenericFilePath;
#[cfg(not(unix))]
use interprocess::local_socket::GenericNamespaced;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const SOURCE: &str = "db7818e09fedd98899490347b86ac9bc9f97b59b";
const ACK_LIMIT: u64 = 16 * 1024;
const REQUEST_WAIT: Duration = Duration::from_secs(3);
const TOTAL_WAIT: Duration = Duration::from_secs(90);
const UNKNOWN_FLOW: &str = "unknown flow_capability: it was never issued to this client, or the flow it named has already been closed";
const INVALID_OWNER: &str = "invalid local client authority";

// Intentionally no Debug/Display/Serialize: C never enters a diagnostic.
struct Owner {
    id: String,
    capability: String,
}

struct PrivateEndpoint(String);

impl PrivateEndpoint {
    async fn connect(&self) -> std::io::Result<LocalSocketStream> {
        #[cfg(unix)]
        let name = std::path::Path::new(&self.0).to_fs_name::<GenericFilePath>()?;
        #[cfg(not(unix))]
        let name = self.0.as_str().to_ns_name::<GenericNamespaced>()?;
        LocalSocketStream::connect(name).await
    }
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("private controller must supply {name}"))
}

async fn exchange(socket: &mut BufReader<LocalSocketStream>, request: Value) -> Value {
    tokio::time::timeout(REQUEST_WAIT, async {
        let mut line =
            serde_json::to_vec(&request).unwrap_or_else(|_| panic!("encode diagnostic request"));
        line.push(b'\n');
        socket
            .get_mut()
            .write_all(&line)
            .await
            .unwrap_or_else(|_| panic!("write private diagnostic request"));
        socket
            .get_mut()
            .flush()
            .await
            .unwrap_or_else(|_| panic!("flush private diagnostic request"));
        let mut reply = Vec::new();
        socket
            .take(ACK_LIMIT)
            .read_until(b'\n', &mut reply)
            .await
            .unwrap_or_else(|_| panic!("read private diagnostic response"));
        assert!(
            reply.last() == Some(&b'\n'),
            "private response must end within the bounded line"
        );
        serde_json::from_slice(&reply)
            .unwrap_or_else(|_| panic!("private diagnostic response must be JSON"))
    })
    .await
    .expect("private diagnostic request deadline")
}

async fn command(endpoint: &PrivateEndpoint, request: Value) -> Value {
    tokio::time::timeout(REQUEST_WAIT, async {
        let stream = endpoint
            .connect()
            .await
            .unwrap_or_else(|_| panic!("connect private diagnostic command socket"));
        exchange(&mut BufReader::new(stream), request).await
    })
    .await
    .expect("private command connect/response deadline")
}

fn check_status(reply: Value) {
    assert!(reply.get("ok").and_then(Value::as_bool) == Some(true));
    assert!(reply.get("error").is_none_or(Value::is_null));
    let data = reply.get("data").expect("private Status data");
    assert!(data.get("version").and_then(Value::as_str) == Some("1.0.0"));
    assert!(data
        .get("joined_networks")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty));
    let realtime = data.get("realtime").expect("private Status realtime advert");
    assert!(realtime.get("supported").and_then(Value::as_bool) == Some(false));
    assert!(realtime
        .get("encodings")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty));
}

async fn events(endpoint: &PrivateEndpoint) -> (BufReader<LocalSocketStream>, Owner) {
    let stream = tokio::time::timeout(REQUEST_WAIT, endpoint.connect())
        .await
        .expect("private Events connection deadline")
        .unwrap_or_else(|_| panic!("connect private Events socket"));
    let mut socket = BufReader::new(stream);
    let reply = exchange(&mut socket, json!({"op": "events_subscribe"})).await;
    assert!(reply.get("ok").and_then(Value::as_bool) == Some(true));
    assert!(reply.get("error").is_none_or(Value::is_null));
    let data = reply.get("data").expect("private Events ACK data");
    assert!(data.get("subscribed").and_then(Value::as_bool) == Some(true));
    let id = data
        .get("client_id")
        .and_then(Value::as_str)
        .expect("private Events id");
    let serial = id
        .strip_prefix('c')
        .and_then(|text| text.parse::<u64>().ok());
    assert!(serial.is_some_and(|serial| id == format!("c{serial}")));
    let capability = data
        .get("client_capability")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty() && text.len() <= 512)
        .expect("private Events capability");
    (
        socket,
        Owner {
            id: id.to_owned(),
            capability: capability.to_owned(),
        },
    )
}

async fn close_never_issued(endpoint: &PrivateEndpoint, owner: &Owner, valid: bool) -> &'static str {
    // No flow-open operation exists in this diagnostic. F cannot name a flow.
    let reply = command(
        endpoint,
        json!({
            "op": "realtime_flow_close",
            "client_id": owner.id,
            "client_capability": if valid { owner.capability.as_str() } else { "not-issued" },
            "flow_capability": "not-issued",
        }),
    )
    .await;
    assert!(reply.get("ok").and_then(Value::as_bool) == Some(false));
    assert!(reply.get("data").is_none_or(Value::is_null));
    match reply.get("error").and_then(Value::as_str) {
        Some(UNKNOWN_FLOW) => "unknown_flow_capability",
        Some(INVALID_OWNER) => "invalid_local_authority",
        _ => panic!("unexpected private close classification; remote text withheld"),
    }
}

#[test]
#[ignore = "requires checksum-pinned private daemon under mesh03_quiet_eof.py"]
fn isolated_quiet_events_eof() {
    let root = PathBuf::from(required("AMS_QUIET_EOF_ROOT"));
    let owner = required("AMS_QUIET_EOF_OWNER");
    assert!(root.is_absolute());
    assert!(root.is_dir() && !root.is_symlink());
    let custody: Value = serde_json::from_slice(
        &std::fs::read(root.join("custody.json")).expect("read private custody marker"),
    )
    .unwrap_or_else(|_| panic!("parse private custody marker"));
    assert!(custody.get("owner").and_then(Value::as_str) == Some(owner.as_str()));
    assert!(custody.get("root").and_then(Value::as_str) == root.to_str());
    let endpoint = PrivateEndpoint(required("AMS_QUIET_EOF_ENDPOINT"));
    #[cfg(unix)]
    assert!(PathBuf::from(&endpoint.0) == root.join("state").join("control.sock"));
    #[cfg(not(unix))]
    assert!(endpoint.0 == format!("ams-eof-{owner}"));
    let quiet_ms: u64 = required("AMS_QUIET_EOF_QUIET_MS")
        .parse()
        .expect("private quiet duration");
    assert!((100..=10_000).contains(&quiet_ms));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            tokio::time::timeout(TOTAL_WAIT, async {
                let startup = Instant::now();
                loop {
                    match tokio::time::timeout(REQUEST_WAIT, endpoint.connect()).await {
                        Ok(Ok(stream)) => {
                            check_status(
                                exchange(&mut BufReader::new(stream), json!({"op": "status"})).await,
                            );
                            break;
                        }
                        _ => {
                            assert!(
                                startup.elapsed() < Duration::from_secs(30),
                                "private daemon startup deadline"
                            );
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
                let (subject, subject_owner) = events(&endpoint).await;
                let (mut observer, _observer_owner) = events(&endpoint).await;
                assert!(
                    close_never_issued(&endpoint, &subject_owner, false).await
                        == "invalid_local_authority"
                );
                assert!(
                    close_never_issued(&endpoint, &subject_owner, true).await
                        == "unknown_flow_capability"
                );

                // Drop the entire raw stream, not merely one split half or a
                // library generation. Every command uses a different socket.
                drop(subject);
                let closed_at = Instant::now();
                let immediate = close_never_issued(&endpoint, &subject_owner, true).await;
                let mut byte = [0_u8; 1];
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(quiet_ms),
                        observer.read(&mut byte),
                    )
                    .await
                    .is_err(),
                    "private observer must remain connected with no traffic during quiet interval"
                );
                let after_quiet = close_never_issued(&endpoint, &subject_owner, true).await;
                check_status(command(&endpoint, json!({"op": "status"})).await);
                drop(observer);

                println!(
                    "MESH03_QUIET_EOF {}",
                    json!({
                        "source": SOURCE,
                        "before_close": "unknown_flow_capability",
                        "wrong_c_control": "invalid_local_authority",
                        "immediate_after_close": immediate,
                        "after_quiet": after_quiet,
                        "quiet_ms": quiet_ms,
                        "elapsed_after_close_ms": closed_at.elapsed().as_millis(),
                        "quiet_observer": true,
                        "flow_open_requests": 0,
                        "gap_reproduced": after_quiet == "unknown_flow_capability",
                        "ack_loss_flow_open": "not_executed_requires_real_flow",
                    })
                );
            })
            .await
            .expect("private quiet-EOF diagnostic total deadline");
        });
}
