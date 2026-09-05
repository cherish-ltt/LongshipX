//! Room Actor:房间权威状态的唯一持有者,单 task 串行处理命令(PRD 9.1 🔴)。
//!
//! * 状态修改只发生在 Actor 内部,天然无锁、可并行(不同房间互不影响);
//! * 广播通过每个成员连接各自的 `RoomEvent` 有界通道投递;
//! * 🔴 广播通道满即丢弃该条消息(慢消费者不影响其他成员,PRD 8.5 策略一),
//!   需要应答的命令(join)通过 oneshot 把结果返回调用方。

use crate::error::AppError;
use longshipx_domain::{PlayerId, Room, RoomId};
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot, watch};

/// 每个成员的房间事件通道容量(由网关创建连接时给出)。
pub const ROOM_EVENT_CAPACITY: usize = 64;

/// 单条聊天文本的最大字符数(服务端权威输入校验,PRD 第 10 章)。
pub const MAX_CHAT_CHARS: usize = 512;

/// 发往 Room Actor 的命令。
pub enum RoomCommand {
    Join {
        player_id: PlayerId,
        nickname: String,
        sink: mpsc::Sender<RoomEvent>,
        reply: oneshot::Sender<Result<(), AppError>>,
    },
    Chat {
        player_id: PlayerId,
        text: String,
    },
    Leave {
        player_id: PlayerId,
        reason: String,
    },
    Close {
        reason: String,
    },
}

/// Actor 向成员连接广播的房间事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomEvent {
    MemberJoined {
        room_id: RoomId,
        player_id: PlayerId,
        nickname: String,
    },
    MemberLeft {
        room_id: RoomId,
        player_id: PlayerId,
    },
    Chat {
        room_id: RoomId,
        sender: PlayerId,
        nickname: String,
        text: String,
    },
    Closed {
        room_id: RoomId,
        reason: String,
    },
}

impl RoomEvent {
    pub fn room_id(&self) -> RoomId {
        match self {
            Self::MemberJoined { room_id, .. }
            | Self::MemberLeft { room_id, .. }
            | Self::Chat { room_id, .. }
            | Self::Closed { room_id, .. } => *room_id,
        }
    }
}

/// Actor 的外部句柄:命令入队 + 终止标志 + 房间标识。
#[derive(Clone)]
pub struct RoomHandle {
    room_id: RoomId,
    commands: mpsc::Sender<RoomCommand>,
    closed: watch::Receiver<bool>,
}

impl RoomHandle {
    /// 阻塞投递命令(通道满时等待,实现房间内命令背压)。
    pub async fn send(&self, command: RoomCommand) -> Result<(), AppError> {
        self.commands
            .send(command)
            .await
            .map_err(|_| AppError::NotFound("房间不存在".into()))
    }

    /// 非阻塞投递(热路径如聊天):满则返回 Busy。
    pub fn try_send(&self, command: RoomCommand) -> Result<(), AppError> {
        self.commands.try_send(command).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => AppError::Busy,
            mpsc::error::TrySendError::Closed(_) => AppError::NotFound("房间不存在".into()),
        })
    }

    pub fn room_id(&self) -> RoomId {
        self.room_id
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub async fn wait_closed(&mut self) {
        while !*self.closed.borrow_and_update() {
            if self.closed.changed().await.is_err() {
                return;
            }
        }
    }
}

struct Member {
    nickname: String,
    sink: mpsc::Sender<RoomEvent>,
}

pub struct RoomActor {
    room: Room,
    members: HashMap<PlayerId, Member>,
    commands: mpsc::Receiver<RoomCommand>,
    closed_tx: watch::Sender<bool>,
    /// 是否曾有成员加入:空房间仅在"曾经有过人"后离开时自然终结。
    had_members: bool,
}

impl RoomActor {
    /// 创建并派生 Actor task;返回供网关/服务使用的句柄。
    pub fn spawn(room: Room, command_capacity: usize) -> RoomHandle {
        let room_id = room.id();
        let (commands_tx, commands_rx) = mpsc::channel(command_capacity);
        let (closed_tx, closed_rx) = watch::channel(false);
        let actor = Self {
            room,
            members: HashMap::new(),
            commands: commands_rx,
            closed_tx,
            had_members: false,
        };
        tokio::spawn(actor.run());
        RoomHandle {
            room_id,
            commands: commands_tx,
            closed: closed_rx,
        }
    }

    async fn run(mut self) {
        while let Some(command) = self.commands.recv().await {
            if self.handle(command) {
                break;
            }
        }
        self.finish("房间通道已关闭");
    }

