//! 요청 JSON 생성. 모든 객체는 `"__type"`이 첫 키여야 한다(WCF JSON 역직렬화기 규칙,
//! `serde_json`의 `preserve_order`로 작성 순서를 유지). 형식은 계획서 5장을 따른다.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};

use crate::types::{Address, DeleteType, FileAttachment, FolderRef, ItemId, ReplyKind};
use crate::{REQUEST_SERVER_VERSION, TIME_ZONE_ID};

/// 목록용 속성 17개(계획서 5.4).
pub const LIST_PROPERTIES: &[&str] = &[
    "Subject",
    "DateTimeReceived",
    "DateTimeSent",
    "IsRead",
    "HasAttachments",
    "Size",
    "From",
    "DisplayTo",
    "DisplayCc",
    "Flag",
    "Importance",
    "ItemClass",
    "InternetMessageId",
    "ConversationId",
    "Preview",
    "ItemLastModifiedTime",
    "IsDraft",
];

/// `SyncFolderItems` 한 번에 받을 최대 변경 수(계획서 5.4).
pub const MAX_CHANGES_RETURNED: u32 = 512;

/// JavaScript `encodeURIComponent`와 같은 규칙: 영숫자와 `-_.!~*'()`만 그대로 둔다.
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// 헤더 전달 방식(`X-OWA-UrlPostData`)에 쓰는 인코딩.
pub fn encode_uri_component(s: &str) -> String {
    utf8_percent_encode(s, URI_COMPONENT).to_string()
}

/// `{Action}JsonRequest:#Exchange`. 액션 이름과 다른 경우(`CreateAttachmentFromLocalFile`)는 직접 지정한다.
pub fn json_request_type(action: &str) -> String {
    format!("{action}JsonRequest:#Exchange")
}

pub fn header() -> Value {
    json!({
        "__type": "JsonRequestHeaders:#Exchange",
        "RequestServerVersion": REQUEST_SERVER_VERSION,
        "TimeZoneContext": {
            "__type": "TimeZoneContext:#Exchange",
            "TimeZoneDefinition": { "__type": "TimeZoneDefinitionType:#Exchange", "Id": TIME_ZONE_ID }
        }
    })
}

/// 요청 공통 머리로 본문을 감싼다(계획서 5.1).
pub fn envelope(request_type: &str, body: Value) -> Value {
    json!({ "__type": request_type, "Header": header(), "Body": body })
}

pub fn folder_id(folder: &FolderRef) -> Value {
    match folder {
        FolderRef::Distinguished(id) => {
            json!({ "__type": "DistinguishedFolderId:#Exchange", "Id": id })
        }
        FolderRef::Id(id) => json!({ "__type": "FolderId:#Exchange", "Id": id }),
    }
}

pub fn item_id(id: &ItemId) -> Value {
    let mut v = json!({ "__type": "ItemId:#Exchange", "Id": id.id });
    if let Some(ck) = &id.change_key {
        v["ChangeKey"] = json!(ck);
    }
    v
}

fn property_uris(names: &[&str]) -> Value {
    names
        .iter()
        .map(|n| json!({ "__type": "PropertyUri:#Exchange", "FieldURI": n }))
        .collect()
}

fn folder_shape() -> Value {
    json!({
        "__type": "FolderResponseShape:#Exchange",
        "BaseShape": "Default",
        "AdditionalProperties": property_uris(&["ParentFolderId", "FolderClass"])
    })
}

fn item_shape(props: &[&str]) -> Value {
    json!({
        "__type": "ItemResponseShape:#Exchange",
        "BaseShape": "IdOnly",
        "AdditionalProperties": property_uris(props)
    })
}

/// `GetFolder`(계획서 5.3).
pub fn get_folder(ids: &[FolderRef]) -> Value {
    json!({
        "__type": "GetFolderRequest:#Exchange",
        "FolderShape": folder_shape(),
        "FolderIds": ids.iter().map(folder_id).collect::<Vec<_>>()
    })
}

/// `FindFolder` 전체 탐색. `Paging: null`이 없으면 역직렬화 오류(계획서 5.1).
pub fn find_folder_deep() -> Value {
    json!({
        "__type": "FindFolderRequest:#Exchange",
        "FolderShape": folder_shape(),
        "ParentFolderIds": [folder_id(&FolderRef::distinguished("msgfolderroot"))],
        "Traversal": "Deep",
        "Paging": null
    })
}

