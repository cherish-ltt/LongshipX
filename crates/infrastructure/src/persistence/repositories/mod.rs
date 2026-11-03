//! domain/application 仓储 trait 的实现:SeaORM(生产)与内存版(测试/开发)。

pub mod account_repo;
pub mod audit_repo;
pub mod memory_repos;
pub mod player_repo;
pub mod registration_store;
pub mod room_repo;

pub use account_repo::SeaAccountRepository;
pub use audit_repo::SeaAuditLogger;
pub use memory_repos::{
    InMemoryAccountRepository, InMemoryPlayerRepository, InMemoryRegistrationStore,
};
pub use player_repo::SeaPlayerRepository;
pub use registration_store::SeaRegistrationStore;
pub use room_repo::InMemoryRoomRepository;
