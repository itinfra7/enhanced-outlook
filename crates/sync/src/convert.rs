//! 서버 목록 속성(`owa::ItemSummary`) → 저장소 속성(`store::MessageProps`).

use eo_owa::ItemSummary;
use eo_store::{MessageProps, parse_rfc3339_ms};

pub fn props(item: &ItemSummary) -> MessageProps {
    let date_sent = item.date_time_sent.as_deref().and_then(parse_rfc3339_ms);
    let mailbox = item.from.as_ref().and_then(|r| r.mailbox.as_ref());
    MessageProps {
        internet_msg_id: item.internet_message_id.clone(),
        conversation_id: item.conversation_id.as_ref().map(|c| c.id.clone()),
        subject: item.subject.clone(),
        from_name: mailbox.and_then(|m| m.name.clone()),
        from_addr: mailbox.and_then(|m| m.email_address.clone()),
        to_list: item.display_to.clone(),
        cc_list: item.display_cc.clone(),
        date_received: item
            .date_time_received
            .as_deref()
            .and_then(parse_rfc3339_ms)
            .or(date_sent)
            .unwrap_or(0),
        date_sent,
        size: item.size,
        has_attachments: item.has_attachments.unwrap_or(false),
        is_read: item.is_read.unwrap_or(false),
        flag: match item.flag.as_ref().and_then(|f| f.flag_status.as_deref()) {
            Some("Flagged") => 1,
            Some("Complete") => 2,
            _ => 0,
        },
        importance: match item.importance.as_deref() {
            Some("Low") => 0,
            Some("High") => 2,
            _ => 1,
        },
        is_draft: item.is_draft.unwrap_or(false),
        item_class: item.item_class.clone(),
        preview: item.preview.clone(),
    }
}
