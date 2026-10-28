//! 계약 테스트: 요청 JSON이 계획서 5장 예시(와 유저스크립트)와 같은지, `__type`이 첫 키인지.

use eo_owa::requests::{self, LIST_PROPERTIES};
use eo_owa::{Address, DeleteType, FolderRef, ItemId, ReplyKind};
use serde_json::{Value, json};

fn me() -> Address {
    Address {
        name: "나".into(),
        email: "me@example.com".into(),
    }
}

fn email(a: &Address) -> Value {
    json!({ "__type": "EmailAddress:#Exchange", "Name": a.name, "EmailAddress": a.email,
            "RoutingType": "SMTP", "MailboxType": "Mailbox" })
}

#[test]
fn reply_all_draft_matches_plan_5_7() {
    let original = ItemId {
        id: "O".into(),
        change_key: Some("OCK".into()),
    };
    let me = me();
    let draft = requests::ReplyDraft {
        kind: ReplyKind::ReplyAll,
        reference: &original,
        from: &me,
        to: std::slice::from_ref(&me),
        cc: &[],
        bcc: &[],
        subject: "RE: t",
        body_html: "<p>x</p>",
    };
    check(
        requests::create_reply(&draft),
        json!({
            "__type": "CreateItemRequest:#Exchange",
            "Items": [{
                "__type": "ReplyAllToItem:#Exchange",
                "ReferenceItemId": { "__type": "ItemId:#Exchange", "Id": "O", "ChangeKey": "OCK" },
                "From": { "__type": "SingleRecipientType:#Exchange", "Mailbox": email(&me) },
                "ToRecipients": [email(&me)], "CcRecipients": [], "BccRecipients": [],
                "NewBodyContent": { "__type": "BodyContentType:#Exchange", "BodyType": "HTML", "Value": "<p>x</p>" },
                "Subject": "RE: t",
                "Importance": "Normal", "Sensitivity": "Normal",
                "IsDeliveryReceiptRequested": false, "IsReadReceiptRequested": false
            }],
            "ClientSupportsIrm": true,
            "OutboundCharset": "AutoDetect",
            "MessageDisposition": "SaveOnly",
            "SubjectPrefix": "RE: ",
            "ComposeOperation": "replyAll"
        }),
    );
}

#[test]
fn new_mail_with_thread_headers_matches_plan_5_7_and_5_8() {
    let me = me();
    let extended = [
        (requests::PROP_IN_REPLY_TO, "<o@x>".to_owned()),
        (requests::PROP_REFERENCES, "<r@x> <o@x>".to_owned()),
    ];
    let mail = requests::NewMail {
        from: &me,
        to: std::slice::from_ref(&me),
        cc: &[],
        bcc: &[],
        subject: "RE: t",
        body_html: "<p>x</p>",
        extended: &extended,
    };
    let prop = |tag: &str, value: &str| {
        json!({ "__type": "ExtendedPropertyType:#Exchange",
                "ExtendedFieldURI": { "__type": "ExtendedPropertyUri:#Exchange", "PropertyTag": tag, "PropertyType": "String" },
                "Value": value })
    };
    check(
        requests::create_new_mail(&mail),
        json!({
            "__type": "CreateItemRequest:#Exchange",
            "Items": [{
                "__type": "Message:#Exchange",
                "Subject": "RE: t",
                "Body": { "__type": "BodyContentType:#Exchange", "BodyType": "HTML", "Value": "<p>x</p>" },
                "Importance": "Normal", "Sensitivity": "Normal",
                "From": { "__type": "SingleRecipientType:#Exchange", "Mailbox": email(&me) },
                "ToRecipients": [email(&me)], "CcRecipients": [], "BccRecipients": [],
                "IsDeliveryReceiptRequested": false, "IsReadReceiptRequested": false,
                "ExtendedProperty": [prop("0x1042", "<o@x>"), prop("0x1039", "<r@x> <o@x>")]
            }],
            "ClientSupportsIrm": true,
            "OutboundCharset": "AutoDetect",
            "MessageDisposition": "SaveOnly",
            "ComposeOperation": "newMail"
        }),
    );
}

