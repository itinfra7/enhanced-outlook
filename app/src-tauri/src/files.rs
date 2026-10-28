//! 첨부 저장·열기(계획서 9.2 머리글의 열기/저장/모두 저장).
//!
//! - 쓴 파일에는 "인터넷에서 받은 파일" 표시(Zone.Identifier, ZoneId=3)를 붙여 Windows가
//!   열 때 경고·보호 보기를 적용하게 한다(브라우저·Outlook과 같은 방식).
//! - 실행 형식(Outlook 1단계 차단 목록 + 서버가 알려 준 차단 형식)은 바로 열지 않는다. 저장은 된다.
//! - 열기용 사본은 `EnhancedOutlook\tmp\{메일 ID}\`에 둔다(포터블 루트 안). 하루 지난 것은 시작할 때 지운다.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Outlook이 기본으로 막는 형식(1단계 첨부)과 디스크 이미지.
const BLOCKED: &[&str] = &[
    "ade",
    "adp",
    "app",
    "application",
    "appref-ms",
    "asp",
    "aspx",
    "asx",
    "bas",
    "bat",
    "bgi",
    "cab",
    "cer",
    "chm",
    "cmd",
    "cnt",
    "com",
    "cpl",
    "crt",
    "csh",
    "der",
    "diagcab",
    "exe",
    "fxp",
    "gadget",
    "grp",
    "hlp",
    "hpj",
    "hta",
    "htc",
    "img",
    "inf",
    "ins",
    "iso",
    "isp",
    "its",
    "jar",
    "jnlp",
    "js",
    "jse",
    "ksh",
    "lnk",
    "mad",
    "maf",
    "mag",
    "mam",
    "maq",
    "mar",
    "mas",
    "mat",
    "mau",
    "mav",
    "maw",
    "mcf",
    "mda",
    "mdb",
    "mde",
    "mdt",
    "mdw",
    "mdz",
    "msc",
    "msh",
    "msh1",
    "msh2",
    "mshxml",
    "msh1xml",
    "msh2xml",
    "msi",
    "msix",
    "msixbundle",
    "msp",
    "mst",
    "msu",
    "ops",
    "osd",
    "pcd",
    "pif",
    "pl",
    "plg",
    "prf",
    "prg",
    "printerexport",
    "ps1",
    "ps1xml",
    "ps2",
    "ps2xml",
    "psc1",
    "psc2",
    "psd1",
    "psdm1",
    "pst",
    "py",
    "pyc",
    "pyo",
    "pyw",
    "pyz",
    "pyzw",
    "reg",
    "scf",
    "scr",
    "sct",
    "settingcontent-ms",
    "shb",
    "shs",
    "theme",
    "tmp",
    "url",
    "vb",
    "vbe",
    "vbp",
    "vbs",
    "vhd",
    "vhdx",
    "vsmacros",
    "vsw",
    "webpnp",
    "website",
    "ws",
    "wsb",
    "wsc",
    "wsf",
    "wsh",
    "xbap",
    "xll",
    "xnk",
];

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Windows 파일 이름으로 쓸 수 있게 고친다(경로 구분자·예약 이름 제거, 길이 제한).
pub fn safe_file_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).to_owned();
    if s.chars().count() > 150 {
        // 확장자는 남기고 앞을 줄인다.
        let ext = Path::new(&s)
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .filter(|e| e.chars().count() <= 16);
        let stem: String = s.chars().take(120).collect();
        s = match ext {
            Some(e) => format!("{stem}.{e}"),
            None => stem,
        };
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    if s.is_empty() || s.chars().all(|c| c == '.') {
        "첨부".to_owned()
    } else {
        s
    }
}

pub fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 바로 열면 안 되는 형식인가. `server_blocked`는 웹메일 설정의 차단 형식(".exe" 또는 "exe").
pub fn is_blocked(name: &str, server_blocked: &[String]) -> bool {
    let ext = extension(name);
    if ext.is_empty() {
        return false;
    }
    BLOCKED.contains(&ext.as_str())
        || server_blocked
            .iter()
            .any(|b| b.trim().trim_start_matches('.').eq_ignore_ascii_case(&ext))
}

/// 같은 이름이 있으면 "이름 (2).확장자"처럼 비어 있는 이름을 찾는다.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map_or_else(|| name.to_owned(), |s| s.to_string_lossy().into_owned());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2..10_000)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or(first)
}

/// 파일을 쓰고 "인터넷에서 받은 파일" 표시를 붙인다.
pub fn write_marked(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    // NTFS 대체 데이터 스트림. FAT·네트워크 드라이브 등에서는 실패해도 무시한다.
    let mut ads = path.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    let _ = std::fs::write(PathBuf::from(ads), "[ZoneTransfer]\r\nZoneId=3\r\n");
    Ok(())
}

/// 열기용 사본 중 하루 지난 것을 지운다(열려 있어 못 지우면 다음에).
pub fn clean_tmp(tmp: &Path) {
    let Ok(rd) = std::fs::read_dir(tmp) else {
        return;
    };
    let day = Duration::from_secs(86_400);
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > day);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("견적서.pdf"), "견적서.pdf");
        assert_eq!(safe_file_name("..\\..\\x/y:z?.txt"), ".._.._x_y_z_.txt");
        assert_eq!(safe_file_name("con.txt"), "_con.txt");
        assert_eq!(safe_file_name("  보고서.docx . "), "보고서.docx");
        assert_eq!(safe_file_name(""), "첨부");
        let long = format!("{}.pdf", "가".repeat(300));
        let s = safe_file_name(&long);
        assert!(s.ends_with(".pdf") && s.chars().count() <= 150, "{s}");
    }

    #[test]
    fn executables_are_blocked() {
        assert!(is_blocked("setup.EXE", &[]));
        assert!(is_blocked("a.tar.js", &[]));
        assert!(!is_blocked("보고서.pdf", &[]));
        assert!(is_blocked("x.abc", &[".abc".into()]));
        assert!(!is_blocked("noext", &[]));
    }

    #[test]
    fn unique_and_marked() {
        let dir = tempfile::tempdir().unwrap();
        let p = unique_path(dir.path(), "a.txt");
        write_marked(&p, b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (2).txt")
        );
        let zone = std::fs::read_to_string(format!("{}:Zone.Identifier", p.display()));
        if let Ok(z) = zone {
            assert!(z.contains("ZoneId=3"));
        }
    }
}
