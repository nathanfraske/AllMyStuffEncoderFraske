//! Listening-service discovery — the "sites" source.
//!
//! Two halves, the same split the rest of the crate uses:
//!
//!  * **Pure** functions — classify a well-known port, parse a
//!    `/proc/net/tcp` table, read a service banner — carry the unit tests
//!    and never touch the network or the live filesystem, so they're
//!    verified on every platform regardless of what's actually listening.
//!  * **Active** [`probe_services`] does a short, defensive TCP connect to
//!    refine an unknown port's guess from its banner. It's deliberately
//!    *not* part of [`crate::scan`] (which must stay cheap and
//!    non-blocking) — a caller assembling site adverts runs it off the hot
//!    path.

use std::io::Read as _;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::types::{ListeningService, ServiceKind};

/// One `LISTEN` socket as parsed out of a `/proc/net/tcp[6]` line — the raw
/// shape before classification and per-port dedup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcListen {
    pub port: u16,
    /// Bound only to loopback (127.0.0.0/8 or ::1).
    pub loopback: bool,
    /// Socket inode, for best-effort process-name resolution.
    pub inode: u64,
}

/// TCP `LISTEN` state in `/proc/net/tcp` (`st` column).
const TCP_LISTEN: &str = "0A";

/// Parse the `LISTEN` sockets out of a `/proc/net/tcp` or `/proc/net/tcp6`
/// table. Pure: the kernel's text in, typed rows out. Malformed lines are
/// skipped, never fatal. `ipv6` selects how the local address column is
/// decoded for loopback detection.
pub fn parse_proc_net_tcp(content: &str, ipv6: bool) -> Vec<ProcListen> {
    let mut out = Vec::new();
    for line in content.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // sl local rem st … uid timeout inode  → at least 10 columns.
        if f.len() < 10 || f[3] != TCP_LISTEN {
            continue;
        }
        let Some((addr_hex, port_hex)) = f[1].split_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let inode = f[9].parse().unwrap_or(0);
        out.push(ProcListen {
            port,
            loopback: is_loopback_hex(addr_hex, ipv6),
            inode,
        });
    }
    out
}

/// Collapse the per-protocol rows to one per port — a service commonly
/// binds both IPv4 and IPv6 (and a wildcard `[::]` socket serves v4 too).
/// A port is loopback-only iff *every* bind for it was loopback; the lowest
/// non-zero inode is kept for process resolution. Returns ports ascending.
/// Pure.
pub fn dedupe_by_port(rows: Vec<ProcListen>) -> Vec<ProcListen> {
    use std::collections::BTreeMap;
    let mut by_port: BTreeMap<u16, ProcListen> = BTreeMap::new();
    for r in rows {
        by_port
            .entry(r.port)
            .and_modify(|acc| {
                // Any non-loopback bind makes the port reachable off-box.
                acc.loopback = acc.loopback && r.loopback;
                if acc.inode == 0 || (r.inode != 0 && r.inode < acc.inode) {
                    acc.inode = r.inode;
                }
            })
            .or_insert(r);
    }
    by_port.into_values().collect()
}

/// Is a `/proc/net/tcp` local-address hex a loopback bind (reachable from
/// this machine alone)? A wildcard bind (`0.0.0.0` / `::`) is *not* loopback
/// — it's on every interface. Pure.
fn is_loopback_hex(addr_hex: &str, ipv6: bool) -> bool {
    if ipv6 {
        // /proc/net/tcp6 stores 16 bytes as four little-endian 32-bit
        // words; `::1` lands as 24 zeros then `01000000`.
        addr_hex.eq_ignore_ascii_case("00000000000000000000000001000000")
    } else {
        // IPv4 is one host-order (little-endian on x86) 32-bit word: the
        // kernel prints 127.0.0.1 as "0100007F", whose little-endian bytes
        // [7F,00,00,01] are the real address. Decode and ask is_loopback.
        u32::from_str_radix(addr_hex, 16)
            .map(|raw| Ipv4Addr::from(raw.to_le_bytes()).is_loopback())
            .unwrap_or(false)
    }
}

// ---- macOS (lsof) -----------------------------------------------------

