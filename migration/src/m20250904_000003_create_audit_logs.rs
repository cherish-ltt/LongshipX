//! audit_logs 表:操作审计(PRD 7.3 🔴 从第一天就有)。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AuditLogs::Table)
                    .col(
                        ColumnDef::new(AuditLogs::Id)
                            .uuid()
                            .not_null()
                            .default(Expr::cust("uuidv7()"))
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AuditLogs::PlayerId).uuid())
                    .col(ColumnDef::new(AuditLogs::Action).text().not_null())
                    .col(ColumnDef::new(AuditLogs::Detail).json_binary())
                    .col(
                        ColumnDef::new(AuditLogs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_audit_logs_player_id")
                            .from(AuditLogs::Table, AuditLogs::PlayerId)
                            .to(Players::Table, Players::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_audit_logs_player_id_created_at")
                    .table(AuditLogs::Table)
                    .col(AuditLogs::PlayerId)
                    .col(AuditLogs::CreatedAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_audit_logs_player_id_created_at")
                    .table(AuditLogs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(AuditLogs::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub enum AuditLogs {
    Table,
    Id,
    PlayerId,
    Action,
    Detail,
    CreatedAt,
}

#[derive(DeriveIden)]
pub enum Players {
    Table,
    Id,
}
