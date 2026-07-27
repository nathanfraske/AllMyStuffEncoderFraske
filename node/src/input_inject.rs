//! The input media plane's *sink* side: apply a remote console's keyboard
//! and mouse events to this machine, via `enigo` (SendInput on Windows,
//! CoreGraphics on macOS, X11 on Linux).
//!
//! Mouse coordinates arrive normalized 0..1 over the sender's *view of one
//! of our screens* (the paired display stream) plus which screen that is —
//! the `screen` field of [`InputAction::MouseMove`], absent for the
//! primary. Injection resolves that monitor's rectangle in the global
//! desktop space and lands the cursor inside it, so control follows the
//! screen the console is showing. The two ends still never negotiate
//! resolutions — exactly the piKVM model, one monitor at a time.
//!
//! On Windows the absolute move is raised by hand (`SendInput` with
//! `MOUSEEVENTF_VIRTUALDESK`): enigo's absolute coordinates normalize
//! against the primary monitor only, which physically can't express a
//! point on a second screen. macOS and X11 take global coordinates
//! through enigo directly.
//!
//! Keys arrive as the DOM `KeyboardEvent.key` value (the layout-resolved
//! character or a name like "Enter"), plus — from senders that know it —
//! the physical `KeyboardEvent.code` ("KeyC"). Injection assumes key
//! *combinations* are the norm, not the exception:
//!
//! - Plain typing with no modifier held injects the resolved character
//!   as itself, so the *typist's* layout wins.
//! - With Shift held the resolved character is the *shifted* one ("A",
//!   "!", "?"), which must not be injected as itself: that would shift a
//!   key whose unshifted face is already the symbol, and the held Shift
//!   either doubles up or (on X11's Unicode keycode remap) lands on an
//!   empty shift level and types nothing. Instead the unshifted keycap of
//!   the physical key is injected and the forwarded Shift composes it —
//!   the base letter for "A", the digit "1" for "!", just like hardware.
//! - A chord (Ctrl/Alt/Meta held) resolves through the *physical* key:
//!   Ctrl+C must land on the remote's C key, never on whatever character
//!   the sender's layout composed under the held modifiers.
//! - Each route's pressed keys are remembered, so a keyup releases
//!   exactly the key its keydown pressed (Shift+1 goes down as "!" but
//!   comes up as "1" — without the memory the remote keeps a stuck key),
//!   and a route tearing down mid-chord lifts everything it still held.
//!
//! `Enigo` isn't `Send`, so one dedicated thread owns it and drains a
//! channel of commands; the mesh just calls [`Injector::apply`] after its
//! route/ownership gates pass. The thread starts lazily on the first
//! event and dies with the app.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

#[cfg(not(windows))]
use enigo::Coordinate;
use enigo::{Axis, Button, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use parking_lot::Mutex;

use allmystuff_session::InputAction;

#[derive(Debug)]
enum Cmd {
    Event {
        route: String,
        action: InputAction,
        session_epoch: u64,
        route_epoch: u64,
    },
}

/// AMS-06: the maximum accounted queue load. A newly admitted key/button down
/// consumes one queue slot plus one reserved slot for its matching release.
/// That keeps the queue bounded while ensuring saturation cannot strand an
/// input that this process accepted as held.
const INPUT_QUEUE_CAP: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PressToken {
    route: String,
    session_epoch: u64,
    route_epoch: u64,
    input: PressInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PressInput {
    Key(String),
    MouseButton(u8),
}

struct InputSender {
    queue: Arc<Mutex<InputQueue>>,
    wake: mpsc::SyncSender<()>,
}

struct InputQueue {
    commands: VecDeque<Cmd>,
    /// Downs accepted by this queue whose matching up has not been accepted
    /// yet. Each entry owns one reserved unit in `accounted_len`.
    admitted_presses: HashSet<PressToken>,
    capacity: usize,
}

#[derive(Default)]
pub struct Injector {
    tx: Mutex<Option<InputSender>>,
    session_epoch: Arc<AtomicU64>,
    next_route_epoch: AtomicU64,
    route_epochs: Arc<Mutex<HashMap<String, u64>>>,
    pending_releases: Arc<Mutex<PendingReleases>>,
}

/// A snapshot of one active input route's local lifetime. The mesh captures
/// this before its route/authority checks and queues it with the event. A
/// teardown that races either check invalidates the lease, so a late key-down
/// cannot be stamped with the successor route's generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLease {
    session_epoch: u64,
    route_epoch: u64,
}

#[derive(Default)]
struct PendingReleases {
    all: bool,
    routes: HashSet<String>,
}

impl Cmd {
    fn press_transition(&self) -> Option<(PressToken, bool)> {
        let Self::Event {
            route,
            action,
            session_epoch,
            route_epoch,
        } = self;
        let (input, down) = match action {
            InputAction::Key {
                key, code, down, ..
            } => (
                PressInput::Key(KeyTracker::identity(key, code.as_deref())),
                *down,
            ),
            InputAction::MouseButton { button, down } => (PressInput::MouseButton(*button), *down),
            _ => return None,
        };
        Some((
            PressToken {
                route: route.clone(),
                session_epoch: *session_epoch,
                route_epoch: *route_epoch,
                input,
            },
            down,
        ))
    }

    fn is_lossy_continuous(&self) -> bool {
        matches!(
            self,
            Self::Event {
                action: InputAction::MouseMove { .. }
                    | InputAction::MouseMoveRel { .. }
                    | InputAction::Wheel { .. },
                ..
            }
        )
    }
}

impl InputQueue {
    fn new(capacity: usize) -> Self {
        Self {
            commands: VecDeque::new(),
            admitted_presses: HashSet::new(),
            capacity,
        }
    }

    fn accounted_len(&self) -> usize {
        self.commands.len() + self.admitted_presses.len()
    }

    /// Nonblocking admission with one reserved unit for every first down.
    ///
    /// A matching up exchanges its reservation for a queue slot, so it fits
    /// even when the queue is otherwise saturated. New downs and best-effort
    /// events evict continuous motion first, then fail closed if admitting
    /// them would consume capacity reserved for an already accepted release.
    fn enqueue(&mut self, cmd: Cmd) -> bool {
        if self.coalesce_continuous(&cmd) {
            return true;
        }

        if let Some((token, down)) = cmd.press_transition() {
            if down {
                if self.admitted_presses.contains(&token) {
                    return self.enqueue_best_effort(cmd, 1);
                }
                if !self.make_room(2) {
                    return false;
                }
                self.admitted_presses.insert(token);
                self.commands.push_back(cmd);
                debug_assert!(self.accounted_len() <= self.capacity);
                return true;
            }

            if self.admitted_presses.remove(&token) {
                // Removing the release reservation and appending the release
                // leave the accounted length unchanged.
                self.commands.push_back(cmd);
                debug_assert!(self.accounted_len() <= self.capacity);
                return true;
            }
        }

        self.enqueue_best_effort(cmd, 1)
    }

    fn enqueue_best_effort(&mut self, cmd: Cmd, units: usize) -> bool {
        if !self.make_room(units) {
            return false;
        }
        self.commands.push_back(cmd);
        debug_assert!(self.accounted_len() <= self.capacity);
        true
    }

    fn make_room(&mut self, units: usize) -> bool {
        while self.accounted_len().saturating_add(units) > self.capacity {
            let Some(index) = self
                .commands
                .iter()
                .position(|queued| queued.is_lossy_continuous())
            else {
                return false;
            };
            self.commands.remove(index);
        }
        true
    }

    /// Adjacent motion can be collapsed without crossing a click, key, route,
    /// or generation boundary. Absolute motion keeps the freshest point;
    /// relative motion and wheel input preserve total delta.
    fn coalesce_continuous(&mut self, newer: &Cmd) -> bool {
        let Some(Cmd::Event {
            route: old_route,
            action: old_action,
            session_epoch: old_session_epoch,
            route_epoch: old_route_epoch,
        }) = self.commands.back_mut()
        else {
            return false;
        };
        let Cmd::Event {
            route: new_route,
            action: new_action,
            session_epoch: new_session_epoch,
            route_epoch: new_route_epoch,
        } = newer;
        if old_route != new_route
            || old_session_epoch != new_session_epoch
            || old_route_epoch != new_route_epoch
        {
            return false;
        }

        match (old_action, new_action) {
            (
                InputAction::MouseMove {
                    x: old_x,
                    y: old_y,
                    screen: old_screen,
                },
                InputAction::MouseMove {
                    x: new_x,
                    y: new_y,
                    screen: new_screen,
                },
            ) if old_screen == new_screen => {
                *old_x = *new_x;
                *old_y = *new_y;
                true
            }
            (
                InputAction::MouseMoveRel {
                    dx: old_dx,
                    dy: old_dy,
                },
                InputAction::MouseMoveRel {
                    dx: new_dx,
                    dy: new_dy,
                },
            )
            | (
                InputAction::Wheel {
                    dx: old_dx,
                    dy: old_dy,
                },
                InputAction::Wheel {
                    dx: new_dx,
                    dy: new_dy,
                },
            ) => {
                let dx = *old_dx + *new_dx;
                let dy = *old_dy + *new_dy;
                if !dx.is_finite() || !dy.is_finite() {
                    return false;
                }
                *old_dx = dx;
                *old_dy = dy;
                true
            }
            _ => false,
        }
    }

    fn clear_generation(&mut self, route: &str, session_epoch: u64, route_epoch: u64) {
        self.admitted_presses.retain(|press| {
            press.route != route
                || press.session_epoch != session_epoch
                || press.route_epoch != route_epoch
        });
    }

    fn clear_session(&mut self, session_epoch: u64) {
        self.admitted_presses
            .retain(|press| press.session_epoch != session_epoch);
    }
}

impl Injector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a new local lifetime for an active input route. Reusing a route id
    /// always mints a different generation and schedules cleanup of any state
    /// retained by its predecessor.
    pub fn activate_route(&self, route: &str) {
        let route_epoch = self
            .next_route_epoch
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
            .max(1);
        let (replaced_route_epoch, session_epoch) = {
            let mut route_epochs = self.route_epochs.lock();
            let session_epoch = self.session_epoch.load(Ordering::SeqCst);
            (
                route_epochs.insert(route.to_string(), route_epoch),
                session_epoch,
            )
        };
        if let Some(replaced_route_epoch) = replaced_route_epoch {
            let tx = self.tx.lock();
            let Some(sender) = tx.as_ref() else { return };
            sender
                .queue
                .lock()
                .clear_generation(route, session_epoch, replaced_route_epoch);
            drop(tx);
            self.pending_releases
                .lock()
                .routes
                .insert(route.to_string());
            self.wake_releases();
        }
    }

    /// Capture the active generation before the caller checks route ownership.
    /// Unknown and already-ended route ids do not allocate bookkeeping.
    pub fn lease(&self, route: &str) -> Option<InputLease> {
        let route_epochs = self.route_epochs.lock();
        let route_epoch = route_epochs.get(route).copied()?;
        Some(InputLease {
            session_epoch: self.session_epoch.load(Ordering::SeqCst),
            route_epoch,
        })
    }

    /// Queue one event for injection using the generation captured before the
    /// mesh authorization gate. Starts the injector thread on first use; if
    /// the platform refuses, the failure is logged once and events are dropped.
    pub fn apply(&self, route: &str, action: InputAction, lease: InputLease) {
        self.send_event(Cmd::Event {
            route: route.into(),
            action,
            session_epoch: lease.session_epoch,
            route_epoch: lease.route_epoch,
        });
    }

    /// A control route ended (teardown, peer drop): lift any keys or mouse
    /// buttons its stream still holds down. No-op for routes that never held
    /// anything.
    pub fn release_route(&self, route: &str) {
        // Removing the active generation is the fence. Unknown ids are a true
        // no-op, so rejected forged ids cannot grow this map or its pending set.
        let (route_epoch, session_epoch) = {
            let mut route_epochs = self.route_epochs.lock();
            let Some(route_epoch) = route_epochs.remove(route) else {
                return;
            };
            (route_epoch, self.session_epoch.load(Ordering::SeqCst))
        };
        // Never spawns the thread: if it isn't running, nothing is held.
        let tx = self.tx.lock();
        let Some(sender) = tx.as_ref() else { return };
        sender
            .queue
            .lock()
            .clear_generation(route, session_epoch, route_epoch);
        drop(tx);
        self.pending_releases
            .lock()
            .routes
            .insert(route.to_string());
        self.wake_releases();
    }

    /// The daemon session disappeared, so every old control route is invalid.
    /// Release the injector's authoritative held-input set instead of relying
    /// on the replacement session to enumerate routes that no longer exist.
    pub fn release_all(&self) {
        let retired_session_epoch = {
            let mut route_epochs = self.route_epochs.lock();
            let retired_session_epoch = self.session_epoch.fetch_add(1, Ordering::SeqCst);
            route_epochs.clear();
            retired_session_epoch
        };
        let tx = self.tx.lock();
        let Some(sender) = tx.as_ref() else { return };
        sender.queue.lock().clear_session(retired_session_epoch);
        drop(tx);
        let mut pending = self.pending_releases.lock();
        pending.all = true;
        pending.routes.clear();
        drop(pending);
        self.wake_releases();
    }

    fn send_event(&self, cmd: Cmd) {
        let mut tx = self.tx.lock();
        if tx.is_none() {
            // The wake channel only needs one token: commands live in the
            // bounded policy queue, and one pending wake means the worker will
            // observe every command already admitted there.
            let (wake, rx) = mpsc::sync_channel::<()>(1);
            let queue = Arc::new(Mutex::new(InputQueue::new(INPUT_QUEUE_CAP)));
            let session_epoch = self.session_epoch.clone();
            let route_epochs = self.route_epochs.clone();
            let pending_releases = self.pending_releases.clone();
            let worker_queue = queue.clone();
            std::thread::spawn(move || {
                run_injector(
                    rx,
                    worker_queue,
                    session_epoch,
                    route_epochs,
                    pending_releases,
                )
            });
            *tx = Some(InputSender { queue, wake });
        }
        if let Some(sender) = tx.as_ref() {
            if !sender.queue.lock().enqueue(cmd) {
                return;
            }
            match sender.wake.try_send(()) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                // The thread ended (platform refused); allow a retry on the next
                // event rather than wedging forever.
                Err(mpsc::TrySendError::Disconnected(_)) => *tx = None,
            }
        }
    }

    fn wake_releases(&self) {
        let mut tx = self.tx.lock();
        let Some(sender) = tx.as_ref() else { return };
        match sender.wake.try_send(()) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
            Err(mpsc::TrySendError::Disconnected(_)) => *tx = None,
        }
    }
}

