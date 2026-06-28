#[cfg(not(test))]
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex as StdMutex},
};
use std::{future::Future, path::Path, time::Duration};

use crate::{commands::StdinManager, protocol::ControlRequest};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rand::RngCore;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use serde_json::Value;
#[cfg(not(test))]
use tokio::task::JoinHandle;

use super::{
    api::{
        classify_send_response, GetConfigResponse, GetUploadUrlResponse, IlinkApiClient,
        IlinkHttpRequest, SendMessageResponse, SendResponseClass, TypingStatus, UploadMediaType,
    },
    inbound::{InboundWechatMedia, InboundWechatMediaKind},
    media::{
        aes_128_ecb_pkcs7_padded_size, build_cdn_download_request, build_cdn_upload_request,
        decrypt_aes_128_ecb_pkcs7, encrypt_aes_128_ecb_pkcs7, parse_cdn_aes_key,
        WechatCdnDownloadRequest, WechatCdnUploadRequest,
    },
    now_ms,
    store::WechatStateStore,
    text::{filter_wechat_markdown, split_wechat_text_messages_with_preferences},
    turn::WechatTurnEffect,
};

const MAX_WECHAT_FILE_BYTES: u64 = 25 * 1024 * 1024;
const WECHAT_SEND_MANIFEST_BASENAME: &str = "tokenicode-wechat-send-files.json";
const MAX_WECHAT_SEND_MANIFEST_FILES: usize = 10;
const SEND_CIRCUIT_OPEN_MS: u64 = 30_000;
const TYPING_TICKET_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
#[cfg(not(test))]
const TYPING_KEEPALIVE_INTERVAL_MS: u64 = 5_000;