/// `SyncFolderHierarchy`(계획서 5.3). 최초엔 `SyncState`를 생략한다.
pub fn sync_folder_hierarchy(sync_state: Option<&str>) -> Value {
    let mut body = json!({
        "__type": "SyncFolderHierarchyRequest:#Exchange",
        "FolderShape": folder_shape(),
        "SyncFolderId": {
            "__type": "TargetFolderId:#Exchange",
            "BaseFolderId": folder_id(&FolderRef::distinguished("msgfolderroot"))
        }
    });
    if let Some(state) = sync_state {
        body["SyncState"] = json!(state);
    }
    body
}

/// `SyncFolderItems`(계획서 5.4). 최초엔 `SyncState`를 생략한다.
pub fn sync_folder_items(
    folder: &FolderRef,
    sync_state: Option<&str>,
    max_changes: u32,
    props: &[&str],
) -> Value {
    let mut body = json!({
        "__type": "SyncFolderItemsRequest:#Exchange",
        "ItemShape": item_shape(props),
        "SyncFolderId": { "__type": "TargetFolderId:#Exchange", "BaseFolderId": folder_id(folder) }
    });
    if let Some(state) = sync_state {
        body["SyncState"] = json!(state);
    }
    body["MaxChangesReturned"] = json!(max_changes);
    body["SyncScope"] = json!("NormalItems");
    body
}

/// `FindItem` 최신순 페이지(계획서 5.4). 서버 측 `Restriction`은 쓰지 않는다.
pub fn find_item(folder: &FolderRef, offset: u32, max_entries: u32, props: &[&str]) -> Value {
    json!({
        "__type": "FindItemRequest:#Exchange",
        "ItemShape": item_shape(props),
        "ParentFolderIds": [folder_id(folder)],
        "Traversal": "Shallow",
        "Paging": {
            "__type": "IndexedPageView:#Exchange",
            "BasePoint": "Beginning",
            "Offset": offset,
            "MaxEntriesReturned": max_entries
        },
        "ViewFilter": "All",
        "SortOrder": [{
            "__type": "SortResults:#Exchange",
            "Order": "Descending",
            "Path": { "__type": "PropertyUri:#Exchange", "FieldURI": "DateTimeReceived" }
        }]
    })
}

/// `GetItem` + `IncludeMimeContent`: 원문 EML(계획서 5.5).
pub fn get_item_mime(ids: &[ItemId]) -> Value {
    json!({
        "__type": "GetItemRequest:#Exchange",
        "ItemShape": {
            "__type": "ItemResponseShape:#Exchange",
            "BaseShape": "IdOnly",
            "IncludeMimeContent": true
        },
        "ItemIds": ids.iter().map(item_id).collect::<Vec<_>>()
    })
}

/// `GetItem`: 지정한 속성만(원문 없이).
pub fn get_item_properties(ids: &[ItemId], props: &[&str]) -> Value {
    json!({
        "__type": "GetItemRequest:#Exchange",
        "ItemShape": item_shape(props),
        "ItemIds": ids.iter().map(item_id).collect::<Vec<_>>()
    })
}

fn email_address(a: &Address) -> Value {
    json!({
        "__type": "EmailAddress:#Exchange",
        "Name": a.name,
        "EmailAddress": a.email,
        "RoutingType": "SMTP",
        "MailboxType": "Mailbox"
    })
}

fn addresses(list: &[Address]) -> Vec<Value> {
    list.iter().map(email_address).collect()
}

fn body_html(html: &str) -> Value {
    json!({ "__type": "BodyContentType:#Exchange", "BodyType": "HTML", "Value": html })
}

/// MAPI 문자열 확장 속성. 0x1042 = In-Reply-To, 0x1039 = References(계획서 5.8).
pub const PROP_IN_REPLY_TO: &str = "0x1042";
pub const PROP_REFERENCES: &str = "0x1039";

/// 새 메일 초안(계획서 5.7 ①). `MessageDisposition: SaveOnly`라 보내지 않는다.
pub struct NewMail<'a> {
    pub from: &'a Address,
    pub to: &'a [Address],
    pub cc: &'a [Address],
    pub bcc: &'a [Address],
    pub subject: &'a str,
    pub body_html: &'a str,
    /// (PropertyTag, 값) 문자열 확장 속성. 로컬 보관본 회신의 스레드 머리글에 쓴다(5.8).
    pub extended: &'a [(&'a str, String)],
}

