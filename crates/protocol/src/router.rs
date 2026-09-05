//! 消息路由表(PRD 4.3 protocol/router.rs):opcode → handler。
//!
//! 泛型参数 `C` 是每个连接的处理上下文(由网关定义),协议层不感知业务。

use crate::error::ProtocolError;
use crate::messages::{InboundMessage, OutboundMessage};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// 处理器返回的 boxed future。
pub type HandlerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<OutboundMessage>, ProtocolError>> + Send + 'a>>;

type BoxedHandler<C> = Arc<dyn Fn(C, InboundMessage) -> HandlerFuture<'static> + Send + Sync>;

/// opcode → handler 路由表;未注册的 opcode 返回 `UnsupportedOpcode`。
#[derive(Default, Clone)]
pub struct Router<C> {
    routes: HashMap<u16, BoxedHandler<C>>,
}

impl<C: Send + Sync + 'static> Router<C> {
    pub fn new() -> Self {
        Self {
            routes: HashMap::new(),
        }
    }

    /// 注册一个 opcode 的处理器。
    pub fn route<F, Fut>(&mut self, opcode: u16, handler: F)
    where
        F: Fn(C, InboundMessage) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Option<OutboundMessage>, ProtocolError>> + Send + 'static,
    {
        self.routes.insert(
            opcode,
            Arc::new(move |ctx, message| Box::pin(handler(ctx, message))),
        );
    }

    pub fn has_route(&self, opcode: u16) -> bool {
        self.routes.contains_key(&opcode)
    }

    pub fn route_count(&self) -> usize {
        self.routes.len()
    }

    /// 按 opcode 查找并调用处理器;消息自身携带的 opcode 用于查表。
    pub async fn dispatch(
        &self,
        ctx: C,
        message: InboundMessage,
    ) -> Result<Option<OutboundMessage>, ProtocolError> {
        let handler = self
            .routes
            .get(&message.opcode())
            .ok_or(ProtocolError::UnsupportedOpcode(message.opcode()))?
            .clone();
        handler(ctx, message).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::decode_inbound;
    use crate::opcodes::OP_C2S_HEARTBEAT;

    #[derive(Clone)]
    struct Ctx {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    fn build_router() -> Router<Ctx> {
        let mut router = Router::new();
        router.route(
            OP_C2S_HEARTBEAT,
            |ctx: Ctx, message: InboundMessage| async move {
                ctx.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                assert!(matches!(message, InboundMessage::Heartbeat(_)));
                Ok(None)
            },
        );
        router
    }

    #[tokio::test]
    async fn dispatches_registered_opcode() {
        let router = build_router();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ctx = Ctx {
            calls: calls.clone(),
        };
        let message = decode_inbound(OP_C2S_HEARTBEAT, &[]).unwrap();
        router.dispatch(ctx, message).await.unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn unregistered_opcode_is_rejected() {
        let router = build_router();
        let ctx = Ctx {
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let message = InboundMessage::Unknown(0x0777);
        let err = router.dispatch(ctx, message).await.unwrap_err();
        assert_eq!(err, ProtocolError::UnsupportedOpcode(0x0777));
    }

    #[test]
    fn route_metadata_is_exposed() {
        let router = build_router();
        assert!(router.has_route(OP_C2S_HEARTBEAT));
        assert!(!router.has_route(0x0009));
        assert_eq!(router.route_count(), 1);
    }
}
