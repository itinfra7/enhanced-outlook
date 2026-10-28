use eo_owa::responses;
use eo_owa::{ChangeKind, Folder, FolderId, mail_folder_tree};
use serde_json::json;

#[test]
fn transfer_and_delete_responses() {
    let v = json!({ "Body": { "ResponseMessages": { "Items": [
        { "ResponseClass": "Success", "Items": [ { "__type": "Message:#Exchange",
            "ItemId": { "__type": "ItemId:#Exchange", "Id": "NEW", "ChangeKey": "CK" } } ] },
        { "ResponseClass": "Success", "Items": [] },
        { "ResponseClass": "Error", "ResponseCode": "ErrorItemNotFound", "MessageText": "없음" }
    ] } } });
    let r = responses::parse_transfer("CopyItem", &v).unwrap();
    assert_eq!(r[0].as_ref().unwrap().as_ref().unwrap().id, "NEW");
    assert_eq!(r[1], Ok(None));
    assert_eq!(r[2].as_ref().unwrap_err().code, "ErrorItemNotFound");

    let d = responses::parse_delete(&v).unwrap();
    assert!(d[0].is_ok() && d[1].is_ok());
    assert_eq!(d[2].as_ref().unwrap_err().code, "ErrorItemNotFound");
}

fn ok(item: serde_json::Value) -> serde_json::Value {
    let mut m = json!({ "ResponseClass": "Success", "ResponseCode": "NoError" });
    m.as_object_mut()
        .unwrap()
        .extend(item.as_object().unwrap().clone());
    json!({ "Body": { "ResponseMessages": { "Items": [m] } } })
}

#[test]
fn hierarchy_changes_array() {
    let v = ok(json!({
        "SyncState": "H1",
        "IncludesLastFolderInRange": true,
        "Changes": [{
            "__type": "SyncFolderHierarchyCreateType:#Exchange",
            "ChangeType": "Create",
            "Folder": {
                "__type": "Folder:#Exchange",
                "FolderId": { "__type": "FolderId:#Exchange", "Id": "F1", "ChangeKey": "C" },
                "ParentFolderId": { "__type": "FolderId:#Exchange", "Id": "R" },
                "DisplayName": "받은 편지함", "FolderClass": "IPF.Note",
                "TotalCount": 171, "ChildFolderCount": 0, "UnreadCount": 3
            }
        }]
    }));
    let h = responses::parse_sync_folder_hierarchy(&v).unwrap();
    assert_eq!(h.sync_state, "H1");
    assert_eq!(h.changes.len(), 1);
    let c = &h.changes[0];
    assert_eq!(c.kind, ChangeKind::Create);
    assert_eq!(c.type_name, "SyncFolderHierarchyCreateType");
    let f = c.folder.as_ref().unwrap();
    assert_eq!(f.display_name, "받은 편지함");
    assert_eq!(f.total_count, Some(171));
    assert_eq!(c.folder_id.as_ref().unwrap().id, "F1");
}

fn sync_page(extra: serde_json::Value, changes: usize) -> serde_json::Value {
    let items: Vec<_> = (0..changes)
        .map(|i| {
            json!({ "__type": "SyncFolderItemsCreateType:#Exchange",
                         "Item": { "ItemId": { "Id": format!("I{i}") } } })
        })
        .collect();
    let mut changes_obj = json!({ "Changes": items });
    let mut message = json!({ "SyncState": "S" });
    for (k, v) in extra.as_object().unwrap() {
        if let Some(inner) = k.strip_prefix("Changes.") {
            changes_obj[inner] = v.clone();
        } else {
            message[k] = v.clone();
        }
    }
    message["Changes"] = changes_obj;
    ok(message)
}