#[cfg(not(test))]
static TYPING_KEEPALIVE_TASKS: LazyLock<StdMutex<HashMap<String, JoinHandle<()>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WechatEffectDispatch {
    pub claude_effect_count: usize,
    pub wechat_effect_count: usize,
    pub desktop_user_messages: Vec<WechatDesktopUserMessage>,
    pub desktop_clear_conversations: Vec<WechatDesktopClearConversation>,
    pub desktop_stops: Vec<WechatDesktopStop>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatDesktopClearConversation {
    pub desktop_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatDesktopStop {
    pub desktop_session_id: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WechatLifecycleEffect {
    NotifyStart,
    NotifyStop,
}

const WECHAT_REMOTE_CLAUDE_INSTRUCTION: &str = "\
这是 TOKENICODE 的微信接入会话。用户在微信里等待回复。你可以正常回复文本，TOKENICODE 会转发到微信。\
需要把新生成的文件发给微信用户时，请使用 Write 工具把要发送的文件写到当前工作区；Write 成功后 TOKENICODE 会自动上传并发送到微信。\
如果要发送电脑里已经存在的图片或文件，不要覆盖原文件；请用 Write 写一个名为 tokenicode-wechat-send-files.json 的 JSON 清单，内容格式为 {\"send_files\":[\"/absolute/path/to/file\"]}，TOKENICODE 会上传并发送清单里的真实文件。\
png、jpg、jpeg、jpe、jfif、gif、webp、bmp、svg、ico 等图片路径会作为微信图片发送，其他路径会作为微信文件发送。\
不要因为不能直接操作微信而说通道不支持，也不要用 Bash 重定向或 cp 作为要发送文件的最终生成动作。";

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
                    "content": wechat_remote_user_content_for_claude(text),
                },
            });
            stdin_mgr
                .send(desktop_session_id, &payload.to_string())
                .await?;
            Ok(true)
        }
        WechatTurnEffect::SendClaudeSlashCommand {
            desktop_session_id,
            command,
        } => {
            stdin_mgr.send(desktop_session_id, command).await?;
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
    execute_turn_effects_with_media_and_upload(
        stdin_mgr,
        store,
        effects,
        |request| async move {
            IlinkApiClient::new(None)
                .execute_json::<Value>(request)
                .await
        },
        download_cdn_media_bytes,
        upload_cdn_media_bytes,
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
    execute_turn_effects_with_media_and_upload(
        stdin_mgr,
        store,
        effects,
        execute_wechat_request,
        download_cdn_media_bytes,
        upload_cdn_media_bytes,
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
    execute_turn_effects_with_media_and_upload(
        stdin_mgr,
        store,
        effects,
        &mut execute_wechat_request,
        &mut download_media,
        upload_cdn_media_bytes,
    )
    .await
}

pub async fn execute_turn_effects_with_media_and_upload<F, Fut, G, Gut, H, Hut>(
    stdin_mgr: &StdinManager,
    store: &WechatStateStore,
    effects: &[WechatTurnEffect],
    mut execute_wechat_request: F,
    mut download_media: G,
    mut upload_media: H,
) -> Result<WechatEffectDispatch, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
    G: FnMut(WechatCdnDownloadRequest) -> Gut,
    Gut: Future<Output = Result<Vec<u8>, String>>,
    H: FnMut(WechatCdnUploadRequest) -> Hut,
    Hut: Future<Output = Result<String, String>>,
{
    let mut dispatch = WechatEffectDispatch::default();
    for effect in effects {
        let mut desktop_user_message = desktop_user_message_for_text_effect(effect);
        if let Some(clear) = desktop_clear_conversation_for_effect(effect) {
            dispatch.desktop_clear_conversations.push(clear);
        }
        if let Some(stop) = desktop_stop_for_effect(effect) {
            dispatch.desktop_stops.push(stop);
        }
        let mut handled_claude = match execute_claude_effect(stdin_mgr, effect).await {
            Ok(handled) => handled,
            Err(err) if is_stale_clear_slash_effect(effect) => {
                eprintln!(
                    "[WeChat] clear slash command skipped because desktop route is stale: {err}"
                );
                false
            }
            Err(err) => return Err(err),
        };
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
        if execute_wechat_effect_with_upload(
            effect,
            store,
            &mut execute_wechat_request,
            &mut upload_media,
        )
        .await?
        {
            dispatch.wechat_effect_count += 1;
        }
    }
    Ok(dispatch)
}

fn is_stale_clear_slash_effect(effect: &WechatTurnEffect) -> bool {
    matches!(
        effect,
        WechatTurnEffect::SendClaudeSlashCommand { command, .. }
            if command.trim().eq_ignore_ascii_case("/clear")
    )
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
            "content": wechat_remote_user_content_for_claude(&content),
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
    execute_wechat_effect_with_upload(
        effect,
        store,
        |request| async move {
            IlinkApiClient::new(None)
                .execute_json::<Value>(request)
                .await
        },
        upload_cdn_media_bytes,
    )
    .await
}

pub async fn execute_wechat_effect_with<F, Fut>(
    effect: &WechatTurnEffect,
    store: &WechatStateStore,
    execute_request: F,
) -> Result<bool, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    execute_wechat_effect_with_upload(effect, store, execute_request, upload_cdn_media_bytes).await
}

pub async fn execute_wechat_effect_with_upload<F, Fut, H, Hut>(
    effect: &WechatTurnEffect,
    store: &WechatStateStore,
    mut execute_request: F,
    mut upload_media: H,
) -> Result<bool, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
    H: FnMut(WechatCdnUploadRequest) -> Hut,
    Hut: Future<Output = Result<String, String>>,
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
            let preferences = store.load_preferences()?;
            let chunks = split_wechat_text_messages_with_preferences(
                &filtered_text,
                preferences.split_outbound_text_by_line_breaks,
            );
            eprintln!(
                "[WeChat] sending text reply as {} message(s), original_chars={}, filtered_chars={}, line_split={}",
                chunks.len(),
                text.chars().count(),
                filtered_text.chars().count(),
                preferences.split_outbound_text_by_line_breaks
            );
            for chunk in chunks {
                let request =
                    client.send_text_request(to_user_id, &context_token, &chunk, &new_client_id());
                execute_send_message_request(store, request, &mut execute_request).await?;
            }
            Ok(true)
        }
        WechatTurnEffect::SendWeChatFile {
            to_user_id,
            context_token,
            path,
            caption,
        } => {
            let context_token = resolve_context_token(store, to_user_id, context_token)?;
            ensure_send_circuit_closed(store)?;
            execute_wechat_file_effect(
                &client,
                store,
                to_user_id,
                &context_token,
                path,
                caption.as_deref(),
                &mut execute_request,
                &mut upload_media,
            )
            .await?;
            Ok(true)
        }
        WechatTurnEffect::StartTyping {
            to_user_id,
            context_token,
        } => {
            let keepalive_key = typing_keepalive_key(&account.account_id, to_user_id);
            execute_typing_effect(
                &client,
                store,
                &keepalive_key,
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
            let keepalive_key = typing_keepalive_key(&account.account_id, to_user_id);
            execute_typing_effect(
                &client,
                store,
                &keepalive_key,
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

async fn execute_wechat_file_effect<F, Fut, H, Hut>(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    to_user_id: &str,
    context_token: &str,
    path: &str,
    caption: Option<&str>,
    execute_request: &mut F,
    upload_media: &mut H,
) -> Result<(), String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
    H: FnMut(WechatCdnUploadRequest) -> Hut,
    Hut: Future<Output = Result<String, String>>,
{
    let path = Path::new(path);
    if let Some(paths) = wechat_send_manifest_paths(path)? {
        eprintln!(
            "[WeChat] sending {} file(s) listed in manifest {}",
            paths.len(),
            path.display()
        );
        for path in paths {
            send_single_wechat_file(
                client,
                store,
                to_user_id,
                context_token,
                Path::new(&path),
                caption,
                execute_request,
                upload_media,
            )
            .await?;
        }
        return Ok(());
    }

    send_single_wechat_file(
        client,
        store,
        to_user_id,
        context_token,
        path,
        caption,
        execute_request,
        upload_media,
    )
    .await
}

async fn send_single_wechat_file<F, Fut, H, Hut>(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    to_user_id: &str,
    context_token: &str,
    path: &Path,
    caption: Option<&str>,
    execute_request: &mut F,
    upload_media: &mut H,
) -> Result<(), String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
    H: FnMut(WechatCdnUploadRequest) -> Hut,
    Hut: Future<Output = Result<String, String>>,
{
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("WeChat file send stat failed for {}: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "WeChat file send path is not a file: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_WECHAT_FILE_BYTES {
        return Err(format!(
            "WeChat file too large: {} bytes, max {} bytes",
            metadata.len(),
            MAX_WECHAT_FILE_BYTES
        ));
    }

    let plaintext = std::fs::read(path)
        .map_err(|err| format!("WeChat file send read failed for {}: {err}", path.display()))?;
    let raw_size = plaintext.len() as u64;
    let encrypted_size = aes_128_ecb_pkcs7_padded_size(plaintext.len()) as u64;
    let raw_md5 = format!("{:x}", md5::compute(&plaintext));
    let filekey = random_hex_16();
    let aes_key = random_bytes_16();
    let aeskey_hex = lower_hex(&aes_key);
    let is_image = is_wechat_image_path(path);
    let media_type = if is_image {
        UploadMediaType::Image
    } else {
        UploadMediaType::File
    };
    eprintln!(
        "[WeChat] uploading {} as {} to WeChat",
        path.display(),
        if is_image { "image" } else { "file" }
    );

    let upload_url_request = client.get_upload_url_request(
        &filekey,
        media_type,
        to_user_id,
        raw_size,
        &raw_md5,
        encrypted_size,
        &aeskey_hex,
    );
    let upload_url_response: GetUploadUrlResponse =
        parse_response(execute_request(upload_url_request).await?)?;
    if upload_url_response.ret.unwrap_or_default() != 0 {
        return Err(format!(
            "WeChat getuploadurl failed: ret={:?} errcode={:?} errmsg={:?}",
            upload_url_response.ret, upload_url_response.errcode, upload_url_response.errmsg
        ));
    }

    let encrypted = encrypt_aes_128_ecb_pkcs7(&plaintext, &aes_key)?;
    let upload_request = build_cdn_upload_request(
        upload_url_response.upload_param.as_deref(),
        upload_url_response.upload_full_url.as_deref(),
        &filekey,
        encrypted,
    )?;
    let encrypt_query_param = upload_media(upload_request).await?;
    let aes_key_base64 = BASE64_STANDARD.encode(aeskey_hex.as_bytes());
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "file".into());
    let caption = caption.map(filter_wechat_markdown);
    let caption = caption.as_deref();
    let send_request = if is_image {
        client.send_image_request(
            to_user_id,
            context_token,
            &encrypt_query_param,
            &aes_key_base64,
            encrypted_size,
            caption,
            &new_client_id(),
        )
    } else {
        client.send_file_request(
            to_user_id,
            context_token,
            &encrypt_query_param,
            &aes_key_base64,
            &file_name,
            raw_size,
            caption,
            &new_client_id(),
        )
    };

    execute_send_message_request(store, send_request, execute_request).await
}

fn wechat_send_manifest_paths(path: &Path) -> Result<Option<Vec<String>>, String> {
    let is_manifest = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value == WECHAT_SEND_MANIFEST_BASENAME)
        .unwrap_or(false);
    if !is_manifest {
        return Ok(None);
    }

    let content = std::fs::read_to_string(path).map_err(|err| {
        format!(
            "WeChat send manifest read failed for {}: {err}",
            path.display()
        )
    })?;
    let value: Value = serde_json::from_str(&content).map_err(|err| {
        format!(
            "WeChat send manifest must be JSON at {}: {err}",
            path.display()
        )
    })?;
    let paths_value = value
        .get("send_files")
        .or_else(|| value.get("paths"))
        .unwrap_or(&value);
    let Some(paths) = paths_value.as_array() else {
        return Err(format!(
            "WeChat send manifest must contain a send_files array: {}",
            path.display()
        ));
    };

    let paths: Vec<String> = paths
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(MAX_WECHAT_SEND_MANIFEST_FILES)
        .map(ToOwned::to_owned)
        .collect();
    if paths.is_empty() {
        return Err(format!(
            "WeChat send manifest has no file paths: {}",
            path.display()
        ));
    }

    Ok(Some(paths))
}

async fn execute_send_message_request<F, Fut>(
    store: &WechatStateStore,
    request: IlinkHttpRequest,
    execute_request: &mut F,
) -> Result<(), String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    ensure_send_circuit_closed(store)?;
    let response: SendMessageResponse = parse_response(execute_request(request).await?)?;
    match classify_send_response(&response) {
        SendResponseClass::Ok => Ok(()),
        SendResponseClass::RateLimited => {
            trip_send_circuit(store)?;
            Err("WeChat sendmessage rate limited".into())
        }
        SendResponseClass::StaleSession => {
            Err("WeChat sendmessage stale session; reconnect required".into())
        }
        SendResponseClass::Failed => Err(format!(
            "WeChat sendmessage failed: ret={:?} errcode={:?} errmsg={:?}",
            response.ret, response.errcode, response.errmsg
        )),
    }
}

fn ensure_send_circuit_closed(store: &WechatStateStore) -> Result<(), String> {
    let Some(open_until_ms) = store.load_send_circuit_open_until()? else {
        return Ok(());
    };
    let now = now_ms();
    if now >= open_until_ms {
        store.clear_send_circuit()?;
        return Ok(());
    }

    Err(format!(
        "WeChat sendmessage circuit breaker open, {}ms remaining",
        open_until_ms.saturating_sub(now)
    ))
}

fn trip_send_circuit(store: &WechatStateStore) -> Result<(), String> {
    store.save_send_circuit_open_until(now_ms().saturating_add(SEND_CIRCUIT_OPEN_MS))
}

async fn execute_typing_effect<F, Fut>(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    keepalive_key: &str,
    to_user_id: &str,
    context_token: &str,
    status: TypingStatus,
    execute_request: &mut F,
) -> Result<bool, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    if status == TypingStatus::Stop {
        stop_typing_keepalive(keepalive_key);
    }

    let Some(ticket) =
        resolve_typing_ticket(client, store, to_user_id, context_token, execute_request).await
    else {
        return Ok(true);
    };

    let sent = send_typing_best_effort(client, to_user_id, &ticket, status, execute_request).await;
    if status == TypingStatus::Start && sent {
        start_typing_keepalive(
            keepalive_key.to_string(),
            client.clone(),
            to_user_id.to_string(),
            ticket,
        );
    }
    Ok(true)
}

