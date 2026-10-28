//! 터미널 입력. 비밀번호는 숨김 입력으로 받아 메모리에만 둔다(계획서 5.2).

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use eo_owa::{
    Client, ClientConfig, CredentialPrompt, Credentials, Error, LoginInfo, Password, PromptFuture,
};

/// 계정 잠금을 피하려고 한 번에 묻는 최대 횟수.
const MAX_ATTEMPTS: u32 = 3;

/// 아이디도 저장하지 않고 실행할 때마다 입력받는다(명령줄 인자는 셸 기록 파일에 남으므로 받지 않는다).
fn read_username() -> Result<String> {
    read_username_as("아이디")
}

/// 비밀번호 입력 중 Ctrl+C로 끊으면 콘솔이 "입력이 안 보이는" 모드로 남아, 같은 창에서 다시 실행하면 아이디
/// 입력이 바로 빈 값으로 끝났다(사용자 지적 2026-10-23). 입력을 받기 전에 줄 단위·보이기·Ctrl+C 처리를 되돌린다.
#[allow(unsafe_code)]
fn repair_console() {
    use windows_sys::Win32::System::Console::{
        ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode, GetStdHandle,
        STD_INPUT_HANDLE, SetConsoleMode,
    };
    // SAFETY: 표준 입력 핸들의 콘솔 모드를 읽고 쓰기만 한다(콘솔이 아니면 GetConsoleMode가 실패해 그대로 둔다).
    unsafe {
        let h = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(h, &mut mode) != 0 {
            let want = mode | ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT;
            if want != mode {
                SetConsoleMode(h, want);
            }
        }
    }
}

pub fn read_username_as(label: &str) -> Result<String> {
    repair_console();
    eprint!("{label}: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let user = line.trim().to_owned();
    if user.is_empty() {
        bail!("아이디가 비었습니다");
    }
    Ok(user)
}

pub async fn read_password() -> Result<Password> {
    // rpassword의 안내 문구 출력은 콘솔 코드 페이지(CP949)에서 UTF-8이 깨지므로 안내는 표준 출력으로 찍는다.
    eprint!("비밀번호: ");
    std::io::stderr().flush()?;
    let password = tokio::task::spawn_blocking(rpassword::read_password)
        .await?
        .context("비밀번호 입력 실패")?;
    Ok(Password::new(password))
}

/// 세션 중 비밀번호가 거부되면(비밀번호 변경 등) 터미널에서 다시 묻는다.
#[derive(Default)]
struct TerminalPrompt {
    asked: AtomicU32,
}

impl CredentialPrompt for TerminalPrompt {
    fn credentials_rejected<'a>(&'a self, username: &'a str) -> PromptFuture<'a> {
        Box::pin(async move {
            if self.asked.fetch_add(1, Ordering::SeqCst) >= MAX_ATTEMPTS {
                eprintln!("비밀번호 재입력 횟수를 넘었습니다.");
                return None;
            }
            eprintln!("\n서버가 비밀번호를 거부했습니다(비밀번호가 바뀌었나요?). 다시 입력하세요.");
            let password = read_password().await.ok()?;
            Some(Credentials {
                username: username.to_owned(),
                password,
            })
        })
    }
}

/// 아이디·비밀번호를 입력받아 로그인한다. 비밀번호가 틀리면 최대 3번까지 다시 묻는다.
pub async fn login(cfg: ClientConfig) -> Result<(Client, LoginInfo)> {
    let username = read_username()?;
    let client = Client::new(cfg, Some(Arc::new(TerminalPrompt::default())));
    for attempt in 1..=MAX_ATTEMPTS {
        let password = read_password().await?;
        if password.is_empty() {
            bail!("비밀번호가 비었습니다");
        }
        let creds = Credentials {
            username: username.clone(),
            password,
        };
        match client.login(creds).await {
            Ok(info) => return Ok((client, info)),
            Err(Error::BadCredentials { .. }) if attempt < MAX_ATTEMPTS => {
                eprintln!("아이디 또는 비밀번호가 맞지 않습니다. 다시 입력하세요.");
            }
            Err(e) => return Err(e).context("로그인 실패"),
        }
    }
    bail!("로그인 실패")
}
