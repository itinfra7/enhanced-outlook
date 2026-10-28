//! 가져오기(계획서 10장 7단계): EML 폴더·ZIP과 유저스크립트 HTML 백업(폴더·ZIP)을 로컬 전용 메일로 넣는다.
//!
//! - 폴더 구조: 고른 폴더(또는 ZIP 안) 기준 상대 경로의 폴더들이 메일 폴더가 된다. 끝의 `yyyy\mm`
//!   (이 앱의 내보내기와 유저스크립트 백업이 쓰는 날짜 폴더)는 뺀다. 남는 것이 없으면 고른 폴더 이름.
//!   같은 이름의 기존 폴더에 합친다(`Store::find_or_create_folder_path`).
//! - `{메일}_files\` 안은 HTML 백업의 첨부·이미지라 메일로 가져오지 않는다.
//! - 유저스크립트 ZIP은 약 300MB씩 나뉘어 있어, 한 번에 고른 모든 원본의 파일을 한 목록으로 묶어 찾는다.
//! - 서버에는 올리지 않는다. 가져온 메일은 읽음으로 넣고, 색인은 호출한 쪽이 `index_pending`으로 한다.
//! - **레거시 HCL Domino 메일 백업**(사용자 요청 2026-10-16, `eo_mime::domino`): HTML을 보고 알아본다.
//!   `HCL Domino 메일\{메일함}`(받은 메일·외부 메일·보낸 메일 등)에 넣고, 기간 폴더(`2021년 02월`, `2018년`)는 뺀다.
//!   `$$_Attach_$$`·`$$_Image_$$` 안은 첨부·그림이라 메일로 가져오지 않는다.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use eo_mime::{backup, domino};
use eo_store::{ImportOutcome, MessageProps, Store};

use crate::{Error, Result};

/// 파일 하나의 최대 크기(이보다 크면 건너뜀). 잘못된 ZIP 머리글로 메모리를 다 쓰지 않게.
const MAX_FILE: u64 = 512 * 1024 * 1024;
/// 보고서에 남길 오류 수.
const MAX_ERRORS: usize = 20;
/// 원본 폴더 이름이 없을 때.
const FALLBACK_FOLDER: &str = "가져온 메일";
/// HCL Domino 백업을 넣는 맨 위 폴더(사용자 요청).
pub const DOMINO_FOLDER: &str = "HCL Domino 메일";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// 저장하지 않고 해석·복원만 해 본다(통계 확인용).
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportProgress {
    pub done: usize,
    pub total: usize,
    pub imported: usize,
    pub duplicates: usize,
    pub skipped: usize,
    /// 지금 처리 중인 파일(상대 경로).
    pub current: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// 가져올 후보(EML·HTML 파일) 수.
    pub total: usize,
    pub imported: usize,
    /// 그중 HTML 백업에서 복원한 수.
    pub restored_html: usize,
    /// 그중 HCL Domino 백업에서 가져온 수(HTML에서 복원 + EML).
    pub restored_domino: usize,
    /// 이미 있어서 건너뛴 수.
    pub duplicates: usize,
    /// 해석하지 못했거나(백업 형식이 아닌 HTML 등) 읽지 못한 수.
    pub skipped: usize,
    /// 새로 만든 폴더 경로("상위/하위").
    pub created_folders: Vec<String>,
    /// 메일이 들어간 폴더 번호들(화면 갱신용).
    pub folders: Vec<i64>,
    /// 폴더 경로("상위/하위")별 가져온(검사만 하면 가져올) 통수, 경로순.
    pub per_folder: Vec<(String, usize)>,
    /// 가져온 EML 크기 합계(압축 전).
    pub eml_bytes: u64,
    /// 복원한 메일이 가리키는데 백업에 없던 파일 수.
    pub missing_files: usize,
    /// 아무도 가리키지 않아 첨부로 붙인 백업 파일 수(HCL Domino).
    pub extra_files: usize,
    /// 첨부 칸에 이름만 있고 파일은 없는 첨부 수(첨부를 내려받지 않고 저장한 HCL Domino 백업). 이름은 본문 끝에 남긴다.
    pub not_downloaded: usize,
    /// 서버 폴더 목록을 아직 받지 않아(한 번도 로그인하지 않음) 넣지 않은 메일 수. HCL Domino 메일은 "HCL Domino 메일"
    /// 아래에만 들어가서 바로 넣지만, 다른 백업은 같은 이름의 서버 폴더(받은 편지함 등)에 합쳐야 해서 기다린다.
    pub need_login: usize,
    /// 앞쪽 오류 몇 개("파일: 이유").
    pub errors: Vec<String>,
    pub cancelled: bool,
    /// 검사만 했다(저장하지 않음).
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
enum Loc {
    Fs(PathBuf),
    Zip {
        archive: usize,
        index: usize,
    },
    /// PST·OST 안의 메일(열어 둔 파일 번호, 메일 노드).
    Pst {
        file: usize,
        node: u32,
    },
}

/// Outlook 데이터 파일: PST(512바이트 페이지, `outlook-pst`) 또는 OST 4K(HCL Traveler `.ims`, `ost`).
enum MailFile {
    Pst(crate::pst::PstFile),
    Ost(crate::ost::OstFile),
}

impl MailFile {
    /// 머리의 형식 번호로 고른다(확장자와 상관없이: `.ims`는 OST 4K다).
    fn open(path: &Path) -> std::result::Result<Self, String> {
        let mut head = [0u8; 12];
        File::open(path)
            .and_then(|mut f| f.read_exact(&mut head))
            .map_err(|e| e.to_string())?;
        if &head[0..4] == b"!BDN" && u16::from_le_bytes([head[10], head[11]]) == 36 {
            return crate::ost::OstFile::open(path).map(Self::Ost);
        }
        crate::pst::PstFile::open(path).map(Self::Pst)
    }

    fn items(&self) -> std::result::Result<Vec<crate::pst::PstItem>, String> {
        match self {
            Self::Pst(p) => p.items(),
            Self::Ost(o) => o.items(),
        }
    }

    fn eml(&self, node: u32) -> std::result::Result<Option<Vec<u8>>, String> {
        match self {
            Self::Pst(p) => p.eml(node),
            Self::Ost(o) => o.eml(node),
        }
    }
}

/// Outlook 데이터 파일 확장자(PST, OST, HCL Traveler 백업 `.ims`).
fn is_mail_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".pst", ".ost", ".ims"].iter().any(|e| lower.ends_with(e))
}

/// HCL Traveler(.ims)의 Outlook 기본 폴더 이름 → HCL Domino 웹메일 메일함 이름(실시간 연동 폴더에 합친다).
fn domino_box_name(name: &str) -> Option<&'static str> {
    match name.trim().to_lowercase().as_str() {
        "받은 편지함" | "inbox" => Some("받은 메일"),
        "보낸 편지함" | "sent items" => Some("보낸 메일"),
        "지운 편지함" | "deleted items" => Some("지운 메일"),
        "임시 보관함" | "drafts" => Some("미발송 메일"),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Eml,
    Html,
    Pst,
    /// OST 4K(HCL Traveler `.ims`): PST와 같지만 출처 표시가 다르다.
    Ims,
}

#[derive(Debug, Clone)]
struct Candidate {
    /// 원본 기준 상대 경로 조각(마지막이 파일 이름).
    rel: Vec<String>,
    loc: Loc,
    kind: Kind,
    /// 상대 경로에 폴더가 없을 때 쓸 폴더 이름.
    fallback: String,
}

