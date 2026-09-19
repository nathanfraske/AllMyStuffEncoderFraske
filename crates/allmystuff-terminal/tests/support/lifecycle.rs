//! Real PTY regressions for the host extracted from ee9cc150f4bdc3fd49a547b563f0381dae3685f5.
//! Original node/src/terminal.rs blob: 2e6c0a323bc921264812048251554bc9a5bfeb5b.
//! Included as a cfg(test) child of host.rs to use the existing private open_with seam.
//! All commands, routes, sessions and directories belong to the current fixture.
//! No default shell discovery, node runtime registration, Mesh, or ordinary state.

#![cfg(any(unix, windows))]

use super::{OutMsg, TermAttach};
use portable_pty::CommandBuilder;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::{error::TryRecvError, Receiver};

const STEP_LIMIT: Duration = Duration::from_secs(20);
const CLEANUP_LIMIT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_TRANSCRIPT: usize = 1024 * 1024;

struct LifecycleSpawner;

impl crate::TaskSpawner for LifecycleSpawner {
    fn spawn<F>(future: F) -> tokio::task::JoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
        RUNTIME
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()
                    .expect("fixture runtime")
            })
            .spawn(future)
    }
}

type FixtureHost = super::TerminalHost<LifecycleSpawner>;

struct PrivateDirectory {
    path: PathBuf,
    parent: PathBuf,
    preserve: bool,
}

impl PrivateDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = shell_compatible_path(
            std::env::temp_dir()
                .canonicalize()
                .expect("temporary parent"),
        );
        for _ in 0..4096 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                "ams-terminal-lifecycle-{}-{sequence}",
                std::process::id()
            ));
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&path) {
                Ok(()) => {
                    return Self {
                        path,
                        parent,
                        preserve: false,
                    };
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create exclusive terminal fixture: {error}"),
            }
        }
        panic!("could not reserve an exclusive terminal fixture directory");
    }
}

impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        if self.preserve {
            return;
        }
        // Only the exact directory reserved by create_dir above is recursive.
        assert_eq!(self.path.parent(), Some(self.parent.as_path()));
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if std::thread::panicking() {
                eprintln!(
                    "fixture directory cleanup failed at {:?}: {error}",
                    self.path
                );
            } else {
                panic!(
                    "fixture directory cleanup failed at {:?}: {error}",
                    self.path
                );
            }
        }
    }
}

struct OwnedSession {
    id: String,
    cleanup_rx: Receiver<OutMsg>,
}

struct Fixture {
    host: FixtureHost,
    owned: Vec<OwnedSession>,
    directory: PrivateDirectory,
}

impl Fixture {
    fn new() -> Self {
        Self {
            host: FixtureHost::new(),
            owned: Vec::new(),
            directory: PrivateDirectory::new(),
        }
    }

    fn open(
        &mut self,
        session: &str,
        route: &str,
        cols: u16,
        rows: u16,
        commands: Vec<CommandBuilder>,
    ) -> TermAttach {
        let attach = self
            .host
            .open_with(Some(session), route, cols, rows, commands)
            .expect("open explicitly commanded fixture PTY");
        if attach.created {
            // Register cleanup before any test assertion can panic. The
            // receiver observes when the reader/wait output senders have gone.
            self.owned.push(OwnedSession {
                id: session.to_owned(),
                cleanup_rx: attach.rx.resubscribe(),
            });
        }
        attach
    }

    fn start(&mut self, session: &str, route: &str) -> Viewer {
        let commands = protocol_commands(&self.directory.path);
        let attach = self.open(session, route, 100, 40, commands);
        assert!(attach.created);
        assert!(attach.scrollback.is_empty());
        let mut viewer = Viewer::new(attach);
        viewer.wait_marker(b"AMS:READY");
        viewer
    }

    fn attach(&mut self, session: &str, route: &str) -> Viewer {
        // An existing session must ignore candidates entirely. An accidental
        // fresh spawn fails rather than discovering the user's normal shell.
        let attach = self.open(session, route, 80, 24, Vec::new());
        assert!(!attach.created);
        assert_eq!(attach.session_id, session);
        Viewer::new(attach)
    }

