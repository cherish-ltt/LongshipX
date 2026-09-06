//! 帧编解码微基准:长度前缀编解码在不同载荷尺寸下的纯 CPU 开销(无网络 IO)。
//!
//! 运行:`cargo bench --bench frame_codec -p longshipx-net-kit`

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use longshipx_net_kit::codec::{Frame, encode_frame, read_frame};
use tokio::runtime::Builder;

/// 基准用帧长上限(大于最大样例 64KiB,避免命中上限拒绝路径)。
const MAX_FRAME_SIZE: usize = 256 * 1024;
/// 载荷尺寸梯度:空载荷 / 控制消息 / 小消息 / 大消息 / 帧上限量级。
const PAYLOAD_SIZES: [usize; 5] = [0, 64, 1024, 16 * 1024, 64 * 1024];

/// 构造填充固定字节的样例帧(0xA5,避免被压缩/去重类优化特殊化)。
fn sample_frame(opcode: u16, payload_len: usize) -> Frame {
    Frame::new(opcode, vec![0xA5; payload_len])
}

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("net-kit/frame_encode");
    for &size in &PAYLOAD_SIZES {
        let frame = sample_frame(0x0012, size);
        group.throughput(Throughput::Bytes(frame.wire_len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &frame, |b, frame| {
            b.iter(|| black_box(encode_frame(black_box(frame), MAX_FRAME_SIZE).unwrap()));
        });
    }
    group.finish();
}

fn bench_decode(c: &mut Criterion) {
    // 解析无 IO,仅用 current_thread 运行时驱动 async read_frame。
    let rt = Builder::new_current_thread().build().expect("构建运行时");
    let mut group = c.benchmark_group("net-kit/frame_decode");
    for &size in &PAYLOAD_SIZES {
        let bytes = encode_frame(&sample_frame(0x0012, size), MAX_FRAME_SIZE).unwrap();
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_function(BenchmarkId::from_parameter(size), |b| {
            b.iter(|| {
                rt.block_on(async {
                    // &[u8] 实现 AsyncRead:每次迭代从同一缓冲重新解析,无拷贝。
                    let mut cursor: &[u8] = black_box(bytes.as_slice());
                    let frame = read_frame(&mut cursor, MAX_FRAME_SIZE)
                        .await
                        .unwrap()
                        .unwrap();
                    black_box(frame)
                })
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_encode, bench_decode);
criterion_main!(benches);
