//! 房间运行时:Actor 模式(PRD 9.1)——每个房间一个 task 串行处理命令,无锁;
//! 以及跨房间的 RoomService 门面。

pub mod actor;
pub mod service;

pub use actor::{ROOM_EVENT_CAPACITY, RoomCommand, RoomEvent, RoomHandle};
pub use service::{RoomService, RoomServiceConfig};
