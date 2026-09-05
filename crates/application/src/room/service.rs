//! RoomService:跨房间门面。负责房间的创建/查找/命令投递与领域事件发布;
//! 房间内部状态由各自 Actor 串行持有(无全局锁,PRD 9.2 🔴)。

use crate::error::AppError;
use crate::ports::EventPublisher;
use crate::room::actor::{MAX_CHAT_CHARS, RoomActor, RoomCommand, RoomEvent, RoomHandle};
use chrono::Utc;
use longshipx_domain::{DomainEvent, PlayerId, Room, RoomId};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone, Copy)]
pub struct RoomServiceConfig {
    /// 每个 Room Actor 命令队列容量(有界背压)。
    pub command_capacity: usize,
    /// 单房间最大玩家数。
    pub max_players: u32,
}

pub struct RoomService {
    registry: Mutex<HashMap<RoomId, RoomHandle>>,
    publisher: Arc<dyn EventPublisher>,
    config: RoomServiceConfig,
}

impl RoomService {
    pub fn new(config: RoomServiceConfig, publisher: Arc<dyn EventPublisher>) -> Self {
        Self {
            registry: Mutex::new(HashMap::new()),
            publisher,
            config,
        }
    }

    /// 加入指定房间;`room_id` 为空则创建新房间并由调用者成为首名成员。
    pub async fn join_or_create(
        &self,
        room_id: Option<RoomId>,
        player_id: PlayerId,
        nickname: String,
        sink: mpsc::Sender<RoomEvent>,
    ) -> Result<RoomId, AppError> {
        match room_id {
            Some(id) => {
                self.join(id, player_id, nickname, sink).await?;
                Ok(id)
            },
            None => self.create_and_join(player_id, nickname, sink).await,
        }
    }

    async fn create_and_join(
        &self,
        player_id: PlayerId,
        nickname: String,
        sink: mpsc::Sender<RoomEvent>,
    ) -> Result<RoomId, AppError> {
        let room = Room::open(self.config.max_players, Utc::now());
        let handle = RoomActor::spawn(room, self.config.command_capacity);
        let room_id = handle.room_id();
        self.registry.lock().await.insert(room_id, handle.clone());
        if let Err(err) = self.join(room_id, player_id, nickname, sink).await {
            self.registry.lock().await.remove(&room_id);
            return Err(err);
        }
        Ok(room_id)
    }

    async fn join(
        &self,
        room_id: RoomId,
        player_id: PlayerId,
        nickname: String,
        sink: mpsc::Sender<RoomEvent>,
    ) -> Result<(), AppError> {
        let handle = self
            .handle_for(room_id)
            .await
            .ok_or_else(|| AppError::NotFound("房间不存在".into()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(RoomCommand::Join {
                player_id,
                nickname,
                sink,
                reply: reply_tx,
            })
            .await?;
        reply_rx
            .await
            .map_err(|_| AppError::Internal("房间已关闭,加入结果未知".into()))??;
        self.publish(DomainEvent::PlayerJoinedRoom {
            room_id,
            player_id,
            at: Utc::now(),
        })
        .await;
        Ok(())
    }

    /// 聊天:热路径,非阻塞投递;队列满返回 Busy。
    pub async fn chat(
        &self,
        room_id: RoomId,
        player_id: PlayerId,
        text: String,
    ) -> Result<(), AppError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(AppError::Validation("聊天内容不能为空".into()));
        }
        if trimmed.chars().count() > MAX_CHAT_CHARS {
            return Err(AppError::Validation(format!(
                "聊天内容不能超过 {MAX_CHAT_CHARS} 字"
            )));
        }
        let handle = self
            .handle_for(room_id)
            .await
            .ok_or_else(|| AppError::NotFound("房间不存在".into()))?;
        handle.try_send(RoomCommand::Chat {
            player_id,
            text: trimmed.to_string(),
        })
    }

    /// 离开房间(幂等:房间不存在视为已离开)。
    pub async fn leave(
        &self,
        room_id: RoomId,
        player_id: PlayerId,
        reason: String,
    ) -> Result<(), AppError> {
        let Some(handle) = self.handle_for(room_id).await else {
            return Ok(());
        };
        handle.try_send(RoomCommand::Leave { player_id, reason })?;
        self.publish(DomainEvent::PlayerLeftRoom {
            room_id,
            player_id,
            reason: "client leave".into(),
            at: Utc::now(),
        })
        .await;
        Ok(())
    }

    /// 断线清理:把玩家从其所在的所有房间移除。
    pub async fn leave_all(&self, player_id: PlayerId, reason: &str) -> Result<(), AppError> {
        for handle in self.live_handles().await {
            let _ = handle.try_send(RoomCommand::Leave {
                player_id,
                reason: reason.to_string(),
            });
        }
        Ok(())
    }

    /// 优雅停机:向所有房间广播 Close 并清空注册表(PRD 13.2)。
    pub async fn close_all(&self, reason: &str) -> Result<(), AppError> {
        let handles: Vec<RoomHandle> = {
            let mut registry = self.registry.lock().await;
            registry.drain().map(|(_, handle)| handle).collect()
        };
        for handle in handles {
            if let Err(err) = handle
                .send(RoomCommand::Close {
                    reason: reason.to_string(),
                })
                .await
            {
                tracing::warn!(error = %err, room = %handle.room_id(), "关闭房间失败");
            }
        }
        Ok(())
    }

