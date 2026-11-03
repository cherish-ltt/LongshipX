use super::*;
use longshipx_application::auth::LoginDependencies;
use longshipx_application::ports::AuditLogger;
use longshipx_application::{GetPlayerProfile, LoginUseCase, RegisterUseCase};
use longshipx_gateway::http::HttpState;
use longshipx_infrastructure::persistence::repositories::{
    InMemoryAccountRepository, InMemoryRegistrationStore,
};

struct Audit;

#[async_trait::async_trait]
impl AuditLogger for Audit {
    async fn record(
        &self,
        _: Option<longshipx_domain::PlayerId>,
        _: &str,
        _: String,
    ) -> Result<(), longshipx_application::AppError> {
        Ok(())
    }
}

async fn request(
    addr: std::net::SocketAddr,
    path: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let mut client = TestClient::connect(addr).await;
    let body = body.to_string();
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    client.writer.write_all(request.as_bytes()).await.unwrap();
    client.writer.flush().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        client.reader.read_to_end(&mut response),
    )
    .await
    .unwrap()
    .unwrap();
    let response = String::from_utf8(response).unwrap();
    let (header, body) = response.split_once("\r\n\r\n").unwrap();
    let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).unwrap())
}

#[tokio::test]
async fn https_register_login_to_kcp_profile_and_relogin_revocation() {
    let server = super::kcp::server(test_config("127.0.0.1:0".parse().unwrap())).await;
    let accounts = Arc::new(InMemoryAccountRepository::new());
    let hasher = Arc::new(
        longshipx_infrastructure::password::Argon2PasswordHasher::new(8192, 1, 1, 2).unwrap(),
    );
    let audit = Arc::new(Audit);
    let state = Arc::new(HttpState {
        register: Arc::new(RegisterUseCase::new(
            accounts.clone(),
            Arc::new(InMemoryRegistrationStore::new(
                accounts.clone(),
                server.players.clone(),
            )),
            hasher.clone(),
            audit.clone(),
        )),
        login: Arc::new(LoginUseCase::new(
            LoginDependencies {
                accounts,
                players: server.players.clone(),
                tokens: server.tokens.clone(),
                hasher,
                audit,
            },
            Duration::from_secs(60),
        )),
        profile: Arc::new(GetPlayerProfile::new(server.players.clone())),
        tokens: server.tokens.clone(),
    });
    let (cert, key) = test_certs();
    let tls = longshipx_net_kit::tls::server_config_from_pem_bytes(cert, key).unwrap();
    let addr = longshipx_gateway::http::serve_https(
        "127.0.0.1:0".parse().unwrap(),
        tls,
        longshipx_gateway::http::routes::router(state),
        server.shutdown_tx.subscribe(),
    )
    .await
    .unwrap();
    let credentials = serde_json::json!({"username":"blackbox", "password":"blackbox-password-2026", "nickname":"blackbox"});
    assert_eq!(request(addr, "/register", credentials.clone()).await.0, 201);
    let (status, reply) = request(addr, "/login", credentials.clone()).await;
    assert_eq!(status, 200);
    let mut client = TestClient::connect_transport(server.addr, true).await;
    client
        .send(&InboundMessage::Bind(pb::BindRequest {
            token: reply["token"].as_str().unwrap().into(),
        }))
        .await;
    assert!(
        matches!(client.recv_expect("login token binds over KCP").await, OutboundMessage::BindResult(r) if r.ok)
    );
    client
        .send(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
        .await;
    assert!(
        matches!(client.recv_expect("registered profile").await, OutboundMessage::Profile(p) if p.ok && p.nickname.as_deref() == Some("blackbox"))
    );
    assert_eq!(request(addr, "/login", credentials).await.0, 200);
    client
        .send(&InboundMessage::GetProfile(pb::GetProfileRequest {}))
        .await;
    super::kcp::closed(&mut client).await;
    super::kcp::stop(server).await;
}

#[tokio::test]
#[ignore = "requires LONGSHIPX_QUICKSTART_CLIENT_BIN pointing to the built example"]
async fn quickstart_client_process_smoke_tcp_and_kcp() {
    let binary = std::env::var("LONGSHIPX_QUICKSTART_CLIENT_BIN").unwrap();
    let dir = std::env::temp_dir().join(format!("longshipx-smoke-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&dir).unwrap();
    let cert = dir.join("ca.pem");
    std::fs::write(&cert, test_certs().0).unwrap();
    for kcp in [false, true] {
        let tokens = Arc::new(InMemoryTokenStore::new());
        let server = spawn_server_transport(
            test_config("127.0.0.1:0".parse().unwrap()),
            tokens.clone(),
            tokens,
            build_router(),
            kcp,
        )
        .await;
        let (bind, _) = make_player_with_token(&server, "cli-smoke").await;
        let child = tokio::process::Command::new(&binary)
            .args([
                "--transport",
                if kcp { "kcp" } else { "tcp" },
                "--server",
                &server.addr.to_string(),
                "--token",
                &bind.token,
                "--root-ca",
            ])
            .arg(&cert)
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(Duration::from_secs(15), child)
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.contains("BindResult ok=true")
                && stdout.contains("Profile: ok=true")
                && stdout.contains("全链路 OK"),
            "{stdout}"
        );
        println!(
            "transport={} quickstart_client exit=0 bind/profile/room/chat OK",
            if kcp { "kcp" } else { "tcp" }
        );
        super::kcp::stop(server).await;
    }
    std::fs::remove_dir_all(dir).unwrap();
}
