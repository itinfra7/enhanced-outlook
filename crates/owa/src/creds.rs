use std::fmt;
use std::future::Future;
use std::pin::Pin;

use zeroize::Zeroizing;

/// 비밀번호. 실행 중 메모리에만 두고, 버려질 때 0으로 지운다. `Debug`로 값이 새지 않는다.
#[derive(Clone)]
pub struct Password(Zeroizing<String>);

impl Password {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(***)")
    }
}

#[derive(Clone, Debug)]
pub struct Credentials {
    pub username: String,
    pub password: Password,
}

pub type PromptFuture<'a> = Pin<Box<dyn Future<Output = Option<Credentials>> + Send + 'a>>;

/// 메모리의 비밀번호로 재로그인이 거부됐을 때(비밀번호 변경 등) 사용자에게 다시 묻는다.
/// 앱은 로그인 창, `owa-cli`는 터미널 숨김 입력으로 구현한다. `None`이면 포기.
pub trait CredentialPrompt: Send + Sync {
    fn credentials_rejected<'a>(&'a self, username: &'a str) -> PromptFuture<'a>;
}
