//! 操作审计(PRD 7.3 audit_logs):application 的 AuditLogger 端口实现。

use crate::persistence::entities::audit_log::ActiveModel;
use async_trait::async_trait;
use chrono::Utc;
use ppt_tcp_application::error::AppError;
use ppt_tcp_application::ports::AuditLogger;
use ppt_tcp_domain::PlayerId;
use sea_orm::{ActiveModelTrait, DatabaseConnection};
use std::sync::Arc;

pub struct SeaAuditLogger {
    db: Arc<DatabaseConnection>,
}

impl SeaAuditLogger {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AuditLogger for SeaAuditLogger {
    async fn record(
        &self,
        player_id: Option<PlayerId>,
        action: &str,
        detail: String,
    ) -> Result<(), AppError> {
        // detail 约定为 JSON 文本;解析失败按字符串存,不影响审计主流程。
        let detail_json = serde_json::from_str::<serde_json::Value>(&detail)
            .unwrap_or(serde_json::Value::String(detail));
        let active = ActiveModel {
            id: sea_orm::Set(uuid::Uuid::now_v7()),
            player_id: sea_orm::Set(player_id.map(|id| id.0)),
            action: sea_orm::Set(action.to_string()),
            detail: sea_orm::Set(Some(detail_json)),
            created_at: sea_orm::Set(Utc::now().into()),
        };
        active
            .insert(self.db.as_ref())
            .await
            .map(|_| ())
            .map_err(|err| AppError::Storage(format!("审计写入失败: {err}")))
    }
}
