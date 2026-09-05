//! 房间聚合:一局对局/一个共享上下文(PRD 第 2/9 章)。

mod aggregate;
mod repository;

pub use aggregate::{MAX_ROOM_PLAYERS, Room, RoomId, RoomState};
pub use repository::RoomRepository;
