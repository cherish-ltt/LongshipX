//! 协议层基准:protobuf 消息与帧的编解码往返、路由表分发开销(纯 CPU,无网络 IO)。
//!
//! 运行:`cargo bench --bench codec -p longshipx-protocol`

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use longshipx_net_kit::codec::{Codec as _, encode_frame};
use longshipx_protocol::generated as pb;
use longshipx_protocol::{ClientCodec, GameCodec, InboundMessage, OutboundMessage, Router};
use tokio::runtime::Builder;

/// 基准用帧长上限(> 4KiB 最大样本)。
const MAX_FRAME_SIZE: usize = 256 * 1024;

/// 典型 C2S 样本:覆盖轻量控制消息与大小两档聊天载荷。
fn inbound_samples() -> Vec<(&'static str, InboundMessage)> {
    vec![
        (
            "bind",
            InboundMessage::Bind(pb::BindRequest {
                token: "t".repeat(32),
            }),
        ),
        (
            "heartbeat",
            InboundMessage::Heartbeat(pb::HeartbeatPing { client_ts_ms: 1 }),
        ),
        (
            "join_room",
            InboundMessage::JoinRoom(pb::JoinRoomRequest { room_id: None }),
        ),
        (
            "chat_64B",
            InboundMessage::RoomChat(pb::RoomChatRequest {
                text: "a".repeat(64),
            }),
        ),
        (
            "chat_4KiB",
            InboundMessage::RoomChat(pb::RoomChatRequest {
                text: "a".repeat(4096),
            }),
        ),
        (
            "get_profile",
            InboundMessage::GetProfile(pb::GetProfileRequest {}),
        ),
    ]
}

/// 典型 S2C 样本:绑定回执 / 心跳应答 / 房间聊天广播 / 玩家档案。
fn outbound_samples() -> Vec<(&'static str, OutboundMessage)> {
    let chat = pb::room_event_notification::RoomChat {
        room_id: "r".repeat(36),
        sender_id: "p".repeat(36),
        sender_nickname: "n".to_string(),
        text: "a".repeat(64),
    };
    vec![
        (
            "bind_result",
            OutboundMessage::BindResult(pb::BindResult {
                ok: true,
                player_id: Some("p".repeat(36)),
                nickname: Some("基准员".to_string()),
                error: None,
            }),
        ),
        (
            "heartbeat_ack",
            OutboundMessage::HeartbeatAck(pb::HeartbeatAck { server_ts_ms: 1 }),
        ),
        (
            "room_event_chat",
            OutboundMessage::RoomEvent(pb::RoomEventNotification {
                event: Some(pb::room_event_notification::Event::Chat(chat)),
            }),
        ),
        (
            "profile",
            OutboundMessage::Profile(pb::ProfileResponse {
                ok: true,
                player_id: Some("p".repeat(36)),
                nickname: Some("基准员".to_string()),
                level: Some(10),
                exp: Some(1234),
                last_login_at_ms: Some(1),
                error: None,
            }),
        ),
    ]
}

fn bench_client_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/client_encode");
    for (name, message) in inbound_samples() {
        let wire_len = ClientCodec.encode(&message).unwrap().wire_len() as u64;
        group.throughput(Throughput::Bytes(wire_len));
        group.bench_function(BenchmarkId::from_parameter(name), move |b| {
            b.iter(|| black_box(ClientCodec.encode(black_box(&message)).unwrap()));
        });
    }
    group.finish();
}

fn bench_server_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/server_decode");
    for (name, message) in inbound_samples() {
        let frame = ClientCodec.encode(&message).unwrap();
        let wire_len = encode_frame(&frame, MAX_FRAME_SIZE).unwrap().len() as u64;
        group.throughput(Throughput::Bytes(wire_len));
        group.bench_function(BenchmarkId::from_parameter(name), move |b| {
            b.iter(|| black_box(GameCodec.decode(black_box(&frame)).unwrap()));
        });
    }
    group.finish();
}

fn bench_server_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/server_encode");
    for (name, message) in outbound_samples() {
        let wire_len = GameCodec.encode(&message).unwrap().wire_len() as u64;
        group.throughput(Throughput::Bytes(wire_len));
        group.bench_function(BenchmarkId::from_parameter(name), move |b| {
            b.iter(|| black_box(GameCodec.encode(black_box(&message)).unwrap()));
        });
    }
    group.finish();
}

fn bench_client_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/client_decode");
    for (name, message) in outbound_samples() {
        let frame = GameCodec.encode(&message).unwrap();
        let wire_len = encode_frame(&frame, MAX_FRAME_SIZE).unwrap().len() as u64;
        group.throughput(Throughput::Bytes(wire_len));
        group.bench_function(BenchmarkId::from_parameter(name), move |b| {
            b.iter(|| black_box(ClientCodec.decode(black_box(&frame)).unwrap()));
        });
    }
    group.finish();
}

/// 客户端编码 + 服务端解码的完整 C2S 转换链路。
fn bench_c2s_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocol/c2s_roundtrip");
    for (name, message) in inbound_samples() {
        group.bench_function(BenchmarkId::from_parameter(name), move |b| {
            b.iter(|| {
                let frame = ClientCodec.encode(&message).unwrap();
                black_box(GameCodec.decode(&frame).unwrap())
            });
        });
    }
    group.finish();
}

/// 路由表分发:命中(no-op 处理器)与未注册 opcode 的错误路径。
fn bench_router_dispatch(c: &mut Criterion) {
    let rt = Builder::new_current_thread().build().expect("构建运行时");
    let mut router: Router<u32> = Router::new();
    router.route(0x0002, |_ctx: u32, _message: InboundMessage| async {
        Ok(None)
    });

    let heartbeat = InboundMessage::Heartbeat(pb::HeartbeatPing { client_ts_ms: 1 });
    let unknown = InboundMessage::Unknown(0x0FFF);

    let mut group = c.benchmark_group("protocol/router_dispatch");
    group.bench_function("hit_heartbeat", |b| {
        b.iter(|| {
            rt.block_on(async { black_box(router.dispatch(0, heartbeat.clone()).await.unwrap()) })
        });
    });
    group.bench_function("miss_unknown", |b| {
        b.iter(|| {
            rt.block_on(async { black_box(router.dispatch(0, unknown.clone()).await.is_err()) })
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_client_encode,
    bench_server_decode,
    bench_server_encode,
    bench_client_decode,
    bench_c2s_roundtrip,
    bench_router_dispatch,
);
criterion_main!(benches);
