//! HTTP 层共享状态。

use ppt_tcp_application::ports::SessionTokenStore;
use ppt_tcp_application::{GetPlayerProfile, LoginUseCase, RegisterUseCase};
use std::sync::Arc;

#[derive(Clone)]
pub struct HttpState {
    pub register: Arc<RegisterUseCase>,
    pub login: Arc<LoginUseCase>,
    pub profile: Arc<GetPlayerProfile>,
    pub tokens: Arc<dyn SessionTokenStore>,
}