    /// 处理一条命令;返回 true 表示 Actor 应当终止。
    fn handle(&mut self, command: RoomCommand) -> bool {
        match command {
            RoomCommand::Join {
                player_id,
                nickname,
                sink,
                reply,
            } => {
                self.handle_join(player_id, nickname, sink, reply);
            },
            RoomCommand::Chat { player_id, text } => self.handle_chat(player_id, text),
            RoomCommand::Leave { player_id, reason } => {
                self.handle_leave(player_id, &reason);
            },
            RoomCommand::Close { reason } => {
                self.finish(&reason);
                return true;
            },
        }
        self.had_members && self.room.member_count() == 0
    }

    fn handle_join(
        &mut self,
        player_id: PlayerId,
        nickname: String,
        sink: mpsc::Sender<RoomEvent>,
        reply: oneshot::Sender<Result<(), AppError>>,
    ) {
        let mut newly_joined = false;
        let outcome = match self.room.try_join(player_id) {
            Ok(true) => {
                self.members.insert(
                    player_id,
                    Member {
                        nickname: nickname.clone(),
                        sink,
                    },
                );
                newly_joined = true;
                self.had_members = true;
                Ok(())
            },
            Ok(false) => Ok(()), // 幂等重复加入
            Err(err) => Err(AppError::from(err)),
        };
        let _ = reply.send(outcome);
        if newly_joined {
            self.broadcast(RoomEvent::MemberJoined {
                room_id: self.room.id(),
                player_id,
                nickname,
            });
        }
    }