fn run_injector(
    rx: mpsc::Receiver<()>,
    queue: Arc<Mutex<InputQueue>>,
    session_epoch: Arc<AtomicU64>,
    route_epochs: Arc<Mutex<HashMap<String, u64>>>,
    pending_releases: Arc<Mutex<PendingReleases>>,
) {
    // Inbound control is what the user *feels* — keep the injector
    // responsive under exactly the load that made them reach for it.
    crate::os_perf::boost_media_thread();
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("input injection unavailable on this machine: {e}");
            return;
        }
    };
    let mut screens = ScreenMap::load();
    // Each control route's held keyboard and mouse state. Modifiers steer how
    // the next key resolves, and everything still down is lifted when the
    // route or daemon session goes away.
    let mut routes: HashMap<String, RouteInputState> = HashMap::new();
    // Cleanup state is kept separate from live route ids. That lets a reused
    // route start a fresh generation while failed OS releases from its
    // predecessor retain authority to retry on a later worker wake.
    let mut release_retries = Vec::new();
    let mut live_release_retries = Vec::new();
    // A failed OS down is remembered until its matching up so the healing
    // path for an unobserved down does not release an input we know failed.
    let mut failed_presses = HashSet::new();
    while rx.recv().is_ok() {
        {
            let current_session_epoch = session_epoch.load(Ordering::SeqCst);
            let current_routes = route_epochs.lock();
            failed_presses.retain(|press: &PressToken| {
                press.session_epoch == current_session_epoch
                    && current_routes.get(&press.route).copied() == Some(press.route_epoch)
            });
        }
        loop {
            retry_live_releases(
                &mut enigo,
                &mut routes,
                &mut live_release_retries,
                &session_epoch,
                &route_epochs,
            );
            flush_pending_releases(
                &mut enigo,
                &mut routes,
                &pending_releases,
                &mut release_retries,
            );
            let Some(Cmd::Event {
                route,
                action,
                session_epoch: event_session_epoch,
                route_epoch: event_route_epoch,
            }) = queue.lock().commands.pop_front()
            else {
                break;
            };
            if !event_generation_is_current(
                &session_epoch,
                &route_epochs,
                &route,
                event_session_epoch,
                event_route_epoch,
            ) {
                continue;
            }
            // A viewer clicking or typing at a *dark* console is the "remote
            // login wakes the machine" moment — relight the panel so the
            // stream they're driving blind comes back. No-op while frames
            // flow (rate-limited and gated inside).
            if matches!(
                action,
                InputAction::MouseButton { .. } | InputAction::Key { .. }
            ) {
                crate::wake::force_display_on_if_dark();
            }
            let mut failed_release = None;
            let result = match action {
                InputAction::MouseMove { x, y, screen } => {
                    let rect = screens.resolve(screen);
                    let (gx, gy) = rect.denorm(x, y);
                    move_mouse_global(&mut enigo, gx, gy)
                }
                // Pointer-lock deltas: raw relative motion, straight through —
                // adjacent queued deltas preserve total movement; injection
                // still does no screen resolution or clamping.
                InputAction::MouseMoveRel { dx, dy } => enigo
                    .move_mouse(dx.round() as i32, dy.round() as i32, enigo::Coordinate::Rel)
                    .map_err(|e| e.to_string()),
                InputAction::MouseButton { button, down } => {
                    let state = routes.entry(route.clone()).or_default();
                    let input = PressInput::MouseButton(button);
                    let press = PressToken {
                        route: route.clone(),
                        session_epoch: event_session_epoch,
                        route_epoch: event_route_epoch,
                        input: input.clone(),
                    };
                    if down {
                        if state.holds_button(button)
                            && cancel_live_release_retry(
                                &mut live_release_retries,
                                &route,
                                event_session_epoch,
                                event_route_epoch,
                                &input,
                            )
                        {
                            // The failed up left the OS input down. This new
                            // down adopts it and cancels the retry instead of
                            // letting a later retry release the new press.
                            failed_presses.remove(&press);
                            Ok(())
                        } else {
                            let result = inject_button_press(&mut enigo, state, button);
                            if result.is_err() {
                                failed_presses.insert(press);
                            } else {
                                failed_presses.remove(&press);
                            }
                            result
                        }
                    } else {
                        let skip_release = suppress_unaccepted_release(
                            &mut failed_presses,
                            &press,
                            state.holds_button(button),
                        );
                        let result = if skip_release {
                            Ok(())
                        } else {
                            release_button(&mut enigo, state, button)
                        };
                        if result.is_err() && state.holds_button(button) {
                            failed_release = Some(LiveReleaseRetry {
                                route: route.clone(),
                                session_epoch: event_session_epoch,
                                route_epoch: event_route_epoch,
                                input,
                            });
                        }
                        result
                    }
                }
                InputAction::Wheel { dx, dy } => {
                    let mut r = Ok(());
                    if dy.abs() >= 0.5 {
                        r = enigo
                            .scroll(dy.round() as i32, Axis::Vertical)
                            .map_err(|e| e.to_string());
                    }
                    if r.is_ok() && dx.abs() >= 0.5 {
                        r = enigo
                            .scroll(dx.round() as i32, Axis::Horizontal)
                            .map_err(|e| e.to_string());
                    }
                    r
                }
                InputAction::Key {
                    ref key,
                    ref code,
                    down,
                } => {
                    let tracker = &mut routes.entry(route.clone()).or_default().keys;
                    let identity = KeyTracker::identity(key, code.as_deref());
                    let input = PressInput::Key(identity.clone());
                    let press = PressToken {
                        route: route.clone(),
                        session_epoch: event_session_epoch,
                        route_epoch: event_route_epoch,
                        input: input.clone(),
                    };
                    if down {
                        if tracker.holds_identity(&identity)
                            && cancel_live_release_retry(
                                &mut live_release_retries,
                                &route,
                                event_session_epoch,
                                event_route_epoch,
                                &input,
                            )
                        {
                            failed_presses.remove(&press);
                            Ok(())
                        } else {
                            let result =
                                inject_key_press(&mut enigo, tracker, key, code.as_deref());
                            if result.is_err() {
                                failed_presses.insert(press);
                            } else {
                                failed_presses.remove(&press);
                            }
                            result
                        }
                    } else {
                        let skip_release = suppress_unaccepted_release(
                            &mut failed_presses,
                            &press,
                            tracker.holds_identity(&identity),
                        );
                        let result = if skip_release {
                            Ok(())
                        } else {
                            release_key(&mut enigo, tracker, key, code.as_deref())
                        };
                        if result.is_err() && tracker.holds_identity(&identity) {
                            failed_release = Some(LiveReleaseRetry {
                                route: route.clone(),
                                session_epoch: event_session_epoch,
                                route_epoch: event_route_epoch,
                                input,
                            });
                        }
                        result
                    }
                }
                // An input kind a newer viewer introduced that this build can't
                // inject — nothing to do (it decoded as `Unknown` rather than
                // failing the whole frame).
                InputAction::Unknown => Ok(()),
            };
            if let Err(e) = result {
                tracing::debug!("input injection event failed: {e}");
            }
            if let Some(retry) = failed_release {
                if !live_release_retries.contains(&retry) {
                    live_release_retries.push(retry);
                }
            }
        }
    }
}

