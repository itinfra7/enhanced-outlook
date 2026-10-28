//! 앱 로그(계획서 10장 8단계): `EnhancedOutlook\logs\app.log`, 1MB가 넘으면 `app.1.log`…`app.4.log`로 돌린다.
//!
//! - 남기는 것: 시작·종료, 로그인 성공·실패(사유 종류만), 동기화·작업 오류, 발송 성공·실패, 가져오기·내보내기 건수,
//!   설정 변경, 비정상 종료(패닉).
//! - **남기지 않는 것:** 비밀번호, 아이디, 메일 제목·주소·본문(owa-cli와 같은 원칙).
//! - 시각은 한국 시간이다(앱의 다른 표시와 같다).

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 이 크기가 넘으면 돌린다.
const MAX_BYTES: u64 = 1024 * 1024;
/// 돌린 파일을 몇 개까지 둘까(`.1`~`.4`).
const KEEP: usize = 4;

struct Logger {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}

static LOG: OnceLock<Mutex<Logger>> = OnceLock::new();

/// 로그 폴더를 정한다(처음 한 번). 실패해도 앱은 돈다(로그만 없다).
pub fn init(root: &Path) {
    let dir = root.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("app.log");
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    let _ = LOG.set(Mutex::new(Logger {
        path,
        file: None,
        size,
    }));
}

fn now_text() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    let secs = ms.div_euclid(1000) + 9 * 3600;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

/// `name.log` → `name.1.log`…(가장 오래된 것은 덮어쓴다).
pub fn rotate(path: &Path) {
    let numbered = |n: usize| {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        path.with_file_name(format!("{stem}.{n}.log"))
    };
    for n in (1..KEEP).rev() {
        let from = numbered(n);
        if from.exists() {
            let _ = std::fs::rename(&from, numbered(n + 1));
        }
    }
    let _ = std::fs::rename(path, numbered(1));
}

fn write(level: &str, msg: &str) {
    let Some(log) = LOG.get() else { return };
    let mut l = log.lock().unwrap_or_else(|e| e.into_inner());
    let line = format!(
        "{} {level} {}\r\n",
        now_text(),
        msg.replace(['\r', '\n'], " ")
    );
    if l.size + line.len() as u64 > MAX_BYTES {
        l.file = None;
        rotate(&l.path);
        l.size = 0;
    }
    if l.file.is_none() {
        l.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&l.path)
            .ok();
    }
    if let Some(f) = l.file.as_mut()
        && f.write_all(line.as_bytes()).is_ok()
    {
        let _ = f.flush();
        l.size += line.len() as u64;
    }
}

pub fn info(msg: impl AsRef<str>) {
    write("정보", msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    write("주의", msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    write("오류", msg.as_ref());
}

/// 측정·점검 기록(`perf.log`, `uitest.log`)에 덧붙인다. 1MB가 넘으면 같은 방식으로 돌린다.
pub fn append_rotated(path: &Path, lines: &[String]) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        rotate(path);
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    for l in lines {
        writeln!(f, "{l}")?;
    }
    Ok(())
}

/// 패닉을 `app.log`에 남긴다(창이 없는 실행이라 표준 오류는 보이지 않는다).
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let place = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let what = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let thread = std::thread::current()
            .name()
            .unwrap_or("이름 없음")
            .to_owned();
        error(format!("패닉({thread} 스레드, {place}): {what}"));
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_four_old_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.log");
        for k in 0..6 {
            std::fs::write(&p, format!("{k}")).unwrap();
            rotate(&p);
        }
        let read =
            |n: usize| std::fs::read_to_string(dir.path().join(format!("x.{n}.log"))).unwrap();
        assert_eq!(
            (read(1), read(2), read(3), read(4)),
            ("5".into(), "4".into(), "3".into(), "2".into())
        );
        assert!(!dir.path().join("x.5.log").exists() && !p.exists());
        assert_eq!(now_text().len(), 23);
    }
}
