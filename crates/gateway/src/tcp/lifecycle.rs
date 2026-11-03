use super::context::{AuthState, ConnContext, GatewayDeps};
use parking_lot::Mutex;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::AbortHandle;
use uuid::Uuid;

pub(super) struct ConnectionGuard {
    deps: Arc<GatewayDeps>,
    auth: Arc<Mutex<AuthState>>,
    conn_id: Uuid,
    peer_ip: IpAddr,
    pub writer: AbortHandle,
    pub translator: AbortHandle,
    pub cleanup_timeout: Duration,
    cleaned: bool,
}

impl ConnectionGuard {
    pub fn new(
        ctx: ConnContext,
        writer: AbortHandle,
        translator: AbortHandle,
        cleanup_timeout: Duration,
    ) -> Self {
        Self {
            deps: ctx.deps.clone(),
            auth: ctx.auth.clone(),
            conn_id: ctx.conn_id,
            peer_ip: ctx.peer_ip,
            writer,
            translator,
            cleanup_timeout,
            cleaned: false,
        }
    }

    pub async fn cleanup(&mut self) {
        cleanup_room(&self.deps, &self.auth, self.conn_id, self.cleanup_timeout).await;
        self.cleaned = true;
    }
}

async fn cleanup_room(
    deps: &GatewayDeps,
    auth: &Mutex<AuthState>,
    conn_id: Uuid,
    timeout: Duration,
) {
    let player = auth.lock().player.clone();
    if let Some(player) = player {
        match tokio::time::timeout(
            timeout,
            deps.rooms.leave_all(player.player_id, "connection closed"),
        )
        .await
        {
            Ok(Ok(())) => {},
            Ok(Err(err)) => {
                tracing::error!(error = %err, conn = %conn_id, "room cleanup failed")
            },
            Err(_) => tracing::error!(conn = %conn_id, "room cleanup deadline exceeded"),
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.writer.abort();
        self.translator.abort();
        let authenticated = self.auth.lock().player.is_some();
        if !authenticated {
            self.deps.auth_gate.release(self.peer_ip);
        }
        if !self.cleaned && authenticated {
            // Aborting a handler must not skip the FIFO leave command after an in-flight join.
            let deps = self.deps.clone();
            let auth = self.auth.clone();
            let conn_id = self.conn_id;
            let timeout = self.cleanup_timeout;
            tokio::spawn(async move {
                cleanup_room(&deps, &auth, conn_id, timeout).await;
                deps.connections.remove(conn_id);
                metrics::gauge!("longshipx_connections_active").decrement(1.0);
            });
        } else {
            self.deps.connections.remove(self.conn_id);
            metrics::gauge!("longshipx_connections_active").decrement(1.0);
        }
    }
}
