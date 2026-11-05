use super::KcpSettings;
use kcp_ovo::core::kcp::Kcp;
use kcp_ovo::helper::{KcpPacketHeader, current_millis};
use std::{io, net::SocketAddr, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

pub(super) fn packet_conv(mut packet: &[u8], mtu: u32) -> Option<u32> {
    if packet.len() > mtu as usize {
        return None;
    }
    let (header, data) = KcpPacketHeader::from_bytes(&mut packet).ok()?;
    (header.conv != 0 && header.frg == 0 && header.len as usize == data.len())
        .then_some(header.conv)
}

pub(super) fn core(
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    conv: u32,
    settings: KcpSettings,
) -> io::Result<Kcp> {
    let mut core = Kcp::new(conv, settings.core_config()).map_err(io::Error::other)?;
    let connected = socket.peer_addr().is_ok();
    core.set_output(move |packet| {
        let socket = socket.clone();
        async move {
            Ok(if connected {
                socket.send(&packet).await?
            } else {
                socket.send_to(&packet, peer).await?
            })
        }
    });
    Ok(core)
}

pub(super) async fn run(
    mut core: Kcp,
    pipe: DuplexStream,
    mut packets: mpsc::Receiver<Vec<u8>>,
    acknowledged: watch::Sender<u64>,
    settings: KcpSettings,
) -> io::Result<()> {
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let mut tick = tokio::time::interval(settings.interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_input = Instant::now();
    let mut sent = 0;
    let mut eof = false;
    let mut outgoing = vec![0; core.mss as usize];
    let mut incoming = Vec::new();
    let mut offset = 0;
    let mut inputs = 0;
    loop {
        if core.waitsnd() == 0 {
            acknowledged.send_replace(sent);
        }
        if incoming.is_empty() {
            match core.recv() {
                Ok(bytes) => {
                    incoming = bytes;
                    offset = 0;
                },
                Err(kcp_ovo::KcpError::QueueEmpty) => {},
                Err(err) => return Err(io::Error::other(err)),
            }
        }
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(last_input + settings.idle_timeout) => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "KCP peer idle"));
            },
            _ = tick.tick() => {
                tokio::time::timeout_at(last_input + settings.idle_timeout, core.update(current_millis()))
                    .await.map_err(io::Error::other)?.map_err(io::Error::other)?;
                inputs = 0;
            },
            packet = packets.recv() => {
                let Some(packet) = packet else { return Err(io::Error::new(io::ErrorKind::BrokenPipe, "KCP receiver closed")); };
                core.input(&packet).map_err(io::Error::other)?;
                last_input = Instant::now();
                inputs += 1;
                if inputs >= settings.queue_capacity {
                    // A duplicate flood must not grow the upstream ACK list between flush ticks.
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "KCP input budget exceeded"));
                }
            },
            result = writer.write(&incoming[offset..]), if !incoming.is_empty() => {
                let len = result?;
                if len == 0 { return Err(io::ErrorKind::WriteZero.into()); }
                offset += len;
                if offset == incoming.len() { incoming = Vec::new(); offset = 0; }
            },
            result = reader.read(&mut outgoing), if !eof && core.waitsnd() < usize::from(settings.window) => {
                let len = result?;
                eof = len == 0;
                if !eof {
                    core.send(&outgoing[..len]).map_err(io::Error::other)?;
                    sent += len as u64;
                }
            }
        }
    }
}
