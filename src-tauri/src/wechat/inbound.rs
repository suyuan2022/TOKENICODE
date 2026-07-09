use crate::wechat::{
    api::{CdnMedia, FileItem, ImageItem, MessageItem, WechatMessage},
    turn::InboundWechatText,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};

const MESSAGE_TYPE_USER: i32 = 1;
const MESSAGE_ITEM_TEXT: i32 = 1;
const MESSAGE_ITEM_IMAGE: i32 = 2;
const MESSAGE_ITEM_VOICE: i32 = 3;
const MESSAGE_ITEM_FILE: i32 = 4;

pub fn parse_inbound_text(
    message: WechatMessage,
    received_at_ms: u64,
) -> Result<Option<InboundWechatText>, String> {
    parse_inbound_text_message(&message, received_at_ms)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedInboundWechatMessage {
    Text(InboundWechatText),
    Media(InboundWechatMedia),
    UnsupportedVoice {
        from_user_id: String,
        context_token: String,
    },
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundWechatMedia {
    pub message_id: String,
    pub from_user_id: String,
    pub context_token: String,
    pub received_at_ms: u64,
    pub kind: InboundWechatMediaKind,
    pub file_name: Option<String>,
    pub size_hint: Option<String>,
    pub cdn: InboundWechatCdnMedia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundWechatMediaKind {
    Image,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundWechatCdnMedia {
    pub encrypt_query_param: Option<String>,
    pub aes_key: String,
    pub encrypt_type: Option<i32>,
    pub full_url: Option<String>,
}

pub fn parse_inbound_message(
    message: WechatMessage,
    received_at_ms: u64,
) -> Result<ParsedInboundWechatMessage, String> {
    if message.message_type != Some(MESSAGE_TYPE_USER) {
        return Ok(ParsedInboundWechatMessage::Ignore);
    }

    if let Some(inbound) = parse_inbound_media_message(&message, received_at_ms)? {
        return Ok(ParsedInboundWechatMessage::Media(inbound));
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
    let message_id = message_id_from_message(message)?;

    Ok(Some(InboundWechatText {
        message_id,
        from_user_id,
        context_token,
        text,
        received_at_ms,
    }))
}

fn parse_inbound_media_message(
    message: &WechatMessage,
    received_at_ms: u64,
) -> Result<Option<InboundWechatMedia>, String> {
    for item in &message.item_list {
        if let Some(media) = image_media_from_item(message, item, received_at_ms)? {
            return Ok(Some(media));
        }
        if let Some(media) = file_media_from_item(message, item, received_at_ms)? {
            return Ok(Some(media));
        }
    }
    Ok(None)
}

fn image_media_from_item(
    message: &WechatMessage,
    item: &MessageItem,
    received_at_ms: u64,
) -> Result<Option<InboundWechatMedia>, String> {
    if item.item_type != Some(MESSAGE_ITEM_IMAGE) {
        return Ok(None);
    }
    let Some(image_item) = item.image_item.as_ref() else {
        return Ok(None);
    };
    let Some(cdn) = downloadable_cdn_media(image_item.media.as_ref(), image_aes_key(image_item)?)
    else {
        return Ok(None);
    };

    Ok(Some(InboundWechatMedia {
        message_id: message_id_from_message(message)?,
        from_user_id: required(message.from_user_id.as_deref(), "from_user_id")?,
        context_token: required(message.context_token.as_deref(), "context_token")?,
        received_at_ms,
        kind: InboundWechatMediaKind::Image,
        file_name: None,
        size_hint: image_item.mid_size.map(|size| size.to_string()),
        cdn,
    }))
}

fn file_media_from_item(
    message: &WechatMessage,
    item: &MessageItem,
    received_at_ms: u64,
) -> Result<Option<InboundWechatMedia>, String> {
    if item.item_type != Some(MESSAGE_ITEM_FILE) {
        return Ok(None);
    }
    let Some(file_item) = item.file_item.as_ref() else {
        return Ok(None);
    };
    let Some(cdn) = downloadable_cdn_media(file_item.media.as_ref(), file_aes_key(file_item))
    else {
        return Ok(None);
    };

    Ok(Some(InboundWechatMedia {
        message_id: message_id_from_message(message)?,
        from_user_id: required(message.from_user_id.as_deref(), "from_user_id")?,
        context_token: required(message.context_token.as_deref(), "context_token")?,
        received_at_ms,
        kind: InboundWechatMediaKind::File,
        file_name: non_empty_owned(file_item.file_name.as_deref()),
        size_hint: non_empty_owned(file_item.len.as_deref()),
        cdn,
    }))
}

fn downloadable_cdn_media(
    media: Option<&CdnMedia>,
    aes_key: Option<String>,
) -> Option<InboundWechatCdnMedia> {
    let media = media?;
    let encrypt_query_param = non_empty_owned(media.encrypt_query_param.as_deref());
    let full_url = non_empty_owned(media.full_url.as_deref());
    if encrypt_query_param.is_none() && full_url.is_none() {
        return None;
    }
    Some(InboundWechatCdnMedia {
        encrypt_query_param,
        aes_key: aes_key?,
        encrypt_type: media.encrypt_type,
        full_url,
    })
}

fn image_aes_key(image_item: &ImageItem) -> Result<Option<String>, String> {
    if let Some(aeskey) = image_item
        .aeskey
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(base64_from_hex_aes_key(aeskey)?));
    }

    Ok(image_item
        .media
        .as_ref()
        .and_then(|media| non_empty_owned(media.aes_key.as_deref())))
}

fn file_aes_key(file_item: &FileItem) -> Option<String> {
    file_item
        .media
        .as_ref()
        .and_then(|media| non_empty_owned(media.aes_key.as_deref()))
}

fn base64_from_hex_aes_key(hex_key: &str) -> Result<String, String> {
    let hex_key = hex_key.trim();
    if hex_key.len() != 32 {
        return Err("image_item.aeskey must be 32 hex characters".into());
    }

    let mut bytes = Vec::with_capacity(16);
    for pair in hex_key.as_bytes().chunks_exact(2) {
        let pair = std::str::from_utf8(pair).map_err(|_| "image_item.aeskey must be hex")?;
        let byte = u8::from_str_radix(pair, 16)
            .map_err(|_| "image_item.aeskey must be hex".to_string())?;
        bytes.push(byte);
    }

    Ok(BASE64_STANDARD.encode(bytes))
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

fn message_id_from_message(message: &WechatMessage) -> Result<String, String> {
    message
        .message_id
        .map(|id| id.to_string())
        .or_else(|| message.client_id.clone())
        .or_else(|| message.seq.map(|seq| format!("seq-{seq}")))
        .ok_or_else(|| "inbound WeChat user message missing message_id".to_string())
}

fn non_empty_owned(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
    use crate::wechat::api::{
        CdnMedia, FileItem, ImageItem, MessageItem, MessageItemType, TextItem, VoiceItem,
        WechatMessage,
    };

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
    fn parses_image_message_as_downloadable_media() {
        let parsed = parse_inbound_message(
            user_message(
                12,
                vec![MessageItem {
                    item_type: Some(MessageItemType::Image as i32),
                    image_item: Some(ImageItem {
                        media: Some(CdnMedia {
                            encrypt_query_param: Some("image-query".into()),
                            aes_key: Some("media-key".into()),
                            encrypt_type: Some(1),
                            full_url: Some("https://novac2c.cdn.weixin.qq.com/c2c/download".into()),
                        }),
                        aeskey: Some("0123456789abcdeffedcba9876543210".into()),
                        mid_size: Some(1234),
                        ..ImageItem::default()
                    }),
                    ..MessageItem::default()
                }],
                Some("ctx-image"),
            ),
            456,
        )
        .unwrap();

        assert_eq!(
            parsed,
            ParsedInboundWechatMessage::Media(InboundWechatMedia {
                message_id: "12".into(),
                from_user_id: "user-1".into(),
                context_token: "ctx-image".into(),
                received_at_ms: 456,
                kind: InboundWechatMediaKind::Image,
                file_name: None,
                size_hint: Some("1234".into()),
                cdn: InboundWechatCdnMedia {
                    encrypt_query_param: Some("image-query".into()),
                    aes_key: "ASNFZ4mrze/+3LqYdlQyEA==".into(),
                    encrypt_type: Some(1),
                    full_url: Some("https://novac2c.cdn.weixin.qq.com/c2c/download".into()),
                },
            })
        );
    }

    #[test]
    fn parses_file_message_as_downloadable_media() {
        let parsed = parse_inbound_message(
            user_message(
                13,
                vec![MessageItem {
                    item_type: Some(MessageItemType::File as i32),
                    file_item: Some(FileItem {
                        media: Some(CdnMedia {
                            encrypt_query_param: Some("file-query".into()),
                            aes_key: Some("file-key".into()),
                            encrypt_type: Some(1),
                            full_url: None,
                        }),
                        file_name: Some("report.pdf".into()),
                        len: Some("4096".into()),
                    }),
                    ..MessageItem::default()
                }],
                Some("ctx-file"),
            ),
            789,
        )
        .unwrap();

        assert_eq!(
            parsed,
            ParsedInboundWechatMessage::Media(InboundWechatMedia {
                message_id: "13".into(),
                from_user_id: "user-1".into(),
                context_token: "ctx-file".into(),
                received_at_ms: 789,
                kind: InboundWechatMediaKind::File,
                file_name: Some("report.pdf".into()),
                size_hint: Some("4096".into()),
                cdn: InboundWechatCdnMedia {
                    encrypt_query_param: Some("file-query".into()),
                    aes_key: "file-key".into(),
                    encrypt_type: Some(1),
                    full_url: None,
                },
            })
        );
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
