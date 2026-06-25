use std::{
    future::Future,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{commands::StdinManager, protocol::ControlRequest};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use serde_json::Value;

use super::{
    api::{
        classify_send_response, GetConfigResponse, IlinkApiClient, IlinkHttpRequest,
        SendMessageResponse, SendResponseClass, TypingStatus,
    },
    inbound::{InboundWechatMedia, InboundWechatMediaKind},
    media::{
        build_cdn_download_request, decrypt_aes_128_ecb_pkcs7, parse_cdn_aes_key,
        WechatCdnDownloadRequest,
    },
    store::WechatStateStore,
    turn::WechatTurnEffect,
};

const MAX_WECHAT_TEXT_CHARS: usize = 3_800;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WechatEffectDispatch {
    pub claude_effect_count: usize,
    pub wechat_effect_count: usize,
    pub desktop_user_messages: Vec<WechatDesktopUserMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatDesktopUserMessage {
    pub desktop_session_id: String,
    pub content: String,
    pub attachments: Vec<WechatDesktopAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatDesktopAttachment {
    pub name: String,
    pub path: String,
    pub is_image: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WechatLifecycleEffect {
    NotifyStart,
    NotifyStop,
}

pub async fn execute_claude_effect(
    stdin_mgr: &StdinManager,
    effect: &WechatTurnEffect,
) -> Result<bool, String> {
    match effect {
        WechatTurnEffect::SendToClaude {
            desktop_session_id,
            text,
        } => {
            let payload = json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": text,
                },
            });
            stdin_mgr
                .send(desktop_session_id, &payload.to_string())
                .await?;
            Ok(true)
        }
        WechatTurnEffect::RespondPermission {
            desktop_session_id,
            request_id,
            allow,
            tool_use_id,
            updated_input,
        } => {
            let mut inner = serde_json::Map::new();
            if *allow {
                inner.insert("behavior".into(), Value::String("allow".into()));
                inner.insert("updatedInput".into(), updated_input.clone());
            } else {
                inner.insert("behavior".into(), Value::String("deny".into()));
                inner.insert(
                    "message".into(),
                    Value::String("User denied this operation".into()),
                );
            }
            if let Some(tool_use_id) = tool_use_id {
                inner.insert("toolUseID".into(), Value::String(tool_use_id.clone()));
            }

            let payload = json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": request_id,
                    "response": inner,
                },
            });
            stdin_mgr
                .send(desktop_session_id, &payload.to_string())
                .await?;
            Ok(true)
        }
        WechatTurnEffect::InterruptClaude { desktop_session_id } => {
            let payload = serde_json::to_string(&ControlRequest::interrupt())
                .map_err(|err| format!("Failed to serialize interrupt request: {err}"))?;
            stdin_mgr.send(desktop_session_id, &payload).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub async fn execute_turn_effects(
    stdin_mgr: &StdinManager,
    store: &WechatStateStore,
    effects: &[WechatTurnEffect],
) -> Result<WechatEffectDispatch, String> {
    execute_turn_effects_with_media(
        stdin_mgr,
        store,
        effects,
        |request| async move {
            IlinkApiClient::new(None)
                .execute_json::<Value>(request)
                .await
        },
        download_cdn_media_bytes,
    )
    .await
}

pub async fn execute_turn_effects_with<F, Fut>(
    stdin_mgr: &StdinManager,
    store: &WechatStateStore,
    effects: &[WechatTurnEffect],
    execute_wechat_request: F,
) -> Result<WechatEffectDispatch, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    execute_turn_effects_with_media(
        stdin_mgr,
        store,
        effects,
        execute_wechat_request,
        download_cdn_media_bytes,
    )
    .await
}