async fn resolve_typing_ticket<F, Fut>(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    to_user_id: &str,
    context_token: &str,
    execute_request: &mut F,
) -> Option<String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    match store.load_typing_ticket(to_user_id) {
        Ok(Some(cached))
            if !cached.ticket.trim().is_empty()
                && now_ms().saturating_sub(cached.fetched_at_ms) < TYPING_TICKET_TTL_MS =>
        {
            return Some(cached.ticket.trim().to_string());
        }
        Ok(_) => {}
        Err(err) => eprintln!("[WeChat] load typing ticket cache failed: {err}"),
    }

    let config_request = client.get_config_request(to_user_id, Some(context_token));
    let config_value = match execute_request(config_request).await {
        Ok(value) => value,
        Err(err) => {
            eprintln!("[WeChat] getconfig for typing failed: {err}");
            return None;
        }
    };
    let config: GetConfigResponse = match parse_response(config_value) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("[WeChat] parse getconfig for typing failed: {err}");
            return None;
        }
    };
    if config.ret.unwrap_or_default() != 0 {
        return None;
    }
    let ticket = config
        .typing_ticket
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;

    if let Err(err) = store.save_typing_ticket(to_user_id, &ticket, now_ms()) {
        eprintln!("[WeChat] save typing ticket cache failed: {err}");
    }
    Some(ticket)
}

