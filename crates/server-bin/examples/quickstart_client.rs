//! 快速开始客户端(README 示例的可运行版本):
//! TCP+TLS 绑定 → 获取角色信息 → 加入房间聊天(HTTP 注册/登录见 README 的 curl 示例)。
//!
//! ```bash
//! # 1. 先通过 HTTP 拿 token(见 README):
//! #    curl -s -XPOST localhost:8081/login -d '{"username":"quickstart","password":"super-secret"}'
//! # 2. 运行示例(--root-ca 传 mkcert 根证书,可用 `mkcert -CAROOT` 查询):
//! cargo run -p ppt-tcp-server-bin --example quickstart_client -- \
//!   --token <上一步的token> --server 127.0.0.1:8080 \
//!   --root-ca "$(mkcert -CAROOT)/rootCA.pem"
//! ```

use ppt_tcp_net_kit::codec::{Codec as _, read_frame, write_frame};
use ppt_tcp_protocol::generated::{
    BindRequest, GetProfileRequest, JoinRoomRequest, RoomChatRequest,
};
use ppt_tcp_protocol::{ClientCodec, InboundMessage, OutboundMessage};
use rustls::pki_types::pem::PemObject;
use std::sync::Arc;

fn flag_value(args: &[String], flag: &str, default: &str) -> String {
    args.iter()
        .position(|item| item == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let token = flag_value(&args, "--token", "");
    if token.is_empty() {
        return Err("缺少 --token:请先通过 HTTP /login 获取(见本文件头部说明)".into());
    }
    let server_addr = flag_value(&args, "--server", "127.0.0.1:8080");
    let root_ca = flag_value(&args, "--root-ca", "$(mkcert -CAROOT)/rootCA.pem");

    // ── 第 1 步:建立 TCP+TLS 连接(信任 mkcert 根证书) ──
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls::pki_types::CertificateDer::pem_file_iter(&root_ca)? {
        roots.add(cert?)?;
    }
    let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
    let tcp = tokio::net::TcpStream::connect(&server_addr).await?;
    let domain = rustls::pki_types::ServerName::try_from("localhost".to_string())?;
    let (mut reader, mut writer) = tokio::io::split(connector.connect(domain, tcp).await?);
    println!("TLS 握手完成:{server_addr}");

    let max_frame = 65_536usize;
    // ── 第 2 步:绑定(建连后第一条消息必须是 Bind{token},PRD 8.3) ──
    write_frame(
        &mut writer,
        &ClientCodec.encode(&InboundMessage::Bind(BindRequest {
            token: token.clone(),
        }))?,
        max_frame,
    )
    .await?;
    match next_message(&mut reader).await? {
        OutboundMessage::BindResult(result) => {
            println!("BindResult ok={} player={:?}", result.ok, result.player_id);
        },
        other => return Err(format!("期望 BindResult,收到 {other:?}").into()),
    }

    // ── 第 3 步:获取角色信息(服务端权威数值,opcode 0x0013) ──
    write_frame(
        &mut writer,
        &ClientCodec.encode(&InboundMessage::GetProfile(GetProfileRequest {}))?,
        max_frame,
    )
    .await?;
    match next_message(&mut reader).await? {
        OutboundMessage::Profile(profile) => println!(
            "Profile: ok={} nickname={:?} level={:?} exp={:?} last_login={:?}",
            profile.ok, profile.nickname, profile.level, profile.exp, profile.last_login_at_ms
        ),
        other => return Err(format!("期望 Profile,收到 {other:?}").into()),
    }

    // ── 第 4 步:建房并聊天(回执走房间广播,opcode 0x0010/0x0012) ──
    write_frame(
        &mut writer,
        &ClientCodec.encode(&InboundMessage::JoinRoom(JoinRoomRequest { room_id: None }))?,
        max_frame,
    )
    .await?;
    if let OutboundMessage::RoomEvent(event) = next_message(&mut reader).await? {
        println!("房间事件:{event:?}");
    }
    write_frame(
        &mut writer,
        &ClientCodec.encode(&InboundMessage::RoomChat(RoomChatRequest {
            text: "大家好,我来了".into(),
        }))?,
        max_frame,
    )
    .await?;
    if let OutboundMessage::RoomEvent(event) = next_message(&mut reader).await? {
        println!("房间事件:{event:?}");
    }
    println!("示例完成:绑定 → 档案 → 房间 → 聊天 全链路 OK");
    Ok(())
}

async fn next_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<OutboundMessage, Box<dyn std::error::Error>> {
    let frame = read_frame(reader, 65_536).await?.ok_or("服务端关闭连接")?;
    Ok(ClientCodec.decode(&frame)?)
}