    fn send(&self, route: &str, line: &str) {
        let mut bytes = line.as_bytes().to_vec();
        #[cfg(windows)]
        bytes.push(b'\r');
        #[cfg(unix)]
        bytes.push(b'\n');
        assert!(self.host.write(route, bytes), "input refused for {route}");
    }

    fn close(&self, session: &str) {
        self.host.close(session);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // close is synchronous and targets only ids created in this host.
        // It kills directly before asking the control thread to shut down.
        // Run on both success and panic, including after the last detach.
        for owned in &self.owned {
            self.host.close(&owned.id);
        }
        let deadline = Instant::now() + CLEANUP_LIMIT;
        for owned in &mut self.owned {
            loop {
                match owned.cleanup_rx.try_recv() {
                    Err(TryRecvError::Closed) => break,
                    Ok(_) | Err(TryRecvError::Lagged(_)) => {}
                    Err(TryRecvError::Empty) => std::thread::sleep(POLL_INTERVAL),
                }
                if Instant::now() >= deadline {
                    // Never delete a cwd whose child shutdown was not observed.
                    self.directory.preserve = true;
                    let message = format!(
                        "fixture PTY {:?} did not close; retained {:?}",
                        owned.id, self.directory.path
                    );
                    if std::thread::panicking() {
                        eprintln!("{message}");
                        return;
                    }
                    panic!("{message}");
                }
            }
        }
    }
}

struct Viewer {
    replay: Vec<u8>,
    bytes: Vec<u8>,
    rx: Receiver<OutMsg>,
    sizes: Vec<(u16, u16)>,
    exits: Vec<Option<i32>>,
    closed: bool,
}

impl Viewer {
    fn new(attach: TermAttach) -> Self {
        Self {
            bytes: attach.scrollback.clone(),
            replay: attach.scrollback,
            rx: attach.rx,
            sizes: Vec::new(),
            exits: Vec::new(),
            closed: false,
        }
    }

    fn poll(&mut self) {
        match self.rx.try_recv() {
            Ok(OutMsg::Data(bytes)) => {
                self.bytes.extend_from_slice(&bytes);
                assert!(self.bytes.len() <= MAX_TRANSCRIPT, "runaway fixture output");
            }
            Ok(OutMsg::Resize { cols, rows }) => self.sizes.push((cols, rows)),
            Ok(OutMsg::Exit(code)) => self.exits.push(code),
            Err(TryRecvError::Closed) => self.closed = true,
            Err(TryRecvError::Lagged(count)) => panic!("fixture lost {count} output messages"),
            Err(TryRecvError::Empty) => std::thread::sleep(POLL_INTERVAL),
        }
    }

    fn wait(&mut self, what: &str, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + STEP_LIMIT;
        while !predicate(self) {
            assert!(
                !self.closed && Instant::now() < deadline,
                "waiting for {what}; closed={}, exits={:?}, transcript={:?}",
                self.closed,
                self.exits,
                String::from_utf8_lossy(&self.bytes)
            );
            self.poll();
        }
    }

    fn wait_marker(&mut self, marker: &[u8]) {
        self.wait(&String::from_utf8_lossy(marker), |viewer| {
            contains(&viewer.bytes, marker)
        });
    }

    fn wait_size(&mut self, size: (u16, u16)) {
        self.wait("resize broadcast", |viewer| viewer.sizes.contains(&size));
    }

    fn wait_exit(&mut self) {
        self.wait("authoritative child exit", |viewer| {
            !viewer.exits.is_empty()
        });
    }

    fn wait_closed(&mut self) {
        self.wait("all PTY output senders to close", |viewer| viewer.closed);
    }
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

fn isolate_command(mut command: CommandBuilder, cwd: &Path) -> CommandBuilder {
    command.cwd(cwd);
    // These are per-child values, never process-global environment changes.
    command.env("HOME", cwd);
    command.env("USERPROFILE", cwd);
    command.env("XDG_CONFIG_HOME", cwd);
    command.env("ENV", "");
    command.env("BASH_ENV", "");
    command.env("TERM", "xterm-256color");
    command
}

fn shell_compatible_path(path: PathBuf) -> PathBuf {
    // canonicalize uses a verbatim disk prefix on Windows. cmd interprets
    // that prefix as an unsupported UNC cwd and may switch to Windows' own
    // directory. Keep the exact resolved disk location in ordinary form.
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            if let Prefix::VerbatimDisk(drive) = prefix.kind() {
                let mut ordinary = PathBuf::from(format!("{}:\\", char::from(drive)));
                ordinary.extend(path.components().skip(2));
                return ordinary;
            }
            assert!(
                matches!(prefix.kind(), Prefix::Disk(_)),
                "cmd fixture requires a local disk temporary directory: {path:?}"
            );
        }
    }
    path
}