async fn send_typing_best_effort<F, Fut>(
    client: &IlinkApiClient,
    to_user_id: &str,
    ticket: &str,
    status: TypingStatus,
    execute_request: &mut F,
) -> bool
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let typing_request = client.send_typing_request(to_user_id, ticket, status);
    match execute_request(typing_request).await {
        Ok(value) => {
            let ret = value.get("ret").and_then(Value::as_i64).unwrap_or_default();
            if ret != 0 {
                let errmsg = value
                    .get("errmsg")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                eprintln!("[WeChat] sendtyping returned ret={ret} errmsg={errmsg}");
                return false;
            }
            true
        }
        Err(err) => {
            eprintln!("[WeChat] sendtyping failed: {err}");
            false
        }
    }
}

fn typing_keepalive_key(account_id: &str, to_user_id: &str) -> String {
    format!("{account_id}:{to_user_id}")
}

#[cfg(not(test))]
fn start_typing_keepalive(key: String, client: IlinkApiClient, to_user_id: String, ticket: String) {
    stop_typing_keepalive(&key);
    let handle = tokio::spawn(async move {
        run_typing_keepalive_loop(TYPING_KEEPALIVE_INTERVAL_MS, || {
            let client = client.clone();
            let request = client.send_typing_request(&to_user_id, &ticket, TypingStatus::Start);
            async move {
                let value = client
                    .execute_json::<Value>(request)
                    .await
                    .map_err(|err| format!("sendtyping keepalive failed: {err}"))?;
                let ret = value.get("ret").and_then(Value::as_i64).unwrap_or_default();
                if ret == 0 {
                    Ok(())
                } else {
                    let errmsg = value
                        .get("errmsg")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Err(format!(
                        "sendtyping keepalive returned ret={ret} errmsg={errmsg}"
                    ))
                }
            }
        })
        .await;
    });
    TYPING_KEEPALIVE_TASKS.lock().unwrap().insert(key, handle);
}

#[cfg(test)]
fn start_typing_keepalive(
    _key: String,
    _client: IlinkApiClient,
    _to_user_id: String,
    _ticket: String,
) {
}

#[cfg(not(test))]
fn stop_typing_keepalive(key: &str) {
    if let Some(handle) = TYPING_KEEPALIVE_TASKS.lock().unwrap().remove(key) {
        handle.abort();
    }
}

#[cfg(test)]
fn stop_typing_keepalive(_key: &str) {}

