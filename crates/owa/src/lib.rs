//! 웹메일(Outlook on the web, Exchange 2019) 내부 JSON API 클라이언트.
//!
//! 로그인(NTLM 또는 폼 기반, 서버를 보고 자동 판별), 캐너리, `POST /owa/service.svc?action=...`
//! 공통 호출, 오류 판정, 재로그인을 담당한다(계획서 5장). UI·저장소 크레이트에 의존하지 않는다.
//!
//! - [`requests`]: 요청 본문 생성(순수 함수, 계약 테스트 대상)
//! - [`responses`]: 응답 해석(순수 함수)
//! - [`Client`]: 세션·전송·재로그인과 위 둘을 묶은 형식화된 호출
//! - `transport`: HTTP/1.1 연결 풀(NTLM은 연결마다 인증), `ntlm`: Windows SSPI, `cbt`: 채널 바인딩
//! - [`domino`]: HCL Domino 웹메일(Domino) 클라이언트, [`notes`]: 그 응답 해석·요청 허용 목록

mod cbt;
mod client;
mod creds;
pub mod domino;
mod error;
pub mod notes;
mod ntlm;
pub mod requests;
pub mod responses;
mod transport;
mod types;

pub use client::{
    AuthScheme, Client, ClientConfig, LoginInfo, PostMode, SessionEvent, SessionEventKind,
};
pub use creds::{CredentialPrompt, Credentials, Password, PromptFuture};
pub use error::{Error, ItemError, Result};
pub use types::{
    Address, AttachmentId, ChangeKind, DeleteType, FileAttachment, FindItemPage, Flag, Folder,
    FolderChange, FolderId, FolderNode, FolderRef, HierarchySync, ItemChange, ItemId, ItemSummary,
    ItemSync, Mailbox, Recipient, RecipientEntry, ReplyKind, ServerAttachment, Signature,
    UserConfig, mail_folder_tree,
};

/// 사용자가 넣은 서버 주소 → URL. 앞뒤 공백을 빼고, `https://`가 없으면 붙이며, 경로·질의는 버린다.
/// http·https이고 호스트가 있어야 한다.
pub fn web_url(input: &str) -> Result<url::Url> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return Err(Error::BadAddress("비었습니다".into()));
    }
    let with_scheme = if s.contains("://") {
        s.to_owned()
    } else {
        format!("https://{s}")
    };
    let mut url =
        url::Url::parse(&with_scheme).map_err(|e| Error::BadAddress(format!("{s}: {e}")))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none_or(str::is_empty) {
        return Err(Error::BadAddress(format!(
            "{s}: http·https 주소가 아닙니다"
        )));
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// 자동 재로그인의 실패가 서버에 닿지 못한 연결 실패·서버 오류(5xx)인가. 이때만 아이디·비밀번호를 두고 나중에
/// 다시 시도한다. 나머지(비밀번호 거부, 예상 밖 로그인 응답 등)는 다시 시도하지 않는다.
pub fn transient_login_error(e: &Error) -> bool {
    match e {
        Error::Http(_) => true,
        Error::Status { status, .. } => *status >= 500,
        _ => false,
    }
}

/// 고정한 메일인가(1.1.0): 웹메일은 고정할 때 RenewTime을 4500-09-01로 둔다(풀면 없앤다). 연도만 본다.
pub fn is_pinned(renew_time: Option<&str>) -> bool {
    renew_time
        .and_then(|s| s.get(..4))
        .and_then(|y| y.parse::<u32>().ok())
        .is_some_and(|y| y >= 4000)
}

/// 모든 요청 머리에 넣는 서버 버전(계획서 5.1).
pub const REQUEST_SERVER_VERSION: &str = "Exchange2016";

/// 요청 머리의 시간대 ID(계획서 5.1).
pub const TIME_ZONE_ID: &str = "Korea Standard Time";

/// 기본 User-Agent: 최신 Chrome(계획서 5.1).
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36";

#[cfg(test)]
mod pin_tests {
    use super::*;

    #[test]
    fn pin_value_and_request() {
        assert!(is_pinned(Some("4500-09-01T09:00:00+09:00")));
        assert!(!is_pinned(Some("2026-11-10T09:43:06+09:00")));
        assert!(!is_pinned(None));
        let id = ItemId {
            id: "AAA".into(),
            change_key: Some("CK".into()),
        };
        let v = requests::update_pin(&[(id.clone(), true), (id, false)]);
        let c = &v["ItemChanges"];
        assert_eq!(c[0]["Updates"][0]["__type"], "SetItemField:#Exchange");
        assert_eq!(
            c[0]["Updates"][0]["Item"]["RenewTime"],
            requests::PIN_RENEW_TIME
        );
        assert_eq!(c[1]["Updates"][0]["__type"], "DeleteItemField:#Exchange");
        assert_eq!(c[1]["Updates"][0]["Path"]["FieldURI"], "RenewTime");
        assert!(
            c[0]["ItemId"].get("ChangeKey").is_none(),
            "AlwaysOverwrite라 Id만"
        );
        assert!(requests::LIST_PROPERTIES.contains(&"RenewTime"));
    }
}
