//! SeaORM 迁移(PRD 7.3/7.4):accounts / players / audit_logs。
//! 🔴 禁止在生产库手工执行未纳入本文件的 DDL。

pub use sea_orm_migration::MigratorTrait;

mod m20250904_000001_create_accounts;
mod m20250904_000002_create_players;
mod m20250904_000003_create_audit_logs;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![
            Box::new(m20250904_000001_create_accounts::Migration),
            Box::new(m20250904_000002_create_players::Migration),
            Box::new(m20250904_000003_create_audit_logs::Migration),
        ]
    }
}

/// 在指定数据库连接上执行全部迁移(服务启动时调用)。
pub async fn run_migrations(
    db: &sea_orm_migration::sea_orm::DatabaseConnection,
) -> Result<(), sea_orm_migration::sea_orm::DbErr> {
    Migrator::up(db, None).await
}
