//! 房间仓储:当前阶段房间由进程内 Actor 持有(PRD 7.5),
//! 这里提供内存快照实现,预留未来跨进程/持久化的替换点。

use async_trait::async_trait;
use longshipx_domain::{RepoError, Room, RoomId, RoomRepository};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct InMemoryRoomRepository {
    rooms: Mutex<HashMap<RoomId, Room>>,
}

impl InMemoryRoomRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.rooms.lock().expect("room repo 锁中毒").len()
    }

    pub fn is_empty(&self) -> bool {
        self.rooms.lock().expect("room repo 锁中毒").is_empty()
    }
}

#[async_trait]
impl RoomRepository for InMemoryRoomRepository {
    async fn find_by_id(&self, id: RoomId) -> Result<Option<Room>, RepoError> {
        Ok(self
            .rooms
            .lock()
            .expect("room repo 锁中毒")
            .get(&id)
            .cloned())
    }

    async fn save(&self, room: &Room) -> Result<(), RepoError> {
        self.rooms
            .lock()
            .expect("room repo 锁中毒")
            .insert(room.id(), room.clone());
        Ok(())
    }

    async fn delete(&self, id: RoomId) -> Result<(), RepoError> {
        self.rooms.lock().expect("room repo 锁中毒").remove(&id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn save_find_delete_roundtrip() {
        let repo = InMemoryRoomRepository::new();
        let room = Room::open(4, Utc::now());
        let id = room.id();
        repo.save(&room).await.unwrap();
        assert_eq!(repo.len(), 1);
        let found = repo.find_by_id(id).await.unwrap().unwrap();
        assert_eq!(found.id(), id);
        repo.delete(id).await.unwrap();
        assert!(repo.find_by_id(id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_room_is_none() {
        let repo = InMemoryRoomRepository::new();
        assert!(
            repo.find_by_id(RoomId(uuid::Uuid::now_v7()))
                .await
                .unwrap()
                .is_none()
        );
    }
}