pub fn create_new_mail(m: &NewMail<'_>) -> Value {
    let mut item = json!({
        "__type": "Message:#Exchange",
        "Subject": m.subject,
        "Body": body_html(m.body_html),
        "Importance": "Normal",
        "Sensitivity": "Normal",
        "From": { "__type": "SingleRecipientType:#Exchange", "Mailbox": email_address(m.from) },
        "ToRecipients": addresses(m.to),
        "CcRecipients": addresses(m.cc),
        "BccRecipients": addresses(m.bcc),
        "IsDeliveryReceiptRequested": false,
        "IsReadReceiptRequested": false
    });
    if !m.extended.is_empty() {
        item["ExtendedProperty"] = m
            .extended
            .iter()
            .map(|(tag, value)| {
                json!({
                    "__type": "ExtendedPropertyType:#Exchange",
                    "ExtendedFieldURI": {
                        "__type": "ExtendedPropertyUri:#Exchange",
                        "PropertyTag": tag,
                        "PropertyType": "String"
                    },
                    "Value": value
                })
            })
            .collect();
    }
    json!({
        "__type": "CreateItemRequest:#Exchange",
        "Items": [item],
        "ClientSupportsIrm": true,
        "OutboundCharset": "AutoDetect",
        "MessageDisposition": "SaveOnly",
        "ComposeOperation": "newMail"
    })
}

/// 원문(EML)으로 항목 만들기(계획서 5.8 대안, 11장 #9). `folder`에 저장만 한다.
pub fn create_item_from_mime(mime: &[u8], folder: &FolderRef) -> Value {
    json!({
        "__type": "CreateItemRequest:#Exchange",
        "Items": [{
            "__type": "Message:#Exchange",
            "MimeContent": { "CharacterSet": "UTF-8", "Value": STANDARD.encode(mime) }
        }],
        "SavedItemFolderId": { "__type": "TargetFolderId:#Exchange", "BaseFolderId": folder_id(folder) },
        "MessageDisposition": "SaveOnly"
    })
}

/// `CreateAttachmentFromLocalFile` 액션의 요청 형식 이름(액션 이름과 다르다, 계획서 5.7 ②).
pub const CREATE_ATTACHMENT_REQUEST_TYPE: &str = "CreateAttachmentJsonRequest:#Exchange";

/// 첨부 추가(계획서 5.7 ②). 인라인 이미지는 `content_id`를 주고 `is_inline: true`.
pub fn create_attachment(parent: &ItemId, att: &FileAttachment) -> Value {
    let mut a = json!({
        "__type": "FileAttachment:#Exchange",
        "Name": att.name,
        "ContentType": att.content_type,
        "Content": STANDARD.encode(&att.content),
        "Size": att.content.len(),
        "IsInline": att.is_inline,
        "IsContactPhoto": false
    });
    if let Some(cid) = &att.content_id {
        a["ContentId"] = json!(cid);
    }
    json!({
        "__type": "CreateAttachmentRequest:#Exchange",
        "ParentItemId": item_id(parent),
        "Attachments": [a],
        "RequireImageType": false,
        "IncludeContentIdInResponse": att.content_id.is_some(),
        "ClientSupportsIrm": true
    })
}

/// 회신·전체 회신·전달 초안(계획서 5.7 ①). `MessageDisposition: SaveOnly`라 보내지 않는다.
/// `ReferenceItemDocumentId`는 넣지 않는다(11장 #7).
pub struct ReplyDraft<'a> {
    pub kind: ReplyKind,
    /// 원본(ChangeKey 포함).
    pub reference: &'a ItemId,
    pub from: &'a Address,
    pub to: &'a [Address],
    pub cc: &'a [Address],
    pub bcc: &'a [Address],
    pub subject: &'a str,
    pub body_html: &'a str,
}

pub fn create_reply(d: &ReplyDraft<'_>) -> Value {
    json!({
        "__type": "CreateItemRequest:#Exchange",
        "Items": [{
            "__type": d.kind.item_type(),
            "ReferenceItemId": item_id(d.reference),
            "From": { "__type": "SingleRecipientType:#Exchange", "Mailbox": email_address(d.from) },
            "ToRecipients": addresses(d.to),
            "CcRecipients": addresses(d.cc),
            "BccRecipients": addresses(d.bcc),
            "NewBodyContent": body_html(d.body_html),
            "Subject": d.subject,
            "Importance": "Normal",
            "Sensitivity": "Normal",
            "IsDeliveryReceiptRequested": false,
            "IsReadReceiptRequested": false
        }],
        "ClientSupportsIrm": true,
        "OutboundCharset": "AutoDetect",
        "MessageDisposition": "SaveOnly",
        "SubjectPrefix": d.kind.subject_prefix(),
        "ComposeOperation": d.kind.compose_operation()
    })
}

