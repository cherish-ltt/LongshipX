//! 连接管线基准:loopback 真实 TCP/TLS 链路上的握手、小帧往返延迟与大帧吞吐。
//!
//! 服务端走 `split_connection` 完整路径(读半部解析 + 有界发送队列 + 写 task 冲刷),
//! 因此数字包含框架的全部每连接开销。
//!
//! 运行:`cargo bench --bench connection -p longshipx-net-kit`

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use longshipx_net_kit::backpressure::OutboundSender;
use longshipx_net_kit::codec::Frame;
use longshipx_net_kit::connection::{ConnectionConfig, FrameReader, split_connection};
use longshipx_net_kit::tls::server_acceptor_from_pem_bytes;
use rustls::pki_types::pem::PemObject as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio_rustls::client::TlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// 单帧上限(> 64KiB 大帧样例)。
const MAX_FRAME_SIZE: usize = 128 * 1024;
/// 发送队列容量(有界,模拟生产配置量级)。
const QUEUE_CAPACITY: usize = 256;
/// 往返延迟样例载荷。
const PING_PAYLOAD: usize = 32;
/// 吞吐样例:32 帧 × 64KiB,一次迭代写满后读回,测满管线吞吐。
const BULK_PAYLOAD: usize = 64 * 1024;
const BULK_FRAMES: usize = 32;

type ClientHalf = (FrameReader<TcpStream>, OutboundSender, JoinHandle<()>);
type TlsHalf = (
    FrameReader<TlsStream<TcpStream>>,
    OutboundSender,
    JoinHandle<()>,
);

fn bench_config() -> ConnectionConfig {
    ConnectionConfig {
        max_frame_size: MAX_FRAME_SIZE,
        send_queue_capacity: QUEUE_CAPACITY,
    }
}

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("构建运行时")
}

/// 基准进程独立的自签证书装置。
fn cert_pems() -> (&'static str, &'static str) {
    static CERTS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    let pair = CERTS.get_or_init(|| {
        let key = rcgen::KeyPair::generate().expect("生成密钥");
        let params = rcgen::CertificateParams::new(vec!["localhost".to_string()]).expect("参数");
        let cert = params.self_signed(&key).expect("自签");
        (cert.pem(), key.serialize_pem())
    });
    (pair.0.as_str(), pair.1.as_str())
}

fn tls_client() -> TlsConnector {
    let (cert_pem, _) = cert_pems();
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_slice_iter(cert_pem.as_bytes()) {
        roots.add(cert.expect("合法证书")).expect("证书入库");
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("TLS 版本")
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

fn server_domain() -> rustls::pki_types::ServerName<'static> {
    rustls::pki_types::ServerName::try_from("localhost".to_string()).expect("合法域名")
}

/// 回声:读半部逐帧解析后经有界发送队列原样回发(写 task 负责编码 + 冲刷)。
async fn echo_loop<S>(mut reader: FrameReader<S>, sender: OutboundSender)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    while let Ok(Some(frame)) = reader.read_frame().await {
        if sender.send_frame(frame).await.is_err() {
            break;
        }
    }
}

async fn serve_plain_echo(listener: TcpListener) {
    loop {
        let (io, _) = listener.accept().await.expect("accept 失败");
        let (reader, sender, _writer) = split_connection(io, bench_config());
        tokio::spawn(echo_loop(reader, sender));
    }
}

async fn serve_tls_echo(listener: TcpListener, acceptor: TlsAcceptor) {
    loop {
        let (io, _) = listener.accept().await.expect("accept 失败");
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            if let Ok(tls) = acceptor.accept(io).await {
                let (reader, sender, _writer) = split_connection(tls, bench_config());
                tokio::spawn(echo_loop(reader, sender));
            }
        });
    }
}

async fn serve_accept_only(listener: TcpListener, acceptor: TlsAcceptor) {
    loop {
        let (io, _) = listener.accept().await.expect("accept 失败");
        let acceptor = acceptor.clone();
        // 只完成握手并立即丢弃连接:隔离出纯 TLS 1.3 握手成本。
        tokio::spawn(async move {
            let _tls = acceptor.accept(io).await;
        });
    }
}

/// 启动服务端并返回监听地址;句柄由调用方持有(基准结束随之丢弃)。
fn spawn_plain_echo(rt: &Runtime) -> std::net::SocketAddr {
    rt.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定端口");
        let addr = listener.local_addr().expect("本地地址");
        tokio::spawn(serve_plain_echo(listener));
        addr
    })
}

fn spawn_tls_echo(rt: &Runtime) -> std::net::SocketAddr {
    rt.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定端口");
        let addr = listener.local_addr().expect("本地地址");
        let (cert, key) = cert_pems();
        let acceptor = server_acceptor_from_pem_bytes(cert, key).expect("构建 TLS acceptor");
        tokio::spawn(serve_tls_echo(listener, acceptor));
        addr
    })
}

