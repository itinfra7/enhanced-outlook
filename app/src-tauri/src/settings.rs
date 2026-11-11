//! 설정·화면 상태·비정상 종료 표시(계획서 10장 8단계). 모두 `EnhancedOutlook\` 안의 파일이다(레지스트리·`%APPDATA%` 쓰지 않음).
//!
//! - `settings.json`: 동기화 주기, 외부 이미지 정책. 설정 화면에서 바꾼다.
//! - `state.json`: 창 위치·크기, 마지막으로 본 폴더. 바뀔 때마다 몇 초 안에 저장되어 비정상 종료 뒤에도 그 자리에서 다시 시작한다.
//! - `running`: 실행 중 표시. 정상 종료 때 지운다. 시작할 때 남아 있으면 지난 실행이 비정상 종료된 것이다.
//!
//! 메일 데이터(DB는 WAL, 작업 대기열, 작성 중 메일 5초 자동 저장)는 원래 비정상 종료에도 남는다.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 받은 편지함 확인 주기로 고를 수 있는 값(초).
pub const INBOX_CHOICES: [u64; 4] = [10, 30, 60, 300];
/// 나머지 폴더·폴더 목록 확인 주기로 고를 수 있는 값(초).
pub const OTHERS_CHOICES: [u64; 3] = [60, 300, 900];
/// 글자 크기(화면 확대, %): 설정에서 직접 적거나 Ctrl+휠로(0.7.0 사용자 요청).
pub const ZOOM_MIN: u64 = 80;
pub const ZOOM_MAX: u64 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RemoteImages {
    /// 기본: 막고 "이미지 표시"·"이 보낸 사람은 늘 표시"로 연다(계획서 9.2).
    Ask,
    /// 늘 표시.
    Always,
}

/// 받은 편지함 자동 정리 기준(사용자 요청 2026-10-20). 서버에서만 지우고 로컬은 보관으로 남긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CleanupMode {
    #[default]
    Off,
    /// `days`일 지난 메일.
    Age,
    /// 메일함 사용량이 `percent`% 이상이면 오래된 메일부터, 그 아래가 될 만큼.
    Quota,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cleanup {
    pub mode: CleanupMode,
    pub days: u32,
    pub percent: u32,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self {
            mode: CleanupMode::Off,
            days: 30,
            percent: 90,
        }
    }
}

/// HCL Domino 계정 연동(사용자 요청 2026-10-21). 켜고 로그인하면 웹메일의 메일을 'HCL Domino 메일' 아래 로컬로 가져온다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DominoSettings {
    pub enabled: bool,
    /// 포털 주소(켤 때 사용자가 넣는다, 소스에 기본값 없음).
    pub portal: String,
    /// 받은 메일 자동 정리(서버에서만 삭제, Outlook 받은 편지함 정리와 따로).
    pub cleanup: Cleanup,
    /// 통합 편지함(사용자 요청 2026-10-21): 폴더 목록 맨 위에 Outlook 받은 편지함·HCL Domino 받은 메일·외부 메일을
    /// 함께 보여 주고, Outlook 폴더는 'Outlook 메일' 아래로 묶는다. 켜면 시작·로그인 때 통합 편지함을 연다.
    pub unified: bool,
    // 중복 전달 메일 자동 삭제(`dedupe_forwards`, 0.7.0~0.8.4)는 0.9.0에서 뺐다(사용자 요청). 예전 settings.json의
    // 값은 읽을 때 무시한다(이름을 다시 쓰지 않는다).
    /// Outlook에서 리다이렉트되어 온 HCL Domino 사본을 서버·로컬에서 지운다(1.1.0, `crate::redirects`).
    pub remove_redirects: bool,
    /// 알림 메일의 `[연결문서]`가 가리키는 업무 문서를 본문 아래에 붙인다(1.2.0, `crate::linked`).
    pub linked_docs: bool,
}

/// 새 메일 알림 내용(사용자 요청 2026-10-21). 읽지 않은 메일 수(받은 편지함, 통합 편지함을 켰으면 통합 편지함)는 늘 보인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum NotifyMode {
    Off,
    /// "새로운 메일 도착"(기본).
    #[default]
    Arrived,
    Sender,
    SenderSubject,
}

