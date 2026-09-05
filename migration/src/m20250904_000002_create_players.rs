//! players 表:玩家角色档案(PRD 7.3)。

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Players::Table)
                    .col(
                        ColumnDef::new(Players::Id)
                            .uuid()
                            .not_null()
                            .default(Expr::cust("uuidv7()"))
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Players::AccountId).uuid().not_null())
                    .col(ColumnDef::new(Players::Nickname).text().not_null())
                    .col(
                        ColumnDef::new(Players::Level)
                            .integer()
                            .not_null()
                            .default(1),
                    )
                    .col(
                        ColumnDef::new(Players::Exp)
                            .big_integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(Players::LastLoginAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(Players::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_players_account_id")
                            .from(Players::Table, Players::AccountId)
                            .to(Accounts::Table, Accounts::Id)
                            .on_delete(ForeignKeyAction::Restrict),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_players_account_id")
                    .table(Players::Table)
                    .col(Players::AccountId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name("idx_players_account_id")
                    .table(Players::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(Players::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
pub enum Players {
    Table,
    Id,
    AccountId,
    Nickname,
    Level,
    Exp,
    LastLoginAt,
    CreatedAt,
}

#[derive(DeriveIden)]
pub enum Accounts {
    Table,
    Id,
}