fn spawn_accept_only(rt: &Runtime) -> std::net::SocketAddr {
    rt.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定端口");
        let addr = listener.local_addr().expect("本地地址");
        let (cert, key) = cert_pems();
        let acceptor = server_acceptor_from_pem_bytes(cert, key).expect("构建 TLS acceptor");
        tokio::spawn(serve_accept_only(listener, acceptor));
        addr
    })
}

async fn connect_plain(addr: std::net::SocketAddr) -> ClientHalf {
    let io = TcpStream::connect(addr).await.expect("连接失败");
    let (reader, sender, writer) = split_connection(io, bench_config());
    (reader, sender, writer)
}

async fn connect_tls(addr: std::net::SocketAddr, connector: &TlsConnector) -> TlsHalf {
    let tcp = TcpStream::connect(addr).await.expect("连接失败");
    let io = connector
        .connect(server_domain(), tcp)
        .await
        .expect("TLS 握手失败");
    let (reader, sender, writer) = split_connection(io, bench_config());
    (reader, sender, writer)
}

fn bench_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("net-kit/roundtrip");
    let rt = runtime();
    let frame = Frame::new(0x0012, vec![0u8; PING_PAYLOAD]);

    let addr = spawn_plain_echo(&rt);
    let (mut reader, sender, _writer) = rt.block_on(connect_plain(addr));
    group.bench_function("tcp_32B", |b| {
        b.iter(|| {
            rt.block_on(async {
                sender.send_frame(frame.clone()).await.expect("发送失败");
                black_box(
                    reader
                        .read_frame()
                        .await
                        .expect("读失败")
                        .expect("对端关闭"),
                )
            })
        });
    });

    let addr = spawn_tls_echo(&rt);
    let connector = tls_client();
    let (mut reader, sender, _writer) = rt.block_on(connect_tls(addr, &connector));
    group.bench_function("tls_32B", |b| {
        b.iter(|| {
            rt.block_on(async {
                sender.send_frame(frame.clone()).await.expect("发送失败");
                black_box(
                    reader
                        .read_frame()
                        .await
                        .expect("读失败")
                        .expect("对端关闭"),
                )
            })
        });
    });

    group.finish();
}

fn bench_handshake(c: &mut Criterion) {
    let mut group = c.benchmark_group("net-kit/tls_handshake");
    let rt = runtime();
    let addr = spawn_accept_only(&rt);
    let connector = tls_client();

    group.bench_function("connect_tls13", |b| {
        b.iter(|| {
            rt.block_on(async {
                let tcp = TcpStream::connect(addr).await.expect("连接失败");
                black_box(
                    connector
                        .connect(server_domain(), tcp)
                        .await
                        .expect("握手失败"),
                )
            })
        });
    });

    group.finish();
}

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("net-kit/bulk_throughput");
    let rt = runtime();
    let frames: Vec<Frame> = (0..BULK_FRAMES)
        .map(|_| Frame::new(0x0012, vec![0u8; BULK_PAYLOAD]))
        .collect();
    let total_bytes = BULK_FRAMES as u64 * (BULK_PAYLOAD + 6) as u64;

    let addr = spawn_plain_echo(&rt);
    let (mut reader, sender, _writer) = rt.block_on(connect_plain(addr));
    group.throughput(Throughput::Bytes(total_bytes));
    group.bench_function("tcp_64KiB_x32", |b| {
        b.iter(|| {
            rt.block_on(async {
                for frame in &frames {
                    sender.send_frame(frame.clone()).await.expect("发送失败");
                }
                for _ in 0..BULK_FRAMES {
                    black_box(
                        reader
                            .read_frame()
                            .await
                            .expect("读失败")
                            .expect("对端关闭"),
                    );
                }
            })
        });
    });

    let addr = spawn_tls_echo(&rt);
    let connector = tls_client();
    let (mut reader, sender, _writer) = rt.block_on(connect_tls(addr, &connector));
    group.throughput(Throughput::Bytes(total_bytes));
    group.bench_function("tls_64KiB_x32", |b| {
        b.iter(|| {
            rt.block_on(async {
                for frame in &frames {
                    sender.send_frame(frame.clone()).await.expect("发送失败");
                }
                for _ in 0..BULK_FRAMES {
                    black_box(
                        reader
                            .read_frame()
                            .await
                            .expect("读失败")
                            .expect("对端关闭"),
                    );
                }
            })
        });
    });

    group.finish();
}

criterion_group!(benches, bench_roundtrip, bench_handshake, bench_throughput);
criterion_main!(benches);
