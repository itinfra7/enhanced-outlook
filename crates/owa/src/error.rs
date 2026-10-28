pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP 통신 오류: {0}")]
    Http(String),

    #[error("HTTP {status} ({context})")]
    Status { status: u16, context: String },

    /// 아이디·비밀번호 거부. 폼 로그인은 로그인 화면 `reason=2`, NTLM은 AUTHENTICATE 뒤의 401.
    #[error("아이디 또는 비밀번호가 맞지 않습니다 ({reason})")]
    BadCredentials { reason: String },

    /// SSPI(NTLM) 호출 실패 등 인증 절차 자체의 오류.
    #[error("인증 처리 오류: {0}")]
    Auth(String),

    #[error("로그인 응답을 해석할 수 없습니다: {0}")]
    LoginUnexpected(String),

    /// 로그인한 적이 없거나, 비밀번호 재입력을 사용자가 포기했다.
    #[error("로그인이 필요합니다")]
    LoginRequired,

    /// 재로그인 직후에도 세션 만료 응답이 왔다.
    #[error("세션이 만료됐고 재로그인 후에도 복구되지 않았습니다")]
    SessionExpired,

    /// HTTP 500 + `Body.ErrorCode/ExceptionName/FaultMessage`: 요청 형식 오류. 재시도하지 않는다.
    #[error("{action} 요청 형식 오류: {code} {message}")]
    RequestFormat {
        action: String,
        code: String,
        message: String,
    },

    /// `ResponseClass != "Success"`인 응답(단일 항목 호출).
    #[error("{action} 실패: {code} {message}")]
    Response {
        action: String,
        code: String,
        message: String,
    },

    #[error("{action} 응답 해석 실패: {detail}")]
    Decode { action: String, detail: String },

    #[error("잘못된 주소: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// 사용자가 넣은 서버 주소를 쓸 수 없다.
    #[error("서버 주소가 올바르지 않습니다: {0}")]
    BadAddress(String),

    /// HCL Domino: 허용 목록 밖의 요청(웹메일의 삭제·이동 등은 모두 GET 주소라 막는다).
    #[error("허용하지 않은 HCL Domino 요청: {0}")]
    NotAllowed(String),
}

/// 여러 항목을 한 번에 다루는 호출에서 항목 하나의 실패(`ResponseClass != "Success"`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ItemError {
    pub code: String,
    pub message: String,
}