pub async fn execute_turn_effects_with_media<F, Fut, G, Gut>(
    stdin_mgr: &StdinManager,
    store: &WechatStateStore,
    effects: &[WechatTurnEffect],
    mut execute_wechat_request: F,
    mut download_media: G,
) -> Result<WechatEffectDispatch, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
    G: FnMut(WechatCdnDownloadRequest) -> Gut,
    Gut: Future<Output = Result<Vec<u8>, String>>,
{
    let mut dispatch = WechatEffectDispatch::default();
    for effect in effects {
        let mut desktop_user_message = desktop_user_message_for_text_effect(effect);
        let mut handled_claude = execute_claude_effect(stdin_mgr, effect).await?;
        if !handled_claude {
            if let Some(message) =
                execute_claude_media_effect_with(stdin_mgr, store, effect, &mut download_media)
                    .await?
            {
                handled_claude = true;
                desktop_user_message = Some(message);
            }
        }
        if handled_claude {
            dispatch.claude_effect_count += 1;
            if let Some(message) = desktop_user_message {
                dispatch.desktop_user_messages.push(message);
            }
        }
        if execute_wechat_effect_with(effect, store, &mut execute_wechat_request).await? {
            dispatch.wechat_effect_count += 1;
        }
    }
    Ok(dispatch)
}

pub async fn execute_claude_media_effect_with<F, Fut>(
    stdin_mgr: &StdinManager,
    store: &WechatStateStore,
    effect: &WechatTurnEffect,
    mut download_media: F,
) -> Result<Option<WechatDesktopUserMessage>, String>
where
    F: FnMut(WechatCdnDownloadRequest) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, String>>,
{
    let WechatTurnEffect::DownloadMediaToClaude {
        desktop_session_id,
        media,
    } = effect
    else {
        return Ok(None);
    };

    let request = build_cdn_download_request(&media.cdn)?;
    let encrypted = download_media(request).await?;
    let aes_key = parse_cdn_aes_key(&media.cdn.aes_key)?;
    let decrypted = decrypt_aes_128_ecb_pkcs7(&encrypted, &aes_key)?;
    let saved_path = store.save_inbound_media(media, &decrypted)?;
    let content = media_prompt_for_claude(media, &saved_path);
    let payload = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content,
        },
    });
    stdin_mgr
        .send(desktop_session_id, &payload.to_string())
        .await?;
    Ok(Some(WechatDesktopUserMessage {
        desktop_session_id: desktop_session_id.clone(),
        content: media_display_text(media),
        attachments: vec![WechatDesktopAttachment {
            name: saved_path
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .unwrap_or_else(|| "media".into()),
            path: saved_path.display().to_string(),
            is_image: media.kind == InboundWechatMediaKind::Image,
        }],
    }))
}

pub async fn execute_wechat_effect(
    effect: &WechatTurnEffect,
    store: &WechatStateStore,
) -> Result<bool, String> {
    execute_wechat_effect_with(effect, store, |request| async move {
        IlinkApiClient::new(None)
            .execute_json::<Value>(request)
            .await
    })
    .await
}

pub async fn execute_wechat_lifecycle_effect(
    effect: WechatLifecycleEffect,
    store: &WechatStateStore,
) -> Result<bool, String> {
    execute_wechat_lifecycle_effect_with(effect, store, |request| async move {
        IlinkApiClient::new(None)
            .execute_json::<Value>(request)
            .await
    })
    .await
}

pub async fn execute_wechat_lifecycle_effect_with<F, Fut>(
    effect: WechatLifecycleEffect,
    store: &WechatStateStore,
    mut execute_request: F,
) -> Result<bool, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let Some(account) = store.load_account()? else {
        return Ok(false);
    };
    let client = IlinkApiClient::with_base_url(Some(account.bot_token), account.base_url);
    let request = match effect {
        WechatLifecycleEffect::NotifyStart => client.notify_start_request(),
        WechatLifecycleEffect::NotifyStop => client.notify_stop_request(),
    };

    match execute_request(request).await {
        Ok(response) => {
            if let Some(ret) = response.get("ret").and_then(Value::as_i64) {
                if ret != 0 {
                    let errmsg = response
                        .get("errmsg")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    eprintln!("[WeChat] lifecycle notify returned ret={ret} errmsg={errmsg}");
                }
            }
        }
        Err(err) => {
            eprintln!("[WeChat] lifecycle notify failed: {err}");
        }
    }
    Ok(true)
}

