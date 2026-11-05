use super::{KcpSettings, KcpStream, driver};
use crate::Transport;
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, Notify, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

struct Route {
    packets: mpsc::Sender<Vec<u8>>,
    abort: tokio::task::AbortHandle,
    closed_at: Option<Instant>,
}

fn expire_routes(routes: &mut HashMap<(SocketAddr, u32), Route>, idle: std::time::Duration) {
    let now = Instant::now();
    routes.retain(|_, route| {
        if !route.packets.is_closed() {
            return true;
        }
        now.duration_since(*route.closed_at.get_or_insert(now)) < idle
    });
}

pub struct KcpTransport {
    addr: SocketAddr,
    accepted: Mutex<mpsc::Receiver<(KcpStream, SocketAddr)>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    closed: watch::Sender<bool>,
}

impl KcpTransport {
    pub async fn close(&self) {
        self.closed.send_replace(true);
        if let Some(worker) = self.worker.lock().await.take() {
            worker.abort();
            let _ = worker.await;
        }
        let mut accepted = self.accepted.lock().await;
        accepted.close();
        while accepted.try_recv().is_ok() {}
    }

    /// Raw accept is deliberately separate from TLS so admission runs before the handshake.
    pub async fn bind(addr: SocketAddr, settings: KcpSettings) -> io::Result<Self> {
        settings.validate()?;
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let addr = socket.local_addr()?;
        let (tx, rx) = mpsc::channel(settings.max_sessions);
        let (closed, _) = watch::channel(false);
        let worker = tokio::spawn(async move {
            if let Err(err) = listen(socket, settings, tx).await {
                tracing::error!(error = %err, "KCP listener stopped");
            }
        });
        Ok(Self {
            addr,
            accepted: Mutex::new(rx),
            worker: Mutex::new(Some(worker)),
            closed,
        })
    }
}

#[async_trait::async_trait]
impl Transport for KcpTransport {
    type Conn = KcpStream;

    async fn accept(&self) -> io::Result<(Self::Conn, SocketAddr)> {
        let mut closed = self.closed.subscribe();
        let accepted = tokio::select! {
            biased;
            _ = closed.wait_for(|stop| *stop) => None,
            value = async { self.accepted.lock().await.recv().await } => value,
        };
        accepted.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "KCP listener stopped"))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.addr)
    }
}

impl Drop for KcpTransport {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.get_mut() {
            worker.abort();
        }
    }
}

async fn listen(
    socket: Arc<UdpSocket>,
    settings: KcpSettings,
    accepted: mpsc::Sender<(KcpStream, SocketAddr)>,
) -> io::Result<()> {
    let mut sessions = HashMap::<(SocketAddr, u32), Route>::new();
    let cleanup = Arc::new(Notify::new());
    let mut sweep = tokio::time::interval(settings.interval);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut packet = vec![0; settings.mtu as usize + 1];
    loop {
        tokio::select! {
            biased;
            _ = accepted.closed() => return Ok(()),
            _ = cleanup.notified() => expire_routes(&mut sessions, settings.idle_timeout),
            _ = sweep.tick() => expire_routes(&mut sessions, settings.idle_timeout),
            result = socket.recv_from(&mut packet) => {
                let (len, peer) = result?;
                let Some(conv) = driver::packet_conv(&packet[..len], settings.mtu) else { continue; };
                let key = (peer, conv);
                if let Some(route) = sessions.get(&key) {
                    if route.packets.try_send(packet[..len].to_vec()).is_err() { route.abort.abort(); }
                    continue;
                }
                // Closed routes occupy their bounded slot until expiry: late packets must not resurrect TLS sessions.
                expire_routes(&mut sessions, settings.idle_timeout);
                if sessions.len() >= settings.max_sessions { continue; }
                let (tx, rx) = mpsc::channel(settings.queue_capacity);
                let stream = KcpStream::spawn(socket.clone(), key, settings, rx, cleanup.clone())?;
                let abort = stream.abort_handle();
                tx.try_send(packet[..len].to_vec()).map_err(io::Error::other)?;
                if accepted.try_send((stream, peer)).is_ok() {
                    sessions.insert(key, Route { packets: tx, abort, closed_at: None });
                }
            }
        }
    }
}
