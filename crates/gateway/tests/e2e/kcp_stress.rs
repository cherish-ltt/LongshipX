use super::*;

#[path = "../../benches/support/mod.rs"]
mod support;

#[path = "../../../net-kit/tests/support/allocations.rs"]
mod allocations;

fn resident_kib() -> u64 {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[tokio::test]
#[ignore = "bounded stress and resource comparison; run alone with --test-threads=1 --nocapture"]
async fn compare_tcp_kcp_64_256_connections() {
    support::raise_fd_limit();
    tokio::time::timeout(Duration::from_secs(180), async {
        for kcp in [false, true] {
            for count in [64, 256] {
                let mut config = test_config("127.0.0.1:0".parse().unwrap());
                config.max_connections = 256;
                config.accept_concurrency = 64;
                config.unauth_timeout = Duration::from_secs(15);
                config.heartbeat_timeout = Duration::from_secs(30);
                config.send_queue_capacity = 256;
                let tokens = Arc::new(InMemoryTokenStore::new());
                let mut settings = kcp_settings();
                settings.max_sessions = 256;
                settings.idle_timeout = Duration::from_secs(30);
                let server = spawn_server_transport(config, tokens.clone(), tokens, build_router(), false).await;
                let mut addr = server.addr;
                let mut kcp_task = None;
                if kcp {
                    let (cert, key) = test_certs();
                    let acceptor = longshipx_net_kit::tls::server_acceptor_from_pem_bytes(cert, key).unwrap();
                    let gateway = TcpGateway::bind_kcp(config, settings, acceptor, server.deps.clone()).await.unwrap();
                    addr = gateway.local_addr().unwrap();
                    let shutdown = server.shutdown_tx.subscribe();
                    kcp_task = Some(tokio::spawn(async move { gateway.run(shutdown).await }));
                }
                let mut clients = Vec::new();
                let rss = resident_kib();
                let heap = allocations::live_bytes();
                // Bind before moving into the next batch; the existing single-IP admission limit remains active.
                for offset in (0..count).step_by(4) {
                    let mut tasks = Vec::new();
                    for n in offset..(offset + 4).min(count) {
                        let (bind, _) = make_player_with_token(&server, &format!("load-{n}")).await;
                        tasks.push(move || async move {
                            let mut client = TestClient::connect_with_settings(addr, kcp, settings).await;
                            client.send(&InboundMessage::Bind(bind)).await;
                            assert!(matches!(client.recv_expect("stress bind").await, OutboundMessage::BindResult(r) if r.ok));
                            client
                        });
                    }
                    clients.extend(support::ConnectGate::new(4).run(tasks).await);
                }
                assert_eq!(server.deps.connections.active_count(), count);
                let rss_delta = resident_kib().saturating_sub(rss);
                let heap_delta = allocations::live_bytes().saturating_sub(heap);
                let start = std::time::Instant::now();
                let mut tasks = tokio::task::JoinSet::new();
                for mut client in clients {
                    tasks.spawn(async move {
                        let mut samples = Vec::new();
                        for n in 0..30 {
                            let start = std::time::Instant::now();
                            client.send(&InboundMessage::Heartbeat(pb::HeartbeatPing { client_ts_ms: n })).await;
                            assert!(matches!(client.recv_expect("stress heartbeat").await, OutboundMessage::HeartbeatAck(_)));
                            samples.push(start.elapsed().as_micros() as u64);
                        }
                        (client, samples)
                    });
                }
                let mut samples = Vec::new();
                let mut held = Vec::new();
                while let Some(result) = tasks.join_next().await {
                    let (client, times) = result.unwrap();
                    held.push(client);
                    samples.extend(times);
                }
                let elapsed = start.elapsed().as_secs_f64();
                samples.sort_unstable();
                println!("transport={} connections={count} requests={} p50_us={} p95_us={} p99_us={} requests_per_sec={:.1} rss_delta_kib={rss_delta} live_heap_delta_bytes={heap_delta}",
                    if kcp {"kcp+tls"} else {"tcp+tls"}, samples.len(), samples[samples.len()/2], samples[samples.len()*95/100], samples[samples.len()*99/100], samples.len() as f64 / elapsed);
                server.shutdown_tx.send_replace(true);
                if let Some(task) = kcp_task { task.await.unwrap(); }
                super::kcp::stop(server).await;
                drop(held);
            }
        }
    }).await.unwrap();
}

#[tokio::test]
#[ignore = "synthetic one-way latency comparison; run alone with --test-threads=1 --nocapture"]
async fn compare_tcp_kcp_delayed_business_messages() {
    tokio::time::timeout(Duration::from_secs(90), async {
        for kcp in [false, true] {
            let tokens = Arc::new(InMemoryTokenStore::new());
            let server = spawn_server_transport(test_config("127.0.0.1:0".parse().unwrap()), tokens.clone(), tokens, build_router(), kcp).await;
            let mut udp_proxy = None;
            let mut tcp_proxy = None;
            let addr = if kcp {
                let proxy = super::kcp_network::udp_proxy(server.addr, false, false, 20).await;
                let addr = proxy.addr;
                udp_proxy = Some(proxy);
                addr
            } else {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let addr = listener.local_addr().unwrap();
                tcp_proxy = Some(tokio::spawn(async move {
                    let (front, _) = listener.accept().await.unwrap();
                    let back = TcpStream::connect(server.addr).await.unwrap();
                    front.set_nodelay(true).unwrap();
                    back.set_nodelay(true).unwrap();
                    let (mut ar, mut aw) = front.into_split();
                    let (mut br, mut bw) = back.into_split();
                    async fn copy_delayed<R: tokio::io::AsyncRead + Unpin, W: tokio::io::AsyncWrite + Unpin>(r: &mut R, w: &mut W) {
                        let mut buffer = [0; 8192];
                        loop {
                            let n = r.read(&mut buffer).await.unwrap();
                            if n == 0 { return; }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            if w.write_all(&buffer[..n]).await.is_err() { return; }
                        }
                    }
                    tokio::select! {
                        _ = copy_delayed(&mut ar, &mut bw) => {},
                        _ = copy_delayed(&mut br, &mut aw) => {},
                    }
                }));
                addr
            };
            let rss = resident_kib();
            let heap = allocations::live_bytes();
            let mut client = TestClient::connect_transport(addr, kcp).await;
            let (bind, _) = make_player_with_token(&server, "delayed").await;
            client.send(&InboundMessage::Bind(bind)).await;
            assert!(matches!(client.recv_expect("delayed bind").await, OutboundMessage::BindResult(r) if r.ok));
            let rss_delta = resident_kib().saturating_sub(rss);
            let heap_delta = allocations::live_bytes().saturating_sub(heap);
            let mut samples = Vec::new();
            let total = std::time::Instant::now();
            for n in 0..100 {
                let start = std::time::Instant::now();
                client.send(&InboundMessage::Heartbeat(pb::HeartbeatPing {client_ts_ms:n})).await;
                assert!(matches!(client.recv_expect("delayed heartbeat").await, OutboundMessage::HeartbeatAck(_)));
                samples.push(start.elapsed().as_micros() as u64);
            }
            let elapsed = total.elapsed().as_secs_f64();
            samples.sort_unstable();
            println!("transport={} synthetic_one_way_delay_ms=20 requests=100 p50_us={} p95_us={} p99_us={} requests_per_sec={:.1} rss_delta_kib={rss_delta} live_heap_delta_bytes={heap_delta}", if kcp {"kcp+tls"} else {"tcp+tls"}, samples[50], samples[95], samples[99], 100.0 / elapsed);
            super::kcp::stop(server).await;
            if let Some(task) = tcp_proxy { task.abort(); let _ = task.await; }
            drop(udp_proxy);
        }
    }).await.unwrap();
}