pub async fn execute_wechat_effect_with<F, Fut>(
    effect: &WechatTurnEffect,
    store: &WechatStateStore,
    mut execute_request: F,
) -> Result<bool, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let Some(account) = store.load_account()? else {
        return Ok(false);
    };
    let client = IlinkApiClient::with_base_url(Some(account.bot_token), account.base_url);

    match effect {
        WechatTurnEffect::SendWeChatText {
            to_user_id,
            context_token,
            text,
        } => {
            let context_token = resolve_context_token(store, to_user_id, context_token)?;
            let filtered_text = filter_wechat_markdown(text);
            for chunk in split_wechat_text_chunks(&filtered_text) {
                let request =
                    client.send_text_request(to_user_id, &context_token, &chunk, &new_client_id());
                let response: SendMessageResponse =
                    parse_response(execute_request(request).await?)?;
                match classify_send_response(&response) {
                    SendResponseClass::Ok => {}
                    SendResponseClass::RateLimited => {
                        return Err("WeChat sendmessage rate limited".into());
                    }
                    SendResponseClass::StaleSession => {
                        return Err("WeChat sendmessage stale session; reconnect required".into());
                    }
                    SendResponseClass::Failed => {
                        return Err(format!(
                            "WeChat sendmessage failed: ret={:?} errcode={:?} errmsg={:?}",
                            response.ret, response.errcode, response.errmsg
                        ));
                    }
                }
            }
            Ok(true)
        }
        WechatTurnEffect::StartTyping {
            to_user_id,
            context_token,
        } => {
            execute_typing_effect(
                &client,
                to_user_id,
                context_token,
                TypingStatus::Start,
                &mut execute_request,
            )
            .await
        }
        WechatTurnEffect::StopTyping {
            to_user_id,
            context_token,
        } => {
            execute_typing_effect(
                &client,
                to_user_id,
                context_token,
                TypingStatus::Stop,
                &mut execute_request,
            )
            .await
        }
        _ => Ok(false),
    }
}

fn resolve_context_token(
    store: &WechatStateStore,
    to_user_id: &str,
    context_token: &str,
) -> Result<String, String> {
    if !context_token.trim().is_empty() {
        return Ok(context_token.to_string());
    }

    store
        .load_context_token(to_user_id)?
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| format!("WeChat context_token missing for user {to_user_id}"))
}

async fn execute_typing_effect<F, Fut>(
    client: &IlinkApiClient,
    to_user_id: &str,
    context_token: &str,
    status: TypingStatus,
    execute_request: &mut F,
) -> Result<bool, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let config_request = client.get_config_request(to_user_id, Some(context_token));
    let config_value = match execute_request(config_request).await {
        Ok(value) => value,
        Err(err) => {
            eprintln!("[WeChat] getconfig for typing failed: {err}");
            return Ok(true);
        }
    };
    let config: GetConfigResponse = match parse_response(config_value) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("[WeChat] parse getconfig for typing failed: {err}");
            return Ok(true);
        }
    };
    let Some(ticket) = config
        .typing_ticket
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|_| config.ret.unwrap_or_default() == 0)
    else {
        return Ok(true);
    };

    let typing_request = client.send_typing_request(to_user_id, &ticket, status);
    if let Err(err) = execute_request(typing_request).await {
        eprintln!("[WeChat] sendtyping failed: {err}");
    }
    Ok(true)
}

fn parse_response<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|err| format!("parse iLink response: {err}"))
}

fn new_client_id() -> String {
    format!("tc-{}-{}", now_ms(), rand::random::<u32>())
}