/// 실서버(보낸 편지함 733통)에서 첫 512건 뒤 이어받지 않던 버그의 회귀 테스트.
#[test]
fn item_sync_continues_when_page_is_full() {
    // 표시가 없으면 받은 건수로 판단: 꽉 찼으면 계속, 덜 찼으면 끝.
    assert!(
        !responses::parse_sync_folder_items(&sync_page(json!({}), 3), 3)
            .unwrap()
            .includes_last
    );
    assert!(
        responses::parse_sync_folder_items(&sync_page(json!({}), 2), 3)
            .unwrap()
            .includes_last
    );
    // Changes 객체 안의 표시, 문자열 값도 받는다.
    let p = sync_page(json!({ "Changes.IncludesLastItemInRange": false }), 1);
    assert!(
        !responses::parse_sync_folder_items(&p, 3)
            .unwrap()
            .includes_last
    );
    let p = sync_page(json!({ "IncludesLastItemInRange": "False" }), 1);
    assert!(
        !responses::parse_sync_folder_items(&p, 3)
            .unwrap()
            .includes_last
    );
}

#[test]
fn item_sync_nested_changes_all_kinds() {
    let v = ok(json!({
        "SyncState": "S2",
        "IncludesLastItemInRange": false,
        "TotalCount": 171,
        "Changes": { "Changes": [
            { "__type": "SyncFolderItemsCreateType:#Exchange",
              "Item": { "__type": "Message:#Exchange",
                        "ItemId": { "__type": "ItemId:#Exchange", "Id": "I1", "ChangeKey": "K1" },
                        "Subject": "보고서", "IsRead": false, "Size": 1024,
                        "DateTimeReceived": "2026-10-15T14:06:38+09:00",
                        "From": { "Mailbox": { "Name": "김", "EmailAddress": "kim@example.com", "RoutingType": "SMTP" } },
                        "InternetMessageId": "<a@b>", "LastModifiedTime": "2026-10-15T14:06:39+09:00" } },
            { "__type": "SyncFolderItemsDeleteType:#Exchange",
              "ItemId": { "__type": "ItemId:#Exchange", "Id": "I2" } },
            { "__type": "SyncFolderItemsReadFlagType:#Exchange",
              "ItemId": { "__type": "ItemId:#Exchange", "Id": "I3" }, "IsRead": true },
            { "__type": "SomethingNewType:#Exchange" }
        ] }
    }));
    let s = responses::parse_sync_folder_items(&v, 512).unwrap();
    assert!(!s.includes_last);
    assert_eq!(s.total_count, Some(171));
    let kinds: Vec<_> = s.changes.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            ChangeKind::Create,
            ChangeKind::Delete,
            ChangeKind::ReadFlag,
            ChangeKind::Other
        ]
    );
    let item = s.changes[0].item.as_ref().unwrap();
    assert_eq!(item.subject.as_deref(), Some("보고서"));
    assert_eq!(
        item.from
            .as_ref()
            .unwrap()
            .mailbox
            .as_ref()
            .unwrap()
            .email_address
            .as_deref(),
        Some("kim@example.com")
    );
    assert_eq!(
        item.last_modified_time.as_deref(),
        Some("2026-10-15T14:06:39+09:00")
    );
    assert_eq!(
        s.changes[0].item_id.as_ref().unwrap().change_key.as_deref(),
        Some("K1")
    );
    assert_eq!(s.changes[1].item_id.as_ref().unwrap().id, "I2");
    assert_eq!(s.changes[2].is_read, Some(true));
    assert_eq!(s.changes[3].type_name, "SomethingNewType");
}

#[test]
fn get_item_mime_success_and_item_error() {
    let v = json!({ "Body": { "ResponseMessages": { "Items": [
        { "ResponseClass": "Success", "Items": [ { "MimeContent": { "CharacterSet": "UTF-8", "Value": "RnJvbTogYQ0KDQpib2R5" } } ] },
        { "ResponseClass": "Error", "ResponseCode": "ErrorItemNotFound", "MessageText": "없음" }
    ] } } });
    let r = responses::parse_get_item_mime(&v).unwrap();
    assert_eq!(r[0].as_deref().unwrap(), b"From: a\r\n\r\nbody");
    assert_eq!(r[1].as_ref().unwrap_err().code, "ErrorItemNotFound");
}

#[test]
fn single_reports_error_class() {
    let v = json!({ "Body": { "ResponseMessages": { "Items": [
        { "ResponseClass": "Error", "ResponseCode": "ErrorInvalidIdMalformed", "MessageText": "Id is malformed." }
    ] } } });
    let e = responses::single("CreateFolder", &v).unwrap_err();
    assert!(e.to_string().contains("ErrorInvalidIdMalformed"), "{e}");
}