fn set_item_field(field: &str, item: Value) -> Value {
    json!({
        "__type": "SetItemField:#Exchange",
        "Path": { "__type": "PropertyUri:#Exchange", "FieldURI": field },
        "Item": item
    })
}

/// 초안 갱신 + 발송(계획서 5.7 ③). 받는 사람을 `to`로 다시 지정하고 `SendAndSaveCopy`로 보낸다.
pub fn send_draft(draft: &ItemId, to: &[Address], compose_operation: &str) -> Value {
    json!({
        "__type": "UpdateItemRequest:#Exchange",
        "ItemChanges": [{
            "__type": "ItemChange:#Exchange",
            "ItemId": item_id(draft),
            "Updates": [set_item_field(
                "ToRecipients",
                json!({ "__type": "Message:#Exchange", "ToRecipients": addresses(to) })
            )]
        }],
        "ConflictResolution": "AlwaysOverwrite",
        "MessageDisposition": "SendAndSaveCopy",
        "SendCalendarInvitationsOrCancellations": "SendToNone",
        "SuppressReadReceipts": false,
        "ComposeOperation": compose_operation,
        "OutboundCharset": "AutoDetect",
        "SendOnNotFoundError": true,
        "ClientSupportsIrm": true
    })
}

/// 읽음·플래그를 바꾸는 `UpdateItem`(계획서 5.6). 항목마다 같은 필드 하나를 바꾼다.
/// `AlwaysOverwrite`라 ChangeKey 없이 `Id`만 넣는다(이동·복사·삭제와 같은 이유).
fn update_field(field: &str, items: Vec<(&ItemId, Value)>) -> Value {
    json!({
        "__type": "UpdateItemRequest:#Exchange",
        "ItemChanges": items
            .into_iter()
            .map(|(id, item)| json!({
                "__type": "ItemChange:#Exchange",
                "ItemId": { "__type": "ItemId:#Exchange", "Id": id.id },
                "Updates": [set_item_field(field, item)]
            }))
            .collect::<Vec<_>>(),
        "ConflictResolution": "AlwaysOverwrite",
        "MessageDisposition": "SaveOnly",
        "SendCalendarInvitationsOrCancellations": "SendToNone",
        "SuppressReadReceipts": true,
        "ClientSupportsIrm": true,
        "OutboundCharset": "AutoDetect"
    })
}

/// 읽음·안읽음(계획서 5.6, 묶음 최대 10건).
pub fn update_read(items: &[(ItemId, bool)]) -> Value {
    update_field(
        "IsRead",
        items
            .iter()
            .map(|(id, read)| (id, json!({ "__type": "Message:#Exchange", "IsRead": read })))
            .collect(),
    )
}

/// 플래그 상태 이름: 0 없음, 1 플래그, 2 완료.
pub fn flag_status(flag: i64) -> &'static str {
    match flag {
        1 => "Flagged",
        2 => "Complete",
        _ => "NotFlagged",
    }
}

/// 플래그(계획서 5.6). `flag`: 0 없음, 1 플래그, 2 완료.
pub fn update_flag(items: &[(ItemId, i64)]) -> Value {
    update_field(
        "Flag",
        items
            .iter()
            .map(|(id, flag)| {
                (
                    id,
                    json!({
                        "__type": "Message:#Exchange",
                        "Flag": { "__type": "FlagType:#Exchange", "FlagStatus": flag_status(*flag) }
                    }),
                )
            })
            .collect(),
    )
}

/// 초안의 내용(계획서 5.7 ③, 0.7.0 임시 보관함 저장). 회신·전달 초안의 본문도 이것으로 통째로 바꾼다
/// (웹메일과 같은 방식: 서버가 만든 인용 대신 작성 창의 인용문을 쓴다).
pub struct DraftFields<'a> {
    pub to: &'a [Address],
    pub cc: &'a [Address],
    pub bcc: &'a [Address],
    pub subject: &'a str,
    pub body: &'a str,
    /// 일반 텍스트(웹메일 "일반 텍스트로 전환").
    pub plain_text: bool,
    /// 0 낮음, 1 보통, 2 높음(웹메일 "중요도 설정").
    pub importance: i64,
    /// 우편물 종류: 0 보통, 1 개인, 2 비공개, 3 비밀 우편(웹메일 "메시지 옵션").
    pub sensitivity: i64,
    pub read_receipt: bool,
    pub delivery_receipt: bool,
}

