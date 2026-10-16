//! A standalone application selects only realtime control tooling.
//! The normal dependency graph has no node, GUI, capture or codec libraries.

#[cfg(feature = "realtime")]
pub mod selected {
    use allmystuff_mesh_control::realtime::{FlowSpec, RealtimeSession, Result};

    /// Preserve both outcomes: a local write is not a delivery receipt, and a
    /// failed write must not skip the known flow's single awaited close.
    pub struct UnitAttempt {
        pub write: Result<()>,
        pub close: Result<()>,
    }

    /// The application supplies the explicit session, peer, encoding and limits.
    /// It retains the owner through cancellation and always awaits owner close
    /// afterwards, including when the open outcome is uncertain.
    pub async fn send_one_unit(
        owner: &mut RealtimeSession,
        spec: FlowSpec,
        duration_us: u32,
        payload: &[u8],
    ) -> Result<UnitAttempt> {
        let mut flow = owner.open_flow(spec).await?;
        let write = flow.send(duration_us, payload).await;
        let close = flow.close().await;
        Ok(UnitAttempt { write, close })
    }
}