#[test]
fn inline_attachment_matches_plan_5_7() {
    let draft = ItemId {
        id: "D".into(),
        change_key: Some("DCK".into()),
    };
    let att = eo_owa::FileAttachment {
        name: "p.png".into(),
        content_type: "image/png".into(),
        content: vec![1, 2, 3],
        is_inline: true,
        content_id: Some("cid1".into()),
    };
    check(
        requests::create_attachment(&draft, &att),
        json!({
            "__type": "CreateAttachmentRequest:#Exchange",
            "ParentItemId": { "__type": "ItemId:#Exchange", "Id": "D", "ChangeKey": "DCK" },
            "Attachments": [{ "__type": "FileAttachment:#Exchange", "Name": "p.png", "ContentType": "image/png",
                              "Content": "AQID", "Size": 3, "IsInline": true, "IsContactPhoto": false,
                              "ContentId": "cid1" }],
            "RequireImageType": false,
            "IncludeContentIdInResponse": true,
            "ClientSupportsIrm": true
        }),
    );
    assert_eq!(
        requests::CREATE_ATTACHMENT_REQUEST_TYPE,
        "CreateAttachmentJsonRequest:#Exchange"
    );
}

#[test]
fn item_from_mime_shape() {
    check(
        requests::create_item_from_mime(
            b"Subject: x\r\n\r\nb",
            &FolderRef::distinguished("drafts"),
        ),
        json!({
            "__type": "CreateItemRequest:#Exchange",
            "Items": [{ "__type": "Message:#Exchange",
                        "MimeContent": { "CharacterSet": "UTF-8", "Value": "U3ViamVjdDogeA0KDQpi" } }],
            "SavedItemFolderId": { "__type": "TargetFolderId:#Exchange",
                                   "BaseFolderId": { "__type": "DistinguishedFolderId:#Exchange", "Id": "drafts" } },
            "MessageDisposition": "SaveOnly"
        }),
    );
}

#[test]
fn send_draft_matches_plan_5_7() {
    let draft = ItemId {
        id: "D".into(),
        change_key: Some("DCK".into()),
    };
    check(
        requests::send_draft(&draft, &[me()], "replyAll"),
        json!({
            "__type": "UpdateItemRequest:#Exchange",
            "ItemChanges": [{
                "__type": "ItemChange:#Exchange",
                "ItemId": { "__type": "ItemId:#Exchange", "Id": "D", "ChangeKey": "DCK" },
                "Updates": [{
                    "__type": "SetItemField:#Exchange",
                    "Path": { "__type": "PropertyUri:#Exchange", "FieldURI": "ToRecipients" },
                    "Item": { "__type": "Message:#Exchange", "ToRecipients": [email(&me())] }
                }]
            }],
            "ConflictResolution": "AlwaysOverwrite",
            "MessageDisposition": "SendAndSaveCopy",
            "SendCalendarInvitationsOrCancellations": "SendToNone",
            "SuppressReadReceipts": false,
            "ComposeOperation": "replyAll",
            "OutboundCharset": "AutoDetect",
            "SendOnNotFoundError": true,
            "ClientSupportsIrm": true
        }),
    );
}

#[test]
fn update_draft_sets_all_fields_per_plan_5_7() {
    let draft = ItemId {
        id: "D".into(),
        change_key: Some("DCK".into()),
    };
    let field = |name: &str, value: Value| {
        let mut item = json!({ "__type": "Message:#Exchange" });
        item[name] = value;
        json!({ "__type": "SetItemField:#Exchange", "Path": prop(name), "Item": item })
    };
    let fields = requests::DraftFields {
        to: &[me()],
        cc: &[],
        bcc: &[me()],
        subject: "RE: 제목",
        body: "<p>본문</p>",
        plain_text: false,
        importance: 2,
        sensitivity: 3,
        read_receipt: true,
        delivery_receipt: false,
    };
    let mut want = json!({
        "__type": "UpdateItemRequest:#Exchange",
        "ItemChanges": [{
            "__type": "ItemChange:#Exchange",
            "ItemId": { "__type": "ItemId:#Exchange", "Id": "D", "ChangeKey": "DCK" },
            "Updates": [
                field("ToRecipients", json!([email(&me())])),
                field("CcRecipients", json!([])),
                field("BccRecipients", json!([email(&me())])),
                field("Subject", json!("RE: 제목")),
                field("Body", json!({ "__type": "BodyContentType:#Exchange", "BodyType": "HTML", "Value": "<p>본문</p>" })),
                field("Importance", json!("High")),
                field("Sensitivity", json!("Confidential")),
                field("IsReadReceiptRequested", json!(true)),
                field("IsDeliveryReceiptRequested", json!(false))
            ]
        }],
        "ConflictResolution": "AlwaysOverwrite",
        "MessageDisposition": "SendAndSaveCopy",
        "SendCalendarInvitationsOrCancellations": "SendToNone",
        "SuppressReadReceipts": false,
        "ComposeOperation": "reply",
        "OutboundCharset": "AutoDetect",
        "SendOnNotFoundError": true,
        "ClientSupportsIrm": true
    });
    check(
        requests::update_draft(&draft, &fields, "reply", true),
        want.clone(),
    );
    // 임시 보관함 저장(0.7.0): 같은 내용을 보내지 않고 저장만.
    want["MessageDisposition"] = json!("SaveOnly");
    want.as_object_mut().unwrap().remove("SendOnNotFoundError");
    check(
        requests::update_draft(&draft, &fields, "reply", false),
        want,
    );
}

