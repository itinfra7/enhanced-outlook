//! 가져오기(7단계, 계획서 10장): EML 폴더·유저스크립트 HTML 백업 ZIP → 원래 폴더 구조로 들어오고 검색된다.
//! - "받은 편지함"·"Inbox"는 지금의 받은 편지함에, 백업에서 맨 위로 펼쳐진 하위 폴더("프로젝트")는
//!   같은 이름의 기존 폴더에 합치고, 없는 폴더는 로컬 폴더로 만든다.
//! - 같은 것을 다시 가져오면 모두 중복으로 건너뛴다.
//! - `_files` 안의 첨부(EML 포함)는 메일로 가져오지 않는다.

use std::io::Write;
use std::sync::atomic::AtomicBool;

use eo_search::{SearchIndex, SearchOptions};
use eo_store::{ServerFolder, Store};
use eo_sync::{import_paths, index_pending};

fn eml(subject: &str, id: &str, date: &str) -> String {
    format!(
        "From: =?UTF-8?B?6rmA66+87IiY?= <minsu@x.com>\r\nTo: me@x.com\r\nSubject: {subject}\r\n\
         Date: {date}\r\nMessage-ID: <{id}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\r\n{subject} 본문 가져오기점검\r\n"
    )
}

/// 유저스크립트 backupOne 형식(제목에 괄호: encodeURIComponent는 괄호를 그대로 둔다).
fn backup_html(dir_enc: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"ko\"><head><meta charset=\"utf-8\">\n<title>결산 (최종)</title>\n\
         <style></style></head><body>\n\
         <div class=\"hdr\"><h1>결산 (최종)</h1><table>\
         <tr><td class=\"k\">보낸 사람</td><td>이지영 &lt;jy@x.com&gt;</td></tr>\
         <tr><td class=\"k\">받는 사람</td><td>me@x.com</td></tr>\
         <tr><td class=\"k\">받은 날짜</td><td>2025-12-30 09:00:00</td></tr>\
         <tr><td class=\"k\">보낸 날짜</td><td>2025-12-30 08:59:58</td></tr></table></div>\n\
         <div class=\"mailbody\">\n<p>연말 결산표 복원점검</p><img src=\"{dir_enc}/image001.png\">\n</div>\n\
         <div class=\"atts\"><b>첨부파일 (1)</b><ul><li><a href=\"{dir_enc}/%EA%B2%B0%EC%82%B0.xlsx\" \
         target=\"_blank\">결산.xlsx</a> <span class=\"sz\">(1 KB)</span></li></ul></div>\n</body></html>"
    )
}