/// One `LISTEN` socket parsed from a line of `lsof -nP -iTCP -sTCP:LISTEN`
/// — the macOS source (there's no `/proc/net/tcp` there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsofListen {
    pub port: u16,
    /// Bound only to loopback (127.0.0.0/8 or ::1).
    pub loopback: bool,
    /// The owning process (lsof's `COMMAND` column).
    pub process: String,
}

/// Build the [`ListeningService`]s from `lsof -nP -iTCP -sTCP:LISTEN`
/// output: parse each `LISTEN` row, then collapse to one service per port (a
/// server usually binds both IPv4 and IPv6). A port is loopback-only iff
/// *every* bind for it was loopback. Pure, so it's fixture-tested without a
/// Mac present.
pub fn services_from_lsof(content: &str) -> Vec<ListeningService> {
    use std::collections::BTreeMap;
    let mut by_port: BTreeMap<u16, (bool, String)> = BTreeMap::new();
    for r in parse_lsof_listen(content) {
        by_port
            .entry(r.port)
            .and_modify(|(lb, proc)| {
                // Any non-loopback bind makes the port reachable off-box.
                *lb = *lb && r.loopback;
                if proc.is_empty() {
                    *proc = r.process.clone();
                }
            })
            .or_insert((r.loopback, r.process));
    }
    by_port
        .into_iter()
        .map(|(port, (loopback, process))| service_from_port(port, loopback, process))
        .collect()
}

/// Parse the `LISTEN` rows out of `lsof -nP -iTCP -sTCP:LISTEN` output, one
/// per (process, bound address). The header row and anything malformed are
/// skipped. Pure.
pub fn parse_lsof_listen(content: &str) -> Vec<LsofListen> {
    content.lines().filter_map(parse_lsof_line).collect()
}

/// One lsof line → its listening socket. The `NAME` column (the bound
/// `addr:port`) is the token right before the trailing `(LISTEN)`, and the
/// `COMMAND` column is the first token — robust to lsof's column spacing.
fn parse_lsof_line(line: &str) -> Option<LsofListen> {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let listen_pos = toks.iter().position(|t| t.starts_with("(LISTEN"))?;
    let process = toks.first()?.to_string();
    let name = toks.get(listen_pos.checked_sub(1)?)?;
    // Split host from port on the *last* colon (IPv6 addresses are bracketed,
    // e.g. `[::1]:5173`, so only the final colon separates the port).
    let (host, port_s) = name.rsplit_once(':')?;
    let port: u16 = port_s.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some(LsofListen {
        port,
        loopback: is_loopback_host(host),
        process,
    })
}

/// Is an lsof / Get-NetTCPConnection host part a loopback bind? `127.x` and
/// `::1` (bracketed or not) are loopback-only; a wildcard (`*`, `0.0.0.0`,
/// `[::]`, `::`) or a specific interface address is reachable off-box. Pure.
fn is_loopback_host(host: &str) -> bool {
    host == "[::1]" || host == "::1" || host.starts_with("127.")
}

// ---- Windows (Get-NetTCPConnection) -----------------------------------

/// Build the [`ListeningService`]s from the rows of a `Get-NetTCPConnection
/// -State Listen | ConvertTo-Json` dump (Windows), already normalised to one
/// JSON object per row (the Windows module's `rows` helper). Reads
/// `LocalAddress`, `LocalPort`, and the owning `Process` name; collapses to
/// one service per port. Pure, so it's fixture-tested without Windows.
pub fn services_from_nettcp_rows(rows: &[serde_json::Value]) -> Vec<ListeningService> {
    use std::collections::BTreeMap;
    let mut by_port: BTreeMap<u16, (bool, String)> = BTreeMap::new();
    for row in rows {
        let Some(port) = nettcp_port(&row["LocalPort"]) else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let addr = row["LocalAddress"].as_str().unwrap_or("");
        let process = row["Process"].as_str().unwrap_or("").trim().to_string();
        let loopback = is_loopback_host(addr);
        by_port
            .entry(port)
            .and_modify(|(lb, proc)| {
                *lb = *lb && loopback;
                if proc.is_empty() {
                    *proc = process.clone();
                }
            })
            .or_insert((loopback, process));
    }
    by_port
        .into_iter()
        .map(|(port, (loopback, process))| service_from_port(port, loopback, process))
        .collect()
}

