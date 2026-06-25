use crate::wechat::{
    api::{MessageItem, WechatMessage},
    turn::InboundWechatText,
};

const MESSAGE_TYPE_USER: i32 = 1;
const MESSAGE_ITEM_TEXT: i32 = 1;
const MESSAGE_ITEM_VOICE: i32 = 3;

pub fn parse_inbound_text(
    message: WechatMessage,
    received_at_ms: u64,
) -> Result<Option<InboundWechatText>, String> {
    parse_inbound_text_message(&message, received_at_ms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedInboundWechatMessage {
    Text(InboundWechatText),
    UnsupportedVoice {
        from_user_id: String,
        context_token: String,
    },
    Ignore,
}

pub fn parse_inbound_message(
    message: WechatMessage,
    received_at_ms: u64,
) -> Result<ParsedInboundWechatMessage, String> {
    if message.message_type != Some(MESSAGE_TYPE_USER) {
        return Ok(ParsedInboundWechatMessage::Ignore);
    }

    if let Some(inbound) = parse_inbound_text_message(&message, received_at_ms)? {
        return Ok(ParsedInboundWechatMessage::Text(inbound));
    }

    if has_voice_without_transcript(&message.item_list) {
        return Ok(ParsedInboundWechatMessage::UnsupportedVoice {
            from_user_id: required(message.from_user_id.as_deref(), "from_user_id")?,
            context_token: required(message.context_token.as_deref(), "context_token")?,
        });
    }

    Ok(ParsedInboundWechatMessage::Ignore)
}

fn parse_inbound_text_message(
    message: &WechatMessage,
    received_at_ms: u64,
) -> Result<Option<InboundWechatText>, String> {
    if message.message_type != Some(MESSAGE_TYPE_USER) {
        return Ok(None);
    }

    let Some(text) = body_from_item_list(&message.item_list) else {
        return Ok(None);
    };
    let from_user_id = required(message.from_user_id.as_deref(), "from_user_id")?;
    let context_token = required(message.context_token.as_deref(), "context_token")?;
    let message_id = message
        .message_id
        .map(|id| id.to_string())
        .or_else(|| message.client_id.clone())
        .or_else(|| message.seq.map(|seq| format!("seq-{seq}")))
        .ok_or_else(|| "inbound WeChat user message missing message_id".to_string())?;

    Ok(Some(InboundWechatText {
        message_id,
        from_user_id,
        context_token,
        text,
        received_at_ms,
    }))
}

fn body_from_item_list(items: &[MessageItem]) -> Option<String> {
    for item in items {
        if let Some(text) = body_from_item(item) {
            return Some(text);
        }
    }
    None
}

fn body_from_item(item: &MessageItem) -> Option<String> {
    match item.item_type {
        Some(MESSAGE_ITEM_TEXT) => {
            let text = item.text_item.as_ref()?.text.as_deref()?.trim();
            if text.is_empty() {
                return None;
            }
            Some(with_quote_prefix(item, text))
        }
        Some(MESSAGE_ITEM_VOICE) => item
            .voice_item
            .as_ref()?
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn has_voice_without_transcript(items: &[MessageItem]) -> bool {
    items.iter().any(|item| {
        item.item_type == Some(MESSAGE_ITEM_VOICE)
            && item
                .voice_item
                .as_ref()
                .and_then(|voice| voice.text.as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .is_none()
    })
}

fn with_quote_prefix(item: &MessageItem, text: &str) -> String {
    let Some(ref_msg) = item.ref_msg.as_ref() else {
        return text.into();
    };

    let mut parts = Vec::new();
    if let Some(title) = ref_msg
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(title.to_string());
    }
    if let Some(message_item) = ref_msg.message_item.as_deref() {
        if let Some(body) = body_from_item(message_item) {
            parts.push(body);
        }
    }

    if parts.is_empty() {
        text.into()
    } else {
        format!("[引用: {}]\n{text}", parts.join(" | "))
    }
}

fn required(value: Option<&str>, field: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("inbound WeChat user message missing {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wechat::api::{MessageItem, TextItem, VoiceItem, WechatMessage};

    #[test]
    fn parses_user_text_message_for_turn_manager() {
        let parsed = parse_inbound_text(
            user_message(7, vec![text_item("hello from wechat")], Some("ctx-1")),
            123,
        )
        .unwrap()
        .unwrap();

        assert_eq!(parsed.message_id, "7");
        assert_eq!(parsed.from_user_id, "user-1");
        assert_eq!(parsed.context_token, "ctx-1");
        assert_eq!(parsed.text, "hello from wechat");
        assert_eq!(parsed.received_at_ms, 123);
    }

    #[test]
    fn includes_quoted_text_context() {
        let parsed = parse_inbound_text(
            user_message(
                8,
                vec![quoted_text_item("earlier text", "reply")],
                Some("ctx-2"),
            ),
            123,
        )
        .unwrap()
        .unwrap();

        assert_eq!(parsed.text, "[引用: earlier text]\nreply");
    }

    #[test]
    fn treats_voice_transcript_as_text() {
        let parsed = parse_inbound_text(
            user_message(
                9,
                vec![MessageItem {
                    item_type: Some(3),
                    voice_item: Some(VoiceItem {
                        text: Some("voice transcript".into()),
                        ..VoiceItem::default()
                    }),
                    ..MessageItem::default()
                }],
                Some("ctx-3"),
            ),
            123,
        )
        .unwrap()
        .unwrap();

        assert_eq!(parsed.text, "voice transcript");
    }

    #[test]
    fn text_parser_ignores_raw_voice_without_context_token() {
        let parsed = parse_inbound_text(
            WechatMessage {
                from_user_id: Some("user-1".into()),
                message_type: Some(1),
                item_list: vec![MessageItem {
                    item_type: Some(3),
                    voice_item: Some(VoiceItem::default()),
                    ..MessageItem::default()
                }],
                ..WechatMessage::default()
            },
            123,
        )
        .unwrap();

        assert_eq!(parsed, None);
    }

    #[test]
    fn ignores_non_user_messages() {
        let parsed = parse_inbound_text(
            WechatMessage {
                message_id: Some(10),
                message_type: Some(2),
                ..user_message(10, vec![text_item("bot echo")], Some("ctx-4"))
            },
            123,
        )
        .unwrap();

        assert_eq!(parsed, None);
    }

    #[test]
    fn rejects_user_text_without_context_token() {
        let err =
            parse_inbound_text(user_message(11, vec![text_item("hello")], None), 123).unwrap_err();

        assert!(err.contains("context_token"));
    }

    fn user_message(
        message_id: i64,
        item_list: Vec<MessageItem>,
        context_token: Option<&str>,
    ) -> WechatMessage {
        WechatMessage {
            message_id: Some(message_id),
            from_user_id: Some("user-1".into()),
            message_type: Some(1),
            item_list,
            context_token: context_token.map(str::to_string),
            ..WechatMessage::default()
        }
    }

    fn text_item(text: &str) -> MessageItem {
        MessageItem {
            item_type: Some(1),
            text_item: Some(TextItem {
                text: Some(text.into()),
            }),
            ..MessageItem::default()
        }
    }

    fn quoted_text_item(quoted: &str, text: &str) -> MessageItem {
        MessageItem {
            ref_msg: Some(crate::wechat::api::RefMessage {
                title: None,
                message_item: Some(Box::new(text_item(quoted))),
            }),
            ..text_item(text)
        }
    }
}