#[test]
fn imports_eml_tree_and_html_backup_zip_into_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    let search = SearchIndex::in_memory().unwrap();
    let (inbox, project) = store
        .transaction(|tx| {
            let f = |id: &str, name: &str, parent: &str| ServerFolder {
                server_id: id.into(),
                server_parent_id: Some(parent.into()),
                name: name.into(),
                folder_class: Some("IPF.Note".into()),
            };
            let inbox = tx.upsert_folder(&f("INBOX", "받은 편지함", "ROOT"))?;
            let project = tx.upsert_folder(&f("PRJ", "프로젝트", "INBOX"))?;
            tx.set_well_known("INBOX", "inbox", 0)?;
            tx.rebuild_tree("ROOT")?;
            Ok((inbox, project))
        })
        .unwrap();

    // 1) 이 앱의 내보내기 형식 EML 폴더.
    let src = tmp.path().join("내보내기");
    let put = |rel: &str, data: &[u8]| {
        let p = src.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    };
    put(
        "받은 편지함/2026/10/a.eml",
        eml("첫째", "a@x", "Thu, 15 Oct 2026 16:34:00 +0900").as_bytes(),
    );
    put(
        "Inbox/2026/09/b.eml",
        eml("둘째", "b@x", "Tue, 15 Sep 2026 10:00:00 +0900").as_bytes(),
    );
    put(
        "옛날 편지함/2019/03/c.eml",
        eml("셋째", "c@x", "Fri, 15 Mar 2019 10:00:00 +0900").as_bytes(),
    );
    put("옛날 편지함/2019/03/메모.txt", b"not mail");

    // 2) 유저스크립트 HTML 백업 ZIP(무압축, UTF-8 이름).
    let base = "20251230_085958_결산 (최종)_k3j2";
    let dir_enc = "20251230_085958_%EA%B2%B0%EC%82%B0%20(%EC%B5%9C%EC%A2%85)_k3j2_files";
    let zip_path = tmp.path().join("메일백업_01.zip");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut add = |name: &str, data: &[u8]| {
            z.start_file(name, o).unwrap();
            z.write_all(data).unwrap();
        };
        add(
            &format!("프로젝트/2025/12/{base}.html"),
            backup_html(dir_enc).as_bytes(),
        );
        add(
            &format!("프로젝트/2025/12/{base}_files/image001.png"),
            b"\x89PNG\r\n\x1a\n",
        );
        add(
            &format!("프로젝트/2025/12/{base}_files/결산.xlsx"),
            b"PK-xlsx",
        );
        // 첨부로 들어 있던 EML은 메일이 아니다.
        add(
            &format!("프로젝트/2025/12/{base}_files/fwd.eml"),
            eml("첨부", "z@x", "Fri, 15 Mar 2019 10:00:00 +0900").as_bytes(),
        );
        add(
            "프로젝트/2025/12/index.html",
            b"<html><body>other</body></html>",
        );
        z.finish().unwrap();
    }

    let cancel = AtomicBool::new(false);
    let paths = vec![src.clone(), zip_path.clone()];
    let mut ticks = 0;
    let r = import_paths(&store, &paths, &cancel, |_| ticks += 1).unwrap();
    assert_eq!(
        (
            r.total,
            r.imported,
            r.restored_html,
            r.duplicates,
            r.skipped
        ),
        (5, 4, 1, 0, 1),
        "{r:?}"
    );
    assert!(r.errors[0].contains("index.html"), "{:?}", r.errors);
    assert_eq!(r.created_folders, ["옛날 편지함"]);
    assert!(ticks >= 5);

    let in_inbox = store.list_ids(Some(inbox)).unwrap();
    assert_eq!(in_inbox.len(), 2, "받은 편지함 + Inbox → 받은 편지함");
    let restored = store.list_ids(Some(project)).unwrap();
    assert_eq!(
        restored.len(),
        1,
        "프로젝트(백업에서는 맨 위) → 받은 편지함\\프로젝트"
    );
    let m = store.message(restored[0]).unwrap().unwrap();
    assert_eq!(m.origin.as_deref(), Some("html"));
    assert_eq!(m.server_state, 2);
    assert_eq!(m.props.subject.as_deref(), Some("결산 (최종)"));
    assert_eq!(m.props.from_addr.as_deref(), Some("jy@x.com"));
    assert!(m.props.has_attachments && m.props.is_read);
    let raw = store.eml().get(m.eml_hash.as_deref().unwrap()).unwrap();
    let shown = eo_mime::render(
        &raw,
        &eo_mime::RenderOptions {
            cid_base: "http://app-cid.localhost/9/".into(),
            allow_remote: false,
        },
    )
    .unwrap();
    assert_eq!(shown.inline_images.len(), 1, "본문 이미지 복원");
    assert_eq!(shown.inline_images[0].data, b"\x89PNG\r\n\x1a\n");
    let atts: Vec<_> = shown
        .attachments
        .iter()
        .filter(|a| !a.inline)
        .map(|a| a.name.as_str())
        .collect();
    assert_eq!(atts, ["결산.xlsx"]);

    // 검색된다.
    index_pending(&store, &search, None).unwrap();
    let hits = |q: &str| search.search(q, &SearchOptions::default()).unwrap().ids;
    assert_eq!(hits("가져오기점검").len(), 3);
    assert_eq!(hits("복원점검"), [restored[0] as u64]);
    assert_eq!(hits("첨부:결산"), [restored[0] as u64]);

    // 다시 가져오면 모두 중복.
    let again = import_paths(&store, &paths, &cancel, |_| {}).unwrap();
    assert_eq!((again.imported, again.duplicates), (0, 4), "{again:?}");
    assert!(again.created_folders.is_empty());
}

#[test]
fn single_html_file_finds_its_files_folder_and_cancel_stops() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("store")).unwrap();
    // 로그인해 서버 폴더 목록을 받은 상태(없으면 HCL Domino 아닌 백업은 로그인을 기다린다).
    store
        .transaction(|tx| {
            tx.upsert_folder(&ServerFolder {
                server_id: "INBOX".into(),
                server_parent_id: Some("ROOT".into()),
                name: "받은 편지함".into(),
                folder_class: Some("IPF.Note".into()),
            })?;
            tx.set_well_known("INBOX", "inbox", 0)?;
            tx.rebuild_tree("ROOT")
        })
        .unwrap();
    let base = "20251230_085958_결산 (최종)_k3j2";
    let dir_enc = "20251230_085958_%EA%B2%B0%EC%82%B0%20(%EC%B5%9C%EC%A2%85)_k3j2_files";
    let html = tmp.path().join(format!("{base}.html"));
    std::fs::write(&html, backup_html(dir_enc)).unwrap();
    let files = tmp.path().join(format!("{base}_files"));
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("image001.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(files.join("결산.xlsx"), b"PK-xlsx").unwrap();

    let cancel = AtomicBool::new(true);
    let r = import_paths(&store, std::slice::from_ref(&html), &cancel, |_| {}).unwrap();
    assert!(r.cancelled && r.imported == 0);

    let cancel = AtomicBool::new(false);
    let r = import_paths(&store, &[html], &cancel, |_| {}).unwrap();
    assert_eq!((r.imported, r.restored_html), (1, 1), "{r:?}");
    assert_eq!(r.created_folders, ["가져온 메일"]);
    let folder = store
        .folders()
        .unwrap()
        .into_iter()
        .find(|f| f.name == "가져온 메일")
        .unwrap();
    assert!(folder.server_id.is_none());
    let id = store.list_ids(Some(folder.id)).unwrap()[0];
    let raw = store
        .eml()
        .get(
            store
                .message(id)
                .unwrap()
                .unwrap()
                .eml_hash
                .as_deref()
                .unwrap(),
        )
        .unwrap();
    let x = eo_mime::extract(&raw).unwrap();
    assert_eq!(
        x.attachment_names.len(),
        2,
        "이미지 + 첨부: {:?}",
        x.attachment_names
    );
}
