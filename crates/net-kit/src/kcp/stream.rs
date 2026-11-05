use super::{KcpSettings, driver};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::net::UdpSocket;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

type Flush = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;

pub struct KcpStream {
    io: DuplexStream,
    worker: JoinHandle<()>,
    receiver: Option<JoinHandle<()>>,
    acknowledged: watch::Receiver<u64>,
    written: u64,
    flush: Option<Flush>,
    cleanup: Arc<Notify>,
}

impl KcpStream {
    pub(super) fn abort_handle(&self) -> tokio::task::AbortHandle {
        self.worker.abort_handle()
    }

    pub(super) fn spawn(
        socket: Arc<UdpSocket>,
        session: (SocketAddr, u32),
        settings: KcpSettings,
        packets: mpsc::Receiver<Vec<u8>>,
        cleanup: Arc<Notify>,
    ) -> io::Result<Self> {
        let (peer, conv) = session;
        let (io, pipe) = tokio::io::duplex(settings.mtu as usize * usize::from(settings.window));
        let (ack_tx, acknowledged) = watch::channel(0);
        let core = driver::core(socket, peer, conv, settings)?;
        let notify = cleanup.clone();
        let worker = tokio::spawn(async move {
            if let Err(err) = driver::run(core, pipe, packets, ack_tx, settings).await {
                tracing::debug!(%peer, error = %err, "KCP session ended");
            }
            notify.notify_one();
        });
        Ok(Self {
            io,
            worker,
            receiver: None,
            acknowledged,
            written: 0,
            flush: None,
            cleanup,
        })
    }

    /// Creates a raw stream for a TLS handshake, not an authenticated application channel.
    pub async fn connect(peer: SocketAddr, settings: KcpSettings) -> io::Result<Self> {
        settings.validate()?;
        let local = if peer.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = Arc::new(UdpSocket::bind(local).await?);
        socket.connect(peer).await?;
        let mut random = [0; 4];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let conv = u32::from_be_bytes(random).max(1);
        let (tx, rx) = mpsc::channel(settings.queue_capacity);
        let mut stream = Self::spawn(
            socket.clone(),
            (peer, conv),
            settings,
            rx,
            Arc::new(Notify::new()),
        )?;
        stream.receiver = Some(tokio::spawn(async move {
            let mut packet = vec![0; settings.mtu as usize + 1];
            loop {
                tokio::select! {
                    _ = tx.closed() => return,
                    result = socket.recv(&mut packet) => match result {
                        Ok(len) if driver::packet_conv(&packet[..len], settings.mtu) == Some(conv) => {
                            if tx.try_send(packet[..len].to_vec()).is_err() { return; }
                        },
                        Ok(_) => {},
                        Err(err) => { tracing::debug!(error = %err, "KCP receive failed"); return; },
                    }
                }
            }
        }));
        Ok(stream)
    }
}

impl AsyncRead for KcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for KcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.io).poll_write(cx, buf);
        if let Poll::Ready(Ok(len)) = result {
            self.written += len as u64;
            self.flush = None;
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if *self.acknowledged.borrow() >= self.written {
            return Poll::Ready(Ok(()));
        }
        let target = self.written;
        let mut ack = self.acknowledged.clone();
        let result = self
            .flush
            .get_or_insert_with(|| {
                Box::pin(async move {
                    ack.wait_for(|count| *count >= target)
                        .await
                        .map(|_| ())
                        .map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "KCP closed before acknowledgement",
                            )
                        })
                })
            })
            .as_mut()
            .poll(cx);
        if result.is_ready() {
            self.flush = None;
        }
        result
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        std::task::ready!(self.as_mut().poll_flush(cx))?;
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

impl Drop for KcpStream {
    fn drop(&mut self) {
        self.worker.abort();
        if let Some(receiver) = &self.receiver {
            receiver.abort();
        }
        self.cleanup.notify_one();
    }
}