/// 스프레드시트 영역 붙여 넣기(0.7.1 사용자 요청): 표로 할지, 엑셀·한셀이 함께 주는 그림으로 할지.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SheetPaste {
    /// 매번 묻는다(기본).
    #[default]
    Ask,
    Table,
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Outlook 웹메일 주소(처음 실행할 때 사용자가 넣는다, 소스에 기본값 없음). 비면 로그인하지 않는다.
    pub outlook_url: String,
    pub inbox_every_secs: u64,
    pub others_every_secs: u64,
    pub remote_images: RemoteImages,
    pub cleanup: Cleanup,
    /// 글자 크기(%, [`ZOOM_MIN`]~[`ZOOM_MAX`]).
    pub zoom: u64,
    pub domino: DominoSettings,
    pub notify: NotifyMode,
    pub sheet_paste: SheetPaste,
    /// 받는 사람 자동 완성에서 숨길 도메인(하위 도메인 포함, 보내기 전용 주소 등, 사용자 요청).
    pub hidden_domains: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            outlook_url: String::new(),
            inbox_every_secs: 10,
            others_every_secs: 60,
            remote_images: RemoteImages::Ask,
            cleanup: Cleanup::default(),
            zoom: 100,
            domino: DominoSettings::default(),
            notify: NotifyMode::default(),
            sheet_paste: SheetPaste::default(),
            hidden_domains: Vec::new(),
        }
    }
}

/// 서버 주소를 `https://호스트`(포트가 있으면 함께) 꼴로. 쓸 수 없는 주소는 앞뒤 공백만 뺀다(저장할 때 거른다).
pub fn normalize_url(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    eo_owa::web_url(s).map_or_else(
        |_| s.to_owned(),
        |u| u.as_str().trim_end_matches('/').to_owned(),
    )
}