/// `LocalPort` is a JSON number, but tolerate a stringified one too.
fn nettcp_port(v: &serde_json::Value) -> Option<u16> {
    v.as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Build a [`ListeningService`] for a port from the well-known-port table.
/// `process` is the best-effort owning process name (or empty).
pub fn service_from_port(port: u16, loopback: bool, process: String) -> ListeningService {
    let kind = classify_port(port);
    ListeningService {
        id: format!("tcp:{port}"),
        name: service_name(kind, port),
        port,
        kind,
        scheme: kind.scheme().to_string(),
        loopback,
        process,
        title: String::new(),
    }
}

/// The label for a service: the classified name, or a bare "Port N" for an
/// unclassified one (so the UI never shows a blank).
fn service_name(kind: ServiceKind, port: u16) -> String {
    if kind == ServiceKind::Other {
        format!("Port {port}")
    } else {
        kind.label().to_string()
    }
}

/// Best-effort classification of a listening port from the IANA / common
/// dev-tooling assignments. Pure and exhaustive over the table; everything
/// else is [`ServiceKind::Other`] (still a tunnelable site). An active
/// [`probe_services`] can upgrade an `Other`/`Http` guess from the banner.
pub fn classify_port(port: u16) -> ServiceKind {
    match port {
        22 => ServiceKind::Ssh,
        21 => ServiceKind::Ftp,
        25 | 587 | 465 => ServiceKind::Smtp,
        53 => ServiceKind::Dns,
        443 | 8443 => ServiceKind::Https,
        445 | 139 => ServiceKind::Smb,
        3306 => ServiceKind::Mysql,
        3389 => ServiceKind::Rdp,
        5432 => ServiceKind::Postgres,
        6379 => ServiceKind::Redis,
        27017..=27019 => ServiceKind::Mongodb,
        5900..=5910 => ServiceKind::Vnc,
        // The web sweet spot — server defaults and the popular dev-server
        // ports. A probe confirms, but http is the right default guess.
        80 | 591 | 2375 | 3000 | 4000 | 5000 | 5173 | 8000 | 8008 | 8080 | 8081 | 8088 | 8888
        | 9000 | 9090 => ServiceKind::Http,
        _ => ServiceKind::Other,
    }
}

/// Classify a service from the first bytes it sends (or that we elicit). Pure
/// — `None` when nothing recognisable, so the caller keeps its port-based
/// guess. Recognises the protocols that announce themselves: an HTTP status
/// line, the SSH / FTP / SMTP greetings, and a TLS `ServerHello` record.
pub fn classify_banner(bytes: &[u8]) -> Option<ServiceKind> {
    if bytes.is_empty() {
        return None;
    }
    // TLS record: handshake (0x16), version major 0x03. A server that wants
    // a ClientHello first won't send this unsolicited, but many do alert.
    if bytes.len() >= 3 && bytes[0] == 0x16 && bytes[1] == 0x03 {
        return Some(ServiceKind::Https);
    }
    let head = &bytes[..bytes.len().min(16)];
    let s = String::from_utf8_lossy(head);
    if s.starts_with("HTTP/") {
        return Some(ServiceKind::Http);
    }
    if s.starts_with("SSH-") {
        return Some(ServiceKind::Ssh);
    }
    if s.starts_with("220") && bytes.len() > 4 {
        // FTP and SMTP both open with a 220 greeting; the text disambiguates.
        let lower = s.to_ascii_lowercase();
        if lower.contains("ftp") {
            return Some(ServiceKind::Ftp);
        }
        if lower.contains("smtp") || lower.contains("esmtp") || lower.contains("mail") {
            return Some(ServiceKind::Smtp);
        }
    }
    None
}

/// Refine a list of discovered services by briefly connecting to each on
/// loopback and reading whatever banner it offers, upgrading the guess when
/// the wire says something more specific than the port did. Best-effort and
/// bounded by `timeout` per port; a refused/timed-out probe leaves the
/// service exactly as the port table classified it.
///
/// Deliberately separate from [`crate::scan`]: this opens sockets, so a
/// caller runs it only when it actually wants live confirmation (assembling
/// site adverts), never on the cheap inventory path.
pub fn probe_services(services: &mut [ListeningService], timeout: Duration) {
    for svc in services.iter_mut() {
        // A banner only ever *sharpens* the guess: `classify_banner` returns
        // a recognised protocol or nothing, so it can never knock a
        // confident port match (5432 → Postgres) back down to Other.
        if let Some(kind) = probe_port(svc.port, timeout) {
            svc.kind = kind;
            svc.name = service_name(kind, svc.port);
            svc.scheme = kind.scheme().to_string();
        }
        // For a web site, fetch its page `<title>` — a far better default
        // name than "Port 3000" when the owner exposes it. Plain HTTP only
        // (the probe carries no TLS), so an `https` site keeps its scheme
        // name and the owner types one; the title is purely a suggestion.
        if svc.kind == ServiceKind::Http {
            if let Some(title) = probe_http_title(svc.port, timeout) {
                svc.title = title;
            }
        }
    }
}

/// Connect to `127.0.0.1:port` and classify whatever it says. For an HTTP-
/// looking port we nudge it with a minimal request so a silent server still
/// reveals itself; everything else we just listen. `None` on any failure.
fn probe_port(port: u16, timeout: Duration) -> Option<ServiceKind> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    // A passive read first catches the protocols that greet (SSH/FTP/SMTP/
    // a TLS alert). If nothing comes promptly, poke it like a browser would
    // and read the (likely HTTP) reply.
    let mut buf = [0u8; 256];
    if let Ok(n) = stream.read(&mut buf) {
        if n > 0 {
            if let Some(k) = classify_banner(&buf[..n]) {
                return Some(k);
            }
        }
    }
    use std::io::Write as _;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .ok()?;
    let n = stream.read(&mut buf).ok()?;
    classify_banner(&buf[..n])
}