async fn download_cdn_media_bytes(request: WechatCdnDownloadRequest) -> Result<Vec<u8>, String> {
    let response = reqwest::Client::new()
        .get(&request.url)
        .timeout(Duration::from_millis(request.timeout_ms))
        .send()
        .await
        .map_err(|err| format!("WeChat CDN download failed: {err}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("WeChat CDN response read failed: {err}"))?;
    if !status.is_success() {
        return Err(format!("WeChat CDN HTTP {status}"));
    }
    Ok(bytes.to_vec())
}

fn media_prompt_for_claude(media: &InboundWechatMedia, saved_path: &Path) -> String {
    let label = match media.kind {
        InboundWechatMediaKind::Image => "微信发来的图片",
        InboundWechatMediaKind::File => "微信发来的文件",
    };
    let name = media
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("：{value}"))
        .unwrap_or_default();
    format!(
        "{label}{name}\n\n[Attached files]\n{}",
        saved_path.display()
    )
}

fn desktop_user_message_for_text_effect(
    effect: &WechatTurnEffect,
) -> Option<WechatDesktopUserMessage> {
    let WechatTurnEffect::SendToClaude {
        desktop_session_id,
        text,
    } = effect
    else {
        return None;
    };

    Some(WechatDesktopUserMessage {
        desktop_session_id: desktop_session_id.clone(),
        content: text.clone(),
        attachments: Vec::new(),
    })
}

fn media_display_text(media: &InboundWechatMedia) -> String {
    let label = match media.kind {
        InboundWechatMediaKind::Image => "微信发来的图片",
        InboundWechatMediaKind::File => "微信发来的文件",
    };
    let name = media
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("：{value}"))
        .unwrap_or_default();
    format!("{label}{name}")
}

