//! 鉴权用例:注册与登录(PRD 6.1)。

mod login;
mod register;

pub use login::{LoginDependencies, LoginUseCase};
pub use register::RegisterUseCase;
