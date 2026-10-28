//! 중복 메일(0.9.0 사용자 요청: 도구 → 중복 메일, 설정 → 자동 정리 "중복 메일 자동 삭제"). 제목·본문·보낸 사람·
//! 받는 사람·참조가 같은 메일. 머리글이 같은 후보는 저장소(`Store::duplicate_candidates`)가 묶고, 여기서는 원문
//! 본문을 비교한다.
//! - 본문: 글(HTML뿐이면 글로 바꾼 것)을 공백을 모두 빼고 비교한다(가져온 경로에 따라 줄바꿈만 다를 수 있다).
//!   첨부 이름도 같아야 한다(글이 없는 그림 메일끼리 같은 메일로 보지 않게).
//! - 원문이 없는 메일은 비교하지 않는다(세어 알려 주고, 원문을 받은 뒤 다시 본다).
//! - 받은 시각은 보지 않는다(사용자 결정: 몇 주 간격으로 온 같은 문구의 알림도 중복).
//! - 묶음마다 받은 시각이 가장 이른 한 통을 남긴다. 시각이 같으면 서버에 있는 것, 그다음 먼저 들어온 것. 늘 한
//!   통은 남는다(사용자 요청).

use std::collections::HashMap;

use eo_store::{DupProgress, DupStage, Store};

use crate::Result;

/// 중복 한 묶음: 남길 메일(가장 이른 것)과 지울 메일들(받은 시각 순).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DupSet {
    /// 묶음 열쇠(머리글 + 본문 비교값의 해시, 판이 바뀌어도 같다). 화면에서 "제외"한 묶음을 저장해 두었다가 다시
    /// 찾아도 제외로 보이게 한다(1.0.5 사용자 요청).
    pub key: String,
    pub keep: i64,
    pub drop: Vec<i64>,
}