/// 고른 원본들의 파일 목록.
#[derive(Default)]
struct Sources {
    zips: Vec<zip::ZipArchive<BufReader<File>>>,
    candidates: Vec<Candidate>,
    /// "상대/경로/파일" → 위치(HTML 백업의 `_files` 찾기용).
    files: HashMap<String, Loc>,
    /// "상대/경로/{메일}_files" → 그 안 파일 이름들.
    dirs: HashMap<String, Vec<String>>,
    /// HCL Domino `.idf` 파일이 있는 폴더들(상대 경로). HCL Domino 백업의 맨 위 폴더다.
    idf_roots: Vec<Vec<String>>,
    /// 열어 둔 PST·OST들.
    psts: Vec<MailFile>,
}

fn io_err(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Task(format!("{}: {e}", path.display()))
}

fn ext_kind(name: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    if lower.ends_with(".eml") {
        Some(Kind::Eml)
    } else if lower.ends_with(".html") || lower.ends_with(".htm") {
        Some(Kind::Html)
    } else {
        None
    }
}

/// 메일이 아니라 첨부·그림이 든 폴더: 유저스크립트 `{메일}_files`, HCL Domino `$$_Attach_$$`·`$$_Image_$$`.
fn is_files_dir(seg: &str) -> bool {
    let lower = seg.to_lowercase();
    lower.ends_with("_files")
        || lower == domino::ATTACH_DIR.to_lowercase()
        || lower == domino::IMAGE_DIR.to_lowercase()
}

impl Sources {
    fn add_file(&mut self, rel: Vec<String>, loc: Loc, fallback: &str) {
        let Some(name) = rel.last() else { return };
        let dirs = &rel[..rel.len() - 1];
        if dirs.iter().any(|d| d == "__MACOSX") {
            return;
        }
        // HCL Domino 백업의 맨 위 폴더 표시(내용은 읽지 않는다).
        if name.to_lowercase().ends_with(".idf") && !self.idf_roots.iter().any(|r| r == dirs) {
            self.idf_roots.push(dirs.to_vec());
        }
        self.files.insert(rel.join("/"), loc.clone());
        if dirs.last().is_some_and(|d| is_files_dir(d)) {
            self.dirs
                .entry(dirs.join("/"))
                .or_default()
                .push(name.clone());
        }
        // 첨부 폴더 안(첨부된 EML 포함)은 메일이 아니다.
        if dirs.iter().any(|d| is_files_dir(d)) {
            return;
        }
        if let Some(kind) = ext_kind(name) {
            self.candidates.push(Candidate {
                rel,
                loc,
                kind,
                fallback: fallback.to_owned(),
            });
        }
    }

    /// PST·OST: 안의 메일 폴더·메일을 후보로 등록한다(폴더 경로는 파일 안의 경로). OST(HCL Traveler `.ims`)는
    /// 맨 위 기본 폴더 이름을 HCL Domino 메일함 이름으로 바꾼다.
    fn add_pst(&mut self, path: &Path) -> Result<()> {
        let pst = MailFile::open(path).map_err(|e| io_err(path, e))?;
        let items = pst.items().map_err(|e| io_err(path, e))?;
        let (kind, ost) = match pst {
            MailFile::Pst(_) => (Kind::Pst, false),
            MailFile::Ost(_) => (Kind::Ims, true),
        };
        let file = self.psts.len();
        let fallback = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| FALLBACK_FOLDER.to_owned());
        for it in items {
            let mut rel = it.folder;
            if ost
                && let Some(first) = rel.first_mut()
                && let Some(b) = domino_box_name(first)
            {
                *first = b.to_owned();
            }
            rel.push(format!("{}.msg", it.node));
            self.candidates.push(Candidate {
                rel,
                loc: Loc::Pst {
                    file,
                    node: it.node,
                },
                kind,
                fallback: fallback.clone(),
            });
        }
        self.psts.push(pst);
        Ok(())
    }

    fn add_zip(&mut self, path: &Path, prefix: &[String]) -> Result<()> {
        let f = File::open(path).map_err(|e| io_err(path, e))?;
        let mut z = zip::ZipArchive::new(BufReader::new(f)).map_err(|e| io_err(path, e))?;
        let archive = self.zips.len();
        for index in 0..z.len() {
            let Ok(entry) = z.by_index_raw(index) else {
                continue;
            };
            if entry.is_dir() {
                continue;
            }
            let Ok(name) = entry.name().map(|n| n.into_owned()) else {
                continue;
            };
            let mut rel: Vec<String> = prefix.to_vec();
            rel.extend(
                name.split(['/', '\\'])
                    .filter(|s| !s.is_empty() && *s != "." && *s != "..")
                    .map(str::to_owned),
            );
            drop(entry);
            self.add_file(rel, Loc::Zip { archive, index }, FALLBACK_FOLDER);
        }
        self.zips.push(z);
        Ok(())
    }

    fn add_dir(&mut self, root: &Path) -> Result<()> {
        let fallback = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| FALLBACK_FOLDER.to_owned());
        let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
        while let Some((dir, rel)) = stack.pop() {
            let rd = std::fs::read_dir(&dir).map_err(|e| io_err(&dir, e))?;
            let mut entries: Vec<_> = rd.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let name = e.file_name().to_string_lossy().into_owned();
                let path = e.path();
                let Ok(ft) = e.file_type() else { continue };
                let mut sub = rel.clone();
                sub.push(name.clone());
                if ft.is_dir() {
                    stack.push((path, sub));
                } else if ft.is_file() {
                    // 폴더 안의 ZIP(유저스크립트 분할 백업)도 연다. 첨부 폴더 안의 ZIP은 첨부다.
                    if name.to_lowercase().ends_with(".zip") && !rel.iter().any(|d| is_files_dir(d))
                    {
                        self.add_zip(&path, &rel)?;
                    } else if is_mail_file(&name) && !rel.iter().any(|d| is_files_dir(d)) {
                        self.add_pst(&path)?;
                    } else {
                        self.add_file(sub, Loc::Fs(path), &fallback);
                    }
                }
            }
        }
        Ok(())
    }

    fn add_path(&mut self, path: &Path) -> Result<()> {
        if path.is_dir() {
            return self.add_dir(path);
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.to_lowercase().ends_with(".zip") {
            return self.add_zip(path, &[]);
        }
        if is_mail_file(&name) {
            return self.add_pst(path);
        }
        // 파일 하나를 고르면 그 옆 `_files`도 찾을 수 있게 같은 폴더의 파일을 등록한다.
        if ext_kind(&name) == Some(Kind::Html)
            && let Some(parent) = path.parent()
        {
            let stem = name.rsplit_once('.').map_or(name.as_str(), |x| x.0);
            let files_dir = parent.join(format!("{stem}_files"));
            if let Ok(rd) = std::fs::read_dir(&files_dir) {
                for e in rd.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_file()) {
                        let n = e.file_name().to_string_lossy().into_owned();
                        self.add_file(
                            vec![format!("{stem}_files"), n],
                            Loc::Fs(e.path()),
                            FALLBACK_FOLDER,
                        );
                    }
                }
            }
        }
        self.add_file(vec![name], Loc::Fs(path.to_path_buf()), FALLBACK_FOLDER);
        Ok(())
    }

    fn read(&mut self, loc: &Loc) -> std::result::Result<Vec<u8>, String> {
        match loc {
            Loc::Fs(p) => {
                let len = std::fs::metadata(p).map_err(|e| e.to_string())?.len();
                if len > MAX_FILE {
                    return Err(format!("파일이 너무 큽니다({} MB)", len / 1024 / 1024));
                }
                std::fs::read(p).map_err(|e| e.to_string())
            }
            Loc::Zip { archive, index } => {
                let mut f = self.zips[*archive]
                    .by_index(*index)
                    .map_err(|e| e.to_string())?;
                if f.size() > MAX_FILE {
                    return Err(format!("파일이 너무 큽니다({} MB)", f.size() / 1024 / 1024));
                }
                let mut buf = Vec::with_capacity(f.size() as usize);
                f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
                Ok(buf)
            }
            Loc::Pst { file, node } => self.psts[*file]
                .eml(*node)?
                .ok_or_else(|| "메일이 아닌 항목(일정·연락처 등)".to_owned()),
        }
    }
}