#[test]
fn folder_menu_requests() {
    let f = FolderRef::Id("F1".into());
    let fid = json!({ "__type": "FolderId:#Exchange", "Id": "F1" });
    check(
        requests::rename_folder(&f, "새 이름"),
        json!({
            "__type": "UpdateFolderRequest:#Exchange",
            "FolderChanges": [{
                "__type": "FolderChange:#Exchange",
                "FolderId": fid,
                "Updates": [{
                    "__type": "SetFolderField:#Exchange",
                    "Path": { "__type": "PropertyUri:#Exchange", "FieldURI": "FolderDisplayName" },
                    "Folder": { "__type": "Folder:#Exchange", "DisplayName": "새 이름" }
                }]
            }]
        }),
    );
    check(
        requests::delete_folder(
            std::slice::from_ref(&f),
            eo_owa::DeleteType::MoveToDeletedItems,
        ),
        json!({ "__type": "DeleteFolderRequest:#Exchange", "FolderIds": [fid], "DeleteType": "MoveToDeletedItems" }),
    );
    check(
        requests::move_folder(std::slice::from_ref(&f), &FolderRef::distinguished("inbox")),
        json!({
            "__type": "MoveFolderRequest:#Exchange",
            "ToFolderId": { "__type": "TargetFolderId:#Exchange",
                "BaseFolderId": { "__type": "DistinguishedFolderId:#Exchange", "Id": "inbox" } },
            "FolderIds": [fid]
        }),
    );
    check(
        requests::empty_folder(std::slice::from_ref(&f), eo_owa::DeleteType::SoftDelete),
        json!({ "__type": "EmptyFolderRequest:#Exchange", "FolderIds": [fid], "DeleteType": "SoftDelete",
            "DeleteSubFolders": false, "AllowSearchFolder": false }),
    );
    check(
        requests::mark_all_read(std::slice::from_ref(&f), true),
        json!({ "__type": "MarkAllItemsAsReadRequest:#Exchange", "ReadFlag": true,
            "SuppressReadReceipts": true, "FolderIds": [fid] }),
    );
}

#[test]
fn delete_attachment_lists_ids() {
    check(
        requests::delete_attachment(&["A1".into(), "A2".into()]),
        json!({
            "__type": "DeleteAttachmentRequest:#Exchange",
            "AttachmentIds": [
                { "__type": "AttachmentId:#Exchange", "Id": "A1" },
                { "__type": "AttachmentId:#Exchange", "Id": "A2" }
            ],
            "ClientSupportsIrm": true
        }),
    );
}