fn event_generation_is_current(
    session_epoch: &AtomicU64,
    route_epochs: &Mutex<HashMap<String, u64>>,
    route: &str,
    event_session_epoch: u64,
    event_route_epoch: u64,
) -> bool {
    event_session_epoch == session_epoch.load(Ordering::SeqCst)
        && route_epochs.lock().get(route).copied() == Some(event_route_epoch)
}

trait ReleaseBackend {
    fn release_button(&mut self, button: Button) -> Result<(), String>;
    fn release_key(&mut self, key: Key) -> Result<(), String>;
}

trait PressBackend {
    fn press_button(&mut self, button: Button) -> Result<(), String>;
    fn press_key(&mut self, key: Key) -> Result<(), String>;
}

impl ReleaseBackend for Enigo {
    fn release_button(&mut self, button: Button) -> Result<(), String> {
        self.button(button, Direction::Release)
            .map_err(|error| error.to_string())
    }

    fn release_key(&mut self, key: Key) -> Result<(), String> {
        self.key(key, Direction::Release)
            .map_err(|error| error.to_string())
    }
}

impl PressBackend for Enigo {
    fn press_button(&mut self, button: Button) -> Result<(), String> {
        self.button(button, Direction::Press)
            .map_err(|error| error.to_string())
    }

    fn press_key(&mut self, key: Key) -> Result<(), String> {
        self.key(key, Direction::Press)
            .map_err(|error| error.to_string())
    }
}

struct ReleaseRetry {
    state: RouteInputState,
    reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LiveReleaseRetry {
    route: String,
    session_epoch: u64,
    route_epoch: u64,
    input: PressInput,
}

impl LiveReleaseRetry {
    fn is_still_held(&self, state: &RouteInputState) -> bool {
        match &self.input {
            PressInput::Key(identity) => state.keys.holds_identity(identity),
            PressInput::MouseButton(button) => state.holds_button(*button),
        }
    }
}

fn cancel_live_release_retry(
    retries: &mut Vec<LiveReleaseRetry>,
    route: &str,
    session_epoch: u64,
    route_epoch: u64,
    input: &PressInput,
) -> bool {
    let Some(index) = retries.iter().position(|retry| {
        retry.route == route
            && retry.session_epoch == session_epoch
            && retry.route_epoch == route_epoch
            && retry.input == *input
    }) else {
        return false;
    };
    retries.remove(index);
    true
}

fn suppress_unaccepted_release(
    failed_presses: &mut HashSet<PressToken>,
    press: &PressToken,
    currently_held: bool,
) -> bool {
    failed_presses.remove(press) && !currently_held
}

fn retry_live_releases<B: ReleaseBackend>(
    backend: &mut B,
    routes: &mut HashMap<String, RouteInputState>,
    retries: &mut Vec<LiveReleaseRetry>,
    session_epoch: &AtomicU64,
    route_epochs: &Mutex<HashMap<String, u64>>,
) {
    let pending = std::mem::take(retries);
    for retry in pending {
        if !event_generation_is_current(
            session_epoch,
            route_epochs,
            &retry.route,
            retry.session_epoch,
            retry.route_epoch,
        ) {
            // Route/session cleanup owns any held predecessor state after its
            // generation is retired. Never look up a same-id successor.
            continue;
        }
        let Some(state) = routes.get_mut(&retry.route) else {
            continue;
        };
        if !retry.is_still_held(state) {
            continue;
        }
        let result = match &retry.input {
            PressInput::Key(identity) => release_held_key(backend, &mut state.keys, identity),
            PressInput::MouseButton(button) => release_button(backend, state, *button),
        };
        if let Err(error) = result {
            tracing::debug!(
                "retrying live input release for {:?} failed: {error}",
                retry.input
            );
            retries.push(retry);
        }
    }
}

fn flush_pending_releases<B: ReleaseBackend>(
    backend: &mut B,
    routes: &mut HashMap<String, RouteInputState>,
    pending: &Mutex<PendingReleases>,
    retries: &mut Vec<ReleaseRetry>,
) {
    retry_failed_releases(backend, retries);

    let (release_all, release_routes) = {
        let mut pending = pending.lock();
        let all = std::mem::take(&mut pending.all);
        let routes = pending.routes.drain().collect::<Vec<_>>();
        (all, routes)
    };
    if release_all {
        for (_, state) in routes.drain() {
            retain_failed_releases(backend, retries, state, "daemon session reset");
        }
        return;
    }
    for route in release_routes {
        if let Some(state) = routes.remove(&route) {
            retain_failed_releases(backend, retries, state, "route end");
        }
    }
}

fn retry_failed_releases<B: ReleaseBackend>(backend: &mut B, retries: &mut Vec<ReleaseRetry>) {
    let retired = std::mem::take(retries);
    for mut retry in retired {
        release_input_state(backend, &mut retry.state, retry.reason);
        if !retry.state.is_empty() {
            retries.push(retry);
        }
    }
}

fn retain_failed_releases<B: ReleaseBackend>(
    backend: &mut B,
    retries: &mut Vec<ReleaseRetry>,
    mut state: RouteInputState,
    reason: &'static str,
) {
    release_input_state(backend, &mut state, reason);
    if !state.is_empty() {
        retries.push(ReleaseRetry { state, reason });
    }
}

/// Land the cursor on a global desktop coordinate (any monitor).
#[cfg(not(windows))]
fn move_mouse_global(enigo: &mut Enigo, gx: i32, gy: i32) -> Result<(), String> {
    // CoreGraphics and X11 take global coordinates as-is.
    enigo
        .move_mouse(gx, gy, Coordinate::Abs)
        .map_err(|e| e.to_string())
}

/// Land the cursor on a global desktop coordinate (any monitor).
///
/// Raised by hand because enigo's `Coordinate::Abs` normalizes against the
/// primary monitor without `MOUSEEVENTF_VIRTUALDESK` — a point on a second
/// screen is simply unreachable through it. With the flag, 0..65535 spans
/// the whole virtual desktop.
#[cfg(windows)]
fn move_mouse_global(_enigo: &mut Enigo, gx: i32, gy: i32) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = i64::from(GetSystemMetrics(SM_CXVIRTUALSCREEN)).max(2);
        let vh = i64::from(GetSystemMetrics(SM_CYVIRTUALSCREEN)).max(2);
        // Pixel → the API's 0..65535 span of the virtual desktop, rounding
        // at the pixel center.
        let nx =
            ((i64::from(gx) - i64::from(vx)).clamp(0, vw - 1) * 65535 + (vw - 1) / 2) / (vw - 1);
        let ny =
            ((i64::from(gy) - i64::from(vy)).clamp(0, vh - 1) * 65535 + (vh - 1) / 2) / (vh - 1);
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: nx as i32,
                    dy: ny as i32,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        if SendInput(&[input], std::mem::size_of::<INPUT>() as i32) == 1 {
            Ok(())
        } else {
            Err("SendInput refused the move".into())
        }
    }
}

