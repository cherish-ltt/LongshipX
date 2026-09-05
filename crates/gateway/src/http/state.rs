//! HTTP 层共享状态。

use longshipx_application::ports::SessionTokenStore;
use longshipx_application::{GetPlayerProfile, LoginUseCase, RegisterUseCase};
use std::sync::Arc;

#[derive(Clone)]
pub struct HttpState {
    pub register: Arc<RegisterUseCase>,
    pub login: Arc<LoginUseCase>,
    pub profile: Arc<GetPlayerProfile>,
    pub tokens: Arc<dyn SessionTokenStore>,
}
