/// One open terminal session a host advertises in answer to a
/// [`RouteControl::TerminalSessionsRequest`] — the row shape the viewer's
/// session picker renders so a fleet member (or another window of this very
/// machine) can discover and attach to a *shared* shell instead of always
/// minting a new one. Mirrors the host engine's `SessionInfo` without the
/// node crate's `terminal` types leaking into the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSessionInfo {
    /// The host-side session id an [`RouteControl::Offer::session`] names to
    /// attach (`term-N`, or whatever the host minted).
    pub session_id: String,
    /// Friendly title (the shell's, falling back to the session id).
    pub title: String,
    /// Unix seconds the session was created — the picker shows its age.
    pub created_unix: u64,
    /// How many viewers are currently attached — `> 1` means already shared.
    pub attachers: usize,
}