/// One monitor's rectangle in the global desktop space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScreenRect {
    id: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    primary: bool,
}

impl ScreenRect {
    /// Normalized 0..1 over this screen → a global pixel inside it.
    fn denorm(&self, nx: f64, ny: f64) -> (i32, i32) {
        let px = (nx.clamp(0.0, 1.0) * (self.w.max(1) - 1) as f64).round() as i32;
        let py = (ny.clamp(0.0, 1.0) * (self.h.max(1) - 1) as f64).round() as i32;
        (self.x + px, self.y + py)
    }
}

/// The monitor layout, refreshed when an event names a screen we don't
/// know (hotplug since the last look).
/// How stale the cached monitor layout may grow before `resolve` re-reads
/// it. A resolution/DPI/layout change mid-session must reach the mapping
/// within a beat — the old code refreshed only when a *named* screen id
/// went missing, so the primary-screen path (the common case) denormalized
/// against stale dimensions forever after a mode change, landing the
/// cursor in the wrong place. Two seconds keeps the enumeration cost
/// invisible at 60 events/s.
const SCREENS_TTL: Duration = Duration::from_secs(2);

struct ScreenMap {
    rects: Vec<ScreenRect>,
    refreshed: Instant,
}

impl ScreenMap {
    fn load() -> Self {
        ScreenMap {
            rects: query_screens(),
            refreshed: Instant::now(),
        }
    }

    /// The rectangle for `screen` — the primary when unnamed, a re-query
    /// then the primary when the named one is gone. The cache re-reads on
    /// [`SCREENS_TTL`] regardless, so a mode change reaches the mapping.
    /// The fallback rect (no monitors readable at all) keeps the old
    /// primary-screen behaviour.
    fn resolve(&mut self, screen: Option<u32>) -> ScreenRect {
        if self.refreshed.elapsed() >= SCREENS_TTL {
            self.rects = query_screens();
            self.refreshed = Instant::now();
        }
        if let Some(id) = screen {
            if let Some(r) = self.rects.iter().find(|r| r.id == id) {
                return *r;
            }
            self.rects = query_screens();
            self.refreshed = Instant::now();
            if let Some(r) = self.rects.iter().find(|r| r.id == id) {
                return *r;
            }
            tracing::debug!("input names screen {id} but it isn't attached — using the primary");
        }
        self.primary()
    }

    fn primary(&self) -> ScreenRect {
        self.rects
            .iter()
            .find(|r| r.primary)
            .or_else(|| self.rects.first())
            .copied()
            .unwrap_or(ScreenRect {
                id: 0,
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
                primary: true,
            })
    }
}

/// Every attached monitor's global rect, via the same enumeration the
/// capture side uses — so the ids in `screen:<id>` capabilities resolve
/// back to the same physical screens here.
fn query_screens() -> Vec<ScreenRect> {
    let Ok(monitors) = xcap::Monitor::all() else {
        return Vec::new();
    };
    monitors
        .iter()
        .filter_map(|m| {
            Some(ScreenRect {
                id: m.id().ok()?,
                x: m.x().unwrap_or(0),
                y: m.y().unwrap_or(0),
                w: m.width().unwrap_or(0).max(1),
                h: m.height().unwrap_or(0).max(1),
                primary: m.is_primary().unwrap_or(false),
            })
        })
        .collect()
}

/// DOM `MouseEvent.button` → enigo button. 3/4 (browser back/forward)
/// don't inject — they'd navigate the *remote*'s browser focus oddly.
fn dom_button(button: u8) -> Option<Button> {
    match button {
        0 => Some(Button::Left),
        1 => Some(Button::Middle),
        2 => Some(Button::Right),
        _ => None,
    }
}

/// Everything one control route currently holds on the remote desktop.
#[derive(Default)]
struct RouteInputState {
    keys: KeyTracker,
    /// DOM button ids in press order. Keeping ids avoids relying on enigo's
    /// platform enum as the map identity and lets cleanup reuse dom_button.
    pressed_buttons: Vec<u8>,
}

impl RouteInputState {
    #[cfg(test)]
    fn press_button(&mut self, button: u8) -> Option<Button> {
        let injected = dom_button(button)?;
        self.commit_button_press(button);
        Some(injected)
    }

    fn commit_button_press(&mut self, button: u8) {
        self.pressed_buttons.retain(|held| *held != button);
        self.pressed_buttons.push(button);
    }

    fn holds_button(&self, button: u8) -> bool {
        self.pressed_buttons.contains(&button)
    }

    fn is_empty(&self) -> bool {
        self.pressed_buttons.is_empty() && self.keys.is_empty()
    }
}

fn inject_button_press<B: PressBackend>(
    backend: &mut B,
    state: &mut RouteInputState,
    dom_button_id: u8,
) -> Result<(), String> {
    let Some(button) = dom_button(dom_button_id) else {
        return Ok(());
    };
    backend.press_button(button)?;
    state.commit_button_press(dom_button_id);
    Ok(())
}

fn release_button<B: ReleaseBackend>(
    backend: &mut B,
    state: &mut RouteInputState,
    dom_button_id: u8,
) -> Result<(), String> {
    let Some(button) = dom_button(dom_button_id) else {
        return Ok(());
    };
    backend.release_button(button)?;
    if let Some(index) = state
        .pressed_buttons
        .iter()
        .position(|held| *held == dom_button_id)
    {
        state.pressed_buttons.remove(index);
    }
    // Release even when the down was missed. This heals a route attached
    // mid-drag instead of preserving an unknown held state. A tracked down is
    // removed only after the OS accepts the release, so failure can retry.
    Ok(())
}

fn inject_key_press<B: PressBackend>(
    backend: &mut B,
    tracker: &mut KeyTracker,
    key: &str,
    code: Option<&str>,
) -> Result<(), String> {
    let Some((identity, injected)) = tracker.press_candidate(key, code) else {
        return Ok(());
    };
    backend.press_key(injected)?;
    tracker.commit_press(identity, injected);
    Ok(())
}

fn release_key<B: ReleaseBackend>(
    backend: &mut B,
    tracker: &mut KeyTracker,
    key: &str,
    code: Option<&str>,
) -> Result<(), String> {
    let Some((injected, identity)) = tracker.release_candidate(key, code) else {
        return Ok(());
    };
    backend.release_key(injected)?;
    if let Some(identity) = identity {
        tracker.commit_release(&identity);
    }
    Ok(())
}