async fn run_typing_keepalive_loop<F, Fut>(interval_ms: u64, mut send_start: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    loop {
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        if let Err(err) = send_start().await {
            eprintln!("[WeChat] {err}");
            return;
        }
    }
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

async fn upload_cdn_media_bytes(request: WechatCdnUploadRequest) -> Result<String, String> {
    let response = reqwest::Client::new()
        .post(&request.url)
        .timeout(Duration::from_millis(request.timeout_ms))
        .header("Content-Type", "application/octet-stream")
        .body(request.encrypted_body)
        .send()
        .await
        .map_err(|err| format!("WeChat CDN upload failed: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("WeChat CDN upload HTTP {status}"));
    }
    response
        .headers()
        .get("x-encrypted-param")
        .ok_or_else(|| "WeChat CDN upload response missing x-encrypted-param".to_string())?
        .to_str()
        .map(|value| value.to_string())
        .map_err(|err| format!("WeChat CDN upload x-encrypted-param invalid: {err}"))
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

fn wechat_remote_user_content_for_claude(user_content: &str) -> String {
    format!("{WECHAT_REMOTE_CLAUDE_INSTRUCTION}\n\n用户消息：\n{user_content}")
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

fn desktop_clear_conversation_for_effect(
    effect: &WechatTurnEffect,
) -> Option<WechatDesktopClearConversation> {
    let WechatTurnEffect::ClearDesktopConversation { desktop_session_id } = effect else {
        return None;
    };

    Some(WechatDesktopClearConversation {
        desktop_session_id: desktop_session_id.clone(),
    })
}

fn desktop_stop_for_effect(effect: &WechatTurnEffect) -> Option<WechatDesktopStop> {
    let WechatTurnEffect::InterruptClaude { desktop_session_id } = effect else {
        return None;
    };

    Some(WechatDesktopStop {
        desktop_session_id: desktop_session_id.clone(),
        source: "wechat".into(),
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

fn is_wechat_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "jpe" | "jfif" | "gif" | "webp" | "bmp" | "svg" | "ico"
            )
        })
        .unwrap_or(false)
}

fn random_bytes_16() -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn random_hex_16() -> String {
    lower_hex(&random_bytes_16())
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::{Child, Command};
    use tokio::time::{timeout, Duration};

    use crate::wechat::{
        api::wechat_channel_version,
        inbound::{InboundWechatCdnMedia, InboundWechatMedia, InboundWechatMediaKind},
        media::{WechatCdnDownloadRequest, WechatCdnUploadRequest},
        store::{WechatAccount, WechatStateStore},
    };

    #[tokio::test]
    async fn send_to_claude_effect_instructs_claude_how_to_send_wechat_files() {
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
        let content = actual["message"]["content"].as_str().unwrap();
        assert_eq!(actual["type"], "user");
        assert_eq!(actual["message"]["role"], "user");
        assert!(content.contains("这是 TOKENICODE 的微信接入会话"));
        assert!(content.contains("请使用 Write 工具"));
        assert!(content.contains("tokenicode-wechat-send-files.json"));
        assert!(content.contains("\"send_files\""));
        assert!(content.contains("图片路径会作为微信图片发送"));
        assert!(content.contains("TOKENICODE 会自动上传并发送到微信"));
        assert!(content.contains("不要因为不能直接操作微信而说通道不支持"));
        assert!(content.ends_with("hello from WeChat"));

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn slash_command_effect_writes_raw_command_to_existing_stdin_manager() {
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        let handled = execute_claude_effect(
            &stdin_mgr,
            &WechatTurnEffect::SendClaudeSlashCommand {
                desktop_session_id: "stdin-1".into(),
                command: "/clear".into(),
            },
        )
        .await
        .unwrap();

        let line = lines.next_line().await.unwrap().unwrap();
        assert!(handled);
        assert_eq!(line, "/clear");

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn clear_desktop_conversation_effect_is_reported_to_frontend_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let stdin_mgr = StdinManager::new();

        let dispatch = execute_turn_effects_with(
            &stdin_mgr,
            &store,
            &[WechatTurnEffect::ClearDesktopConversation {
                desktop_session_id: "stdin-1".into(),
            }],
            |_request| async { Ok(json!({ "ret": 0 })) },
        )
        .await
        .unwrap();

        assert_eq!(dispatch.claude_effect_count, 0);
        assert_eq!(dispatch.wechat_effect_count, 0);
        assert_eq!(
            dispatch.desktop_clear_conversations,
            vec![WechatDesktopClearConversation {
                desktop_session_id: "stdin-1".into(),
            }]
        );
    }

    #[tokio::test]
    async fn clear_context_continues_when_desktop_stdin_route_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let stdin_mgr = StdinManager::new();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let dispatch = execute_turn_effects_with(
            &stdin_mgr,
            &store,
            &[
                WechatTurnEffect::ClearDesktopConversation {
                    desktop_session_id: "stale-stdin".into(),
                },
                WechatTurnEffect::SendClaudeSlashCommand {
                    desktop_session_id: "stale-stdin".into(),
                    command: "/clear".into(),
                },
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                    text: "cleared".into(),
                },
            ],
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

        assert_eq!(dispatch.claude_effect_count, 0);
        assert_eq!(dispatch.wechat_effect_count, 1);
        assert_eq!(
            dispatch.desktop_clear_conversations,
            vec![WechatDesktopClearConversation {
                desktop_session_id: "stale-stdin".into(),
            }]
        );
        assert_eq!(requests.lock().unwrap().len(), 1);
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
        assert!(content.contains("这是 TOKENICODE 的微信接入会话"));
        assert!(content.contains("微信发来的图片"));
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
    async fn interrupt_effect_is_reported_as_wechat_desktop_stop_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        let dispatch = execute_turn_effects_with(
            &stdin_mgr,
            &store,
            &[WechatTurnEffect::InterruptClaude {
                desktop_session_id: "stdin-1".into(),
            }],
            |_request| async { Ok(json!({ "ret": 0 })) },
        )
        .await
        .unwrap();

        let line = timeout(Duration::from_millis(200), lines.next_line())
            .await
            .expect("interrupt effect wrote no control request")
            .unwrap()
            .unwrap();
        let actual: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(actual["request"]["subtype"], "interrupt");
        assert_eq!(
            dispatch.desktop_stops,
            vec![WechatDesktopStop {
                desktop_session_id: "stdin-1".into(),
                source: "wechat".into(),
            }]
        );

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
        assert_eq!(requests.len(), 3);
        let texts: Vec<&str> = requests
            .iter()
            .map(|request| {
                request.body["msg"]["item_list"][0]["text_item"]["text"]
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "标题",
                "引用",
                "**bold** 中文 删除 \n```ts\nconst x = \"~~keep~~\";\n```",
            ]
        );
    }

    #[tokio::test]
    async fn send_wechat_text_effect_splits_claude_line_breaks_into_wechat_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "嗯，这个得看你们几个人、怎么用。\n\n先说结论：如果团队不超过 5-6 个人，我倾向 NAS。\n但坚果云赢在不用管。\n```ts\nconst size = \"50G\";\nconsole.log(size);\n```\n下一步看文件大小。".into(),
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
        let texts: Vec<&str> = requests
            .iter()
            .map(|request| {
                request.body["msg"]["item_list"][0]["text_item"]["text"]
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "嗯，这个得看你们几个人、怎么用。",
                "先说结论：如果团队不超过 5-6 个人，我倾向 NAS。",
                "但坚果云赢在不用管。\n```ts\nconst size = \"50G\";\nconsole.log(size);\n```",
                "下一步看文件大小。",
            ]
        );
    }

    #[tokio::test]
    async fn send_wechat_text_effect_can_keep_line_breaks_in_one_wechat_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        store
            .save_preferences(&crate::wechat::store::WechatPreferences {
                split_outbound_text_by_line_breaks: false,
            })
            .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "第一段\n第二段\n第三段".into(),
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
        assert_eq!(
            requests[0].body["msg"]["item_list"][0]["text_item"]["text"],
            "第一段\n第二段\n第三段"
        );
    }

    #[tokio::test]
    async fn send_wechat_text_rate_limit_opens_circuit_and_next_send_fails_fast() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let first_error = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "first".into(),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(json!({ "ret": -2, "errmsg": "frequency limited" }))
                }
            },
        )
        .await
        .unwrap_err();

        assert!(first_error.contains("rate limited"));

        let captured = requests.clone();
        let second_error = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "second".into(),
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
        .unwrap_err();

        assert!(second_error.contains("circuit breaker open"));
        assert_eq!(requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stale_session_response_does_not_open_rate_limit_circuit() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let first_error = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "first".into(),
            },
            &store,
            move |request| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(request);
                    Ok(json!({ "ret": -2, "errmsg": "unknown error" }))
                }
            },
        )
        .await
        .unwrap_err();

        assert!(first_error.contains("stale session"));

        let captured = requests.clone();
        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "second".into(),
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
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn expired_send_circuit_allows_next_message_and_clears_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        store.save_send_circuit_open_until(1).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();

        let handled = execute_wechat_effect_with(
            &WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "after cooldown".into(),
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
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert_eq!(store.load_send_circuit_open_until().unwrap(), None);
    }

    #[tokio::test]
    async fn send_wechat_file_effect_fails_fast_without_upload_when_circuit_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        store
            .save_send_circuit_open_until(now_ms().saturating_add(30_000))
            .unwrap();
        let file_path = dir.path().join("generated.png");
        std::fs::write(&file_path, b"hello generated image").unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let uploads = Arc::new(Mutex::new(Vec::<WechatCdnUploadRequest>::new()));
        let captured_requests = requests.clone();
        let captured_uploads = uploads.clone();

        let error = execute_wechat_effect_with_upload(
            &WechatTurnEffect::SendWeChatFile {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                path: file_path.display().to_string(),
                caption: None,
            },
            &store,
            move |request| {
                let captured_requests = captured_requests.clone();
                async move {
                    captured_requests.lock().unwrap().push(request);
                    Ok(json!({ "ret": 0 }))
                }
            },
            move |request| {
                let captured_uploads = captured_uploads.clone();
                async move {
                    captured_uploads.lock().unwrap().push(request);
                    Ok("download-param".to_string())
                }
            },
        )
        .await
        .unwrap_err();

        assert!(error.contains("circuit breaker open"));
        assert!(requests.lock().unwrap().is_empty());
        assert!(uploads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn send_wechat_file_effect_uploads_image_and_posts_image_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let file_path = dir.path().join("generated.png");
        std::fs::write(&file_path, b"hello generated image").unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let uploads = Arc::new(Mutex::new(Vec::<WechatCdnUploadRequest>::new()));
        let captured_requests = requests.clone();
        let captured_uploads = uploads.clone();
        let stdin_mgr = StdinManager::new();

        let dispatch = execute_turn_effects_with_media_and_upload(
            &stdin_mgr,
            &store,
            &[WechatTurnEffect::SendWeChatFile {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                path: file_path.display().to_string(),
                caption: None,
            }],
            move |request| {
                let captured_requests = captured_requests.clone();
                async move {
                    let mut requests = captured_requests.lock().unwrap();
                    let is_upload_request = request.url.ends_with("/ilink/bot/getuploadurl");
                    requests.push(request);
                    if is_upload_request {
                        Ok(json!({ "ret": 0, "upload_param": "upload=param" }))
                    } else {
                        Ok(json!({ "ret": 0 }))
                    }
                }
            },
            |_request| async { unreachable!("image upload should not download media") },
            move |request| {
                let captured_uploads = captured_uploads.clone();
                async move {
                    captured_uploads.lock().unwrap().push(request);
                    Ok("download-param".to_string())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(dispatch.wechat_effect_count, 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);

        let upload_url_request = &requests[0];
        assert_eq!(
            upload_url_request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/getuploadurl"
        );
        assert_eq!(upload_url_request.body["media_type"], 1);
        assert_eq!(upload_url_request.body["to_user_id"], "user-1");
        assert_eq!(upload_url_request.body["rawsize"], 21);
        assert_eq!(upload_url_request.body["filesize"], 32);
        assert_eq!(upload_url_request.body["no_need_thumb"], true);
        assert_eq!(
            upload_url_request.body["base_info"]["channel_version"],
            serde_json::Value::String(wechat_channel_version())
        );
        let filekey = upload_url_request.body["filekey"].as_str().unwrap();
        assert_eq!(filekey.len(), 32);
        let aeskey = upload_url_request.body["aeskey"].as_str().unwrap();
        assert_eq!(aeskey.len(), 32);

        let uploads = uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            uploads[0].url,
            format!(
                "https://novac2c.cdn.weixin.qq.com/c2c/upload?encrypted_query_param=upload%3Dparam&filekey={filekey}"
            )
        );
        assert_eq!(uploads[0].encrypted_body.len(), 32);
        assert_ne!(uploads[0].encrypted_body, b"hello generated image");

        let send_message_request = &requests[1];
        assert_eq!(
            send_message_request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendmessage"
        );
        assert_eq!(send_message_request.body["msg"]["to_user_id"], "user-1");
        assert_eq!(send_message_request.body["msg"]["context_token"], "ctx-1");
        assert_eq!(send_message_request.body["msg"]["item_list"][0]["type"], 2);
        assert_eq!(
            send_message_request.body["msg"]["item_list"][0]["image_item"]["media"]
                ["encrypt_query_param"],
            "download-param"
        );
        assert_eq!(
            send_message_request.body["msg"]["item_list"][0]["image_item"]["media"]["aes_key"],
            BASE64_STANDARD.encode(aeskey.as_bytes())
        );
        assert_eq!(
            send_message_request.body["msg"]["item_list"][0]["image_item"]["mid_size"],
            32
        );
    }

    #[tokio::test]
    async fn send_wechat_file_effect_uploads_non_image_as_file_message() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let file_path = dir.path().join("report.txt");
        let plaintext = b"hello generated document";
        std::fs::write(&file_path, plaintext).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let uploads = Arc::new(Mutex::new(Vec::<WechatCdnUploadRequest>::new()));
        let captured_requests = requests.clone();
        let captured_uploads = uploads.clone();
        let stdin_mgr = StdinManager::new();

        let dispatch = execute_turn_effects_with_media_and_upload(
            &stdin_mgr,
            &store,
            &[WechatTurnEffect::SendWeChatFile {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                path: file_path.display().to_string(),
                caption: Some("##### 附件\n*中文*".into()),
            }],
            move |request| {
                let captured_requests = captured_requests.clone();
                async move {
                    let mut requests = captured_requests.lock().unwrap();
                    let is_upload_request = request.url.ends_with("/ilink/bot/getuploadurl");
                    requests.push(request);
                    if is_upload_request {
                        Ok(json!({
                            "ret": 0,
                            "upload_full_url": "https://novac2c.cdn.weixin.qq.com/c2c/full-upload"
                        }))
                    } else {
                        Ok(json!({ "ret": 0 }))
                    }
                }
            },
            |_request| async { unreachable!("file upload should not download media") },
            move |request| {
                let captured_uploads = captured_uploads.clone();
                async move {
                    captured_uploads.lock().unwrap().push(request);
                    Ok("download-param".to_string())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(dispatch.wechat_effect_count, 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body["media_type"], 3);
        assert_eq!(requests[0].body["rawsize"], plaintext.len() as u64);
        let uploads = uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        assert_eq!(
            uploads[0].url,
            "https://novac2c.cdn.weixin.qq.com/c2c/full-upload"
        );
        assert_ne!(uploads[0].encrypted_body, plaintext);

        let send_message_request = &requests[1];
        assert_eq!(send_message_request.body["msg"]["item_list"][0]["type"], 1);
        assert_eq!(
            send_message_request.body["msg"]["item_list"][0]["text_item"]["text"],
            "附件\n中文"
        );
        assert_eq!(send_message_request.body["msg"]["item_list"][1]["type"], 4);
        assert_eq!(
            send_message_request.body["msg"]["item_list"][1]["file_item"]["media"]
                ["encrypt_query_param"],
            "download-param"
        );
        assert_eq!(
            send_message_request.body["msg"]["item_list"][1]["file_item"]["file_name"],
            "report.txt"
        );
        assert_eq!(
            send_message_request.body["msg"]["item_list"][1]["file_item"]["len"],
            plaintext.len().to_string()
        );
    }

    #[tokio::test]
    async fn send_wechat_file_manifest_uploads_listed_image_and_file_paths() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let image_path = dir.path().join("existing.jpeg");
        let file_path = dir.path().join("report.txt");
        let manifest_path = dir.path().join(WECHAT_SEND_MANIFEST_BASENAME);
        std::fs::write(&image_path, b"existing image bytes").unwrap();
        std::fs::write(&file_path, b"existing file bytes").unwrap();
        std::fs::write(
            &manifest_path,
            format!(
                r#"{{"send_files":["{}","{}"]}}"#,
                image_path.display(),
                file_path.display()
            ),
        )
        .unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let uploads = Arc::new(Mutex::new(Vec::<WechatCdnUploadRequest>::new()));
        let captured_requests = requests.clone();
        let captured_uploads = uploads.clone();
        let stdin_mgr = StdinManager::new();

        let dispatch = execute_turn_effects_with_media_and_upload(
            &stdin_mgr,
            &store,
            &[WechatTurnEffect::SendWeChatFile {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                path: manifest_path.display().to_string(),
                caption: None,
            }],
            move |request| {
                let captured_requests = captured_requests.clone();
                async move {
                    let mut requests = captured_requests.lock().unwrap();
                    let is_upload_request = request.url.ends_with("/ilink/bot/getuploadurl");
                    requests.push(request);
                    if is_upload_request {
                        Ok(json!({ "ret": 0, "upload_param": "upload=param" }))
                    } else {
                        Ok(json!({ "ret": 0 }))
                    }
                }
            },
            |_request| async { unreachable!("manifest upload should not download media") },
            move |request| {
                let captured_uploads = captured_uploads.clone();
                async move {
                    captured_uploads.lock().unwrap().push(request);
                    Ok("download-param".to_string())
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(dispatch.wechat_effect_count, 1);
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].body["media_type"], 1);
        assert_eq!(requests[1].body["msg"]["item_list"][0]["type"], 2);
        assert_eq!(requests[2].body["media_type"], 3);
        assert_eq!(requests[3].body["msg"]["item_list"][0]["type"], 4);
        assert_eq!(
            requests[3].body["msg"]["item_list"][0]["file_item"]["file_name"],
            "report.txt"
        );
        assert_eq!(uploads.lock().unwrap().len(), 2);
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
            first_line
        );
        assert_eq!(
            requests[1].body["msg"]["item_list"][0]["text_item"]["text"],
            second_line
        );
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
    async fn typing_effect_reuses_cached_ticket_without_getconfig() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        store
            .save_typing_ticket("user-1", "cached-ticket", now_ms())
            .unwrap();
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
        assert_eq!(
            requests[0].url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendtyping"
        );
        assert_eq!(requests[0].body["typing_ticket"], "cached-ticket");
        assert_eq!(requests[0].body["status"], 1);
    }

    #[tokio::test]
    async fn typing_effect_refreshes_expired_cached_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        store
            .save_typing_ticket("user-1", "stale-ticket", 1)
            .unwrap();
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
                        Ok(json!({ "ret": 0, "typing_ticket": "fresh-ticket" }))
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
        assert_eq!(requests[1].body["typing_ticket"], "fresh-ticket");
        drop(requests);
        assert_eq!(
            store.load_typing_ticket("user-1").unwrap().unwrap().ticket,
            "fresh-ticket"
        );
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

    #[tokio::test]
    async fn typing_keepalive_loop_repeats_until_send_fails() {
        let sends = Arc::new(Mutex::new(0usize));
        let captured = sends.clone();

        run_typing_keepalive_loop(1, move || {
            let captured = captured.clone();
            async move {
                let mut sends = captured.lock().unwrap();
                *sends += 1;
                if *sends >= 3 {
                    Err("stop keepalive".into())
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert_eq!(*sends.lock().unwrap(), 3);
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