#[cfg(windows)]
fn system_program(relative: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").expect("Windows SystemRoot");
    let path = PathBuf::from(root).join("System32").join(relative);
    assert!(
        path.is_absolute() && path.is_file(),
        "missing system program {path:?}"
    );
    path
}

#[cfg(windows)]
fn protocol_commands(cwd: &Path) -> Vec<CommandBuilder> {
    // Only cmd builtins: no descendants survive an assertion or forced close.
    // Input is a short verb; none contains its response marker. PTY input echo
    // therefore cannot satisfy a response assertion without shell execution.
    let script = concat!(
        "@echo off\r\n",
        "set \"AMS_MEMORY=empty\"\r\n",
        "echo AMS:READY\r\n",
        ":again\r\n",
        "set \"AMS_LINE=\"\r\n",
        "set /p \"AMS_LINE=\"\r\n",
        "if \"%AMS_LINE%\"==\"alpha\" echo AMS:ALPHA\r\n",
        "if \"%AMS_LINE%\"==\"beta\" echo AMS:BETA\r\n",
        "if \"%AMS_LINE%\"==\"before\" echo AMS:BEFORE\r\n",
        "if \"%AMS_LINE%\"==\"after\" echo AMS:AFTER\r\n",
        "if \"%AMS_LINE%\"==\"confirm\" echo AMS:CONFIRM\r\n",
        "if \"%AMS_LINE%\"==\"remember\" set \"AMS_MEMORY=kept\"\r\n",
        "if \"%AMS_LINE%\"==\"remember\" echo AMS:REMEMBERED\r\n",
        "if \"%AMS_LINE%\"==\"recall\" echo AMS:MEMORY:%AMS_MEMORY%\r\n",
        "if \"%AMS_LINE%\"==\"quit\" (\r\n",
        "  echo AMS:GOODBYE\r\n",
        "  exit /b 7\r\n",
        ")\r\n",
        "goto again\r\n"
    );
    fs::write(cwd.join("fixture.cmd"), script).expect("write private command script");
    let mut command = CommandBuilder::new(system_program("cmd.exe"));
    command.args(["/D", "/Q", "/C", "fixture.cmd"]);
    vec![isolate_command(command, cwd)]
}

#[cfg(unix)]
fn protocol_commands(cwd: &Path) -> Vec<CommandBuilder> {
    let script = concat!(
        "memory=empty\n",
        "printf 'AMS:READY\\n'\n",
        "while IFS= read -r line; do\n",
        "  case \"$line\" in\n",
        "    alpha) printf 'AMS:ALPHA\\n';;\n",
        "    beta) printf 'AMS:BETA\\n';;\n",
        "    before) printf 'AMS:BEFORE\\n';;\n",
        "    after) printf 'AMS:AFTER\\n';;\n",
        "    confirm) printf 'AMS:CONFIRM\\n';;\n",
        "    remember) memory=kept; printf 'AMS:REMEMBERED\\n';;\n",
        "    recall) printf 'AMS:MEMORY:%s\\n' \"$memory\";;\n",
        "    quit) printf 'AMS:GOODBYE\\n'; exit 7;;\n",
        "  esac\n",
        "done\n"
    );
    fs::write(cwd.join("fixture.sh"), script).expect("write private command script");
    let mut command = CommandBuilder::new("/bin/sh");
    command.arg("fixture.sh");
    vec![isolate_command(command, cwd)]
}