#[test]
fn format_error_detects_fault() {
    let v = json!({ "Body": { "ErrorCode": 0, "ExceptionName": "SerializationException", "FaultMessage": "bad __type" } });
    assert_eq!(
        responses::format_error(&v),
        Some(("SerializationException".into(), "bad __type".into()))
    );
    assert_eq!(
        responses::format_error(&json!({ "Body": { "ResponseMessages": {} } })),
        None
    );
}

#[test]
fn user_config_quota() {
    let v = json!({
        "SessionSettings": {
            "UserEmailAddress": "me@example.com", "UserDisplayName": "나",
            "QuotaUsed": 540016640, "QuotaWarning": 1012924416, "QuotaSend": "1073741824",
            "MaxMessageSizeInKb": 30720
        },
        "AttachmentPolicy": { "BlockedFileTypes": [".exe", ".bat"] }
    });
    let c = responses::parse_user_config(&v).unwrap();
    assert_eq!(c.email_address.as_deref(), Some("me@example.com"));
    assert!(c.other_addresses.is_empty());
    assert_eq!(c.quota_used, Some(540_016_640));
    assert_eq!(c.quota_send, Some(1_073_741_824));
    assert_eq!(c.max_message_size_kb, Some(30720));
    assert_eq!(c.blocked_file_types, [".exe", ".bat"]);
    assert_eq!(c.signature, None, "서명을 설정하지 않았다");
    // 웹메일 서명(0.7.0): HTML이 있으면 그것, 없으면 글을 줄마다 감싼다.
    let mut v = v;
    v["UserOptions"] = json!({ "SignatureHtml": "", "SignatureText": "홍길동\n<인프라팀>", "AutoAddSignature": true });
    let s = responses::parse_user_config(&v).unwrap().signature.unwrap();
    assert_eq!(s.html, "<div>홍길동</div><div>&lt;인프라팀&gt;</div>");
    assert!(s.auto_new && !s.auto_reply);
    // 로그온 주소·UPN이 대표 주소와 다르면 내 주소로 같이 모은다(같은 것·주소 아닌 것은 뺀다).
    v["SessionSettings"]["LogonEmailAddress"] = json!("ME@example.com");
    v["SessionSettings"]["UserPrincipalName"] = json!("me@corp.example.org");
    let c = responses::parse_user_config(&v).unwrap();
    assert_eq!(c.other_addresses, ["me@corp.example.org"]);
    v["SessionSettings"]["UserPrincipalName"] = json!("DOMAIN\\me");
    assert!(
        responses::parse_user_config(&v)
            .unwrap()
            .other_addresses
            .is_empty()
    );
}

fn folder(id: &str, parent: &str, name: &str, class: Option<&str>) -> Folder {
    Folder {
        folder_id: FolderId {
            id: id.into(),
            change_key: None,
        },
        parent_folder_id: Some(FolderId {
            id: parent.into(),
            change_key: None,
        }),
        display_name: name.into(),
        folder_class: class.map(Into::into),
        distinguished_folder_id: None,
        total_count: None,
        child_folder_count: None,
        unread_count: None,
    }
}

#[test]
fn mail_folder_tree_excludes_non_mail_subtrees() {
    let folders = [
        folder("INBOX", "R", "받은 편지함", Some("IPF.Note")),
        folder("PRJ", "INBOX", "프로젝트", None),
        folder("CON", "R", "연락처", Some("IPF.Contact")),
        folder("CON_SUB", "CON", "하위", Some("IPF.Note")),
        folder("SENT", "R", "보낸 편지함", Some("IPF.Note")),
        folder("RSS", "R", "RSS", Some("IPF.Note.OutlookHomepage")),
    ];
    let tree = mail_folder_tree(&folders, "R");
    let got: Vec<_> = tree.iter().map(|n| (n.depth, n.path.as_str())).collect();
    assert_eq!(
        got,
        [
            (0, "받은 편지함"),
            (1, "받은 편지함 / 프로젝트"),
            (0, "보낸 편지함"),
            (0, "RSS"),
        ]
    );
}