fn split_wechat_text_chunks(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_WECHAT_TEXT_CHARS {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.chars().count() > MAX_WECHAT_TEXT_CHARS {
        let hard_split = byte_index_after_chars(remaining, MAX_WECHAT_TEXT_CHARS);
        let split_at = preferred_split_boundary(remaining, hard_split).unwrap_or(hard_split);
        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk.to_string());
        remaining = rest;
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}

fn preferred_split_boundary(text: &str, hard_split: usize) -> Option<usize> {
    let candidate = &text[..hard_split];
    candidate
        .char_indices()
        .rev()
        .find(|(_, ch)| *ch == '\n')
        .or_else(|| {
            candidate
                .char_indices()
                .rev()
                .find(|(_, ch)| ch.is_whitespace())
        })
        .map(|(index, ch)| index + ch.len_utf8())
        .filter(|index| *index > 0)
}

fn byte_index_after_chars(text: &str, max_chars: usize) -> usize {
    for (index, (byte_index, ch)) in text.char_indices().enumerate() {
        if index + 1 == max_chars {
            return byte_index + ch.len_utf8();
        }
    }
    text.len()
}

fn filter_wechat_markdown(text: &str) -> String {
    let mut filtered = String::with_capacity(text.len());
    let mut in_code_fence = false;

    for line in text.split_inclusive('\n') {
        let (body, line_break) = line
            .strip_suffix('\n')
            .map(|body| (body, "\n"))
            .unwrap_or((line, ""));

        if body.starts_with("```") {
            in_code_fence = !in_code_fence;
            filtered.push_str(line);
            continue;
        }
        if in_code_fence {
            filtered.push_str(line);
            continue;
        }

        filtered.push_str(&filter_wechat_inline_markdown(strip_wechat_line_markers(
            body,
        )));
        filtered.push_str(line_break);
    }

    filtered
}

fn strip_wechat_line_markers(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('>') {
        return rest.trim_start_matches([' ', '\t']);
    }

    let hash_count = line.bytes().take_while(|byte| *byte == b'#').count();
    if (5..=6).contains(&hash_count) && line.as_bytes().get(hash_count) == Some(&b' ') {
        return line[hash_count + 1..].trim_start_matches([' ', '\t']);
    }

    line
}

fn filter_wechat_inline_markdown(line: &str) -> String {
    let without_images = remove_markdown_images(line);
    let without_strikethrough = without_images.replace("~~", "");
    strip_cjk_emphasis(&without_strikethrough)
}

fn remove_markdown_images(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find("![") {
        out.push_str(&remaining[..start]);
        let after_start = &remaining[start + 2..];
        let Some(label_end) = after_start.find("](") else {
            out.push_str(&remaining[start..]);
            return out;
        };
        let after_url_start = &after_start[label_end + 2..];
        let Some(url_end) = after_url_start.find(')') else {
            out.push_str(&remaining[start..]);
            return out;
        };
        remaining = &after_url_start[url_end + 1..];
    }

    out.push_str(remaining);
    out
}

fn strip_cjk_emphasis(text: &str) -> String {
    let text = strip_cjk_wrapping_marker(text, "***");
    let text = strip_cjk_wrapping_marker(&text, "___");
    let text = strip_cjk_single_marker(&text, '*');
    strip_cjk_single_marker(&text, '_')
}

fn strip_cjk_wrapping_marker(text: &str, marker: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(start) = remaining.find(marker) {
        out.push_str(&remaining[..start]);
        let content_start = start + marker.len();
        let Some(end) = remaining[content_start..].find(marker) else {
            out.push_str(&remaining[start..]);
            return out;
        };
        let content_end = content_start + end;
        let content = &remaining[content_start..content_end];
        if contains_cjk(content) {
            out.push_str(content);
        } else {
            out.push_str(marker);
            out.push_str(content);
            out.push_str(marker);
        }
        remaining = &remaining[content_end + marker.len()..];
    }

    out.push_str(remaining);
    out
}

fn strip_cjk_single_marker(text: &str, marker: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(start) = find_single_marker(text, marker, cursor) {
        out.push_str(&text[cursor..start]);
        let content_start = start + marker.len_utf8();
        let Some(end) = find_single_marker(text, marker, content_start) else {
            out.push_str(&text[start..]);
            return out;
        };
        let content = &text[content_start..end];
        if contains_cjk(content) {
            out.push_str(content);
        } else {
            out.push(marker);
            out.push_str(content);
            out.push(marker);
        }
        cursor = end + marker.len_utf8();
    }

    out.push_str(&text[cursor..]);
    out
}

fn find_single_marker(text: &str, marker: char, start: usize) -> Option<usize> {
    text[start..].char_indices().find_map(|(offset, ch)| {
        let index = start + offset;
        if ch == marker && !has_adjacent_marker(text, index, marker) {
            Some(index)
        } else {
            None
        }
    })
}

fn has_adjacent_marker(text: &str, index: usize, marker: char) -> bool {
    let before = text[..index].chars().next_back();
    let after = text[index + marker.len_utf8()..].chars().next();
    before == Some(marker) || after == Some(marker)
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(|ch| {
        ('\u{2E80}'..='\u{9FFF}').contains(&ch)
            || ('\u{AC00}'..='\u{D7AF}').contains(&ch)
            || ('\u{F900}'..='\u{FAFF}').contains(&ch)
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use serde_json::json;
    use std::path::Path;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::{Child, Command};
    use tokio::time::{timeout, Duration};

    use crate::wechat::{
        inbound::{InboundWechatCdnMedia, InboundWechatMedia, InboundWechatMediaKind},
        media::WechatCdnDownloadRequest,
        store::{WechatAccount, WechatStateStore},
    };

    #[tokio::test]
    async fn send_to_claude_effect_writes_user_ndjson_to_existing_stdin_manager() {
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        execute_claude_effect(
            &stdin_mgr,
            &WechatTurnEffect::SendToClaude {
                desktop_session_id: "stdin-1".into(),
                text: "hello from WeChat".into(),
            },
        )
        .await
        .unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let actual: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            actual,
            json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": "hello from WeChat",
                },
            })
        );

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn download_media_to_claude_effect_saves_decrypted_file_and_writes_attachment_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;
        let captured_downloads = Arc::new(Mutex::new(Vec::<WechatCdnDownloadRequest>::new()));
        let encrypted = BASE64_STANDARD
            .decode("xhoD0c7E8emien3r349dx0yFRk8xSm9+WOSTOn8Wjn4=")
            .unwrap();
        let captured = captured_downloads.clone();

        let dispatch = execute_turn_effects_with_media(
            &stdin_mgr,
            &store,
            &[WechatTurnEffect::DownloadMediaToClaude {
                desktop_session_id: "stdin-1".into(),
                media: InboundWechatMedia {
                    message_id: "img-1".into(),
                    from_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                    received_at_ms: 1_000,
                    kind: InboundWechatMediaKind::Image,
                    file_name: None,
                    size_hint: Some("2048".into()),
                    cdn: InboundWechatCdnMedia {
                        encrypt_query_param: Some("cdn=query".into()),
                        aes_key: "MDEyMzQ1Njc4OWFiY2RlZg==".into(),
                        encrypt_type: Some(1),
                        full_url: None,
                    },
                },
            }],
            |_request| async { Ok(json!({ "ret": 0 })) },
            move |request| {
                let captured = captured.clone();
                let encrypted = encrypted.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(encrypted)
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(dispatch.claude_effect_count, 1);
        assert_eq!(dispatch.wechat_effect_count, 0);
        assert_eq!(
            captured_downloads.lock().unwrap()[0].url,
            "https://novac2c.cdn.weixin.qq.com/c2c/download?encrypted_query_param=cdn%3Dquery"
        );

        let line = lines.next_line().await.unwrap().unwrap();
        let actual: serde_json::Value = serde_json::from_str(&line).unwrap();
        let content = actual["message"]["content"].as_str().unwrap();
        assert!(content.starts_with("微信发来的图片"));
        let saved_path = content
            .split("[Attached files]\n")
            .nth(1)
            .expect("attachment marker")
            .trim();
        assert!(Path::new(saved_path).starts_with(dir.path().join("inbound-media")));
        assert_eq!(std::fs::read(saved_path).unwrap(), b"hello wechat media");
        assert_eq!(
            dispatch.desktop_user_messages,
            vec![WechatDesktopUserMessage {
                desktop_session_id: "stdin-1".into(),
                content: "微信发来的图片".into(),
                attachments: vec![WechatDesktopAttachment {
                    name: Path::new(saved_path)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                    path: saved_path.into(),
                    is_image: true,
                }],
            }]
        );

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn approve_permission_effect_writes_control_response_with_original_tool_input() {
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        execute_claude_effect(
            &stdin_mgr,
            &WechatTurnEffect::RespondPermission {
                desktop_session_id: "stdin-1".into(),
                request_id: "perm-1".into(),
                allow: true,
                tool_use_id: Some("toolu-1".into()),
                updated_input: json!({ "command": "pnpm test" }),
            },
        )
        .await
        .unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let actual: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            actual,
            json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": "perm-1",
                    "response": {
                        "behavior": "allow",
                        "updatedInput": { "command": "pnpm test" },
                        "toolUseID": "toolu-1",
                    },
                },
            })
        );

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn deny_permission_effect_writes_control_response_with_denial_message() {
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        execute_claude_effect(
            &stdin_mgr,
            &WechatTurnEffect::RespondPermission {
                desktop_session_id: "stdin-1".into(),
                request_id: "perm-1".into(),
                allow: false,
                tool_use_id: Some("toolu-1".into()),
                updated_input: json!({ "command": "pnpm test" }),
            },
        )
        .await
        .unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        let actual: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            actual,
            json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": "perm-1",
                    "response": {
                        "behavior": "deny",
                        "message": "User denied this operation",
                        "toolUseID": "toolu-1",
                    },
                },
            })
        );

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn interrupt_effect_writes_control_request_to_existing_stdin_manager() {
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        let handled = execute_claude_effect(
            &stdin_mgr,
            &WechatTurnEffect::InterruptClaude {
                desktop_session_id: "stdin-1".into(),
            },
        )
        .await
        .unwrap();

        let line = timeout(Duration::from_millis(200), lines.next_line())
            .await
            .expect("interrupt effect wrote no control request")
            .unwrap()
            .unwrap();
        let actual: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert!(handled);
        assert_eq!(actual["type"], "control_request");
        assert!(actual["request_id"].as_str().is_some());
        assert_eq!(actual["request"]["subtype"], "interrupt");

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn send_wechat_text_effect_posts_message_with_saved_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "hello from Claude".into(),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(json!({ "ret": 0 }))
                }
            },
        )
        .await
        .unwrap();

        assert!(handled);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendmessage"
        );
        assert_eq!(
            request.headers.get("Authorization"),
            Some(&"Bearer bot-token".into())
        );
        assert_eq!(request.body["msg"]["to_user_id"], "user-1");
        assert_eq!(request.body["msg"]["context_token"], "ctx-1");
        assert_eq!(
            request.body["msg"]["item_list"][0]["text_item"]["text"],
            "hello from Claude"
        );
        assert!(request.body["msg"]["client_id"]
            .as_str()
            .unwrap()
            .starts_with("tc-"));
    }

    #[tokio::test]
    async fn send_wechat_text_effect_uses_persisted_context_token_when_effect_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        store.save_context_token("user-1", "ctx-latest").unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: String::new(),
                text: "async follow-up".into(),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(json!({ "ret": 0 }))
                }
            },
        )
        .await
        .unwrap();

        assert!(handled);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body["msg"]["to_user_id"], "user-1");
        assert_eq!(requests[0].body["msg"]["context_token"], "ctx-latest");
        assert_eq!(
            requests[0].body["msg"]["item_list"][0]["text_item"]["text"],
            "async follow-up"
        );
    }

    #[tokio::test]
    async fn send_wechat_text_effect_filters_wechat_incompatible_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "##### 标题\n> 引用\n**bold** *中文* ~~删除~~ ![alt](https://x.test/a.png)\n```ts\nconst x = \"~~keep~~\";\n```\n".into(),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(json!({ "ret": 0 }))
                }
            },
        )
        .await
        .unwrap();

        assert!(handled);
        let requests = requests.lock().unwrap();
        assert_eq!(
            requests[0].body["msg"]["item_list"][0]["text_item"]["text"],
            "标题\n引用\n**bold** 中文 删除 \n```ts\nconst x = \"~~keep~~\";\n```\n"
        );
    }

    #[tokio::test]
    async fn lifecycle_start_effect_posts_notify_start_with_saved_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_lifecycle_effect_with(
            WechatLifecycleEffect::NotifyStart,
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(json!({ "ret": 0 }))
                }
            },
        )
        .await
        .unwrap();

        assert!(handled);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(
            request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/msg/notifystart"
        );
        assert_eq!(
            request.headers.get("Authorization"),
            Some(&"Bearer bot-token".into())
        );
        assert!(request.body.get("base_info").is_some());
    }

    #[tokio::test]
    async fn lifecycle_notify_failure_is_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();

        let handled = execute_wechat_lifecycle_effect_with(
            WechatLifecycleEffect::NotifyStop,
            &store,
            |_request| async { Err("network down".into()) },
        )
        .await
        .unwrap();

        assert!(handled);
    }

    #[tokio::test]
    async fn lifecycle_notify_ret_failure_is_non_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();

        let handled = execute_wechat_lifecycle_effect_with(
            WechatLifecycleEffect::NotifyStop,
            &store,
            |_request| async { Ok(json!({ "ret": -1, "errmsg": "unsupported" })) },
        )
        .await
        .unwrap();

        assert!(handled);
    }

    #[tokio::test]
    async fn send_wechat_text_effect_splits_long_text_on_newline_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let first_line = "好".repeat(3_790);
        let second_line = "界".repeat(20);

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: format!("{first_line}\n{second_line}"),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(json!({ "ret": 0 }))
                }
            },
        )
        .await
        .unwrap();

        assert!(handled);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body["msg"]["context_token"], "ctx-1");
        assert_eq!(requests[1].body["msg"]["context_token"], "ctx-1");
        assert_eq!(
            requests[0].body["msg"]["item_list"][0]["text_item"]["text"],
            format!("{first_line}\n")
        );
        assert_eq!(
            requests[1].body["msg"]["item_list"][0]["text_item"]["text"],
            second_line
        );
    }

    #[test]
    fn split_wechat_text_chunks_keeps_unicode_characters_intact_without_boundaries() {
        let text = format!("{}{}", "好".repeat(MAX_WECHAT_TEXT_CHARS), "界");

        let chunks = split_wechat_text_chunks(&text);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), MAX_WECHAT_TEXT_CHARS);
        assert_eq!(chunks[0], "好".repeat(MAX_WECHAT_TEXT_CHARS));
        assert_eq!(chunks[1], "界");
    }

    #[tokio::test]
    async fn start_typing_effect_fetches_ticket_and_posts_typing_start() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::StartTyping {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    let mut requests = captured.lock().unwrap();
                    requests.push(request);
                    if requests.len() == 1 {
                        Ok(json!({ "ret": 0, "typing_ticket": "ticket-1" }))
                    } else {
                        Ok(json!({ "ret": 0 }))
                    }
                }
            },
        )
        .await
        .unwrap();

        assert!(handled);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].url,
            "https://ilinkai.weixin.qq.com/ilink/bot/getconfig"
        );
        assert_eq!(requests[0].body["ilink_user_id"], "user-1");
        assert_eq!(requests[0].body["context_token"], "ctx-1");
        assert_eq!(
            requests[1].url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendtyping"
        );
        assert_eq!(requests[1].body["ilink_user_id"], "user-1");
        assert_eq!(requests[1].body["typing_ticket"], "ticket-1");
        assert_eq!(requests[1].body["status"], 1);
    }

    #[tokio::test]
    async fn stop_typing_effect_fetches_ticket_and_posts_typing_stop() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::StopTyping {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    let mut requests = captured.lock().unwrap();
                    requests.push(request);
                    if requests.len() == 1 {
                        Ok(json!({ "ret": 0, "typing_ticket": "ticket-1" }))
                    } else {
                        Ok(json!({ "ret": 0 }))
                    }
                }
            },
        )
        .await
        .unwrap();

        assert!(handled);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].url,
            "https://ilinkai.weixin.qq.com/ilink/bot/getconfig"
        );
        assert_eq!(
            requests[1].url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendtyping"
        );
        assert_eq!(requests[1].body["ilink_user_id"], "user-1");
        assert_eq!(requests[1].body["typing_ticket"], "ticket-1");
        assert_eq!(requests[1].body["status"], 2);
    }

    #[tokio::test]
    async fn typing_effect_treats_config_failure_as_handled() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::StartTyping {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
            },
            &store,
            |_request| async { Err("network down".into()) },
        )
        .await
        .unwrap();

        assert!(handled);
    }

    async fn spawn_echo_session(
        stdin_mgr: &StdinManager,
        stdin_id: &str,
    ) -> (
        Child,
        tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    ) {
        let mut child = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        stdin_mgr.insert(stdin_id.into(), stdin).await;

        (child, BufReader::new(stdout).lines())
    }

    fn account() -> WechatAccount {
        WechatAccount {
            bot_token: "bot-token".into(),
            account_id: "bot-id".into(),
            base_url: "https://ilinkai.weixin.qq.com".into(),
            user_id: "bot-user".into(),
            created_at_ms: 123,
        }
    }
}