    fn handle_chat(&mut self, player_id: PlayerId, text: String) {
        let Some(member) = self.members.get(&player_id) else {
            return;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.chars().count() > MAX_CHAT_CHARS {
            return;
        }
        self.broadcast(RoomEvent::Chat {
            room_id: self.room.id(),
            sender: player_id,
            nickname: member.nickname.clone(),
            text: trimmed.to_string(),
        });
    }

    fn handle_leave(&mut self, player_id: PlayerId, reason: &str) {
        match self.room.try_leave(player_id) {
            Ok(true) => {
                // MemberLeft 作为离开回执广播给所有成员(含离开者)。
                self.broadcast(RoomEvent::MemberLeft {
                    room_id: self.room.id(),
                    player_id,
                });
                if self.had_members && self.room.member_count() == 0 {
                    // 最后一名成员离开:先广播 Closed(离开者仍在册,能收到),再回收其通道。
                    self.finish("最后一名成员已离开");
                }
                self.members.remove(&player_id);
            },
            Ok(false) => {},
            Err(err) => tracing::warn!(error = %err, "离开房间被领域规则拒绝"),
        }
        let _ = reason;
    }

    /// 向全体成员广播;投递策略:通道满则丢弃该成员的这条消息(PRD 8.5)。
    fn broadcast(&self, event: RoomEvent) {
        for member in self.members.values() {
            let _ = member.sink.try_send(event.clone());
        }
    }

    /// 关闭房间并广播 Closed;重复调用无副作用。
    fn finish(&mut self, reason: &str) {
        if self.room.close() {
            self.broadcast(RoomEvent::Closed {
                room_id: self.room.id(),
                reason: reason.to_string(),
            });
        }
        let _ = self.closed_tx.send_replace(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn room(max: u32) -> Room {
        Room::open(max, Utc::now())
    }

    fn player() -> PlayerId {
        PlayerId(uuid::Uuid::now_v7())
    }

    async fn join_actor(actor: &RoomHandle, player_id: PlayerId) -> mpsc::Receiver<RoomEvent> {
        let (tx, rx) = mpsc::channel(ROOM_EVENT_CAPACITY);
        let (reply_tx, reply_rx) = oneshot::channel();
        actor
            .send(RoomCommand::Join {
                player_id,
                nickname: "n".into(),
                sink: tx,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        rx
    }

    #[tokio::test]
    async fn join_broadcasts_member_joined_to_all() {
        let handle = RoomActor::spawn(room(4), 16);
        let p1 = player();
        let p2 = player();
        let (tx1, mut rx1) = mpsc::channel(ROOM_EVENT_CAPACITY);
        let (reply1, reply1_rx) = oneshot::channel();
        handle
            .send(RoomCommand::Join {
                player_id: p1,
                nickname: "a".into(),
                sink: tx1,
                reply: reply1,
            })
            .await
            .unwrap();
        reply1_rx.await.unwrap().unwrap();
        // 自己的加入广播就是加入回执。
        let own = rx1.recv().await.unwrap();
        assert!(matches!(own, RoomEvent::MemberJoined { player_id, .. } if player_id == p1));

        let (tx2, mut rx2) = mpsc::channel(ROOM_EVENT_CAPACITY);
        let (reply2, reply2_rx) = oneshot::channel();
        handle
            .send(RoomCommand::Join {
                player_id: p2,
                nickname: "b".into(),
                sink: tx2,
                reply: reply2,
            })
            .await
            .unwrap();
        reply2_rx.await.unwrap().unwrap();
        // 新成员的加入对双方均可见。
        let seen_by_1 = rx1.recv().await.unwrap();
        let seen_by_2 = rx2.recv().await.unwrap();
        assert!(matches!(seen_by_1, RoomEvent::MemberJoined { player_id, .. } if player_id == p2));
        assert!(matches!(seen_by_2, RoomEvent::MemberJoined { player_id, .. } if player_id == p2));
    }

    #[tokio::test]
    async fn chat_reaches_members_but_not_outsiders() {
        let handle = RoomActor::spawn(room(4), 16);
        let insider = player();
        let mut insider_rx = join_actor(&handle, insider).await;
        insider_rx.recv().await.unwrap(); // 消费自己的 join 事件

        handle
            .try_send(RoomCommand::Chat {
                player_id: insider,
                text: "大家好".into(),
            })
            .unwrap();
        let event = insider_rx.recv().await.unwrap();
        assert!(matches!(event, RoomEvent::Chat { text, .. } if text == "大家好"));

        // 非成员聊天与空白聊天被静默忽略。
        handle
            .try_send(RoomCommand::Chat {
                player_id: player(),
                text: "偷听".into(),
            })
            .unwrap();
        handle
            .try_send(RoomCommand::Chat {
                player_id: insider,
                text: "   ".into(),
            })
            .unwrap();
        let nothing =
            tokio::time::timeout(std::time::Duration::from_millis(20), insider_rx.recv()).await;
        assert!(nothing.is_err());
    }

    #[tokio::test]
    async fn leave_notifies_and_last_member_closes_room() {
        let handle = RoomActor::spawn(room(4), 16);
        let p1 = player();
        let p2 = player();
        let mut rx1 = join_actor(&handle, p1).await;
        let mut rx2 = join_actor(&handle, p2).await;
        // 消费加入回执:rx1 收到自己的加入 + p2 的加入广播;rx2 收到自己的加入。
        rx1.recv().await.unwrap();
        rx1.recv().await.unwrap();
        rx2.recv().await.unwrap();

        handle
            .send(RoomCommand::Leave {
                player_id: p1,
                reason: "主动退出".into(),
            })
            .await
            .unwrap();
        let event = rx2.recv().await.unwrap();
        assert!(matches!(event, RoomEvent::MemberLeft { player_id, .. } if player_id == p1));
        // p1 自己也收到回执,随后通道随成员记录移除而关闭。
        let own = rx1.recv().await.unwrap();
        assert!(matches!(own, RoomEvent::MemberLeft { player_id, .. } if player_id == p1));
        let closed = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv()).await;
        assert!(!matches!(closed, Ok(Some(_))), "通道应关闭或超时无事件");

        handle
            .send(RoomCommand::Leave {
                player_id: p2,
                reason: "主动退出".into(),
            })
            .await
            .unwrap();
        let ack = rx2.recv().await.unwrap();
        assert!(matches!(ack, RoomEvent::MemberLeft { player_id, .. } if player_id == p2));
        let event = rx2.recv().await.unwrap();
        assert!(matches!(event, RoomEvent::Closed { .. }));
        assert!(handle.is_closed());
    }

    #[tokio::test]
    async fn close_command_broadcasts_closed_event() {
        let mut handle = RoomActor::spawn(room(4), 16);
        let mut rx = join_actor(&handle, player()).await;
        rx.recv().await.unwrap();
        handle
            .send(RoomCommand::Close {
                reason: "服务器维护".into(),
            })
            .await
            .unwrap();
        let event = rx.recv().await.unwrap();
        assert!(matches!(event, RoomEvent::Closed { reason, .. } if reason == "服务器维护"));
        handle.wait_closed().await;
        assert!(handle.is_closed());
    }

    #[tokio::test]
    async fn full_room_rejects_join_with_conflict() {
        let handle = RoomActor::spawn(room(1), 16);
        join_actor(&handle, player()).await;
        let (tx, _rx) = mpsc::channel(8);
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(RoomCommand::Join {
                player_id: player(),
                nickname: "late".into(),
                sink: tx,
                reply: reply_tx,
            })
            .await
            .unwrap();
        let outcome = reply_rx.await.unwrap();
        assert_eq!(outcome.unwrap_err(), AppError::Conflict("房间已满".into()));
    }

    #[tokio::test]
    async fn repeat_join_is_idempotent_without_duplicate_event() {
        let handle = RoomActor::spawn(room(4), 16);
        let p = player();
        let mut rx = join_actor(&handle, p).await;
        rx.recv().await.unwrap(); // 首次加入事件

        let (tx, _rx2) = mpsc::channel(8);
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(RoomCommand::Join {
                player_id: p,
                nickname: "n".into(),
                sink: tx,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let nothing = tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv()).await;
        assert!(nothing.is_err(), "重复加入不应再次广播");
    }
}