#[cfg(windows)]
fn size_commands(cwd: &Path) -> Vec<CommandBuilder> {
    // A fixed system PowerShell command, not the user's configured shell.
    // Console reads the child-visible ConPTY window dimensions without parsing
    // localized `mode con` output or spawning another child. No profile or
    // execution-policy override is used; the script is an explicit argument.
    let mut command = CommandBuilder::new(system_program("WindowsPowerShell/v1.0/powershell.exe"));
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
    command.arg(concat!(
        "$ErrorActionPreference='Stop'; ",
        "[Console]::WriteLine('AMS:READY'); ",
        "while (($line=[Console]::ReadLine()) -ne $null) { ",
        "if ($line -eq 'size') { ",
        "[Console]::WriteLine(('AMS:SIZE:{0}:{1}:END' -f ",
        "[Console]::WindowWidth,[Console]::WindowHeight)) ",
        "} }"
    ));
    vec![isolate_command(command, cwd)]
}

#[cfg(unix)]
fn size_commands(cwd: &Path) -> Vec<CommandBuilder> {
    // stty is the existing Unix test's native observation. Choose a fixed
    // system path, never an executable from the user's PATH or fixture cwd.
    let stty = if Path::new("/bin/stty").is_file() {
        "/bin/stty"
    } else {
        "/usr/bin/stty"
    };
    assert!(
        Path::new(stty).is_file(),
        "native resize fixture requires stty"
    );
    let script = format!(
        "printf 'AMS:READY\\n'\nwhile IFS= read -r line; do\n  if [ \"$line\" = size ]; then\n    dimensions=$({stty} size)\n    set -- $dimensions\n    printf 'AMS:SIZE:%s:%s:END\\n' \"$2\" \"$1\"\n  fi\ndone\n"
    );
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", &script]);
    vec![isolate_command(command, cwd)]
}

#[test]
fn multi_attach_fans_out_and_shared_input_reaches_one_shell() {
    let mut fixture = Fixture::new();
    let mut a = fixture.start("shared", "a");
    let mut b = fixture.attach("shared", "b");
    let sessions = fixture.host.list_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].attachers, 2);

    fixture.send("a", "remember");
    a.wait_marker(b"AMS:REMEMBERED");
    b.wait_marker(b"AMS:REMEMBERED");
    fixture.send("b", "recall");
    a.wait_marker(b"AMS:MEMORY:kept");
    b.wait_marker(b"AMS:MEMORY:kept");
    fixture.send("a", "alpha");
    a.wait_marker(b"AMS:ALPHA");
    b.wait_marker(b"AMS:ALPHA");
    fixture.send("b", "beta");
    a.wait_marker(b"AMS:BETA");
    b.wait_marker(b"AMS:BETA");
}

#[test]
fn scrollback_then_live_output_has_no_gap_or_duplicate() {
    let mut fixture = Fixture::new();
    let mut a = fixture.start("replay", "a");
    assert!(
        a.replay.is_empty(),
        "first observer receives only live bytes"
    );
    fixture.send("a", "before");
    a.wait_marker(b"AMS:BEFORE");
    let mut b = fixture.attach("replay", "b");
    assert!(contains(&b.replay, b"AMS:BEFORE"));
    assert!(!contains(&b.replay, b"AMS:AFTER"));

    fixture.send("a", "after");
    a.wait_marker(b"AMS:AFTER");
    b.wait_marker(b"AMS:AFTER");
    // A later shell acknowledgement is a causal fence for prior output.
    assert!(!contains(&a.bytes, b"AMS:CONFIRM"));
    assert!(!contains(&b.bytes, b"AMS:CONFIRM"));
    fixture.send("b", "confirm");
    a.wait_marker(b"AMS:CONFIRM");
    b.wait_marker(b"AMS:CONFIRM");
    // ConPTY can repaint prior text after the attach's native resize. Preserve
    // every raw byte and compare both observers at the same fresh fence: any
    // extra, lost or reordered byte at the replay/live split must still fail.
    assert!(
        a.bytes.len() < super::SCROLLBACK_CAP,
        "fixture transcript must fit entirely in scrollback"
    );
    assert!(
        a.bytes.starts_with(&b.replay),
        "replay must be an exact prefix"
    );
    assert_eq!(
        a.bytes, b.bytes,
        "replay plus live must equal original live output"
    );
    let transcript = String::from_utf8_lossy(&b.bytes);
    assert!(transcript.find("AMS:BEFORE") < transcript.find("AMS:AFTER"));
    assert!(transcript.find("AMS:AFTER") < transcript.find("AMS:CONFIRM"));
}