#[test]
fn find_people_matches_plan_5_9_and_parses_results() {
    check(
        requests::find_people("kim", 10),
        json!({
            "__type": "FindPeopleRequest:#Exchange",
            "IndexedPageItemView": { "__type": "IndexedPageView:#Exchange", "BasePoint": "Beginning", "Offset": 0, "MaxEntriesReturned": 10 },
            "QueryString": "kim",
            "PersonaShape": { "__type": "PersonaResponseShape:#Exchange", "BaseShape": "Default" },
            "ShouldResolveOneOffEmailAddress": false,
            "SearchPeopleSuggestionIndex": true
        }),
    );
    let r = eo_owa::responses::parse_find_people(&json!({ "Body": { "ResultSet": [
        { "DisplayName": "김민수", "EmailAddress": { "Name": "김민수", "EmailAddress": "minsu.kim@x.com" } },
        { "DisplayName": "이지영", "EmailAddresses": [{ "EmailAddress": "jy@x.com" }] },
        { "DisplayName": "배포 목록" }
    ] } }))
    .unwrap();
    assert_eq!(
        r.iter()
            .map(|a| (a.name.as_str(), a.email.as_str()))
            .collect::<Vec<_>>(),
        [("김민수", "minsu.kim@x.com"), ("이지영", "jy@x.com")]
    );
}

fn ids() -> [ItemId; 2] {
    [
        ItemId {
            id: "I1".into(),
            change_key: Some("old".into()),
        },
        ItemId {
            id: "I2".into(),
            change_key: None,
        },
    ]
}

#[test]
fn move_and_copy_match_plan_5_6() {
    for (request_type, v) in [
        (
            "MoveItemRequest:#Exchange",
            requests::move_item(&ids(), &FolderRef::Id("F2".into())),
        ),
        (
            "CopyItemRequest:#Exchange",
            requests::copy_item(&ids(), &FolderRef::Id("F2".into())),
        ),
    ] {
        check(
            v,
            json!({
                "__type": request_type,
                "ToFolderId": { "__type": "TargetFolderId:#Exchange",
                                "BaseFolderId": { "__type": "FolderId:#Exchange", "Id": "F2" } },
                "ItemIds": [ { "__type": "ItemId:#Exchange", "Id": "I1" },
                             { "__type": "ItemId:#Exchange", "Id": "I2" } ]
            }),
        );
    }
}

#[test]
fn delete_matches_plan_5_6() {
    for (t, name) in [
        (DeleteType::MoveToDeletedItems, "MoveToDeletedItems"),
        (DeleteType::SoftDelete, "SoftDelete"),
        (DeleteType::HardDelete, "HardDelete"),
    ] {
        check(
            requests::delete_item(&ids(), t),
            json!({
                "__type": "DeleteItemRequest:#Exchange",
                "ItemIds": [ { "__type": "ItemId:#Exchange", "Id": "I1" },
                             { "__type": "ItemId:#Exchange", "Id": "I2" } ],
                "DeleteType": name,
                "SuppressReadReceipts": true
            }),
        );
    }
}

fn update_body(changes: Value) -> Value {
    json!({
        "__type": "UpdateItemRequest:#Exchange",
        "ItemChanges": changes,
        "ConflictResolution": "AlwaysOverwrite",
        "MessageDisposition": "SaveOnly",
        "SendCalendarInvitationsOrCancellations": "SendToNone",
        "SuppressReadReceipts": true,
        "ClientSupportsIrm": true,
        "OutboundCharset": "AutoDetect"
    })
}

#[test]
fn read_flag_matches_plan_5_6() {
    let [a, b] = ids();
    check(
        requests::update_read(&[(a, true), (b, false)]),
        update_body(json!([
            { "__type": "ItemChange:#Exchange",
              "ItemId": { "__type": "ItemId:#Exchange", "Id": "I1" },
              "Updates": [ { "__type": "SetItemField:#Exchange",
                             "Path": prop("IsRead"),
                             "Item": { "__type": "Message:#Exchange", "IsRead": true } } ] },
            { "__type": "ItemChange:#Exchange",
              "ItemId": { "__type": "ItemId:#Exchange", "Id": "I2" },
              "Updates": [ { "__type": "SetItemField:#Exchange",
                             "Path": prop("IsRead"),
                             "Item": { "__type": "Message:#Exchange", "IsRead": false } } ] }
        ])),
    );
}

