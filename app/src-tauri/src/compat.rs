//! 데이터 폴더 JSON 파일(settings.json·state.json·templates.json)을 너그럽게 읽는다.
//!
//! exe만 바꿔 올린 판이 앞 판의 파일을 그대로 쓰게: 통째로 해석되지 않으면(값의 형식이 바뀌었거나 모르는 값) 읽을 수
//! 있는 항목은 살리고 못 읽는 항목만 기본값으로 둔다. 원래 파일은 `이름.bak`으로 남긴다(다음 저장이 덮어써도
//! 되찾을 수 있게).

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// 잠깐 잠긴 파일(백신 검사 등)을 없는 파일로 보고 기본값을 저장해 버리지 않게 몇 번 다시 읽는다.
fn read_bytes(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let mut last = None;
    for _ in 0..5 {
        match std::fs::read(path) {
            Ok(b) => return Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("읽기 실패")))
}

/// (값, 경고). 파일이 없으면 기본값·경고 없음.
pub fn read_json<T: DeserializeOwned + Serialize + Default>(path: &Path) -> (T, Option<String>) {
    let bytes = match read_bytes(path) {
        Ok(Some(b)) => b,
        Ok(None) => return (T::default(), None),
        Err(e) => {
            return (
                T::default(),
                Some(format!("{} 읽기 실패, 기본값 사용: {e}", path.display())),
            );
        }
    };
    let err = match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => return (v, None),
        Err(e) => e,
    };
    let backup = path.with_extension(format!(
        "{}.bak",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    let saved = std::fs::write(&backup, &bytes).is_ok();
    let note = if saved {
        format!(", 원본은 {}", backup.display())
    } else {
        String::new()
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(file) => {
            let (v, dropped) = lenient::<T>(file);
            (
                v,
                Some(format!(
                    "{} 일부 해석 실패({err}), 못 읽은 항목 [{}]은 기본값{note}",
                    path.display(),
                    dropped.join(", ")
                )),
            )
        }
        Err(_) => (
            T::default(),
            Some(format!(
                "{} 해석 실패, 기본값 사용: {err}{note}",
                path.display()
            )),
        ),
    }
}

/// 기본값에서 시작해 파일의 항목을 하나씩 넣어 보고, 넣으면 해석되지 않는 항목은 뺀다. (값, 뺀 항목 경로)
pub fn lenient<T: DeserializeOwned + Serialize + Default>(file: Value) -> (T, Vec<String>) {
    let ok = |v: &Value| serde_json::from_value::<T>(v.clone()).is_ok();
    let mut acc = match &file {
        Value::Array(_) => Value::Array(Vec::new()),
        _ => serde_json::to_value(T::default()).unwrap_or(Value::Null),
    };
    let mut dropped = Vec::new();
    if ok(&acc) {
        merge(&mut acc, &mut Vec::new(), &file, &ok, &mut dropped);
    }
    let v = serde_json::from_value(acc).unwrap_or_default();
    (v, dropped)
}

fn slot<'a>(root: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    path.iter().try_fold(root, |v, k| match v {
        Value::Object(m) => m.get_mut(k),
        Value::Array(a) => k.parse::<usize>().ok().and_then(|i| a.get_mut(i)),
        _ => None,
    })
}

fn merge(
    root: &mut Value,
    path: &mut Vec<String>,
    file: &Value,
    ok: &dyn Fn(&Value) -> bool,
    dropped: &mut Vec<String>,
) {
    match file {
        Value::Object(fm) => {
            for (k, v) in fm {
                let Some(Value::Object(m)) = slot(root, path) else {
                    return;
                };
                let prev = m.insert(k.clone(), v.clone());
                if ok(root) {
                    continue;
                }
                let Some(Value::Object(m)) = slot(root, path) else {
                    return;
                };
                match prev.clone() {
                    Some(p) => m.insert(k.clone(), p),
                    None => m.remove(k),
                };
                path.push(k.clone());
                match (&prev, v) {
                    (Some(Value::Object(_)), Value::Object(_)) => {
                        merge(root, path, v, ok, dropped);
                    }
                    (Some(Value::Array(_)), Value::Array(_)) => {
                        if let Some(a) = slot(root, path) {
                            *a = Value::Array(Vec::new());
                        }
                        if ok(root) {
                            merge(root, path, v, ok, dropped);
                        } else if let Some(a) = slot(root, path) {
                            *a = prev.clone().unwrap_or(Value::Null);
                            dropped.push(path.join("."));
                        }
                    }
                    _ => dropped.push(path.join(".")),
                }
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let Some(Value::Array(a)) = slot(root, path) else {
                    return;
                };
                a.push(item.clone());
                if !ok(root) {
                    if let Some(Value::Array(a)) = slot(root, path) {
                        a.pop();
                    }
                    path.push(i.to_string());
                    dropped.push(path.join("."));
                    path.pop();
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    enum Mode {
        #[default]
        Off,
        On,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    struct Inner {
        mode: Mode,
        days: u32,
    }

    impl Default for Inner {
        fn default() -> Self {
            Self {
                mode: Mode::Off,
                days: 30,
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    struct Conf {
        url: String,
        inner: Inner,
        list: Vec<u32>,
        opt: Option<Inner>,
    }

    #[test]
    fn keeps_readable_fields_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.json");
        assert_eq!(
            read_json::<Conf>(&p),
            (Conf::default(), None),
            "없으면 기본값"
        );

        // 모르는 열거 값·형식이 다른 값이 섞여도 나머지는 그대로.
        let body = br#"{"url":"https://mail.example.com","inner":{"mode":"weekly","days":45},
            "list":[1,"x",3],"opt":{"mode":"on","days":"7"},"future":true}"#;
        std::fs::write(&p, body).unwrap();
        let (c, warn) = read_json::<Conf>(&p);
        assert_eq!(
            c,
            Conf {
                url: "https://mail.example.com".into(),
                inner: Inner {
                    mode: Mode::Off,
                    days: 45
                },
                list: vec![1, 3],
                opt: None,
            }
        );
        let warn = warn.unwrap();
        assert!(warn.contains("inner.mode") && warn.contains("list.1") && warn.contains("opt"));
        assert_eq!(
            std::fs::read(dir.path().join("settings.json.bak")).unwrap(),
            body,
            "원본을 남긴다"
        );

        std::fs::write(&p, b"{oops").unwrap();
        let (c, warn) = read_json::<Conf>(&p);
        assert!(
            c == Conf::default() && warn.is_some(),
            "JSON이 아니면 기본값"
        );
    }

    #[test]
    fn arrays_keep_readable_items() {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
        struct T {
            name: String,
        }
        let (v, dropped) =
            lenient::<Vec<T>>(serde_json::json!([{"name":"a"}, {"name": 1}, {"name":"c"}]));
        assert_eq!(v, vec![T { name: "a".into() }, T { name: "c".into() }]);
        assert_eq!(dropped, ["1"]);
    }
}
