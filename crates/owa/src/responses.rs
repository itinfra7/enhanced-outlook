//! 응답 JSON 해석(계획서 5장). 서버 응답의 모르는 키는 무시한다.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{Error, ItemError, Result};
use crate::types::{
    Address, AttachmentId, ChangeKind, FindItemPage, Folder, FolderChange, FolderId, HierarchySync,
    ItemChange, ItemId, ItemSummary, ItemSync, Signature, UserConfig,
};

/// HTTP 500 등에서 `Body.ErrorCode/ExceptionName/FaultMessage`가 있으면 `(코드, 메시지)`.
pub fn format_error(v: &Value) -> Option<(String, String)> {
    let body = v.get("Body")?;
    let has = |k| body.get(k).is_some_and(|x| !x.is_null());
    if !(has("ErrorCode") || has("ExceptionName") || has("FaultMessage")) {
        return None;
    }
    let code = ["ExceptionName", "ResponseCode", "ErrorCode"]
        .iter()
        .find_map(|k| body.get(*k).filter(|x| !x.is_null()).map(value_text))
        .unwrap_or_default();
    let message = body.get("FaultMessage").map(value_text).unwrap_or_default();
    Some((code, message))
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `Body.ResponseMessages.Items`.
pub fn response_items<'a>(action: &str, v: &'a Value) -> Result<&'a [Value]> {
    v.pointer("/Body/ResponseMessages/Items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| decode(action, "Body.ResponseMessages.Items 없음"))
}

/// `ResponseClass != "Success"`이면 항목 오류.
pub fn item_error(message: &Value) -> Option<ItemError> {
    let class = message.get("ResponseClass").and_then(Value::as_str)?;
    if class == "Success" {
        return None;
    }
    Some(ItemError {
        code: message
            .get("ResponseCode")
            .and_then(Value::as_str)
            .unwrap_or(class)
            .to_owned(),
        message: message
            .get("MessageText")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// 응답 메시지가 하나뿐인 호출: 첫 메시지가 성공이어야 한다.
pub fn single<'a>(action: &str, v: &'a Value) -> Result<&'a Value> {
    let first = response_items(action, v)?
        .first()
        .ok_or_else(|| decode(action, "응답 메시지 0개"))?;
    match item_error(first) {
        Some(e) => Err(Error::Response {
            action: action.to_owned(),
            code: e.code,
            message: e.message,
        }),
        None => Ok(first),
    }
}

fn decode(action: &str, detail: impl Into<String>) -> Error {
    Error::Decode {
        action: action.to_owned(),
        detail: detail.into(),
    }
}

fn from_value<T: DeserializeOwned>(action: &str, v: &Value) -> Result<T> {
    T::deserialize(v).map_err(|e| decode(action, e.to_string()))
}

fn type_name(v: &Value) -> String {
    let t = v.get("__type").and_then(Value::as_str).unwrap_or("");
    t.split(':').next().unwrap_or("").to_owned()
}

/// 변경 종류: `ChangeType`이 있으면 그것을, 없으면 `__type` 이름으로 판단한다.
fn change_kind(v: &Value, type_name: &str) -> ChangeKind {
    if let Some(ct) = v.get("ChangeType").and_then(Value::as_str) {
        match ct {
            "Create" => return ChangeKind::Create,
            "Update" => return ChangeKind::Update,
            "Delete" => return ChangeKind::Delete,
            "ReadFlagChange" | "ReadFlag" => return ChangeKind::ReadFlag,
            _ => {}
        }
    }
    if type_name.contains("ReadFlag") {
        ChangeKind::ReadFlag
    } else if type_name.contains("Delete") {
        ChangeKind::Delete
    } else if type_name.contains("Create") {
        ChangeKind::Create
    } else if type_name.contains("Update") {
        ChangeKind::Update
    } else {
        ChangeKind::Other
    }
}

/// `Changes`가 배열이거나 `{ "Changes": [...] }` 객체인 두 형태를 모두 받는다.
fn changes_array(message: &Value) -> &[Value] {
    match message.get("Changes") {
        Some(Value::Array(a)) => a.as_slice(),
        Some(obj @ Value::Object(_)) => obj
            .get("Changes")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice),
        _ => &[][..],
    }
}