pub fn importance_name(importance: i64) -> &'static str {
    match importance {
        0 => "Low",
        2 => "High",
        _ => "Normal",
    }
}

pub fn sensitivity_name(sensitivity: i64) -> &'static str {
    match sensitivity {
        1 => "Personal",
        2 => "Private",
        3 => "Confidential",
        _ => "Normal",
    }
}

/// 초안 갱신(계획서 5.7 ③): 받는 사람·참조·숨은 참조·제목·본문·중요도·우편물 종류·확인 요청을 지정한다.
/// `send`면 `SendAndSaveCopy`(발송), 아니면 `SaveOnly`(임시 보관함 저장).
pub fn update_draft(
    draft: &ItemId,
    f: &DraftFields<'_>,
    compose_operation: &str,
    send: bool,
) -> Value {
    let message = |field: &str, value: Value| {
        let mut item = json!({ "__type": "Message:#Exchange" });
        item[field] = value;
        set_item_field(field, item)
    };
    let body = json!({
        "__type": "BodyContentType:#Exchange",
        "BodyType": if f.plain_text { "Text" } else { "HTML" },
        "Value": f.body
    });
    let mut v = json!({
        "__type": "UpdateItemRequest:#Exchange",
        "ItemChanges": [{
            "__type": "ItemChange:#Exchange",
            "ItemId": item_id(draft),
            "Updates": [
                message("ToRecipients", json!(addresses(f.to))),
                message("CcRecipients", json!(addresses(f.cc))),
                message("BccRecipients", json!(addresses(f.bcc))),
                message("Subject", json!(f.subject)),
                message("Body", body),
                message("Importance", json!(importance_name(f.importance))),
                message("Sensitivity", json!(sensitivity_name(f.sensitivity))),
                message("IsReadReceiptRequested", json!(f.read_receipt)),
                message("IsDeliveryReceiptRequested", json!(f.delivery_receipt))
            ]
        }],
        "ConflictResolution": "AlwaysOverwrite",
        "MessageDisposition": if send { "SendAndSaveCopy" } else { "SaveOnly" },
        "SendCalendarInvitationsOrCancellations": "SendToNone",
        "SuppressReadReceipts": false,
        "ComposeOperation": compose_operation,
        "OutboundCharset": "AutoDetect",
        "ClientSupportsIrm": true
    });
    if send {
        v["SendOnNotFoundError"] = json!(true);
    }
    v
}

/// 첨부 지우기(초안에서 뺀 파일·그림, 0.7.0 임시 보관함 저장).
pub fn delete_attachment(ids: &[String]) -> Value {
    json!({
        "__type": "DeleteAttachmentRequest:#Exchange",
        "AttachmentIds": ids
            .iter()
            .map(|id| json!({ "__type": "AttachmentId:#Exchange", "Id": id }))
            .collect::<Vec<_>>(),
        "ClientSupportsIrm": true
    })
}

/// 주소 자동완성(계획서 5.9).
pub fn find_people(query: &str, max: u32) -> Value {
    json!({
        "__type": "FindPeopleRequest:#Exchange",
        "IndexedPageItemView": {
            "__type": "IndexedPageView:#Exchange",
            "BasePoint": "Beginning",
            "Offset": 0,
            "MaxEntriesReturned": max
        },
        "QueryString": query,
        "PersonaShape": { "__type": "PersonaResponseShape:#Exchange", "BaseShape": "Default" },
        "ShouldResolveOneOffEmailAddress": false,
        "SearchPeopleSuggestionIndex": true
    })
}

/// 이동·복사·삭제 대상 ID. 계획서 5.6 형식대로 `Id`만 넣는다(오래된 ChangeKey로 인한 충돌 방지).
fn bare_item_ids(ids: &[ItemId]) -> Vec<Value> {
    ids.iter()
        .map(|id| json!({ "__type": "ItemId:#Exchange", "Id": id.id }))
        .collect()
}