fn release_held_key<B: ReleaseBackend>(
    backend: &mut B,
    tracker: &mut KeyTracker,
    identity: &str,
) -> Result<(), String> {
    let Some(index) = tracker
        .pressed
        .iter()
        .position(|(held, _)| held == identity)
    else {
        return Ok(());
    };
    let key = tracker.pressed[index].1;
    backend.release_key(key)?;
    tracker.pressed.remove(index);
    Ok(())
}

fn release_input_state<B: ReleaseBackend>(
    backend: &mut B,
    state: &mut RouteInputState,
    reason: &str,
) {
    // End drags before lifting their modifier keys, matching a physical user
    // releasing the mouse and then unwinding the chord.
    let buttons = state
        .pressed_buttons
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    for dom_button_id in buttons {
        if let Err(error) = release_button(backend, state, dom_button_id) {
            tracing::debug!(
                "releasing {:?} after {reason} failed: {error}",
                dom_button(dom_button_id)
            );
        }
    }
    for identity in state.keys.release_order() {
        if let Err(error) = release_held_key(backend, &mut state.keys, &identity) {
            tracing::debug!("releasing {identity:?} after {reason} failed: {error}");
        }
    }
}

/// One control route's keyboard state: which keys its stream holds down
/// (each remembered as the enigo key actually injected), in press order.
///
/// The memory carries combinations across the places a stateless mapping
/// breaks: held Ctrl/Alt/Meta switch resolution to the physical key, a
/// keyup releases exactly what its keydown pressed even when the
/// modifiers (and so the DOM `key` value) changed in between, and a dying
/// route can lift everything it still holds.
#[derive(Default)]
struct KeyTracker {
    /// (identity, injected key) per held key — identity is the physical
    /// `code` when the sender knew it, else the `key` value itself.
    pressed: Vec<(String, Key)>,
}

impl KeyTracker {
    fn held(&self, k: Key) -> bool {
        self.pressed.iter().any(|(_, p)| *p == k)
    }

    fn identity(key: &str, code: Option<&str>) -> String {
        match code {
            Some(c) if !c.is_empty() => c.into(),
            _ => key.into(),
        }
    }

    /// A keydown helper for tests and already-accepted synthetic state.
    #[cfg(test)]
    fn press(&mut self, key: &str, code: Option<&str>) -> Option<Key> {
        let (identity, injected) = self.press_candidate(key, code)?;
        self.commit_press(identity, injected);
        Some(injected)
    }

    /// Resolve a keydown without recording held state. The OS injection must
    /// succeed before the caller commits the candidate.
    fn press_candidate(&self, key: &str, code: Option<&str>) -> Option<(String, Key)> {
        let injected = self.resolve(key, code)?;
        Some((Self::identity(key, code), injected))
    }

    fn commit_press(&mut self, identity: String, injected: Key) {
        // Auto-repeat arrives as a burst of re-presses (the remote OS won't
        // repeat an injected key on its own); collapse them to a single held
        // entry so the eventual keyup still lifts exactly once.
        self.pressed.retain(|(held, _)| *held != identity);
        self.pressed.push((identity, injected));
    }

    fn holds_identity(&self, identity: &str) -> bool {
        self.pressed.iter().any(|(held, _)| held == identity)
    }

    /// Resolve a keyup without forgetting its held-state authority. The caller
    /// commits a tracked release only after the OS accepts it.
    fn release_candidate(&self, key: &str, code: Option<&str>) -> Option<(Key, Option<String>)> {
        let id = Self::identity(key, code);
        if let Some((_, injected)) = self.pressed.iter().find(|(held, _)| *held == id) {
            return Some((*injected, Some(id)));
        }
        self.resolve(key, code).map(|injected| (injected, None))
    }

    fn commit_release(&mut self, identity: &str) {
        if let Some(index) = self.pressed.iter().position(|(held, _)| held == identity) {
            self.pressed.remove(index);
        }
    }

    /// Held identities in reverse press order. Cleanup uses identities rather
    /// than draining keys so an OS failure can leave only that key for retry.
    fn release_order(&self) -> Vec<String> {
        self.pressed
            .iter()
            .rev()
            .map(|(identity, _)| identity.clone())
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.pressed.is_empty()
    }

    /// DOM `KeyboardEvent.key` (+ physical `code`) → enigo key, steered by
    /// the modifiers this route currently holds. `None` = a key we
    /// deliberately don't carry (media keys, IME composition, lock keys we
    /// can't faithfully mirror).
    fn resolve(&self, key: &str, code: Option<&str>) -> Option<Key> {
        let mut chars = key.chars();
        let c = match (chars.next(), chars.next()) {
            (Some(c), None) => c,
            _ => return map_named(key),
        };
        let combo = self.held(Key::Control) || self.held(Key::Alt) || self.held(Key::Meta);
        if combo {
            // A chord wants the *physical* key: Ctrl+C is the C key, not
            // whatever character the sender's layout composed under the
            // held modifiers (macOS Option turns letters into "ç", "å"…).
            if let Some(base) = code.and_then(base_char) {
                return Some(Key::Unicode(base));
            }
        }
        if combo || self.held(Key::Shift) {
            // A modifier is already held on this end, so the character the
            // sender's layout composed under it ("A", but also "!", "?",
            // "@"…) must not be injected as itself: that asks the platform
            // to *shift a key whose unshifted face is already the symbol*.
            // On X11 the Unicode path remaps a spare keycode's unshifted
            // level to that symbol, so with Shift held the server reads the
            // shifted level — NoSymbol — and types nothing. That's why
            // capitals worked (handled below) while "!" and "?" vanished.
            // Inject the key's unshifted keycap and let the held Shift
            // compose it, exactly as a real keyboard does.
            if let Some(base) = code.and_then(base_char) {
                return Some(Key::Unicode(base));
            }
            // No physical code (an older sender): a letter can still be
            // de-shifted by lower-casing it; a symbol has to stand as
            // itself and hope the modifier doesn't swallow it.
            if c.is_uppercase() {
                let mut lower = c.to_lowercase();
                if let (Some(l), None) = (lower.next(), lower.next()) {
                    return Some(Key::Unicode(l));
                }
            }
        }
        Some(Key::Unicode(c))
    }
}

/// The physical key a DOM `KeyboardEvent.code` names, as the character on
/// its keycap (US-reference, the convention `code` itself is defined by) —
/// what a chord resolves through. `None` for keys without a stable cap
/// (named keys travel by `key` instead).
fn base_char(code: &str) -> Option<char> {
    if let Some(letter) = code.strip_prefix("Key") {
        let mut chars = letter.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_uppercase() {
                return Some(c.to_ascii_lowercase());
            }
        }
        return None;
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        let mut chars = digit.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_ascii_digit() {
                return Some(c);
            }
        }
        return None;
    }
    if let Some(pad) = code.strip_prefix("Numpad") {
        return match pad {
            "Add" => Some('+'),
            "Subtract" => Some('-'),
            "Multiply" => Some('*'),
            "Divide" => Some('/'),
            "Decimal" => Some('.'),
            _ => {
                let mut chars = pad.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_ascii_digit() => Some(c),
                    _ => None,
                }
            }
        };
    }
    match code {
        "Space" => Some(' '),
        "Minus" => Some('-'),
        "Equal" => Some('='),
        "BracketLeft" => Some('['),
        "BracketRight" => Some(']'),
        "Backslash" => Some('\\'),
        "Semicolon" => Some(';'),
        "Quote" => Some('\''),
        "Backquote" => Some('`'),
        "Comma" => Some(','),
        "Period" => Some('.'),
        "Slash" => Some('/'),
        _ => None,
    }
}