/// Maximum bytes read while hunting for a web page's `<title>` — enough for
/// the headers + a normal `<head>`, capped so a streaming/huge response
/// can't run the probe long.
const TITLE_READ_CAP: usize = 32 * 1024;

/// Fetch `http://127.0.0.1:port/` and return its HTML `<title>`, if any.
/// Plain HTTP over `std::net` (no TLS, no deps); reads until it has the
/// `</title>`, hits [`TITLE_READ_CAP`], or the timeout/EOF. `None` on any
/// failure — it's only ever a default-name suggestion.
fn probe_http_title(port: u16, timeout: Duration) -> Option<String> {
    use std::io::Write as _;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    stream.set_write_timeout(Some(timeout)).ok()?;
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut body = Vec::new();
    let mut buf = [0u8; 4096];
    while body.len() < TITLE_READ_CAP {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                body.extend_from_slice(&buf[..n]);
                // The title lives in <head>, near the top — stop as soon as
                // we've seen the closing tag.
                if body.windows(8).any(|w| w.eq_ignore_ascii_case(b"</title>")) {
                    break;
                }
            }
        }
    }
    extract_html_title(&String::from_utf8_lossy(&body))
}

/// Pull the trimmed, whitespace-collapsed text of the first `<title>…</title>`
/// out of an HTML document (a couple of the common entities decoded, capped
/// to a sane length). Pure. `None` when there's no non-empty title.
pub fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    // Skip past the rest of the opening tag (it may carry attributes).
    let text_start = open + lower[open..].find('>')? + 1;
    let close_rel = lower[text_start..].find("</title>")?;
    let raw = &html[text_start..text_start + close_rel];

    let decoded = raw
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    // Collapse runs of whitespace (titles often wrap across lines).
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(80).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listen_sockets_and_loopback_scope() {
        // A loopback-only HTTP dev server (127.0.0.1:3000), a wildcard SSH
        // (0.0.0.0:22), and an ESTABLISHED socket that must be ignored.
        let content = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 54321 1 0000 100
   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 11111 1 0000 100
   2: 0100007F:1234 0101A8C0:9F0A 01 00000000:00000000 00:00000000 00000000  1000        0 99999 1 0000 100
";
        let rows = parse_proc_net_tcp(content, false);
        assert_eq!(rows.len(), 2, "only the two LISTEN rows: {rows:?}");
        let dev = rows.iter().find(|r| r.port == 3000).unwrap();
        assert!(dev.loopback, "127.0.0.1 bind is loopback-only");
        assert_eq!(dev.inode, 54321);
        let ssh = rows.iter().find(|r| r.port == 22).unwrap();
        assert!(!ssh.loopback, "0.0.0.0 bind is on every interface");
    }

    #[test]
    fn parses_ipv6_loopback_and_wildcard() {
        let content = "\
  sl  local_address                         remote_address                        st … inode
   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000 0 7777 1 0
   1: 00000000000000000000000000000000:1389 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0 0 8888 1 0
";
        let rows = parse_proc_net_tcp(content, true);
        let lo = rows.iter().find(|r| r.port == 8080).unwrap();
        assert!(lo.loopback, "::1 bind is loopback-only");
        let any = rows.iter().find(|r| r.port == 5001).unwrap();
        assert!(!any.loopback, ":: wildcard is on every interface");
    }

    #[test]
    fn classifies_well_known_and_dev_ports() {
        assert_eq!(classify_port(22), ServiceKind::Ssh);
        assert_eq!(classify_port(5432), ServiceKind::Postgres);
        assert_eq!(classify_port(443), ServiceKind::Https);
        assert_eq!(classify_port(5173), ServiceKind::Http); // vite
        assert_eq!(classify_port(3000), ServiceKind::Http); // node dev
        assert_eq!(classify_port(5901), ServiceKind::Vnc);
        assert_eq!(classify_port(27017), ServiceKind::Mongodb);
        // An arbitrary high port is an unnamed (but tunnelable) service.
        assert_eq!(classify_port(49152), ServiceKind::Other);
    }

    #[test]
    fn service_from_port_labels_and_schemes() {
        let http = service_from_port(8080, true, String::new());
        assert_eq!(http.id, "tcp:8080");
        assert_eq!(http.name, "HTTP");
        assert_eq!(http.scheme, "http");
        assert!(http.loopback);

        let unknown = service_from_port(49152, false, "myserver".into());
        assert_eq!(unknown.name, "Port 49152");
        assert_eq!(unknown.scheme, "");
        assert_eq!(unknown.kind, ServiceKind::Other);
        assert_eq!(unknown.process, "myserver");
    }

    #[test]
    fn dedupes_v4_and_v6_binds_of_one_port() {
        // 8080 listens on both 127.0.0.1 (v4) and ::1 (v6): one loopback-only
        // site. 22 listens on 0.0.0.0 *and* ::1: any non-loopback bind wins,
        // so it's reachable off-box.
        let rows = vec![
            ProcListen {
                port: 8080,
                loopback: true,
                inode: 200,
            },
            ProcListen {
                port: 8080,
                loopback: true,
                inode: 100,
            },
            ProcListen {
                port: 22,
                loopback: false,
                inode: 5,
            },
            ProcListen {
                port: 22,
                loopback: true,
                inode: 9,
            },
        ];
        let merged = dedupe_by_port(rows);
        assert_eq!(merged.len(), 2);
        let p8080 = merged.iter().find(|r| r.port == 8080).unwrap();
        assert!(p8080.loopback);
        assert_eq!(p8080.inode, 100, "lowest non-zero inode kept");
        let p22 = merged.iter().find(|r| r.port == 22).unwrap();
        assert!(!p22.loopback, "a wildcard bind makes the port off-box");
    }

    #[test]
    fn parses_lsof_listen_output() {
        // A real-shaped `lsof -nP -iTCP -sTCP:LISTEN` dump: a Vite dev server
        // on loopback v4+v6 (one site, local-only), an off-box server on *,
        // and the header row (must be skipped).
        let content = "\
COMMAND   PID USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
node    12345 casey   23u  IPv4 0xabc      0t0  TCP 127.0.0.1:5173 (LISTEN)
node    12345 casey   24u  IPv6 0xdef      0t0  TCP [::1]:5173 (LISTEN)
ControlCe 567 casey   18u  IPv4 0x123      0t0  TCP *:7000 (LISTEN)
postgres  890 casey    7u  IPv4 0x456      0t0  TCP 127.0.0.1:5432 (LISTEN)
";
        let services = services_from_lsof(content);
        // Three distinct ports (5173 merged across v4+v6).
        assert_eq!(services.len(), 3, "{services:?}");

        let vite = services.iter().find(|s| s.port == 5173).unwrap();
        assert!(vite.loopback, "127.0.0.1 + ::1 only → local-only");
        assert_eq!(vite.process, "node");
        assert_eq!(vite.kind, ServiceKind::Http);

        let ctrl = services.iter().find(|s| s.port == 7000).unwrap();
        assert!(!ctrl.loopback, "a `*` bind is on every interface");

        let pg = services.iter().find(|s| s.port == 5432).unwrap();
        assert_eq!(pg.kind, ServiceKind::Postgres);
        assert!(pg.loopback);
    }

    #[test]
    fn parses_nettcp_rows() {
        // Get-NetTCPConnection JSON: a dev server on loopback v4+v6 (one
        // local-only site), an off-box game server on 0.0.0.0, and Postgres.
        let rows = vec![
            serde_json::json!({"LocalAddress":"127.0.0.1","LocalPort":5173,"Process":"node"}),
            serde_json::json!({"LocalAddress":"::1","LocalPort":5173,"Process":"node"}),
            serde_json::json!({"LocalAddress":"0.0.0.0","LocalPort":7000,"Process":"Steam"}),
            serde_json::json!({"LocalAddress":"127.0.0.1","LocalPort":5432,"Process":"postgres"}),
        ];
        let services = services_from_nettcp_rows(&rows);
        assert_eq!(services.len(), 3, "{services:?}");

        let vite = services.iter().find(|s| s.port == 5173).unwrap();
        assert!(vite.loopback, "127.0.0.1 + ::1 only → local-only");
        assert_eq!(vite.process, "node");
        assert_eq!(vite.kind, ServiceKind::Http);

        let steam = services.iter().find(|s| s.port == 7000).unwrap();
        assert!(!steam.loopback, "0.0.0.0 is on every interface");

        // A stringified port (some ConvertTo-Json shapes) still parses.
        let stringy = vec![serde_json::json!({
            "LocalAddress":"127.0.0.1","LocalPort":"8080","Process":"caddy"
        })];
        assert_eq!(services_from_nettcp_rows(&stringy)[0].port, 8080);
    }

    #[test]
    fn extracts_html_title() {
        assert_eq!(
            extract_html_title("<html><head><title>My Dev App</title></head>").as_deref(),
            Some("My Dev App")
        );
        // Attributes on the tag, and a title wrapped across lines, collapse.
        assert_eq!(
            extract_html_title("<TITLE data-x='1'>\n  Grafana\n  Dashboard\n</TITLE>").as_deref(),
            Some("Grafana Dashboard")
        );
        // A couple of common entities decode.
        assert_eq!(
            extract_html_title("<title>Tom &amp; Jerry &#39;24</title>").as_deref(),
            Some("Tom & Jerry '24")
        );
        // No title, or an empty one, yields nothing (caller keeps its default).
        assert_eq!(extract_html_title("<html><body>hi</body></html>"), None);
        assert_eq!(extract_html_title("<title>   </title>"), None);
    }

    #[test]
    fn lsof_host_loopback_detection() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.1.2.3"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("*"));
        assert!(!is_loopback_host("0.0.0.0"));
        assert!(!is_loopback_host("[::]"));
        assert!(!is_loopback_host("192.168.1.5"));
    }

    #[test]
    fn classifies_banners() {
        assert_eq!(
            classify_banner(b"HTTP/1.1 200 OK\r\n"),
            Some(ServiceKind::Http)
        );
        assert_eq!(
            classify_banner(b"SSH-2.0-OpenSSH_9.6\r\n"),
            Some(ServiceKind::Ssh)
        );
        assert_eq!(
            classify_banner(b"220 mail.example.com ESMTP Postfix"),
            Some(ServiceKind::Smtp)
        );
        assert_eq!(
            classify_banner(b"220 (vsFTPd 3.0.5)"),
            Some(ServiceKind::Ftp)
        );
        // A TLS handshake record (0x16 0x03 ..).
        assert_eq!(
            classify_banner(&[0x16, 0x03, 0x03, 0x00, 0x2a]),
            Some(ServiceKind::Https)
        );
        // Noise / nothing recognisable keeps the port-based guess.
        assert_eq!(classify_banner(b""), None);
        assert_eq!(classify_banner(&[0x00, 0x01, 0x02]), None);
    }
}
