//! 房间仓储接口:当前阶段房间为进程内 Actor 持有(PRD 7.5),
//! 仓储预留快照持久化扩展点,实现在 infrastructure。

use crate::room::{Room, RoomId};
use crate::shared::error::RepoError;

#[async_trait::async_trait]
pub trait RoomRepository: Send + Sync {
    async fn find_by_id(&self, id: RoomId) -> Result<Option<Room>, RepoError>;
    async fn save(&self, room: &Room) -> Result<(), RepoError>;
    async fn delete(&self, id: RoomId) -> Result<(), RepoError>;
}