#[test]
fn detach_one_viewer_keeps_the_shell_and_removes_only_its_route_queue() {
    let mut fixture = Fixture::new();
    let mut a = fixture.start("detach-one", "a");
    let mut b = fixture.attach("detach-one", "b");
    fixture.send("a", "remember");
    a.wait_marker(b"AMS:REMEMBERED");
    fixture.host.ensure_queue("a");
    fixture.host.ensure_queue("b");
    assert!(fixture.host.enqueue("a", b"queued-a".to_vec()));
    assert!(fixture.host.enqueue("b", b"queued-b".to_vec()));

    fixture.host.detach("a");
    assert!(!fixture.host.is_attached("a"));
    assert!(fixture.host.is_attached("b"));
    assert!(!fixture.host.write("a", b"ignored".to_vec()));
    assert!(!fixture.host.resize("a", 10, 10));
    assert!(fixture.host.poll("a").is_empty());
    assert_eq!(fixture.host.poll("b"), b"\x08\x00\x00\x00queued-b");
    assert_eq!(fixture.host.list_sessions()[0].attachers, 1);
    fixture.send("b", "recall");
    b.wait_marker(b"AMS:MEMORY:kept");
}

#[test]
fn last_detach_then_reattach_preserves_scrollback_and_shell_memory() {
    let mut fixture = Fixture::new();
    let mut a = fixture.start("detach-last", "a");
    fixture.send("a", "remember");
    a.wait_marker(b"AMS:REMEMBERED");
    fixture.host.detach("a");
    assert!(!fixture.host.is_attached("a"));
    let sessions = fixture.host.list_sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].attachers, 0);

    let mut b = fixture.attach("detach-last", "b");
    assert!(contains(&b.replay, b"AMS:REMEMBERED"));
    fixture.send("b", "recall");
    b.wait_marker(b"AMS:MEMORY:kept");
    assert_eq!(fixture.host.list_sessions()[0].attachers, 1);
}

#[test]
fn native_size_preserves_placeholder_then_reconciles_minimum_and_detach() {
    let mut fixture = Fixture::new();
    let commands = size_commands(&fixture.directory.path);
    let attach = fixture.open("size", "a", 100, 40, commands);
    let mut a = Viewer::new(attach);
    a.wait_marker(b"AMS:READY");
    let mut b = fixture.attach("size", "b");
    fixture.send("a", "size");
    a.wait_marker(b"AMS:SIZE:100:40:END");
    b.wait_marker(b"AMS:SIZE:100:40:END");
    assert!(
        a.sizes.is_empty(),
        "placeholder attach must not shrink the PTY"
    );
    assert!(b.sizes.is_empty());

    assert!(fixture.host.resize("b", 80, 50));
    a.wait_size((80, 40));
    b.wait_size((80, 40));
    // Resize and Data go through the same control queue, so the size query is
    // ordered after the actual native resize without a guessed sleep interval.
    fixture.send("b", "size");
    a.wait_marker(b"AMS:SIZE:80:40:END");
    b.wait_marker(b"AMS:SIZE:80:40:END");

    assert!(fixture.host.resize("a", 120, 60));
    a.wait_size((80, 50));
    b.wait_size((80, 50));
    fixture.send("a", "size");
    a.wait_marker(b"AMS:SIZE:80:50:END");
    b.wait_marker(b"AMS:SIZE:80:50:END");
    fixture.host.detach("b");
    assert!(!fixture.host.is_attached("b"));
    a.wait_size((120, 60));
    fixture.send("a", "size");
    a.wait_marker(b"AMS:SIZE:120:60:END");
    assert_eq!(a.sizes, [(80, 40), (80, 50), (120, 60)]);
    assert_eq!(b.sizes, [(80, 40), (80, 50)]);
}