#[test]
fn flag_matches_plan_5_6() {
    let [a, b] = ids();
    let flag = |status: &str| {
        json!({ "__type": "SetItemField:#Exchange",
                "Path": prop("Flag"),
                "Item": { "__type": "Message:#Exchange",
                          "Flag": { "__type": "FlagType:#Exchange", "FlagStatus": status } } })
    };
    check(
        requests::update_flag(&[(a.clone(), 1), (b, 0), (a, 2)]),
        update_body(json!([
            { "__type": "ItemChange:#Exchange", "ItemId": { "__type": "ItemId:#Exchange", "Id": "I1" },
              "Updates": [flag("Flagged")] },
            { "__type": "ItemChange:#Exchange", "ItemId": { "__type": "ItemId:#Exchange", "Id": "I2" },
              "Updates": [flag("NotFlagged")] },
            { "__type": "ItemChange:#Exchange", "ItemId": { "__type": "ItemId:#Exchange", "Id": "I1" },
              "Updates": [flag("Complete")] }
        ])),
    );
}

/// WCF JSON 역직렬화기는 `__type`이 객체의 첫 키여야 한다.
fn assert_type_first(v: &Value, path: &str) {
    match v {
        Value::Object(map) => {
            if map.contains_key("__type") {
                assert_eq!(
                    map.keys().next().map(String::as_str),
                    Some("__type"),
                    "{path}: __type이 첫 키가 아님"
                );
            }
            for (k, child) in map {
                assert_type_first(child, &format!("{path}.{k}"));
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                assert_type_first(child, &format!("{path}[{i}]"));
            }
        }
        _ => {}
    }
}

fn check(actual: Value, expected: Value) {
    assert_eq!(actual, expected);
    assert_type_first(&actual, "$");
}

fn prop(name: &str) -> Value {
    json!({ "__type": "PropertyUri:#Exchange", "FieldURI": name })
}

#[test]
fn envelope_header_matches_plan_5_1() {
    let body = json!({ "__type": "GetFolderRequest:#Exchange" });
    check(
        requests::envelope(&requests::json_request_type("GetFolder"), body),
        json!({
            "__type": "GetFolderJsonRequest:#Exchange",
            "Header": {
                "__type": "JsonRequestHeaders:#Exchange",
                "RequestServerVersion": "Exchange2016",
                "TimeZoneContext": {
                    "__type": "TimeZoneContext:#Exchange",
                    "TimeZoneDefinition": { "__type": "TimeZoneDefinitionType:#Exchange", "Id": "Korea Standard Time" }
                }
            },
            "Body": { "__type": "GetFolderRequest:#Exchange" }
        }),
    );
}

#[test]
fn sync_folder_hierarchy_matches_plan_5_3() {
    let shape = json!({
        "__type": "FolderResponseShape:#Exchange", "BaseShape": "Default",
        "AdditionalProperties": [prop("ParentFolderId"), prop("FolderClass")]
    });
    let target = json!({
        "__type": "TargetFolderId:#Exchange",
        "BaseFolderId": { "__type": "DistinguishedFolderId:#Exchange", "Id": "msgfolderroot" }
    });
    check(
        requests::sync_folder_hierarchy(None),
        json!({ "__type": "SyncFolderHierarchyRequest:#Exchange", "FolderShape": shape, "SyncFolderId": target }),
    );
    check(
        requests::sync_folder_hierarchy(Some("S1")),
        json!({
            "__type": "SyncFolderHierarchyRequest:#Exchange",
            "FolderShape": shape, "SyncFolderId": target, "SyncState": "S1"
        }),
    );
}

#[test]
fn find_folder_matches_userscript() {
    check(
        requests::find_folder_deep(),
        json!({
            "__type": "FindFolderRequest:#Exchange",
            "FolderShape": {
                "__type": "FolderResponseShape:#Exchange", "BaseShape": "Default",
                "AdditionalProperties": [prop("ParentFolderId"), prop("FolderClass")]
            },
            "ParentFolderIds": [{ "__type": "DistinguishedFolderId:#Exchange", "Id": "msgfolderroot" }],
            "Traversal": "Deep",
            "Paging": null
        }),
    );
}

#[test]
fn get_folder_accepts_distinguished_and_server_ids() {
    let v = requests::get_folder(&[
        FolderRef::distinguished("inbox"),
        FolderRef::Id("F1".into()),
    ]);
    assert_eq!(
        v["FolderIds"],
        json!([
            { "__type": "DistinguishedFolderId:#Exchange", "Id": "inbox" },
            { "__type": "FolderId:#Exchange", "Id": "F1" }
        ])
    );
    assert_type_first(&v, "$");
}