/// Named DOM `KeyboardEvent.key` values → enigo keys.
fn map_named(key: &str) -> Option<Key> {
    Some(match key {
        "Enter" => Key::Return,
        "Escape" => Key::Escape,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Delete" => Key::Delete,
        // Insert is half a chord on its own (Shift+Insert pastes,
        // Ctrl+Insert copies) — macOS has no such key to press.
        #[cfg(not(target_os = "macos"))]
        "Insert" => Key::Insert,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "Shift" => Key::Shift,
        "Control" => Key::Control,
        "Alt" => Key::Alt,
        "Meta" => Key::Meta,
        "CapsLock" => Key::CapsLock,
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeReleaseBackend {
        key_press_failures_remaining: usize,
        button_press_failures_remaining: usize,
        key_failures_remaining: usize,
        button_failures_remaining: usize,
        key_press_attempts: Vec<Key>,
        button_press_attempts: Vec<Button>,
        key_attempts: Vec<Key>,
        button_attempts: Vec<Button>,
    }

    impl PressBackend for FakeReleaseBackend {
        fn press_button(&mut self, button: Button) -> Result<(), String> {
            self.button_press_attempts.push(button);
            if self.button_press_failures_remaining > 0 {
                self.button_press_failures_remaining -= 1;
                return Err("scripted button press failure".into());
            }
            Ok(())
        }

        fn press_key(&mut self, key: Key) -> Result<(), String> {
            self.key_press_attempts.push(key);
            if self.key_press_failures_remaining > 0 {
                self.key_press_failures_remaining -= 1;
                return Err("scripted key press failure".into());
            }
            Ok(())
        }
    }

    impl ReleaseBackend for FakeReleaseBackend {
        fn release_button(&mut self, button: Button) -> Result<(), String> {
            self.button_attempts.push(button);
            if self.button_failures_remaining > 0 {
                self.button_failures_remaining -= 1;
                return Err("scripted button release failure".into());
            }
            Ok(())
        }

        fn release_key(&mut self, key: Key) -> Result<(), String> {
            self.key_attempts.push(key);
            if self.key_failures_remaining > 0 {
                self.key_failures_remaining -= 1;
                return Err("scripted key release failure".into());
            }
            Ok(())
        }
    }

    fn queued(action: InputAction) -> Cmd {
        Cmd::Event {
            route: "control-r1".into(),
            action,
            session_epoch: 7,
            route_epoch: 11,
        }
    }

    fn queued_key(key: &str, code: &str, down: bool) -> Cmd {
        queued(InputAction::Key {
            key: key.into(),
            code: Some(code.into()),
            down,
        })
    }

    #[test]
    fn saturated_discrete_queue_keeps_admitted_key_release_in_order() {
        let mut queue = InputQueue::new(4);
        assert!(queue.enqueue(queued_key("a", "KeyA", true)));
        // Auto-repeat consumes best-effort command slots but not additional
        // release reservations.
        assert!(queue.enqueue(queued_key("a", "KeyA", true)));
        assert!(queue.enqueue(queued_key("a", "KeyA", true)));
        assert_eq!(queue.accounted_len(), 4);

        // The reserved unit exchanges for this keyup even though no ordinary
        // slot is free.
        assert!(queue.enqueue(queued_key("a", "KeyA", false)));
        assert_eq!(queue.accounted_len(), 4);
        assert!(queue.admitted_presses.is_empty());

        let downs = queue
            .commands
            .into_iter()
            .map(|cmd| match cmd {
                Cmd::Event {
                    action: InputAction::Key { down, .. },
                    ..
                } => down,
                other => panic!("unexpected command: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(downs, vec![true, true, true, false]);
    }

    #[test]
    fn saturated_discrete_queue_keeps_admitted_mouse_release_in_order() {
        let mut queue = InputQueue::new(4);
        for _ in 0..3 {
            assert!(queue.enqueue(queued(InputAction::MouseButton {
                button: 0,
                down: true,
            })));
        }
        assert_eq!(queue.accounted_len(), 4);

        assert!(queue.enqueue(queued(InputAction::MouseButton {
            button: 0,
            down: false,
        })));
        assert_eq!(queue.accounted_len(), 4);
        assert!(queue.admitted_presses.is_empty());

        let downs = queue
            .commands
            .into_iter()
            .map(|cmd| match cmd {
                Cmd::Event {
                    action: InputAction::MouseButton { down, .. },
                    ..
                } => down,
                other => panic!("unexpected command: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(downs, vec![true, true, true, false]);
    }

    #[test]
    fn saturation_evicts_oldest_continuous_input_before_discrete_press() {
        let mut queue = InputQueue::new(6);
        assert!(queue.enqueue(queued_key("a", "KeyA", true)));
        assert!(queue.enqueue(queued(InputAction::MouseMove {
            x: 0.25,
            y: 0.5,
            screen: Some(1),
        })));
        assert!(queue.enqueue(queued(InputAction::Wheel { dx: 0.0, dy: 1.0 })));
        assert!(queue.enqueue(queued(InputAction::MouseMoveRel { dx: 2.0, dy: 3.0 })));
        assert_eq!(queue.accounted_len(), 5);

        // A first down needs its command plus its future release reservation.
        // Only the oldest lossy command is evicted; discrete order is intact.
        assert!(queue.enqueue(queued_key("b", "KeyB", true)));
        assert_eq!(queue.accounted_len(), 6);
        assert!(queue.enqueue(queued_key("a", "KeyA", false)));
        assert!(queue.enqueue(queued_key("b", "KeyB", false)));

        let kinds = queue
            .commands
            .into_iter()
            .map(|cmd| match cmd {
                Cmd::Event {
                    action: InputAction::Key { key, down, .. },
                    ..
                } => format!("{key}:{}", if down { "down" } else { "up" }),
                Cmd::Event {
                    action: InputAction::MouseMove { .. },
                    ..
                } => "absolute".into(),
                Cmd::Event {
                    action: InputAction::MouseMoveRel { .. },
                    ..
                } => "relative".into(),
                Cmd::Event {
                    action: InputAction::Wheel { .. },
                    ..
                } => "wheel".into(),
                other => panic!("unexpected command: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec!["a:down", "wheel", "relative", "b:down", "a:up", "b:up"]
        );
    }

    #[test]
    fn adjacent_continuous_input_coalesces_without_crossing_a_click() {
        let mut queue = InputQueue::new(8);
        assert!(queue.enqueue(queued(InputAction::MouseMove {
            x: 0.1,
            y: 0.2,
            screen: Some(1),
        })));
        assert!(queue.enqueue(queued(InputAction::MouseMove {
            x: 0.8,
            y: 0.9,
            screen: Some(1),
        })));
        assert_eq!(queue.commands.len(), 1);
        match &queue.commands[0] {
            Cmd::Event {
                action: InputAction::MouseMove { x, y, .. },
                ..
            } => assert_eq!((*x, *y), (0.8, 0.9)),
            other => panic!("unexpected command: {other:?}"),
        }

        assert!(queue.enqueue(queued(InputAction::MouseButton {
            button: 0,
            down: true,
        })));
        assert!(queue.enqueue(queued(InputAction::MouseMoveRel { dx: 2.0, dy: -1.0 })));
        assert!(queue.enqueue(queued(InputAction::MouseMoveRel { dx: 3.5, dy: 4.0 })));
        assert_eq!(queue.commands.len(), 3);
        match &queue.commands[2] {
            Cmd::Event {
                action: InputAction::MouseMoveRel { dx, dy },
                ..
            } => assert_eq!((*dx, *dy), (5.5, 3.0)),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn saturation_rejects_new_down_instead_of_spending_an_existing_release_reserve() {
        let mut queue = InputQueue::new(4);
        assert!(queue.enqueue(queued_key("a", "KeyA", true)));
        assert!(queue.enqueue(queued_key("a", "KeyA", true)));
        assert!(queue.enqueue(queued(InputAction::Unknown)));
        assert_eq!(queue.accounted_len(), 4);

        assert!(!queue.enqueue(queued_key("b", "KeyB", true)));
        assert!(queue.enqueue(queued_key("a", "KeyA", false)));
        assert_eq!(queue.accounted_len(), 4);
        assert!(queue.admitted_presses.is_empty());
    }

    #[test]
    fn route_and_session_cleanup_retire_only_matching_release_reservations() {
        let mut queue = InputQueue::new(8);
        assert!(queue.enqueue(queued_key("a", "KeyA", true)));
        let other_generation = Cmd::Event {
            route: "control-r1".into(),
            action: InputAction::MouseButton {
                button: 0,
                down: true,
            },
            session_epoch: 7,
            route_epoch: 12,
        };
        assert!(queue.enqueue(other_generation));
        assert_eq!(queue.admitted_presses.len(), 2);

        queue.clear_generation("control-r1", 7, 11);
        assert_eq!(queue.admitted_presses.len(), 1);
        assert!(queue
            .admitted_presses
            .iter()
            .all(|press| press.route_epoch == 12));

        queue.clear_session(7);
        assert!(queue.admitted_presses.is_empty());
    }

    #[test]
    fn printable_keys_inject_as_unicode() {
        let t = KeyTracker::default();
        assert_eq!(t.resolve("a", Some("KeyA")), Some(Key::Unicode('a')));
        assert_eq!(t.resolve("?", Some("Slash")), Some(Key::Unicode('?')));
        // Layout-resolved characters carry through untranslated.
        assert_eq!(t.resolve("ü", None), Some(Key::Unicode('ü')));
    }

    #[test]
    fn named_keys_map_and_unknown_ones_drop() {
        let t = KeyTracker::default();
        assert_eq!(t.resolve("Enter", None), Some(Key::Return));
        assert_eq!(t.resolve("ArrowLeft", None), Some(Key::LeftArrow));
        assert_eq!(t.resolve("MediaPlayPause", None), None);
    }

    #[test]
    fn chords_resolve_through_the_physical_key() {
        let mut t = KeyTracker::default();
        assert_eq!(t.press("Control", Some("ControlLeft")), Some(Key::Control));
        // Ctrl+C lands on the C key even when the sender's layout
        // composed something else under the held modifiers…
        assert_eq!(t.press("ç", Some("KeyC")), Some(Key::Unicode('c')));
        // …and the shifted variant of a chord stays on the same key.
        assert_eq!(t.press("C", Some("KeyC")), Some(Key::Unicode('c')));
        assert_eq!(t.press("!", Some("Digit1")), Some(Key::Unicode('1')));
        // Named keys chord by name, as always.
        assert_eq!(t.resolve("Tab", Some("Tab")), Some(Key::Tab));
        // An older sender carries no code — the character itself stands,
        // lower-cased so the held modifier isn't asked for twice.
        assert_eq!(t.resolve("T", None), Some(Key::Unicode('t')));
        assert_eq!(t.resolve("t", None), Some(Key::Unicode('t')));
    }

    #[test]
    fn shifted_typing_injects_the_unshifted_keycap() {
        let mut t = KeyTracker::default();
        assert_eq!(t.press("Shift", Some("ShiftLeft")), Some(Key::Shift));
        // The held Shift restores the case on this end.
        assert_eq!(t.resolve("A", Some("KeyA")), Some(Key::Unicode('a')));
        // A shifted symbol injects its key's unshifted keycap too, so the
        // held Shift composes the symbol — injecting "!" itself while Shift
        // is down lands on an empty shift level on X11 and types nothing.
        assert_eq!(t.resolve("!", Some("Digit1")), Some(Key::Unicode('1')));
        assert_eq!(t.resolve("?", Some("Slash")), Some(Key::Unicode('/')));
        // No physical code (older sender): a symbol can only stand as
        // itself, but a letter is still de-shifted by lower-casing.
        assert_eq!(t.resolve("@", None), Some(Key::Unicode('@')));
        assert_eq!(t.resolve("Z", None), Some(Key::Unicode('z')));
    }

    #[test]
    fn keyup_releases_what_its_keydown_pressed() {
        let mut t = KeyTracker::default();
        let mut backend = FakeReleaseBackend::default();
        t.press("Shift", Some("ShiftLeft"));
        // Shift+1 goes down as the unshifted keycap '1' (the held Shift
        // composes the '!' on the remote)…
        assert_eq!(t.press("!", Some("Digit1")), Some(Key::Unicode('1')));
        release_key(&mut backend, &mut t, "Shift", Some("ShiftLeft")).unwrap();
        // …and comes up as "1": the release lifts exactly the key that
        // went down, matched by its physical code regardless of the char.
        release_key(&mut backend, &mut t, "1", Some("Digit1")).unwrap();
        assert!(t.pressed.is_empty());
        // A keyup nothing matches (older sender) still resolves fresh.
        release_key(&mut backend, &mut t, "a", Some("KeyA")).unwrap();
        assert_eq!(
            backend.key_attempts,
            vec![Key::Shift, Key::Unicode('1'), Key::Unicode('a')]
        );
    }

    #[test]
    fn release_all_unwinds_in_reverse_press_order() {
        let mut state = RouteInputState::default();
        state.keys.press("Control", Some("ControlLeft"));
        state.keys.press("Shift", Some("ShiftLeft"));
        state.keys.press("T", Some("KeyT"));
        let mut backend = FakeReleaseBackend::default();
        release_input_state(&mut backend, &mut state, "test");
        assert_eq!(
            backend.key_attempts,
            vec![Key::Unicode('t'), Key::Shift, Key::Control]
        );
        assert!(state.is_empty());
    }

    #[test]
    fn repeated_downs_keep_one_held_entry() {
        let mut t = KeyTracker::default();
        t.press("a", Some("KeyA"));
        t.press("a", Some("KeyA"));
        assert_eq!(t.pressed.len(), 1);
        // Identity falls back to the key value when the code is missing.
        t.press("b", None);
        t.press("b", Some(""));
        assert_eq!(t.pressed.len(), 2);
    }

    #[test]
    fn keycaps_resolve_from_dom_codes() {
        assert_eq!(base_char("KeyA"), Some('a'));
        assert_eq!(base_char("Digit7"), Some('7'));
        assert_eq!(base_char("Numpad3"), Some('3'));
        assert_eq!(base_char("NumpadAdd"), Some('+'));
        assert_eq!(base_char("Space"), Some(' '));
        assert_eq!(base_char("BracketLeft"), Some('['));
        // Named keys have no keycap character — they travel by name.
        assert_eq!(base_char("Enter"), None);
        assert_eq!(base_char("F5"), None);
        assert_eq!(base_char("ControlLeft"), None);
    }

    #[test]
    fn coordinates_denormalize_onto_the_named_screen() {
        // A 1920×1080 primary with a 2560×1440 screen to its right.
        let right = ScreenRect {
            id: 7,
            x: 1920,
            y: 0,
            w: 2560,
            h: 1440,
            primary: false,
        };
        assert_eq!(right.denorm(0.0, 0.0), (1920, 0));
        assert_eq!(right.denorm(1.0, 1.0), (1920 + 2559, 1439));
        let (cx, cy) = right.denorm(0.5, 0.5);
        assert_eq!((cx, cy), (1920 + 1280, 720));
        // Out-of-range input clamps inside the screen, not off the edge.
        assert_eq!(right.denorm(7.0, -3.0), (1920 + 2559, 0));
    }

    #[test]
    fn resolve_prefers_the_named_screen_and_falls_back_to_primary() {
        let primary = ScreenRect {
            id: 1,
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
            primary: true,
        };
        let right = ScreenRect {
            id: 7,
            x: 1920,
            y: 0,
            w: 2560,
            h: 1440,
            primary: false,
        };
        let mut map = ScreenMap {
            rects: vec![primary, right],
            refreshed: Instant::now(),
        };
        assert_eq!(map.resolve(Some(7)), right);
        assert_eq!(map.resolve(None), primary);
        // An unknown id re-queries (nothing here in CI) and lands on the
        // primary of whatever the map now holds.
        let mut map = ScreenMap {
            rects: vec![primary, right],
            refreshed: Instant::now(),
        };
        let fallback = map.resolve(Some(99));
        assert!(fallback.primary);
    }

    #[test]
    fn only_real_mouse_buttons_inject() {
        assert_eq!(dom_button(0), Some(Button::Left));
        assert_eq!(dom_button(2), Some(Button::Right));
        assert_eq!(dom_button(4), None);
    }

    #[test]
    fn mouse_buttons_are_tracked_and_unwound_on_route_end() {
        let mut state = RouteInputState::default();
        assert_eq!(state.press_button(0), Some(Button::Left));
        assert_eq!(state.press_button(2), Some(Button::Right));
        // A repeated down remains one held state.
        assert_eq!(state.press_button(0), Some(Button::Left));
        assert_eq!(state.pressed_buttons, vec![2, 0]);
        let mut backend = FakeReleaseBackend::default();
        release_input_state(&mut backend, &mut state, "test");
        assert_eq!(backend.button_attempts, vec![Button::Left, Button::Right]);
        assert!(state.is_empty());
    }

    #[test]
    fn mouse_up_heals_an_unobserved_down() {
        let mut state = RouteInputState::default();
        let mut backend = FakeReleaseBackend::default();
        release_button(&mut backend, &mut state, 1).unwrap();
        assert_eq!(backend.button_attempts, vec![Button::Middle]);
        assert!(state.pressed_buttons.is_empty());
        assert_eq!(state.press_button(4), None);
        assert!(state.pressed_buttons.is_empty());
    }

    #[test]
    fn failed_os_presses_do_not_commit_held_state() {
        let mut state = RouteInputState::default();
        let mut backend = FakeReleaseBackend {
            key_press_failures_remaining: 1,
            button_press_failures_remaining: 1,
            ..Default::default()
        };

        assert!(inject_key_press(
            &mut backend,
            &mut state.keys,
            "Control",
            Some("ControlLeft")
        )
        .is_err());
        assert!(!state.keys.held(Key::Control));
        assert!(inject_button_press(&mut backend, &mut state, 0).is_err());
        assert!(!state.holds_button(0));

        let key_press = PressToken {
            route: "control-r1".into(),
            session_epoch: 7,
            route_epoch: 11,
            input: PressInput::Key("ControlLeft".into()),
        };
        let button_press = PressToken {
            route: "control-r1".into(),
            session_epoch: 7,
            route_epoch: 11,
            input: PressInput::MouseButton(0),
        };
        let mut failed_presses = HashSet::from([key_press.clone(), button_press.clone()]);
        assert!(suppress_unaccepted_release(
            &mut failed_presses,
            &key_press,
            state.keys.holds_identity("ControlLeft")
        ));
        assert!(suppress_unaccepted_release(
            &mut failed_presses,
            &button_press,
            state.holds_button(0)
        ));
        assert!(failed_presses.is_empty());
        assert!(backend.key_attempts.is_empty());
        assert!(backend.button_attempts.is_empty());

        inject_key_press(
            &mut backend,
            &mut state.keys,
            "Control",
            Some("ControlLeft"),
        )
        .unwrap();
        inject_button_press(&mut backend, &mut state, 0).unwrap();
        assert!(state.keys.held(Key::Control));
        assert!(state.holds_button(0));
        assert_eq!(backend.key_press_attempts, vec![Key::Control, Key::Control]);
        assert_eq!(
            backend.button_press_attempts,
            vec![Button::Left, Button::Left]
        );
    }

    #[test]
    fn failed_live_releases_retry_on_the_next_worker_wake() {
        let route = "control-r1".to_string();
        let mut state = RouteInputState::default();
        state.keys.press("Control", Some("ControlLeft"));
        state.press_button(0);
        let mut routes = HashMap::from([(route.clone(), state)]);
        let session_epoch = AtomicU64::new(7);
        let route_epochs = Mutex::new(HashMap::from([(route.clone(), 11)]));
        let mut backend = FakeReleaseBackend {
            key_failures_remaining: 1,
            button_failures_remaining: 1,
            ..Default::default()
        };

        let state = routes.get_mut(&route).unwrap();
        assert!(release_key(
            &mut backend,
            &mut state.keys,
            "Control",
            Some("ControlLeft")
        )
        .is_err());
        assert!(release_button(&mut backend, state, 0).is_err());
        let mut retries = vec![
            LiveReleaseRetry {
                route: route.clone(),
                session_epoch: 7,
                route_epoch: 11,
                input: PressInput::Key("ControlLeft".into()),
            },
            LiveReleaseRetry {
                route: route.clone(),
                session_epoch: 7,
                route_epoch: 11,
                input: PressInput::MouseButton(0),
            },
        ];

        retry_live_releases(
            &mut backend,
            &mut routes,
            &mut retries,
            &session_epoch,
            &route_epochs,
        );
        assert!(retries.is_empty());
        assert!(routes.get(&route).unwrap().is_empty());
        assert_eq!(backend.key_attempts, vec![Key::Control, Key::Control]);
        assert_eq!(backend.button_attempts, vec![Button::Left, Button::Left]);
    }

    #[test]
    fn stale_live_retry_never_releases_a_same_id_successor() {
        let route = "control-r1".to_string();
        let mut successor = RouteInputState::default();
        successor.keys.press("a", Some("KeyA"));
        successor.press_button(0);
        let mut routes = HashMap::from([(route.clone(), successor)]);
        let session_epoch = AtomicU64::new(7);
        let route_epochs = Mutex::new(HashMap::from([(route.clone(), 12)]));
        let mut retries = vec![
            LiveReleaseRetry {
                route: route.clone(),
                session_epoch: 7,
                route_epoch: 11,
                input: PressInput::Key("KeyA".into()),
            },
            LiveReleaseRetry {
                route: route.clone(),
                session_epoch: 7,
                route_epoch: 11,
                input: PressInput::MouseButton(0),
            },
        ];
        let mut backend = FakeReleaseBackend::default();

        retry_live_releases(
            &mut backend,
            &mut routes,
            &mut retries,
            &session_epoch,
            &route_epochs,
        );
        assert!(retries.is_empty());
        assert!(routes.get(&route).unwrap().keys.holds_identity("KeyA"));
        assert!(routes.get(&route).unwrap().holds_button(0));
        assert!(backend.key_attempts.is_empty());
        assert!(backend.button_attempts.is_empty());
    }

    #[test]
    fn same_generation_repress_adopts_failed_release_without_later_keyup() {
        let route = "control-r1".to_string();
        let mut state = RouteInputState::default();
        state.keys.press("a", Some("KeyA"));
        state.press_button(0);
        let mut routes = HashMap::from([(route.clone(), state)]);
        let session_epoch = AtomicU64::new(7);
        let route_epochs = Mutex::new(HashMap::from([(route.clone(), 11)]));
        let key = PressInput::Key("KeyA".into());
        let button = PressInput::MouseButton(0);
        let mut retries = vec![
            LiveReleaseRetry {
                route: route.clone(),
                session_epoch: 7,
                route_epoch: 11,
                input: key.clone(),
            },
            LiveReleaseRetry {
                route: route.clone(),
                session_epoch: 7,
                route_epoch: 11,
                input: button.clone(),
            },
        ];

        assert!(cancel_live_release_retry(&mut retries, &route, 7, 11, &key));
        assert!(cancel_live_release_retry(
            &mut retries,
            &route,
            7,
            11,
            &button
        ));
        assert!(retries.is_empty());

        let mut backend = FakeReleaseBackend::default();
        retry_live_releases(
            &mut backend,
            &mut routes,
            &mut retries,
            &session_epoch,
            &route_epochs,
        );
        assert!(routes.get(&route).unwrap().keys.holds_identity("KeyA"));
        assert!(routes.get(&route).unwrap().holds_button(0));
        assert!(backend.key_attempts.is_empty());
        assert!(backend.button_attempts.is_empty());
    }

    #[test]
    fn failed_keyup_retains_retry_authority_until_success() {
        let mut tracker = KeyTracker::default();
        tracker.press("Control", Some("ControlLeft"));
        let mut backend = FakeReleaseBackend {
            key_failures_remaining: 1,
            ..Default::default()
        };

        assert!(release_key(&mut backend, &mut tracker, "Control", Some("ControlLeft")).is_err());
        assert!(tracker.held(Key::Control));

        release_key(&mut backend, &mut tracker, "Control", Some("ControlLeft")).unwrap();
        assert!(!tracker.held(Key::Control));
        assert_eq!(backend.key_attempts, vec![Key::Control, Key::Control]);
    }

    #[test]
    fn failed_mouseup_retains_retry_authority_until_success() {
        let mut state = RouteInputState::default();
        state.press_button(0);
        let mut backend = FakeReleaseBackend {
            button_failures_remaining: 1,
            ..Default::default()
        };

        assert!(release_button(&mut backend, &mut state, 0).is_err());
        assert_eq!(state.pressed_buttons, vec![0]);

        release_button(&mut backend, &mut state, 0).unwrap();
        assert!(state.pressed_buttons.is_empty());
        assert_eq!(backend.button_attempts, vec![Button::Left, Button::Left]);
    }

    #[test]
    fn failed_route_cleanup_retries_without_claiming_the_reused_route() {
        let route = "control-r1".to_string();
        let mut old_state = RouteInputState::default();
        old_state.keys.press("Control", Some("ControlLeft"));
        old_state.press_button(0);
        let mut routes = HashMap::from([(route.clone(), old_state)]);
        let pending = Mutex::new(PendingReleases {
            all: false,
            routes: HashSet::from([route.clone()]),
        });
        let mut retries = Vec::new();
        let mut backend = FakeReleaseBackend {
            key_failures_remaining: 1,
            button_failures_remaining: 1,
            ..Default::default()
        };

        flush_pending_releases(&mut backend, &mut routes, &pending, &mut retries);
        assert!(routes.is_empty());
        assert_eq!(retries.len(), 1);
        assert!(!retries[0].state.is_empty());

        // A successor may reuse the route id, but predecessor cleanup lives in
        // the separate retry list and cannot consume the successor's state.
        let mut successor = RouteInputState::default();
        successor.keys.press("Shift", Some("ShiftLeft"));
        routes.insert(route.clone(), successor);

        flush_pending_releases(&mut backend, &mut routes, &pending, &mut retries);
        assert!(retries.is_empty());
        assert!(routes.get(&route).unwrap().keys.held(Key::Shift));
        assert_eq!(backend.key_attempts, vec![Key::Control, Key::Control]);
        assert_eq!(backend.button_attempts, vec![Button::Left, Button::Left]);
    }

    #[test]
    fn rejected_unknown_route_ids_do_not_allocate_release_state() {
        let injector = Injector::new();
        for n in 0..100_000 {
            injector.release_route(&format!("forged-{n}"));
        }
        assert!(injector.route_epochs.lock().is_empty());
        assert!(injector.pending_releases.lock().routes.is_empty());
    }

    #[test]
    fn teardown_fences_an_event_captured_before_authorization() {
        let injector = Injector::new();
        injector.activate_route("control-r1");
        let stale = injector.lease("control-r1").unwrap();

        // This is the route-state teardown that can race between the mesh's
        // authorization check and its enqueue. A same-id successor opens a
        // distinct generation.
        injector.release_route("control-r1");
        injector.activate_route("control-r1");
        let current = injector.lease("control-r1").unwrap();

        assert_ne!(stale, current);
        assert!(!event_generation_is_current(
            &injector.session_epoch,
            &injector.route_epochs,
            "control-r1",
            stale.session_epoch,
            stale.route_epoch,
        ));
        assert!(event_generation_is_current(
            &injector.session_epoch,
            &injector.route_epochs,
            "control-r1",
            current.session_epoch,
            current.route_epoch,
        ));
        assert_eq!(injector.route_epochs.lock().len(), 1);
    }
}