#[test]
fn explicit_close_ends_waiting_shell_and_all_attached_routes() {
    let mut fixture = Fixture::new();
    let mut a = fixture.start("close", "a");
    let mut b = fixture.attach("close", "b");
    fixture.close("close");
    fixture.close("close");
    assert!(fixture.host.list_sessions().is_empty());
    for route in ["a", "b"] {
        assert!(!fixture.host.is_attached(route));
        assert!(!fixture.host.write(route, b"ignored".to_vec()));
        assert!(!fixture.host.resize(route, 80, 24));
    }
    a.wait_closed();
    b.wait_closed();
    assert_eq!(a.exits.len(), 1);
    assert_eq!(b.exits, a.exits);
}

#[test]
fn natural_exit_reports_code_and_leaves_maps_until_explicit_close() {
    let mut fixture = Fixture::new();
    let mut a = fixture.start("exit", "a");
    let mut b = fixture.attach("exit", "b");
    fixture.send("b", "quit");
    a.wait_exit();
    b.wait_exit();
    assert_eq!(a.exits, [Some(7)]);
    assert_eq!(b.exits, [Some(7)]);
    assert!(contains(&a.bytes, b"AMS:GOODBYE"));
    assert!(contains(&b.bytes, b"AMS:GOODBYE"));
    // The wait thread broadcasts status; caller-driven close removes maps.
    // Freeze that distinction rather than introducing automatic pruning.
    assert_eq!(fixture.host.list_sessions().len(), 1);
    assert!(fixture.host.is_attached("a"));
    assert!(fixture.host.is_attached("b"));
    fixture.close("exit");
    a.wait_closed();
    b.wait_closed();
    assert_eq!(a.exits, [Some(7)]);
    assert_eq!(b.exits, [Some(7)]);
    assert!(fixture.host.list_sessions().is_empty());
}

#[test]
fn repeated_attach_is_idempotent_and_reopen_starts_a_fresh_shell() {
    let mut fixture = Fixture::new();
    let mut a = fixture.start("repeat", "a");
    fixture.send("a", "remember");
    a.wait_marker(b"AMS:REMEMBERED");
    let mut again = fixture.attach("repeat", "a");
    assert_eq!(fixture.host.list_sessions()[0].attachers, 1);
    fixture.send("a", "recall");
    again.wait_marker(b"AMS:MEMORY:kept");
    fixture.close("repeat");
    a.wait_closed();
    again.wait_closed();

    // Creation flags, fresh scrollback and process-local memory are observable.
    // The original private generation is always 1; do not invent a bump rule.
    let mut fresh = fixture.start("repeat", "a");
    assert!(!contains(&fresh.bytes, b"AMS:REMEMBERED"));
    fixture.send("a", "recall");
    fresh.wait_marker(b"AMS:MEMORY:empty");
    assert!(!contains(&fresh.bytes, b"AMS:MEMORY:kept"));
    assert_eq!(fixture.host.list_sessions()[0].attachers, 1);
}

#[test]
fn failed_candidate_leaves_no_routes_and_explicit_fallback_can_start() {
    let mut fixture = Fixture::new();
    let missing = fixture
        .directory
        .path
        .join("fixture-program-does-not-exist");
    let error = match fixture.host.open_with(
        Some("fallback"),
        "a",
        100,
        40,
        vec![isolate_command(
            CommandBuilder::new(&missing),
            &fixture.directory.path,
        )],
    ) {
        Ok(attach) => {
            fixture.owned.push(OwnedSession {
                id: attach.session_id,
                cleanup_rx: attach.rx.resubscribe(),
            });
            panic!("nonexistent fixture candidate unexpectedly started");
        }
        Err(error) => error,
    };
    assert!(error.starts_with("couldn't start a shell: "), "{error}");
    assert!(fixture.host.list_sessions().is_empty());
    assert!(!fixture.host.is_attached("a"));

    let mut commands = vec![isolate_command(
        CommandBuilder::new(missing),
        &fixture.directory.path,
    )];
    commands.extend(protocol_commands(&fixture.directory.path));
    let attach = fixture.open("fallback", "a", 100, 40, commands);
    assert!(attach.created);
    let mut viewer = Viewer::new(attach);
    viewer.wait_marker(b"AMS:READY");
    fixture.send("a", "alpha");
    viewer.wait_marker(b"AMS:ALPHA");
}
