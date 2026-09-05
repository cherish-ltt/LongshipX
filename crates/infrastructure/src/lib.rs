//! 基础设施层:实现 domain/application 定义的端口(PRD 4.1/7)。
//!
//! * `config`:全部可调参数经环境变量注入(PRD 18);
//! * `persistence`:SeaORM 实体与仓储实现(实体 ≠ 领域实体,显式转换);
//! * `cache`:Redis token 存储与内存实现;
//! * `password`:argon2id;
//! * `events`:进程内领域事件分发。

pub mod cache;
pub mod config;
pub mod events;
pub mod password;
pub mod persistence;
