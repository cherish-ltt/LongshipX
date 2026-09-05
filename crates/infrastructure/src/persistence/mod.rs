//! 持久化层:SeaORM 实体、Model ⇄ 领域聚合的显式转换与仓储实现。
//! ⚠️ SeaORM 类型绝不允许泄漏到 application/domain(PRD 7.4)。

pub mod converters;
pub mod entities;
pub mod repositories;