#[test]
fn sync_folder_items_matches_plan_5_4() {
    assert_eq!(LIST_PROPERTIES.len(), 17);
    check(
        requests::sync_folder_items(
            &FolderRef::Id("F1".into()),
            Some("S1"),
            512,
            LIST_PROPERTIES,
        ),
        json!({
            "__type": "SyncFolderItemsRequest:#Exchange",
            "ItemShape": {
                "__type": "ItemResponseShape:#Exchange", "BaseShape": "IdOnly",
                "AdditionalProperties": LIST_PROPERTIES.iter().map(|p| prop(p)).collect::<Vec<_>>()
            },
            "SyncFolderId": {
                "__type": "TargetFolderId:#Exchange",
                "BaseFolderId": { "__type": "FolderId:#Exchange", "Id": "F1" }
            },
            "SyncState": "S1",
            "MaxChangesReturned": 512,
            "SyncScope": "NormalItems"
        }),
    );
    let first = requests::sync_folder_items(&FolderRef::Id("F1".into()), None, 512, &[]);
    assert!(
        first.get("SyncState").is_none(),
        "최초 동기화엔 SyncState 생략"
    );
}

#[test]
fn find_item_matches_plan_5_4() {
    check(
        requests::find_item(&FolderRef::Id("F1".into()), 0, 200, &["Subject"]),
        json!({
            "__type": "FindItemRequest:#Exchange",
            "ItemShape": { "__type": "ItemResponseShape:#Exchange", "BaseShape": "IdOnly", "AdditionalProperties": [prop("Subject")] },
            "ParentFolderIds": [ { "__type": "FolderId:#Exchange", "Id": "F1" } ],
            "Traversal": "Shallow",
            "Paging": { "__type": "IndexedPageView:#Exchange", "BasePoint": "Beginning", "Offset": 0, "MaxEntriesReturned": 200 },
            "ViewFilter": "All",
            "SortOrder": [ { "__type": "SortResults:#Exchange", "Order": "Descending",
                             "Path": { "__type": "PropertyUri:#Exchange", "FieldURI": "DateTimeReceived" } } ]
        }),
    );
}

#[test]
fn get_item_mime_matches_plan_5_5() {
    let ids = [
        ItemId {
            id: "I1".into(),
            change_key: None,
        },
        ItemId {
            id: "I2".into(),
            change_key: Some("CK".into()),
        },
    ];
    check(
        requests::get_item_mime(&ids),
        json!({
            "__type": "GetItemRequest:#Exchange",
            "ItemShape": { "__type": "ItemResponseShape:#Exchange", "BaseShape": "IdOnly", "IncludeMimeContent": true },
            "ItemIds": [
                { "__type": "ItemId:#Exchange", "Id": "I1" },
                { "__type": "ItemId:#Exchange", "Id": "I2", "ChangeKey": "CK" }
            ]
        }),
    );
}

#[test]
fn create_folder_shape() {
    check(
        requests::create_folder(&FolderRef::Id("P".into()), "새 폴더"),
        json!({
            "__type": "CreateFolderRequest:#Exchange",
            "ParentFolderId": { "__type": "TargetFolderId:#Exchange", "BaseFolderId": { "__type": "FolderId:#Exchange", "Id": "P" } },
            "Folders": [ { "__type": "Folder:#Exchange", "DisplayName": "새 폴더", "FolderClass": "IPF.Note" } ]
        }),
    );
}

#[test]
fn serialized_request_starts_with_type() {
    let text = requests::envelope(
        "X:#Exchange",
        requests::get_folder(&[FolderRef::distinguished("inbox")]),
    )
    .to_string();
    assert!(
        text.starts_with(r#"{"__type":"X:#Exchange","Header":{"__type":"#),
        "{text}"
    );
}

#[test]
fn encode_uri_component_matches_javascript() {
    assert_eq!(
        requests::encode_uri_component("a b/가"),
        "a%20b%2F%EA%B0%80"
    );
    assert_eq!(requests::encode_uri_component("-_.!~*'()"), "-_.!~*'()");
    assert_eq!(
        requests::encode_uri_component(r#"{"a":1}"#),
        "%7B%22a%22%3A1%7D"
    );
}