fn is_year(s: &str) -> bool {
    s.len() == 4 && s.parse::<u32>().is_ok_and(|y| (1900..=2100).contains(&y))
}

fn is_month(s: &str) -> bool {
    (1..=2).contains(&s.len()) && s.parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))
}

/// HCL Domino 기간 폴더: "2021년 02월", "2018년".
fn is_korean_period(s: &str) -> bool {
    let Some((y, rest)) = s.trim().split_once('년') else {
        return false;
    };
    if !is_year(y.trim()) {
        return false;
    }
    let rest = rest.trim();
    rest.is_empty() || rest.strip_suffix('월').is_some_and(|m| is_month(m.trim()))
}

/// 상대 경로의 폴더들 → 메일 폴더 경로(끝의 yyyy\mm, yyyy, "2021년 02월", "2018년"은 뺀다).
fn folder_path(dirs: &[String], fallback: &str) -> Vec<String> {
    let mut d = dirs.to_vec();
    let n = d.len();
    if n >= 2 && is_year(&d[n - 2]) && is_month(&d[n - 1]) {
        d.truncate(n - 2);
    } else if n >= 1 && (is_year(&d[n - 1]) || is_korean_period(&d[n - 1])) {
        d.truncate(n - 1);
    }
    if d.is_empty() {
        vec![fallback.to_owned()]
    } else {
        d
    }
}