fn transfer(request_type: &str, ids: &[ItemId], to: &FolderRef) -> Value {
    json!({
        "__type": request_type,
        "ToFolderId": { "__type": "TargetFolderId:#Exchange", "BaseFolderId": folder_id(to) },
        "ItemIds": bare_item_ids(ids)
    })
}

/// `MoveItem`(계획서 5.6).
pub fn move_item(ids: &[ItemId], to: &FolderRef) -> Value {
    transfer("MoveItemRequest:#Exchange", ids, to)
}

/// `CopyItem`(계획서 5.6).
pub fn copy_item(ids: &[ItemId], to: &FolderRef) -> Value {
    transfer("CopyItemRequest:#Exchange", ids, to)
}

/// `DeleteItem`(계획서 5.6).
pub fn delete_item(ids: &[ItemId], delete_type: DeleteType) -> Value {
    json!({
        "__type": "DeleteItemRequest:#Exchange",
        "ItemIds": bare_item_ids(ids),
        "DeleteType": delete_type.as_str(),
        "SuppressReadReceipts": true
    })
}

fn folder_ids(ids: &[FolderRef]) -> Vec<Value> {
    ids.iter().map(folder_id).collect()
}

/// 폴더 이름 바꾸기(0.7.0 폴더 메뉴). 속성 이름 `FolderDisplayName`은 2026-10-23 실서버에서 확인했다(서버에서
/// 다시 읽은 이름이 바뀌어 있었다. 웹메일 화면은 새로 고치기 전까지 예전 이름을 보였다).
pub fn rename_folder(id: &FolderRef, display_name: &str) -> Value {
    json!({
        "__type": "UpdateFolderRequest:#Exchange",
        "FolderChanges": [{
            "__type": "FolderChange:#Exchange",
            "FolderId": folder_id(id),
            "Updates": [{
                "__type": "SetFolderField:#Exchange",
                "Path": { "__type": "PropertyUri:#Exchange", "FieldURI": "FolderDisplayName" },
                "Folder": { "__type": "Folder:#Exchange", "DisplayName": display_name }
            }]
        }]
    })
}

/// 폴더 삭제(0.7.0 폴더 메뉴). 앱은 웹메일처럼 지운 편지함으로(`MoveToDeletedItems`), 시험 정리는 `HardDelete`.
pub fn delete_folder(ids: &[FolderRef], delete_type: DeleteType) -> Value {
    json!({
        "__type": "DeleteFolderRequest:#Exchange",
        "FolderIds": folder_ids(ids),
        "DeleteType": delete_type.as_str()
    })
}

/// 폴더 이동(0.7.0 폴더 메뉴).
pub fn move_folder(ids: &[FolderRef], to: &FolderRef) -> Value {
    json!({
        "__type": "MoveFolderRequest:#Exchange",
        "ToFolderId": { "__type": "TargetFolderId:#Exchange", "BaseFolderId": folder_id(to) },
        "FolderIds": folder_ids(ids)
    })
}

/// 폴더 비우기(0.7.0 폴더 메뉴): 일반 폴더는 지운 편지함으로, 지운 편지함은 `SoftDelete`(복구 가능한 항목으로).
/// 하위 폴더는 그대로 둔다.
pub fn empty_folder(ids: &[FolderRef], delete_type: DeleteType) -> Value {
    json!({
        "__type": "EmptyFolderRequest:#Exchange",
        "FolderIds": folder_ids(ids),
        "DeleteType": delete_type.as_str(),
        "DeleteSubFolders": false,
        "AllowSearchFolder": false
    })
}

/// 모두 읽은 상태로 표시(0.7.0 폴더 메뉴).
pub fn mark_all_read(ids: &[FolderRef], read: bool) -> Value {
    json!({
        "__type": "MarkAllItemsAsReadRequest:#Exchange",
        "ReadFlag": read,
        "SuppressReadReceipts": true,
        "FolderIds": folder_ids(ids)
    })
}

/// `CreateFolder`(미검증, 계획서 11장 #11).
pub fn create_folder(parent: &FolderRef, display_name: &str) -> Value {
    json!({
        "__type": "CreateFolderRequest:#Exchange",
        "ParentFolderId": { "__type": "TargetFolderId:#Exchange", "BaseFolderId": folder_id(parent) },
        "Folders": [{ "__type": "Folder:#Exchange", "DisplayName": display_name, "FolderClass": "IPF.Note" }]
    })
}