/// 숨길 도메인: 소문자, 앞의 `@`·`.`와 공백을 빼고, 빈 것·겹친 것은 뺀다.
pub fn normalize_domains(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in list {
        let d = d
            .trim()
            .trim_start_matches(['@', '.'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if !d.is_empty() && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

impl Settings {
    /// 고를 수 없는 값은 가장 가까운 값으로.
    pub fn normalized(mut self) -> Self {
        let near = |v: u64, choices: &[u64]| {
            *choices
                .iter()
                .min_by_key(|c| c.abs_diff(v))
                .unwrap_or(&choices[0])
        };
        self.inbox_every_secs = near(self.inbox_every_secs, &INBOX_CHOICES);
        self.others_every_secs = near(self.others_every_secs, &OTHERS_CHOICES);
        // 나머지 폴더를 받은 편지함보다 자주 볼 수는 없다.
        self.others_every_secs = self.others_every_secs.max(self.inbox_every_secs);
        for c in [&mut self.cleanup, &mut self.domino.cleanup] {
            c.days = c.days.clamp(1, 3650);
            c.percent = c.percent.clamp(1, 100);
        }
        self.zoom = self.zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        self.outlook_url = normalize_url(&self.outlook_url);
        self.domino.portal = normalize_url(&self.domino.portal);
        self.hidden_domains = normalize_domains(&self.hidden_domains);
        self
    }

    /// Outlook 웹메일 주소 형식만 본다(처음 실행 창: 앞 버전에서 HCL Domino를 켜 둔 채 포털 주소가 없어도
    /// Outlook 주소는 저장한다, 1.0.1 사용자 알림).
    pub fn check_outlook(&self) -> Result<(), String> {
        if !self.outlook_url.is_empty() {
            eo_owa::web_url(&self.outlook_url).map_err(|e| format!("Outlook 웹메일 주소: {e}"))?;
        }
        Ok(())
    }

    /// 저장할 수 있는가(서버 주소 형식, HCL Domino를 켰으면 포털 주소). 안 되면 사유.
    pub fn check(&self) -> Result<(), String> {
        self.check_outlook()?;
        if self.domino.enabled && self.domino.portal.is_empty() {
            return Err("HCL Domino 포털 주소를 넣어 주세요.".into());
        }
        if !self.domino.portal.is_empty() {
            eo_owa::web_url(&self.domino.portal)
                .map_err(|e| format!("HCL Domino 포털 주소: {e}"))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowState {
    /// 논리 좌표(화면 배율을 뺀 값).
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UiState {
    pub window: Option<WindowState>,
    pub last_folder: Option<i64>,
    /// 폴더 목록·메일 목록 폭(px, 경계를 끌어 바꾼 것).
    pub folders_width: Option<f64>,
    pub list_width: Option<f64>,
    /// 메일 목록 정렬.
    pub list_sort: Option<SavedSort>,
    /// 즐겨찾기 폴더(폴더 오른쪽 클릭 "즐겨찾기에 추가", 0.7.0).
    pub favorites: Vec<i64>,
    /// 마지막으로 로그인한 아이디(1.1.0 사용자 요청). 로그인 창 아이디 칸의 ✕로 지우면 다시 로그인할 때까지 비운다.
    /// 비밀번호는 저장하지 않는다.
    pub remembered_user: Option<String>,
    pub remembered_domino_user: Option<String>,
}

/// 저장한 목록 정렬(기준, 기본 방향을 뒤집을지).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSort {
    pub key: eo_store::SortKey,
    pub reverse: bool,
}

use crate::compat::read_json;

/// 임시 파일에 쓴 뒤 바꿔 넣는다(쓰다 끊겨도 앞의 파일이 남는다).
fn write_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub fn settings_path(root: &Path) -> PathBuf {
    root.join("settings.json")
}

/// (설정, 경고). 파일이 없으면 기본값. HCL Domino는 사용자가 설정에서 포털 주소를 넣고 켜는 선택 기능이라, 포털
/// 주소가 없으면(앞 버전 설정) 꺼진 것으로 읽는다(1.0.1 사용자 지적).
pub fn load_settings(root: &Path) -> (Settings, Option<String>) {
    let (s, warn): (Settings, _) = read_json(&settings_path(root));
    let mut s = s.normalized();
    if s.domino.portal.is_empty() {
        s.domino.enabled = false;
    }
    (s, warn)
}

pub fn save_settings(root: &Path, s: &Settings) -> std::io::Result<()> {
    write_json(&settings_path(root), s)
}

pub fn load_ui_state(root: &Path) -> UiState {
    let (s, warn) = read_json(&root.join("state.json"));
    if let Some(w) = warn {
        crate::applog::warn(w);
    }
    s
}

pub fn save_ui_state(root: &Path, s: &UiState) -> std::io::Result<()> {
    write_json(&root.join("state.json"), s)
}

/// 실행 중 표시를 남긴다. 이미 있었으면(지난 실행이 비정상 종료) true.
pub fn mark_running(root: &Path) -> bool {
    let path = root.join("running");
    let crashed = path.exists();
    let _ = std::fs::write(&path, std::process::id().to_string());
    crashed
}

pub fn clear_running(root: &Path) {
    let _ = std::fs::remove_file(root.join("running"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_normalize() {
        let dir = tempfile::tempdir().unwrap();
        let (s, warn) = load_settings(dir.path());
        assert_eq!(
            (s.clone(), warn),
            (Settings::default(), None),
            "파일이 없으면 기본값"
        );
        let s = Settings {
            outlook_url: " mail.example.com/owa/ ".into(),
            inbox_every_secs: 300,
            others_every_secs: 60,
            remote_images: RemoteImages::Always,
            cleanup: Cleanup {
                mode: CleanupMode::Quota,
                days: 0,
                percent: 150,
            },
            zoom: 130,
            domino: DominoSettings {
                enabled: true,
                portal: "http://portal.example.com:8080/x".into(),
                cleanup: Cleanup {
                    mode: CleanupMode::Age,
                    days: 99_999,
                    percent: 0,
                },
                unified: true,
                remove_redirects: true,
                linked_docs: true,
            },
            notify: NotifyMode::SenderSubject,
            sheet_paste: SheetPaste::Image,
            hidden_domains: vec![
                " @P.Example.com ".into(),
                "p.example.com".into(),
                "".into(),
                ".send.example.org".into(),
            ],
        }
        .normalized();
        assert_eq!(s.zoom, 130, "글자 크기는 80~150 안의 아무 값");
        assert_eq!(
            s.outlook_url, "https://mail.example.com",
            "https를 붙이고 경로는 버린다"
        );
        assert_eq!(s.domino.portal, "http://portal.example.com:8080");
        assert_eq!(s.hidden_domains, ["p.example.com", "send.example.org"]);
        assert!(s.check().is_ok());
        let no_portal = Settings {
            domino: DominoSettings {
                enabled: true,
                ..DominoSettings::default()
            },
            ..Settings::default()
        };
        assert!(
            no_portal.check().unwrap_err().contains("포털 주소"),
            "켜면 포털 주소가 있어야 한다"
        );
        let first_run = Settings {
            outlook_url: "mail.example.com".into(),
            ..no_portal.clone()
        }
        .normalized();
        assert!(
            first_run.check_outlook().is_ok() && first_run.check().is_err(),
            "처음 실행 창은 포털 주소가 없어도 Outlook 주소를 저장한다"
        );
        let bad = Settings {
            outlook_url: "ftp://x".into(),
            ..Settings::default()
        }
        .normalized();
        assert!(bad.check().is_err());
        assert_eq!(
            (
                Settings {
                    zoom: 30,
                    ..Settings::default()
                }
                .normalized()
                .zoom,
                Settings {
                    zoom: 400,
                    ..Settings::default()
                }
                .normalized()
                .zoom
            ),
            (80, 150),
            "범위 밖은 끝값으로"
        );
        assert_eq!(
            (s.cleanup.days, s.cleanup.percent),
            (1, 100),
            "정리 기준은 1일 이상, 1~100%"
        );
        assert_eq!(
            (s.domino.cleanup.days, s.domino.cleanup.percent),
            (3650, 1),
            "HCL Domino 정리 기준도"
        );
        assert_eq!(
            s.others_every_secs, 300,
            "나머지 폴더는 받은 편지함 주기 이상"
        );
        save_settings(dir.path(), &s).unwrap();
        assert_eq!(load_settings(dir.path()).0, s);
        assert_eq!(
            Settings {
                inbox_every_secs: 7,
                others_every_secs: 100_000,
                ..Settings::default()
            }
            .normalized(),
            Settings {
                inbox_every_secs: 10,
                others_every_secs: 900,
                ..Settings::default()
            }
        );
        std::fs::write(settings_path(dir.path()), b"{oops").unwrap();
        let (s, warn) = load_settings(dir.path());
        assert!(
            s == Settings::default() && warn.is_some(),
            "깨진 파일은 기본값 + 경고"
        );
        // 일부 값만 있어도 나머지는 기본값(0.2.0까지의 파일에는 정리 설정이 없다 → 끔, 30일, 90%).
        std::fs::write(settings_path(dir.path()), br#"{"remoteImages":"always"}"#).unwrap();
        let s = load_settings(dir.path()).0;
        assert_eq!(s.inbox_every_secs, 10);
        assert_eq!(s.cleanup, Cleanup::default());
        assert_eq!(s.zoom, 100);
        assert_eq!(
            s.domino,
            DominoSettings::default(),
            "0.4.0까지의 파일: 연동 끔"
        );
        assert_eq!(s.notify, NotifyMode::Arrived, "알림 기본: 새로운 메일 도착");
        // 0.8.4까지의 파일에 남은 중복 전달 메일 자동 삭제 값은 무시한다.
        // HCL Domino는 포털 주소가 있어야 켜진 것으로 읽는다(앞 버전 설정은 꺼진 채로).
        std::fs::write(
            settings_path(dir.path()),
            br#"{"domino":{"enabled":true,"dedupeForwards":true}}"#,
        )
        .unwrap();
        assert!(!load_settings(dir.path()).0.domino.enabled);
        std::fs::write(
            settings_path(dir.path()),
            br#"{"domino":{"enabled":true,"portal":"portal.example.com"}}"#,
        )
        .unwrap();
        assert!(load_settings(dir.path()).0.domino.enabled);
        std::fs::write(
            settings_path(dir.path()),
            br#"{"cleanup":{"mode":"age","days":45}}"#,
        )
        .unwrap();
        assert_eq!(
            load_settings(dir.path()).0.cleanup,
            Cleanup {
                mode: CleanupMode::Age,
                days: 45,
                percent: 90
            }
        );
        // 모르는 값(다른 판이 쓴 알림 방식)이 있어도 주소·HCL Domino 설정은 잃지 않는다.
        std::fs::write(
            settings_path(dir.path()),
            br#"{"outlookUrl":"https://mail.example.com","notify":"popup",
                "domino":{"enabled":true,"portal":"https://portal.example.com","unified":true}}"#,
        )
        .unwrap();
        let (s, warn) = load_settings(dir.path());
        assert_eq!(s.outlook_url, "https://mail.example.com");
        assert!(s.domino.enabled && s.domino.unified);
        assert_eq!(s.notify, NotifyMode::Arrived);
        assert!(warn.unwrap().contains("notify"));
        assert!(dir.path().join("settings.json.bak").exists());
    }

    #[test]
    fn running_marker_detects_unclean_exit() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!mark_running(dir.path()));
        assert!(
            mark_running(dir.path()),
            "지우지 않고 다시 시작 = 비정상 종료"
        );
        clear_running(dir.path());
        assert!(!mark_running(dir.path()));
        let ui = UiState {
            window: Some(WindowState {
                x: 10.0,
                y: 20.0,
                width: 1200.0,
                height: 700.0,
                maximized: false,
            }),
            last_folder: Some(3),
            folders_width: Some(240.0),
            list_width: Some(420.0),
            list_sort: Some(SavedSort {
                key: eo_store::SortKey::Size,
                reverse: true,
            }),
            favorites: vec![7, 3],
            remembered_user: Some("user01".into()),
            remembered_domino_user: None,
        };
        save_ui_state(dir.path(), &ui).unwrap();
        assert_eq!(load_ui_state(dir.path()), ui);
    }
}