pub(crate) fn props_from(s: &backup::Summary, size: usize, date_received: i64) -> MessageProps {
    let (from_name, from_addr) = match &s.from {
        Some(p) => (
            (!p.name.is_empty()).then(|| p.name.clone()),
            (!p.email.is_empty()).then(|| p.email.clone()),
        ),
        None => (None, None),
    };
    let opt = |v: &str| (!v.is_empty()).then(|| v.to_owned());
    MessageProps {
        internet_msg_id: s.message_id.clone(),
        conversation_id: None,
        subject: Some(s.subject.clone()),
        from_name,
        from_addr,
        to_list: opt(&s.to),
        cc_list: opt(&s.cc),
        date_received,
        date_sent: s.date,
        size: Some(size as i64),
        has_attachments: s.has_attachments,
        is_read: true,
        flag: 0,
        importance: s.importance,
        is_draft: false,
        item_class: Some("IPM.Note".into()),
        preview: opt(&s.preview),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn mtime_ms(loc: &Loc) -> Option<i64> {
    let Loc::Fs(p) = loc else { return None };
    let t = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

/// 한 통을 EML로 만든 결과.
struct Prepared {
    raw: Vec<u8>,
    props: MessageProps,
    /// `import`(EML), `html`(유저스크립트 HTML 백업), `domino`(HCL Domino HTML에서 복원), `domino-eml`(HCL Domino EML).
    origin: &'static str,
    /// 넣을 폴더 경로.
    folder: Vec<String>,
    missing_files: usize,
    extra_files: usize,
    not_downloaded: usize,
}

/// 메일 파일 이름 끝의 HCL Domino 번호: `제목_$147$_.html` → `147`.
fn domino_number(file: &str) -> Option<&str> {
    let stem = file.rsplit_once('.').map_or(file, |x| x.0);
    let inner = stem.strip_suffix("$_")?;
    let i = inner.rfind("_$")?;
    let no = &inner[i + 2..];
    (!no.is_empty() && no.bytes().all(|b| b.is_ascii_digit())).then_some(no)
}

/// HCL Domino 백업의 폴더 경로: `HCL Domino 메일\{메일함…}`.
/// 메일함 이름은 사람마다 다르므로 이름에 기대지 않는다. HCL Domino가 로컬 저장할 때 맨 위 폴더에 만드는 `.idf`
/// 파일(가져오기에는 쓰지 않음)이 있는 폴더를 기준으로, 그 아래 경로를 메일함으로 본다. 그 위 폴더를 골랐을 때를 위한 것이다.
/// `.idf`가 없으면(메일함 폴더 하나만 고름, ZIP에 없음 등) 맨 앞이 "Domino"·"HCL Domino"일 때만 뺀다.
fn domino_folder(dirs: &[String], fallback: &str, idf_roots: &[Vec<String>]) -> Vec<String> {
    let root = idf_roots
        .iter()
        .filter(|r| dirs.starts_with(r))
        .max_by_key(|r| r.len());
    let mut d: Vec<String> = match root {
        Some(r) => dirs[r.len()..].to_vec(),
        None => dirs.to_vec(),
    };
    if root.is_none()
        && d.first().is_some_and(|s| {
            let l = s.trim().to_lowercase();
            l == "domino" || l == "HCL Domino" || l == DOMINO_FOLDER
        })
        && d.len() > 1
    {
        d.remove(0);
    }
    let fallback = if fallback.trim().eq_ignore_ascii_case("domino") {
        FALLBACK_FOLDER
    } else {
        fallback
    };
    let mut out = vec![DOMINO_FOLDER.to_owned()];
    out.extend(folder_path(&d, fallback));
    out
}

/// HCL Domino 백업의 메일 파일인가(EML용, HTML은 내용으로 안다): `제목_$번호$_.eml`이 기간 폴더("2021년 03월",
/// "2018년") 안에 있거나 `.idf` 폴더 아래에 있다.
fn is_domino_layout(dirs: &[String], name: &str, idf_roots: &[Vec<String>]) -> bool {
    domino_number(name).is_some()
        && (dirs.last().is_some_and(|d| is_korean_period(d))
            || idf_roots.iter().any(|r| dirs.starts_with(r)))
}

/// 이 메일 번호의 첨부·그림 파일들(메일 파일 기준 상대 경로, `$$_Attach_$$\…`).
fn domino_files(src: &Sources, dirs: &[String], name: &str) -> Vec<String> {
    let mut files = Vec::new();
    if let Some(no) = domino_number(name) {
        let tag = format!("_${no}$_$");
        for sub in [domino::ATTACH_DIR, domino::IMAGE_DIR] {
            let mut key = dirs.to_vec();
            key.push(sub.to_owned());
            if let Some(names) = src.dirs.get(&key.join("/")) {
                files.extend(
                    names
                        .iter()
                        .filter(|n| n.contains(&tag))
                        .map(|n| format!("{sub}\\{n}")),
                );
            }
        }
    }
    files
}

/// 한 통을 EML로 만든다. 못 만들면 건너뛸 이유.
fn prepare(src: &mut Sources, c: &Candidate) -> std::result::Result<Prepared, String> {
    let data = src.read(&c.loc)?;
    let dirs = &c.rel[..c.rel.len() - 1];
    match c.kind {
        // PST(HCL Domino 로컬 보관 형식)·OST 4K(HCL Traveler `.ims`, HCL Domino 메일함 백업): "HCL Domino 메일" 아래
        // 파일 안의 폴더 경로로.
        Kind::Pst | Kind::Ims => {
            let s = backup::summarize(&data).ok_or("Outlook 메일을 EML로 만들지 못했습니다")?;
            let date = s.date.unwrap_or_else(now_ms);
            let props = props_from(&s, data.len(), date);
            let mut folder = vec![DOMINO_FOLDER.to_owned()];
            folder.extend(dirs.iter().cloned());
            Ok(Prepared {
                raw: data,
                props,
                origin: if c.kind == Kind::Ims {
                    "domino-ims"
                } else {
                    "domino-pst"
                },
                folder,
                missing_files: 0,
                extra_files: 0,
                not_downloaded: 0,
            })
        }
        Kind::Eml => {
            let s = backup::summarize(&data).ok_or("EML을 해석하지 못했습니다")?;
            if s.from.is_none() && s.subject.is_empty() && s.date.is_none() {
                return Err("메일 머리글이 없습니다".into());
            }
            let name = c.rel.last().map_or("", String::as_str);
            if !is_domino_layout(dirs, name, &src.idf_roots) {
                let date = s.date.or_else(|| mtime_ms(&c.loc)).unwrap_or_else(now_ms);
                let props = props_from(&s, data.len(), date);
                return Ok(Prepared {
                    raw: data,
                    props,
                    origin: "import",
                    folder: folder_path(dirs, &c.fallback),
                    missing_files: 0,
                    extra_files: 0,
                    not_downloaded: 0,
                });
            }
            // HCL Domino EML 백업: 틀린 문자셋 표시를 바로잡고(인코딩 안 한 EUC-KR 제목, utf-8이라는 EUC-KR 본문),
            // 따로 저장된 첨부·그림이 있으면 덧붙인다. 그 밖에는 원문 그대로다.
            let (data, s) = match domino::fix_eml_charsets(&data) {
                Some(fixed) => {
                    let s = backup::summarize(&fixed)
                        .ok_or("문자셋을 바로잡은 EML을 해석하지 못했습니다")?;
                    (fixed, s)
                }
                None => (data, s),
            };
            let prefix = dirs.join("/");
            let mut loaded = Vec::new();
            for f in domino_files(src, dirs, name) {
                let key = format!("{prefix}/{}", f.replace('\\', "/"));
                let key = key.trim_start_matches('/');
                if let Some(loc) = src.files.get(key).cloned()
                    && let Ok(bytes) = src.read(&loc)
                {
                    loaded.push((f, bytes));
                }
            }
            let (raw, s) = if loaded.is_empty() {
                (data, s)
            } else {
                let raw = domino::append_attachments(&data, &domino::files_as_attachments(&loaded));
                let s = backup::summarize(&raw).ok_or("첨부를 붙인 EML을 해석하지 못했습니다")?;
                (raw, s)
            };
            let date = s.date.or_else(|| mtime_ms(&c.loc)).unwrap_or_else(now_ms);
            let props = props_from(&s, raw.len(), date);
            Ok(Prepared {
                raw,
                props,
                origin: "domino-eml",
                folder: domino_folder(dirs, &c.fallback, &src.idf_roots),
                missing_files: 0,
                extra_files: loaded.len(),
                not_downloaded: 0,
            })
        }
        Kind::Html => {
            let doc = domino::decode_html(&data);
            if let Some(m) = backup::parse_backup_html(&doc) {
                return prepare_userscript(src, c, &doc, &m);
            }
            let m = domino::parse_domino_html(&doc)
                .ok_or("유저스크립트·HCL Domino 백업 형식이 아닌 HTML입니다")?;
            let name = c.rel.last().map_or("", String::as_str);
            let files = domino_files(src, dirs, name);
            let prefix = dirs.join("/");
            let (raw, stats) = domino::restore_domino_eml(
                &m,
                &files,
                |rel| {
                    let rel = rel.replace('\\', "/");
                    let key = if prefix.is_empty() {
                        rel
                    } else {
                        format!("{prefix}/{rel}")
                    };
                    let loc = src.files.get(&key)?.clone();
                    src.read(&loc).ok()
                },
                &format!("{prefix}/{name}"),
            );
            let s = backup::summarize(&raw).ok_or("복원한 EML을 해석하지 못했습니다")?;
            let date = m
                .received
                .or_else(|| mtime_ms(&c.loc))
                .unwrap_or_else(now_ms);
            let props = props_from(&s, raw.len(), date);
            Ok(Prepared {
                raw,
                props,
                origin: "domino",
                folder: domino_folder(dirs, &c.fallback, &src.idf_roots),
                missing_files: stats.missing_files,
                extra_files: stats.extra_files,
                not_downloaded: stats.not_downloaded,
            })
        }
    }
}

/// 유저스크립트 HTML 백업 한 통.
fn prepare_userscript(
    src: &mut Sources,
    c: &Candidate,
    doc: &str,
    m: &backup::BackupMail,
) -> std::result::Result<Prepared, String> {
    let name = c.rel.last().map_or("", String::as_str);
    let stem = name.rsplit_once('.').map_or(name, |x| x.0);
    let files_dir = format!("{stem}_files");
    let dirs = &c.rel[..c.rel.len() - 1];
    let mut base: Vec<String> = dirs.to_vec();
    base.push(files_dir.clone());
    let names = src.dirs.get(&base.join("/")).cloned().unwrap_or_default();
    let prefix = dirs.join("/");
    let raw = backup::restore_eml(
        m,
        &files_dir,
        &names,
        |rel| {
            let key = if prefix.is_empty() {
                rel.to_owned()
            } else {
                format!("{prefix}/{rel}")
            };
            let loc = src.files.get(&key)?.clone();
            src.read(&loc).ok()
        },
        doc,
    );
    let s = backup::summarize(&raw).ok_or("복원한 EML을 해석하지 못했습니다")?;
    let date = m
        .received
        .or(m.sent)
        .or_else(|| mtime_ms(&c.loc))
        .unwrap_or_else(now_ms);
    let props = props_from(&s, raw.len(), date);
    Ok(Prepared {
        raw,
        props,
        origin: "html",
        folder: folder_path(dirs, &c.fallback),
        missing_files: 0,
        extra_files: 0,
        not_downloaded: 0,
    })
}

/// 가져온다. `paths`는 폴더, ZIP, EML·HTML 파일. 취소하면 그때까지 가져온 것은 남는다.
pub fn import_paths(
    store: &Store,
    paths: &[PathBuf],
    cancel: &AtomicBool,
    progress: impl FnMut(&ImportProgress),
) -> Result<ImportReport> {
    import_paths_with(store, paths, ImportOptions::default(), cancel, progress)
}

/// [`import_paths`]와 같고, 검사만 하기 등 선택을 받는다.
pub fn import_paths_with(
    store: &Store,
    paths: &[PathBuf],
    opts: ImportOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&ImportProgress),
) -> Result<ImportReport> {
    let mut src = Sources::default();
    for p in paths {
        src.add_path(p)?;
    }
    let candidates = std::mem::take(&mut src.candidates);
    let mut report = ImportReport {
        total: candidates.len(),
        dry_run: opts.dry_run,
        ..ImportReport::default()
    };
    let mut per_folder: std::collections::BTreeMap<String, usize> = Default::default();
    let mut folders: HashMap<Vec<String>, i64> = HashMap::new();
    // 서버 폴더가 없으면(로그인 전) HCL Domino 메일만 넣는다: 다른 것은 지금 넣으면 "받은 편지함" 등이 로컬 폴더로 따로 생긴다.
    let server_folders = store.folder_by_well_known("inbox")?.is_some();
    let mut prog = ImportProgress {
        total: candidates.len(),
        ..ImportProgress::default()
    };
    for (k, c) in candidates.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let rel = c.rel.join("/");
        prog.current = rel.clone();
        progress(&prog);
        match prepare(&mut src, c) {
            Err(why) => {
                report.skipped += 1;
                if report.errors.len() < MAX_ERRORS {
                    report.errors.push(format!("{rel}: {why}"));
                }
            }
            Ok(p) if !server_folders && !p.origin.starts_with("domino") => {
                report.need_login += 1;
            }
            Ok(p) => {
                report.missing_files += p.missing_files;
                report.extra_files += p.extra_files;
                report.not_downloaded += p.not_downloaded;
                let label = p.folder.join("/");
                let imported = if opts.dry_run {
                    true
                } else {
                    let folder = match folders.get(&p.folder) {
                        Some(id) => *id,
                        None => {
                            let (id, created) = store.find_or_create_folder_path(&p.folder)?;
                            report.created_folders.extend(created);
                            folders.insert(p.folder.clone(), id);
                            id
                        }
                    };
                    match store.import_message(folder, &p.raw, &p.props, p.origin)? {
                        ImportOutcome::Imported(_) => {
                            if !report.folders.contains(&folder) {
                                report.folders.push(folder);
                            }
                            true
                        }
                        ImportOutcome::Duplicate => {
                            report.duplicates += 1;
                            false
                        }
                    }
                };
                if imported {
                    report.imported += 1;
                    report.eml_bytes += p.raw.len() as u64;
                    *per_folder.entry(label).or_default() += 1;
                    match p.origin {
                        "html" => report.restored_html += 1,
                        "domino" | "domino-eml" | "domino-pst" | "domino-ims" => {
                            report.restored_domino += 1
                        }
                        _ => {}
                    }
                }
            }
        }
        prog.done = k + 1;
        prog.imported = report.imported;
        prog.duplicates = report.duplicates;
        prog.skipped = report.skipped;
    }
    report.per_folder = per_folder.into_iter().collect();
    prog.current.clear();
    progress(&prog);
    Ok(report)
}

/// 이미 가져온 HCL Domino EML의 틀린 문자셋 표시를 바로잡는다(바로잡기 전 버전으로 가져온 메일용).
/// 바꾼 메일의 (번호, 폴더). 가져온 메일(로컬 전용)만 바꾸고 폴더·받은 날짜·읽음·플래그는 그대로 둔다.
/// 다시 해도 바뀌는 것이 없다. 바꾼 메일은 다시 색인하게 표시되므로 부른 쪽이 `index_pending`을 돌린다.
pub fn repair_domino_eml(store: &Store) -> Result<Vec<(i64, i64)>> {
    let mut changed = Vec::new();
    for id in store.message_ids_by_origin("domino-eml")? {
        let Some(m) = store.message(id)? else {
            continue;
        };
        let Some(hash) = m.eml_hash.as_deref() else {
            continue;
        };
        let Ok(raw) = store.eml().get(hash) else {
            continue;
        };
        let Some(fixed) = domino::fix_eml_charsets(&raw) else {
            continue;
        };
        let Some(s) = backup::summarize(&fixed) else {
            continue;
        };
        let mut props = props_from(&s, fixed.len(), m.props.date_received);
        props.is_read = m.props.is_read;
        props.flag = m.props.flag;
        if store.replace_imported_eml(id, &fixed, &props)? {
            changed.push((id, m.folder_id));
        }
    }
    Ok(changed)
}

/// 점검용 견본: 유저스크립트 백업과 같은 형식의 ZIP(무압축, UTF-8 이름)을 만든다. 메일 수를 돌려준다.
/// 받은 편지함 2통(본문 이미지·첨부, 제목 괄호), 프로젝트 1통(항목 첨부), 옛날 편지함 1통(텍스트 본문).
/// 본문마다 검색 확인용 낱말 "백업복원견본"이 들어 있다.
pub fn write_sample_html_backup(dest: &Path) -> Result<usize> {
    use base64::Engine as _;
    use eo_mime::compose::Person;
    use std::io::Write as _;
    // 1×1 PNG(합성 저장소의 서명 이미지와 같은 것).
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .unwrap_or_default();
    let person = |name: &str, email: &str| Person {
        name: name.into(),
        email: email.into(),
    };
    struct Sample {
        folder: &'static str,
        stamp: &'static str,
        subject: &'static str,
        body: &'static str,
        files: Vec<(&'static str, Vec<u8>)>,
        image: Option<&'static str>,
        attach: Vec<(&'static str, bool)>,
        cc: bool,
    }
    let samples = [
        Sample {
            folder: "받은 편지함",
            stamp: "20251128_173000",
            subject: "[견본] 분기 결산 (최종)",
            body: "<p>분기 결산표를 보냅니다. 백업복원견본</p>",
            files: vec![
                ("image001.png", png.clone()),
                ("결산 (최종).xlsx", b"PK\x03\x04sample".to_vec()),
            ],
            image: Some("image001.png"),
            attach: vec![("결산 (최종).xlsx", true)],
            cc: false,
        },
        Sample {
            folder: "받은 편지함",
            stamp: "20251127_090000",
            subject: "[견본] 회의 안내",
            body: "<p>목요일 10시 회의입니다. 백업복원견본</p>",
            files: vec![],
            image: None,
            attach: vec![],
            cc: true,
        },
        Sample {
            folder: "프로젝트",
            stamp: "20251215_140000",
            subject: "[견본] 프로젝트 일정",
            body: "<div><b>일정표</b> 백업복원견본</div>",
            files: vec![],
            image: None,
            attach: vec![("회의 초대", false)],
            cc: false,
        },
        Sample {
            folder: "옛날 편지함",
            stamp: "20190315_100000",
            subject: "[견본] 옛날 메일",
            body: "<pre style=\"white-space:pre-wrap\">옛날 텍스트 메일 백업복원견본</pre>",
            files: vec![],
            image: None,
            attach: vec![],
            cc: false,
        },
    ];
    let f = File::create(dest).map_err(|e| io_err(dest, e))?;
    let mut z = zip::ZipWriter::new(f);
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let zerr = |e: &dyn std::fmt::Display| io_err(dest, e);
    for (k, s) in samples.iter().enumerate() {
        let (y, m) = (&s.stamp[..4], &s.stamp[4..6]);
        let base = format!("{}_{}_s{k}", s.stamp, s.subject);
        let files_dir = format!("{base}_files");
        let at = backup::parse_kst(&format!(
            "{}-{}-{} {}:{}:{}",
            y,
            m,
            &s.stamp[6..8],
            &s.stamp[9..11],
            &s.stamp[11..13],
            &s.stamp[13..15]
        ));
        let mut body = s.body.to_owned();
        if let Some(img) = s.image {
            body.push_str(&format!(
                "<p><img src=\"{}\" alt=\"서명\"></p>",
                backup::enc_path(&[&files_dir, img])
            ));
        }
        let mail = backup::BackupMail {
            subject: s.subject.into(),
            from: vec![person("김민수", "minsu.kim@example.com")],
            to: vec![person("", "me@example.com")],
            cc: if s.cc {
                vec![person("이지영", "jiyoung.lee@example.com")]
            } else {
                vec![]
            },
            received: at,
            sent: at.map(|t| t - 2_000),
            body_html: body,
            attachments: s
                .attach
                .iter()
                .map(|(name, file)| {
                    (
                        (*name).to_owned(),
                        file.then(|| backup::enc_path(&[&files_dir, name])),
                    )
                })
                .collect(),
        };
        let dir = format!("{}/{y}/{m}", s.folder);
        z.start_file(format!("{dir}/{base}.html"), opts)
            .map_err(|e| zerr(&e))?;
        z.write_all(backup::to_backup_html(&mail).as_bytes())
            .map_err(|e| zerr(&e))?;
        for (name, data) in &s.files {
            z.start_file(format!("{dir}/{files_dir}/{name}"), opts)
                .map_err(|e| zerr(&e))?;
            z.write_all(data).map_err(|e| zerr(&e))?;
        }
    }
    z.finish().map_err(|e| zerr(&e))?;
    Ok(samples.len())
}

/// 점검용 견본: HCL Domino 백업과 같은 모양의 폴더(`dest\받은 메일\2021년 03월\…` 등)를 만든다. 메일 수를 돌려준다.
/// 받은 메일 2통(EUC-KR, 본문 그림·첨부, 제목 괄호 / UTF-8), 외부 메일 1통(cid로 남은 그림 + 남은 그림 파일),
/// 보낸 메일 1통(숨은 참조). 본문마다 검색 확인용 낱말 "HCL Domino견본"이 들어 있다. 이름·주소는 지어낸 것이다.
pub fn write_sample_domino_backup(dest: &Path) -> Result<usize> {
    use base64::Engine as _;
    use eo_mime::compose::Person;
    use eo_mime::domino::{DominoMail, encode_korean, to_domino_html};
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==")
        .unwrap_or_default();
    let p = |name: &str, email: &str| Person {
        name: name.into(),
        email: email.into(),
    };
    let write = |rel: &str, data: &[u8]| -> Result<()> {
        let path = dest.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
        }
        std::fs::write(&path, data).map_err(|e| io_err(&path, e))
    };
    let at = |s: &str| backup::parse_kst(s);
    /// (경로, 메일, EUC-KR로 쓸까, 딸린 파일들(상대 경로, 내용))
    type Sample = (&'static str, DominoMail, bool, Vec<(String, Vec<u8>)>);
    let mails: Vec<Sample> = vec![
        (
            "받은 메일\\2021년 03월\\[견본] 분기 결산 (최종)_$147$_.html",
            DominoMail {
                subject: "[견본] 분기 결산 (최종)".into(),
                from: vec![p("김민수", "minsu.kim@example.com")],
                to: vec![
                    p("나", "me@example.com"),
                    p("이지영", "jiyoung.lee@example.com"),
                ],
                received: at("2021-03-02 13:05:22"),
                message_id: Some("<sample147@mail.example.org>".into()),
                file_id: Some("SAMPLE0000000000000000000000147".into()),
                body_html: "<HTML><BODY><P>분기 결산표입니다. HCL Domino견본</P>\
                            <IMG src=\"$$_Image_$$\\$image_001_png__$147$_$28$_.2\"></BODY></HTML>"
                    .into(),
                attachments: vec![(
                    "결산 (최종).xlsx".into(),
                    "$$_Attach_$$\\결산 (최종)_$147$_$190$_.xlsx".into(),
                )],
                ..DominoMail::default()
            },
            true,
            vec![
                (
                    "$$_Image_$$\\$image_001_png__$147$_$28$_.2".to_owned(),
                    png.clone(),
                ),
                (
                    "$$_Attach_$$\\결산 (최종)_$147$_$190$_.xlsx".to_owned(),
                    b"PK\x03\x04sample".to_vec(),
                ),
            ],
        ),
        (
            "받은 메일\\2021년 04월\\[견본] 회의 안내_$201$_.html",
            DominoMail {
                subject: "[견본] 회의 안내".into(),
                from: vec![p("박서연", "Z12@mail.example.org")],
                to: vec![p("나", "me@example.com")],
                cc: vec![p("최도윤", "doyoon@example.com")],
                received: at("2021-04-05 09:00:00"),
                message_id: Some("<sample201@mail.example.org>".into()),
                file_id: Some("SAMPLE0000000000000000000000201".into()),
                body_html: "<P>목요일 10시 회의입니다. HCL Domino견본</P>".into(),
                ..DominoMail::default()
            },
            false,
            vec![],
        ),
        (
            "외부 메일\\2021년 03월\\[견본] 외부 공지_$5$_.html",
            DominoMail {
                subject: "[견본] 외부 공지".into(),
                from: vec![p("Partner", "notice@partner.example.org")],
                to: vec![p("나", "me@example.com")],
                received: at("2021-03-10 18:30:00"),
                message_id: None,
                file_id: Some("SAMPLE0000000000000000000000005".into()),
                body_html: "<P>외부 공지 HCL Domino견본</P><img src=\"cid:logo@partner\">".into(),
                ..DominoMail::default()
            },
            false,
            vec![("$$_Image_$$\\logo_$5$_$1$_.png".to_owned(), png.clone())],
        ),
        (
            "보낸 메일\\2018년\\[견본] 휴가 신청_$9$_.html",
            DominoMail {
                subject: "[견본] 휴가 신청".into(),
                from: vec![p("나", "me@example.com")],
                to: vec![p("팀장", "lead@example.com")],
                bcc: vec![p("인사", "hr@example.com")],
                received: at("2018-12-18 10:20:00"),
                message_id: Some("<sample9@mail.example.org>".into()),
                file_id: Some("SAMPLE0000000000000000000000009".into()),
                body_html: "<P>휴가를 신청합니다. HCL Domino견본</P>".into(),
                ..DominoMail::default()
            },
            true,
            vec![],
        ),
    ];
    for (rel, m, euc, files) in &mails {
        let html = to_domino_html(m, if *euc { "euc-kr" } else { "utf-8" });
        let bytes = if *euc {
            encode_korean(&html)
        } else {
            html.into_bytes()
        };
        write(rel, &bytes)?;
        let dir = rel.rsplit_once('\\').map_or("", |x| x.0);
        for (f, data) in files {
            write(&format!("{dir}\\{f}"), data)?;
        }
    }
    Ok(mails.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 로그인해 서버 폴더 목록을 받은 상태(받은 편지함 하나).
    fn with_inbox(store: &Store) {
        store
            .transaction(|tx| {
                tx.upsert_folder(&eo_store::ServerFolder {
                    server_id: "INBOX".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "받은 편지함".into(),
                    folder_class: Some("IPF.Note".into()),
                })?;
                tx.set_well_known("INBOX", "inbox", 0)?;
                tx.rebuild_tree("ROOT")
            })
            .unwrap();
    }

    /// HCL Domino EML 백업(다운로드 형식 EML): `HCL Domino 메일\{메일함}`으로, 노츠 날짜를 읽고, 따로 저장된 첨부는 덧붙인다.
    /// HCL Domino 모양이 아닌 EML(이 앱의 내보내기 등)은 예전처럼 같은 이름 폴더에 합친다.
    #[test]
    fn domino_eml_backup_goes_under_domino_folder() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let src = dir.path().join("EML 백업");
        let put = |rel: &str, data: &[u8]| {
            let p = src.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, data).unwrap();
        };
        let eml = |id: &str, date: &str| {
            format!(
                "Reply-To: a@x.com\r\nDate: {date}\r\nContent-Type: text/html; charset=\"utf-8\"\r\nMIME-Version: 1.0\r\n\
                 From: =?utf-8?B?6rmA66+87IiY?= <minsu@x.com>\r\nX-Priority: \r\nImportance: \r\nSubject: s {id}\r\n\
                 To: me@x.com\r\nMessage-ID: <{id}@LocalDomain>\r\nCc: \r\nBcc: \r\n\r\n<p>EMLHCL Domino {id}</p>\r\n"
            )
        };
        put(
            "내 메일함\\2025년 10월\\첫째_$12$_.eml",
            eml("OF12", "09-Oct-2025 13:05:22 ZE9").as_bytes(),
        );
        put(
            "내 메일함\\2025년 10월\\둘째_$13$_.eml",
            eml("OF13", "Thu, 9 Oct 2025 13:05:22 +0900").as_bytes(),
        );
        put(
            "내 메일함\\2025년 10월\\$$_Attach_$$\\보고서_$13$_$1$_.xlsx",
            b"PK-x",
        );
        put(
            "보낸 메일\\2018년\\셋째_$7$_.eml",
            eml("OF7", "18-Dec-2018 10:20:00 ZE9").as_bytes(),
        );
        // 이 앱의 내보내기 모양(번호 없음, 연\월 폴더): HCL Domino가 아니다.
        put(
            "보낸 메일\\2025\\10\\보통.eml",
            eml("plain", "Thu, 9 Oct 2025 13:05:22 +0900").as_bytes(),
        );
        let cancel = AtomicBool::new(false);
        // 로그인 전(서버 폴더 없음, 오프라인 사용): HCL Domino 메일만 들어가고, 같은 이름 폴더에 합칠 EML은 기다린다.
        let r = import_paths(&store, std::slice::from_ref(&src), &cancel, |_| {}).unwrap();
        assert_eq!(
            (
                r.imported,
                r.restored_domino,
                r.extra_files,
                r.skipped,
                r.need_login
            ),
            (3, 3, 1, 0, 1),
            "{r:?}"
        );
        assert_eq!(
            r.per_folder,
            [
                ("HCL Domino 메일/내 메일함".to_owned(), 2),
                ("HCL Domino 메일/보낸 메일".to_owned(), 1),
            ]
        );
        // 로그인해 폴더 목록을 받은 뒤 다시 하면 나머지가 들어간다.
        with_inbox(&store);
        let r = import_paths(&store, std::slice::from_ref(&src), &cancel, |_| {}).unwrap();
        assert_eq!((r.imported, r.duplicates, r.need_login), (1, 3, 0), "{r:?}");
        assert_eq!(r.per_folder, [("보낸 메일".to_owned(), 1)]);
        let folders = store.folders().unwrap();
        let mine = folders.iter().find(|f| f.name == "내 메일함").unwrap();
        let rows: Vec<_> = store
            .list_ids(Some(mine.id))
            .unwrap()
            .into_iter()
            .map(|id| store.message(id).unwrap().unwrap())
            .collect();
        let first = rows
            .iter()
            .find(|m| m.props.subject.as_deref() == Some("s OF12"))
            .unwrap();
        assert_eq!(first.origin.as_deref(), Some("domino-eml"));
        assert_eq!(
            first.props.date_received,
            backup::parse_kst("2025-10-09 13:05:22").unwrap(),
            "노츠 날짜"
        );
        let second = rows
            .iter()
            .find(|m| m.props.subject.as_deref() == Some("s OF13"))
            .unwrap();
        assert!(second.props.has_attachments, "따로 저장된 첨부를 덧붙임");
        let x = eo_mime::extract(
            &store
                .eml()
                .get(second.eml_hash.as_deref().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(x.attachment_names, ["보고서.xlsx"]);
        assert!(x.body_text.contains("EMLHCL Domino OF13"));
        // .idf가 있으면 기간 폴더가 없어도 HCL Domino로 본다.
        assert!(is_domino_layout(
            &["받은 메일".to_owned()],
            "x_$1$_.eml",
            &[vec![]]
        ));
        assert!(!is_domino_layout(
            &["받은 메일".to_owned()],
            "x_$1$_.eml",
            &[]
        ));
        assert!(!is_domino_layout(&["2025년 10월".to_owned()], "x.eml", &[]));
    }

    /// 틀린 문자셋 표시(인코딩 안 한 EUC-KR 제목, utf-8이라는 EUC-KR 본문): 가져올 때 바로잡고,
    /// 바로잡기 전에 가져온 메일은 `repair_domino_eml`이 바로잡는다(읽음·플래그는 그대로, 다시 해도 같음).
    #[test]
    fn mislabeled_domino_eml_is_fixed_on_import_and_repair() {
        let mut eml: Vec<u8> = b"From: a@x.com\r\nTo: me@x.com\r\nSubject: ".to_vec();
        eml.extend(domino::encode_korean("주간 보고서 제출"));
        eml.extend(
            b"\r\nDate: 15-Oct-2026 16:34:00 ZE9\r\nMessage-ID: <OF77@LocalDomain>\r\nMIME-Version: 1.0\r\n\
              Content-Type: text/html; charset=\"utf-8\"\r\nContent-Transfer-Encoding: 8bit\r\n\r\n<p>",
        );
        eml.extend(domino::encode_korean("본문 한글 내용입니다"));
        eml.extend(b"</p>\r\n");

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        // 1) 가져올 때 바로잡는다.
        let src = dir.path().join("백업");
        let p = src
            .join("받은 메일")
            .join("2026년 10월")
            .join("x_$77$_.eml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, &eml).unwrap();
        let r = import_paths(&store, &[src], &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(r.imported, 1);
        let id = store.message_ids_by_origin("domino-eml").unwrap()[0];
        let m = store.message(id).unwrap().unwrap();
        assert_eq!(m.props.subject.as_deref(), Some("주간 보고서 제출"));
        let x =
            eo_mime::extract(&store.eml().get(m.eml_hash.as_deref().unwrap()).unwrap()).unwrap();
        assert!(x.body_text.contains("본문 한글 내용입니다"));

        // 2) 바로잡기 전 버전이 넣은 것처럼 깨진 원문을 그대로 넣어 두면, repair가 바로잡는다.
        let (f, _) = store
            .find_or_create_folder_path(&["HCL Domino 메일".into(), "옛 버전".into()])
            .unwrap();
        let garbled = backup::summarize(&eml).unwrap();
        let props = MessageProps {
            date_received: 5,
            ..props_from(&garbled, eml.len(), 5)
        };
        let ImportOutcome::Imported(old) =
            store.import_message(f, &eml, &props, "domino-eml").unwrap()
        else {
            panic!()
        };
        store.mark_flag(&[old], 1, 1).unwrap();
        assert_ne!(
            store
                .message(old)
                .unwrap()
                .unwrap()
                .props
                .subject
                .as_deref(),
            Some("주간 보고서 제출"),
            "깨진 채로 들어감"
        );
        assert_eq!(
            repair_domino_eml(&store).unwrap(),
            [(old, f)],
            "바로잡은 것(이미 바른 1통은 그대로)"
        );
        let m = store.message(old).unwrap().unwrap();
        assert_eq!(
            (
                m.props.subject.as_deref(),
                m.props.flag,
                m.props.date_received,
                m.folder_id
            ),
            (Some("주간 보고서 제출"), 1, 5, f)
        );
        assert!(
            repair_domino_eml(&store).unwrap().is_empty(),
            "다시 해도 바뀌는 것이 없다"
        );
    }

    /// 메일함 이름은 사람마다 다르다. `.idf`가 있으면 그 위 폴더를 골라도 `.idf` 폴더 아래를 메일함으로 보고,
    /// 없으면 고른 폴더 바로 아래를 메일함으로 본다(어느 쪽이든 가져온다).
    #[test]
    fn domino_root_with_and_without_idf() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let parent = dir.path().join("백업 모음");
        let root = parent.join("내 HCL Domino 저장");
        write_sample_domino_backup(&root).unwrap();
        let cancel = AtomicBool::new(false);
        let folders_of = |p: &Path| {
            import_paths_with(
                &store,
                &[p.to_path_buf()],
                ImportOptions { dry_run: true },
                &cancel,
                |_| {},
            )
            .unwrap()
            .per_folder
            .into_iter()
            .map(|(f, _)| f)
            .collect::<Vec<_>>()
        };
        let expected = [
            "HCL Domino 메일/받은 메일",
            "HCL Domino 메일/보낸 메일",
            "HCL Domino 메일/외부 메일",
        ];
        // .idf 없음: 맨 위 폴더를 고르면 그대로, 그 위를 고르면 고른 폴더 아래 이름이 끼어든다.
        assert_eq!(folders_of(&root), expected);
        assert_eq!(
            folders_of(&parent),
            [
                "HCL Domino 메일/내 HCL Domino 저장/받은 메일",
                "HCL Domino 메일/내 HCL Domino 저장/보낸 메일",
                "HCL Domino 메일/내 HCL Domino 저장/외부 메일",
            ]
        );
        // .idf 있음: 그 위 폴더를 골라도 .idf 폴더 아래가 메일함이다.
        std::fs::write(root.join("u000000.idf"), b"SQLite format 3\0").unwrap();
        assert_eq!(folders_of(&root), expected);
        assert_eq!(folders_of(&parent), expected);
    }

    #[test]
    fn sample_domino_backup_imports_into_domino_folder() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        let src = dir.path().join("Domino");
        assert_eq!(write_sample_domino_backup(&src).unwrap(), 4);
        let cancel = AtomicBool::new(false);
        let dry = import_paths_with(
            &store,
            std::slice::from_ref(&src),
            ImportOptions { dry_run: true },
            &cancel,
            |_| {},
        )
        .unwrap();
        assert_eq!(
            (dry.imported, dry.restored_domino, dry.dry_run),
            (4, 4, true)
        );
        assert!(
            store.folders().unwrap().is_empty(),
            "검사만 하면 아무것도 만들지 않는다"
        );
        let r = import_paths(&store, std::slice::from_ref(&src), &cancel, |_| {}).unwrap();
        assert_eq!(
            (
                r.imported,
                r.restored_domino,
                r.skipped,
                r.missing_files,
                r.extra_files
            ),
            (4, 4, 0, 0, 1),
            "{r:?}"
        );
        assert_eq!(
            r.per_folder,
            [
                ("HCL Domino 메일/받은 메일".to_owned(), 2),
                ("HCL Domino 메일/보낸 메일".to_owned(), 1),
                ("HCL Domino 메일/외부 메일".to_owned(), 1),
            ]
        );
        let mut created = r.created_folders.clone();
        created.sort();
        assert_eq!(
            created,
            [
                "HCL Domino 메일",
                "HCL Domino 메일/받은 메일",
                "HCL Domino 메일/보낸 메일",
                "HCL Domino 메일/외부 메일"
            ]
        );
        // 받은 메일 첫 통: 본문 그림·첨부가 복원되고 숨은 참조·원래 Message-ID가 남는다.
        let folders = store.folders().unwrap();
        let inbox = folders.iter().find(|f| f.name == "받은 메일").unwrap();
        assert!(inbox.server_id.is_none());
        let ids = store.list_ids(Some(inbox.id)).unwrap();
        let m = ids
            .iter()
            .map(|id| store.message(*id).unwrap().unwrap())
            .find(|m| m.props.subject.as_deref() == Some("[견본] 분기 결산 (최종)"))
            .unwrap();
        assert_eq!(m.origin.as_deref(), Some("domino"));
        assert_eq!(
            m.props.internet_msg_id.as_deref(),
            Some("<sample147@mail.example.org>")
        );
        let raw = store.eml().get(m.eml_hash.as_deref().unwrap()).unwrap();
        let shown = eo_mime::render(
            &raw,
            &eo_mime::RenderOptions {
                cid_base: "http://app-cid.localhost/1/".into(),
                allow_remote: false,
            },
        )
        .unwrap();
        assert_eq!(shown.inline_images.len(), 1);
        assert!(
            shown
                .attachments
                .iter()
                .any(|a| a.name == "결산 (최종).xlsx")
        );
        let sent = folders.iter().find(|f| f.name == "보낸 메일").unwrap();
        let sent_raw = store
            .eml()
            .get(
                store
                    .message(store.list_ids(Some(sent.id)).unwrap()[0])
                    .unwrap()
                    .unwrap()
                    .eml_hash
                    .as_deref()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            eo_mime::extract(&sent_raw).unwrap().bcc,
            "인사 <hr@example.com>"
        );
        let again = import_paths(&store, &[src], &cancel, |_| {}).unwrap();
        assert_eq!(
            (again.imported, again.duplicates),
            (0, 4),
            "다시 가져오면 모두 중복"
        );
    }

    #[test]
    fn sample_backup_imports_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store")).unwrap();
        with_inbox(&store);
        let zip = dir.path().join("견본_01.zip");
        assert_eq!(write_sample_html_backup(&zip).unwrap(), 4);
        let r = import_paths(&store, &[zip], &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!((r.imported, r.restored_html, r.skipped), (4, 4, 0), "{r:?}");
        assert_eq!(
            r.created_folders,
            ["프로젝트", "옛날 편지함"],
            "받은 편지함은 서버 것에 합친다"
        );
    }

    #[test]
    fn folder_path_drops_date_folders() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            folder_path(&v(&["받은 편지함", "2026", "10"]), "x"),
            v(&["받은 편지함"])
        );
        assert_eq!(
            folder_path(&v(&["프로젝트", "2025 결산", "2025"]), "x"),
            v(&["프로젝트", "2025 결산"])
        );
        assert_eq!(folder_path(&v(&["2026", "10"]), "백업"), v(&["백업"]));
        assert_eq!(
            folder_path(&v(&["12"]), "x"),
            v(&["12"]),
            "월만 있으면 폴더 이름"
        );
        // HCL Domino 기간 폴더.
        assert_eq!(
            folder_path(&v(&["받은 메일", "2021년 02월"]), "x"),
            v(&["받은 메일"])
        );
        assert_eq!(
            folder_path(&v(&["보낸 메일", "2018년"]), "x"),
            v(&["보낸 메일"])
        );
        assert_eq!(
            folder_path(&v(&["2021년 정산"]), "x"),
            v(&["2021년 정산"]),
            "기간이 아닌 이름은 둔다"
        );
        // .idf가 없을 때: 고른 폴더 바로 아래가 메일함(맨 앞 "Domino"만 뺀다).
        assert_eq!(
            domino_folder(&v(&["Domino", "외부 메일", "2021년 03월"]), "x", &[]),
            v(&[DOMINO_FOLDER, "외부 메일"])
        );
        assert_eq!(
            domino_folder(&v(&["2021년 03월"]), "받은 메일", &[]),
            v(&[DOMINO_FOLDER, "받은 메일"]),
            "메일함 폴더 하나만 고르면 그 이름"
        );
        // .idf가 있을 때: 그 폴더 아래가 메일함(위 폴더 이름은 무엇이든 뺀다).
        assert_eq!(
            domino_folder(
                &v(&["백업", "내 메일", "팀 공지", "2021년 03월"]),
                "x",
                &[v(&["백업", "내 메일"])]
            ),
            v(&[DOMINO_FOLDER, "팀 공지"])
        );
        assert_eq!(domino_number("주간 보고_$147$_.html"), Some("147"));
        assert_eq!(domino_number("a_$x$_.html"), None);
        assert_eq!(domino_number("plain.html"), None);
    }
}