fn opt_i64(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn opt_bool(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

pub fn parse_item_change(action: &str, v: &Value) -> Result<ItemChange> {
    let type_name = type_name(v);
    let kind = change_kind(v, &type_name);
    let item: Option<ItemSummary> = match v.get("Item").or_else(|| v.get("Message")) {
        Some(i) if i.is_object() => Some(from_value(action, i)?),
        _ => None,
    };
    let item_id = match v.get("ItemId") {
        Some(id) if id.is_object() => Some(from_value::<ItemId>(action, id)?),
        _ => item.as_ref().and_then(|i| i.item_id.clone()),
    };
    Ok(ItemChange {
        kind,
        type_name,
        is_read: opt_bool(v.get("IsRead")).or_else(|| item.as_ref().and_then(|i| i.is_read)),
        item,
        item_id,
    })
}

pub fn parse_folder_change(action: &str, v: &Value) -> Result<FolderChange> {
    let type_name = type_name(v);
    let kind = change_kind(v, &type_name);
    let folder: Option<Folder> = match v.get("Folder") {
        Some(f) if f.is_object() => Some(from_value(action, f)?),
        _ => None,
    };
    let folder_id = match v.get("FolderId") {
        Some(id) if id.is_object() => Some(from_value::<FolderId>(action, id)?),
        _ => folder.as_ref().map(|f| f.folder_id.clone()),
    };
    Ok(FolderChange {
        kind,
        type_name,
        folder,
        folder_id,
    })
}

pub fn parse_sync_folder_hierarchy(v: &Value) -> Result<HierarchySync> {
    const A: &str = "SyncFolderHierarchy";
    let m = single(A, v)?;
    Ok(HierarchySync {
        changes: changes_array(m)
            .iter()
            .map(|c| parse_folder_change(A, c))
            .collect::<Result<_>>()?,
        sync_state: m
            .get("SyncState")
            .and_then(Value::as_str)
            .ok_or_else(|| decode(A, "SyncState 없음"))?
            .to_owned(),
        includes_last: opt_bool(m.get("IncludesLastFolderInRange")).unwrap_or(true),
    })
}

/// `max_changes`: 요청한 `MaxChangesReturned`. `IncludesLastItemInRange`가 응답 메시지에도
/// `Changes` 객체에도 없으면, 받은 건수가 이보다 적을 때만 끝으로 본다(꽉 차면 이어받는다).
pub fn parse_sync_folder_items(v: &Value, max_changes: u32) -> Result<ItemSync> {
    const A: &str = "SyncFolderItems";
    let m = single(A, v)?;
    let changes: Vec<ItemChange> = changes_array(m)
        .iter()
        .map(|c| parse_item_change(A, c))
        .collect::<Result<_>>()?;
    let includes_last = opt_bool(m.get("IncludesLastItemInRange"))
        .or_else(|| opt_bool(m.pointer("/Changes/IncludesLastItemInRange")))
        .unwrap_or(changes.len() < max_changes as usize);
    Ok(ItemSync {
        changes,
        sync_state: m
            .get("SyncState")
            .and_then(Value::as_str)
            .ok_or_else(|| decode(A, "SyncState 없음"))?
            .to_owned(),
        includes_last,
        total_count: opt_i64(m.get("TotalCount")),
    })
}

/// `GetFolder`: 요청한 순서대로 폴더 또는 항목 오류.
pub fn parse_get_folder(v: &Value) -> Result<Vec<Result<Folder, ItemError>>> {
    const A: &str = "GetFolder";
    response_items(A, v)?
        .iter()
        .map(|m| match item_error(m) {
            Some(e) => Ok(Err(e)),
            None => {
                let f = m
                    .pointer("/Folders/0")
                    .ok_or_else(|| decode(A, "Folders[0] 없음"))?;
                Ok(Ok(from_value(A, f)?))
            }
        })
        .collect()
}

pub fn parse_find_folder(v: &Value) -> Result<Vec<Folder>> {
    const A: &str = "FindFolder";
    let m = single(A, v)?;
    let folders = m
        .pointer("/RootFolder/Folders")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    folders.iter().map(|f| from_value(A, f)).collect()
}

pub fn parse_find_item(v: &Value) -> Result<FindItemPage> {
    const A: &str = "FindItem";
    let m = single(A, v)?;
    let root = m
        .get("RootFolder")
        .ok_or_else(|| decode(A, "RootFolder 없음"))?;
    let items = root
        .get("Items")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    Ok(FindItemPage {
        items: items
            .iter()
            .map(|i| from_value(A, i))
            .collect::<Result<_>>()?,
        includes_last: opt_bool(root.get("IncludesLastItemInRange")).unwrap_or(true),
        indexed_paging_offset: opt_i64(root.get("IndexedPagingOffset")),
        total_items_in_view: opt_i64(root.get("TotalItemsInView")),
    })
}

/// `MoveItem`/`CopyItem`/`CreateItem`/`UpdateItem`: 요청한 순서대로 새(또는 갱신된) 항목 ID(서버가 주면) 또는 항목 오류.
pub fn parse_transfer(action: &str, v: &Value) -> Result<Vec<Result<Option<ItemId>, ItemError>>> {
    response_items(action, v)?
        .iter()
        .map(|m| match item_error(m) {
            Some(e) => Ok(Err(e)),
            None => match m.pointer("/Items/0/ItemId") {
                Some(id) if id.is_object() => Ok(Ok(Some(from_value(action, id)?))),
                _ => Ok(Ok(None)),
            },
        })
        .collect()
}

/// 첨부 추가(계획서 5.7 ②): 첨부 ID(초안의 새 ChangeKey 포함)와 서버가 돌려준 ContentId.
pub fn parse_create_attachment(v: &Value) -> Result<(AttachmentId, Option<String>)> {
    const A: &str = "CreateAttachment";
    let m = single(A, v)?;
    let a = m
        .pointer("/Attachments/0")
        .ok_or_else(|| decode(A, "Attachments[0] 없음"))?;
    let id = a
        .get("AttachmentId")
        .ok_or_else(|| decode(A, "AttachmentId 없음"))?;
    Ok((
        from_value(A, id)?,
        a.get("ContentId")
            .and_then(Value::as_str)
            .map(str::to_owned),
    ))
}

/// `GetItem`(속성만): 요청한 순서대로 항목 또는 항목 오류.
pub fn parse_get_items(v: &Value) -> Result<Vec<Result<ItemSummary, ItemError>>> {
    const A: &str = "GetItem";
    response_items(A, v)?
        .iter()
        .map(|m| match item_error(m) {
            Some(e) => Ok(Err(e)),
            None => {
                let item = m
                    .pointer("/Items/0")
                    .ok_or_else(|| decode(A, "Items[0] 없음"))?;
                Ok(Ok(from_value(A, item)?))
            }
        })
        .collect()
}

/// `DeleteItem`: 요청한 순서대로 성공 또는 항목 오류.
/// `DeleteAttachment`: 요청한 순서대로 초안의 새 ChangeKey 또는 항목 오류.
pub fn parse_delete_attachment(v: &Value) -> Result<Vec<Result<Option<String>, ItemError>>> {
    Ok(response_items("DeleteAttachment", v)?
        .iter()
        .map(|m| match item_error(m) {
            Some(e) => Err(e),
            None => Ok(m
                .pointer("/RootItemId/RootItemChangeKey")
                .and_then(Value::as_str)
                .map(str::to_owned)),
        })
        .collect())
}

pub fn parse_delete(v: &Value) -> Result<Vec<Result<(), ItemError>>> {
    Ok(response_items("DeleteItem", v)?
        .iter()
        .map(|m| match item_error(m) {
            Some(e) => Err(e),
            None => Ok(()),
        })
        .collect())
}

/// `GetItem` + MIME: 요청한 순서대로 원문 바이트 또는 항목 오류.
pub fn parse_get_item_mime(v: &Value) -> Result<Vec<Result<Vec<u8>, ItemError>>> {
    const A: &str = "GetItem";
    response_items(A, v)?
        .iter()
        .map(|m| {
            if let Some(e) = item_error(m) {
                return Ok(Err(e));
            }
            let b64 = m
                .pointer("/Items/0/MimeContent/Value")
                .and_then(Value::as_str)
                .ok_or_else(|| decode(A, "Items[0].MimeContent.Value 없음"))?;
            STANDARD
                .decode(b64)
                .map(Ok)
                .map_err(|e| decode(A, format!("MIME base64 해석 실패: {e}")))
        })
        .collect()
}

/// `GetOwaUserConfiguration`. `SessionSettings`는 최상위 또는 `Body` 아래에 있다.
/// `FindPeople`(계획서 5.9): `Body.ResultSet[]`의 표시 이름과 주소. 주소는 `EmailAddress`(객체 또는 문자열)
/// 또는 `EmailAddresses[0]`. 주소가 없는 항목(배포 목록 등)은 뺀다.
pub fn parse_find_people(v: &Value) -> Result<Vec<Address>> {
    let set = v
        .pointer("/Body/ResultSet")
        .and_then(Value::as_array)
        .ok_or_else(|| decode("FindPeople", "Body.ResultSet 없음"))?;
    let email_of = |e: &Value| -> Option<String> {
        match e {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => e
                .get("EmailAddress")
                .or_else(|| e.get("Address"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        }
    };
    Ok(set
        .iter()
        .filter_map(|p| {
            let email = p
                .get("EmailAddress")
                .and_then(email_of)
                .or_else(|| p.pointer("/EmailAddresses/0").and_then(email_of))?;
            if !email.contains('@') {
                return None;
            }
            let name = p
                .get("DisplayName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Some(Address { name, email })
        })
        .collect())
}

pub fn parse_user_config(v: &Value) -> Result<UserConfig> {
    let root = if v.get("SessionSettings").is_some() {
        v
    } else {
        v.get("Body").unwrap_or(v)
    };
    let s = root
        .get("SessionSettings")
        .ok_or_else(|| decode("GetOwaUserConfiguration", "SessionSettings 없음"))?;
    let text = |k: &str| s.get(k).and_then(Value::as_str).map(str::to_owned);
    let email_address = text("UserEmailAddress");
    let mut other_addresses: Vec<String> = Vec::new();
    for k in ["LogonEmailAddress", "UserPrincipalName"] {
        if let Some(a) = text(k).map(|a| a.trim().to_owned())
            && a.contains('@')
            && !email_address
                .iter()
                .chain(&other_addresses)
                .any(|b| b.eq_ignore_ascii_case(&a))
        {
            other_addresses.push(a);
        }
    }
    Ok(UserConfig {
        email_address,
        other_addresses,
        display_name: text("UserDisplayName"),
        quota_used: opt_i64(s.get("QuotaUsed")),
        quota_warning: opt_i64(s.get("QuotaWarning")),
        quota_send: opt_i64(s.get("QuotaSend")),
        max_message_size_kb: opt_i64(s.get("MaxMessageSizeInKb")),
        max_recipients: opt_i64(s.get("MaxRecipientsPerMessage"))
            .or_else(|| opt_i64(root.get("MaxRecipientsPerMessage"))),
        blocked_file_types: root
            .pointer("/AttachmentPolicy/BlockedFileTypes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        signature: parse_signature(root.get("UserOptions")),
    })
}

/// 웹메일 서명(`UserOptions.SignatureHtml`, 없으면 `SignatureText`). 비어 있으면 없음.
fn parse_signature(o: Option<&Value>) -> Option<Signature> {
    let o = o?;
    let text = |k: &str| {
        o.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let html = text("SignatureHtml").map(str::to_owned).or_else(|| {
        text("SignatureText").map(|t| {
            t.lines()
                .map(|l| {
                    format!(
                        "<div>{}</div>",
                        l.replace('&', "&amp;")
                            .replace('<', "&lt;")
                            .replace('>', "&gt;")
                    )
                })
                .collect::<String>()
        })
    })?;
    let flag = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
    Some(Signature {
        html,
        auto_new: flag("AutoAddSignature"),
        auto_reply: flag("AutoAddSignatureOnReply"),
    })
}