    pub async fn active_rooms(&self) -> usize {
        self.registry.lock().await.len()
    }

    async fn handle_for(&self, room_id: RoomId) -> Option<RoomHandle> {
        let mut registry = self.registry.lock().await;
        let handle = registry.get(&room_id).cloned();
        match handle {
            Some(handle) if handle.is_closed() => {
                registry.remove(&room_id);
                None
            },
            Some(handle) => Some(handle),
            None => None,
        }
    }

    async fn live_handles(&self) -> Vec<RoomHandle> {
        let mut registry = self.registry.lock().await;
        registry.retain(|_, handle| !handle.is_closed());
        registry.values().cloned().collect()
    }

    /// 事件发布失败仅告警:事件是旁路观察点,不阻断房间操作(PRD 5.3)。
    async fn publish(&self, event: DomainEvent) {
        if let Err(err) = self.publisher.publish(event).await {
            tracing::warn!(error = %err, "领域事件发布失败");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingPublisher(AtomicUsize);

    #[async_trait::async_trait]
    impl EventPublisher for CountingPublisher {
        async fn publish(&self, _event: DomainEvent) -> Result<(), AppError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn service(max_players: u32) -> Arc<RoomService> {
        Arc::new(RoomService::new(
            RoomServiceConfig {
                command_capacity: 16,
                max_players,
            },
            Arc::new(CountingPublisher(AtomicUsize::new(0))),
        ))
    }

    #[tokio::test]
    async fn create_and_join_returns_new_room() {
        let rooms = service(4);
        let (tx, _rx) = mpsc::channel(64);
        let room_id = rooms
            .join_or_create(None, PlayerId(uuid::Uuid::now_v7()), "p".into(), tx)
            .await
            .unwrap();
        assert_eq!(rooms.active_rooms().await, 1);
        assert!(!room_id.0.is_nil());
    }

    #[tokio::test]
    async fn join_existing_room_and_chat() {
        let rooms = service(4);
        let creator = PlayerId(uuid::Uuid::now_v7());
        let room_id = {
            let (tx, _rx) = mpsc::channel(64);
            rooms
                .join_or_create(None, creator, "c".into(), tx)
                .await
                .unwrap()
        };
        let joiner = PlayerId(uuid::Uuid::now_v7());
        let (tx, mut rx) = mpsc::channel(64);
        rooms
            .join_or_create(Some(room_id), joiner, "j".into(), tx)
            .await
            .unwrap();
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, RoomEvent::MemberJoined { player_id, .. } if player_id == joiner));

        rooms
            .chat(room_id, joiner, " 一起打排位 ".into())
            .await
            .unwrap();
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, RoomEvent::Chat { text, .. } if text == "一起打排位"));
    }

    #[tokio::test]
    async fn chat_validates_input() {
        let rooms = service(4);
        let player = PlayerId(uuid::Uuid::now_v7());
        let room_id = {
            let (tx, _rx) = mpsc::channel(64);
            rooms
                .join_or_create(None, player, "c".into(), tx)
                .await
                .unwrap()
        };
        assert!(matches!(
            rooms.chat(room_id, player, "   ".into()).await,
            Err(AppError::Validation(_))
        ));
        let long = "字".repeat(MAX_CHAT_CHARS + 1);
        assert!(matches!(
            rooms.chat(room_id, player, long).await,
            Err(AppError::Validation(_))
        ));
        // 未加入房间的玩家聊天:投递成功,但 Actor 忽略。
        assert!(
            rooms
                .chat(room_id, PlayerId(uuid::Uuid::now_v7()), "hi".into())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn chat_to_missing_room_is_not_found() {
        let rooms = service(4);
        let ghost = RoomId(uuid::Uuid::now_v7());
        let err = rooms
            .chat(ghost, PlayerId(uuid::Uuid::now_v7()), "hi".into())
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[tokio::test]
    async fn leave_all_and_close_all_shut_down() {
        let rooms = service(4);
        let (tx, mut rx) = mpsc::channel(64);
        rooms
            .join_or_create(None, PlayerId(uuid::Uuid::now_v7()), "c".into(), tx)
            .await
            .unwrap();
        rooms
            .leave_all(PlayerId(uuid::Uuid::now_v7()), "disconnect")
            .await
            .unwrap();
        rooms.close_all("维护停机").await.unwrap();
        // 最后一名成员的连接最终会收到 Closed 广播。
        let mut got_closed = false;
        while let Ok(event) =
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
        {
            if matches!(event, Some(RoomEvent::Closed { reason, .. }) if reason == "维护停机") {
                got_closed = true;
                break;
            }
        }
        assert!(got_closed);
        assert_eq!(rooms.active_rooms().await, 0);
    }

    #[tokio::test]
    async fn join_full_room_reports_conflict() {
        let rooms = service(1);
        let room_id = {
            let (tx, _rx) = mpsc::channel(64);
            rooms
                .join_or_create(None, PlayerId(uuid::Uuid::now_v7()), "c".into(), tx)
                .await
                .unwrap()
        };
        let (tx, _rx) = mpsc::channel(64);
        let err = rooms
            .join_or_create(
                Some(room_id),
                PlayerId(uuid::Uuid::now_v7()),
                "late".into(),
                tx,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }
}