/// FNV-1a 64비트(저장하는 열쇠라 판마다 같은 값이어야 한다: 표준 `DefaultHasher`는 보장하지 않는다).
fn fnv64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            // 조각 경계(["ab","c"]와 ["a","bc"]가 같은 값이 되지 않게).
            h ^= 0x1f;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        for b in *p {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DupScan {
    pub sets: Vec<DupSet>,
    /// 살펴본 메일 수.
    pub scanned: usize,
    /// 머리글이 같은 메일 중 원문이 없어 본문을 비교하지 못한 수.
    pub without_body: usize,
}

/// 본문 비교값 캐시(자동 삭제가 몇 분마다 같은 원문을 다시 읽지 않게): 메일 번호 → (원문 해시, 비교값).
pub type BodyCache = HashMap<i64, (String, (usize, u64))>;

/// 본문 비교값: 공백을 뺀 글 + 첨부 이름의 (길이, 해시).
fn body_key(raw: &[u8]) -> Option<(usize, u64)> {
    let x = eo_mime::extract(raw)?;
    let text: String = x.body_text.chars().filter(|c| !c.is_whitespace()).collect();
    let mut names = x.attachment_names;
    names.sort();
    let mut parts: Vec<&[u8]> = vec![text.as_bytes()];
    parts.extend(names.iter().map(|n| n.as_bytes()));
    Some((text.len(), fnv64(&parts)))
}

/// 제목·본문·보낸 사람·받는 사람·참조가 같은 메일 묶음.
pub fn exact_duplicates(
    store: &Store,
    cache: &mut BodyCache,
    progress: DupProgress<'_>,
) -> Result<DupScan> {
    let (candidates, scanned) = store.duplicate_candidates(progress)?;
    let total: usize = candidates
        .iter()
        .flat_map(|(_, rows)| rows)
        .filter(|r| r.eml_hash.is_some())
        .count();
    progress(DupStage::Body, 0, total);
    let mut done = 0;
    let mut without_body = 0;
    let mut sets = Vec::new();
    for (head, group) in candidates {
        let mut by_body: Vec<((usize, u64), Vec<i64>)> = Vec::new();
        for r in group {
            let Some(hash) = r.eml_hash.as_deref() else {
                without_body += 1;
                continue;
            };
            done += 1;
            if done % 200 == 0 {
                progress(DupStage::Body, done, total);
            }
            let key = match cache.get(&r.id) {
                Some((h, k)) if h == hash => Some(*k),
                _ => {
                    let k = store.eml().get(hash).ok().and_then(|raw| body_key(&raw));
                    if let Some(k) = k {
                        cache.insert(r.id, (hash.to_owned(), k));
                    }
                    k
                }
            };
            let Some(key) = key else {
                without_body += 1;
                continue;
            };
            // 후보는 받은 시각·번호 순이라 묶음의 첫 메일이 가장 이르다.
            match by_body.iter_mut().find(|(k, _)| *k == key) {
                Some((_, ids)) => ids.push(r.id),
                None => by_body.push((key, vec![r.id])),
            }
        }
        let head_hash = fnv64(&[head.as_bytes()]);
        sets.extend(by_body.into_iter().filter(|(_, ids)| ids.len() > 1).map(
            |((len, body), ids)| DupSet {
                key: format!("{head_hash:016x}{body:016x}{len:x}"),
                keep: ids[0],
                drop: ids[1..].to_vec(),
            },
        ));
    }
    progress(DupStage::Body, total, total);
    Ok(DupScan {
        sets,
        scanned,
        without_body,
    })
}

#[cfg(test)]
mod tests {
    use eo_mime::backup::{NewEml, build_eml};
    use eo_mime::compose::Person;
    use eo_store::{MessageProps, ServerFolder};

    use super::*;

    fn eml(subject: &str, html: &str, at: i64) -> (Vec<u8>, MessageProps) {
        let person = |e: &str| Person {
            name: String::new(),
            email: e.into(),
        };
        let raw = build_eml(&NewEml {
            from: vec![person("kim@x.com")],
            to: vec![person("me@x.com")],
            subject: subject.into(),
            date: Some(at),
            message_id: format!("<{at}-{}@x>", html.len()),
            html: html.into(),
            ..NewEml::default()
        });
        let p = MessageProps {
            subject: Some(subject.into()),
            from_addr: Some("kim@x.com".into()),
            to_list: Some("me@x.com".into()),
            date_received: at,
            ..MessageProps::default()
        };
        (raw, p)
    }

    #[test]
    fn same_body_keeps_the_earliest_and_one_always_remains() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path()).unwrap();
        let inbox = s
            .transaction(|tx| {
                let f = tx.upsert_folder(&ServerFolder {
                    server_id: "INBOX".into(),
                    server_parent_id: Some("ROOT".into()),
                    name: "받은 편지함".into(),
                    folder_class: Some("IPF.Note".into()),
                })?;
                tx.rebuild_tree("ROOT")?;
                Ok(f)
            })
            .unwrap();
        let add = |sid: &str, subject: &str, html: &str, at: i64, with_eml: bool| {
            let (raw, p) = eml(subject, html, at);
            let id = s
                .transaction(|tx| tx.insert_message(inbox, sid, None, &p))
                .unwrap();
            if with_eml {
                assert!(s.attach_eml(id, sid, &raw).unwrap());
            }
            id
        };
        let body = "<p>회의는 3시입니다.</p><p>자료를 챙겨 오세요.</p>";
        // 같은 메일 셋(늦은 것, 이른 것, 줄바꿈만 다른 것) + 본문이 다른 것 + 원문 없는 것.
        let late = add("A1", "회의", body, 3_000, true);
        let early = add("A2", "회의", body, 1_000, true);
        let spaced = add(
            "A3",
            "회의",
            "<p>회의는 3시입니다.</p>\n\n<p>자료를  챙겨 오세요.</p>",
            2_000,
            true,
        );
        let other = add(
            "A4",
            "회의",
            "<p>회의는 4시로 바뀌었습니다.</p>",
            4_000,
            true,
        );
        let _no_eml = add("A5", "회의", body, 5_000, false);
        // 시각까지 같은 두 통: 먼저 들어온 것을 남긴다.
        let t1 = add("B1", "같은 시각", body, 9_000, true);
        let t2 = add("B2", "같은 시각", body, 9_000, true);
        let mut cache = BodyCache::new();
        let scan = exact_duplicates(&s, &mut cache, &mut |_, _, _| {}).unwrap();
        let sets: Vec<(i64, Vec<i64>)> =
            scan.sets.iter().map(|g| (g.keep, g.drop.clone())).collect();
        assert_eq!(sets, vec![(early, vec![spaced, late]), (t1, vec![t2])]);
        assert_ne!(scan.sets[0].key, scan.sets[1].key, "묶음마다 다른 열쇠");
        assert!(scan.sets.iter().all(|g| !g.drop.contains(&g.keep)));
        assert!(!scan.sets.iter().any(|g| g.drop.contains(&other)));
        assert_eq!((scan.scanned, scan.without_body), (7, 1));
        assert_eq!(cache.len(), 6, "다음에는 원문을 다시 읽지 않는다");
        assert_eq!(
            exact_duplicates(&s, &mut cache, &mut |_, _, _| {}).unwrap(),
            scan
        );
        // 열쇠는 원문을 다시 읽어도(캐시 없이) 같다. 같은 묶음에 사본이 하나 더 와도 같다.
        let again = exact_duplicates(&s, &mut BodyCache::new(), &mut |_, _, _| {}).unwrap();
        assert_eq!(again.sets[0].key, scan.sets[0].key);
        let more = add("A6", "회의", body, 6_000, true);
        let grown = exact_duplicates(&s, &mut cache, &mut |_, _, _| {}).unwrap();
        assert!(grown.sets[0].drop.contains(&more));
        assert_eq!(grown.sets[0].key, scan.sets[0].key);
    }

    #[test]
    fn stored_keys_use_a_fixed_hash() {
        // FNV-1a 64 표준 값(판이 바뀌어도 저장한 열쇠가 그대로 맞아야 한다).
        assert_eq!(fnv64(&[b""]), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(&[b"a"]), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(fnv64(&[b"ab", b"c"]), fnv64(&[b"a", b"bc"]));
    }
}
