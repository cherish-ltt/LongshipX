//! accounts 表:登录凭证 + 账号状态(PRD 7.3)。
//! username 使用 CITEXT(大小写不敏感唯一),主键默认 uuidv7()(PG 18 内置)。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Accounts::Table)
                    .col(
                        ColumnDef::new(Accounts::Id)
                            .uuid()
                            .not_null()
                            .default(Expr::cust("uuidv7()"))
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Accounts::Username)
                            .custom(Accounts::Citext)
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Accounts::PasswordHash).text().not_null())
                    .col(
                        ColumnDef::new(Accounts::Status)
                            .small_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Accounts::BannedReason).text())
                    .col(ColumnDef::new(Accounts::BannedUntil).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(Accounts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Accounts::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub enum Accounts {
    Table,
    Id,
    Username,
    PasswordHash,
    Status,
    BannedReason,
    BannedUntil,
    CreatedAt,
    Citext,
}
