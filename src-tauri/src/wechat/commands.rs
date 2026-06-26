use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use qrcode::{render::svg, QrCode};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{
    commands::StdinManager,
    wechat::{
        api::{IlinkApiClient, QrCodeResponse, QrStatusResponse},
        executor::{execute_wechat_effect, execute_wechat_lifecycle_effect, WechatLifecycleEffect},
        login::{parse_qr_code_response, parse_qr_status_response, WechatQrPoll},
        poller::WechatPollingTask,
        runtime::WechatRuntimeHandle,
        store::{default_wechat_state_store, WechatAccount, WechatStateStore},
    },
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatAccountInfo {
    pub account_id: String,
    pub user_id: String,
    pub base_url: String,
}

impl From<WechatAccount> for WechatAccountInfo {
    fn from(account: WechatAccount) -> Self {
        Self {
            account_id: account.account_id,
            user_id: account.user_id,
            base_url: account.base_url,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatStatusResponse {
    pub connected: bool,
    pub account: Option<WechatAccountInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatQrStartResponse {
    pub qrcode_id: String,
    pub qrcode_image: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatQrPollResponse {
    pub status: String,
    pub connected: bool,
    pub message: Option<String>,
    pub account: Option<WechatAccountInfo>,
    pub redirect_base_url: Option<String>,
}

#[tauri::command]
pub async fn wechat_get_status(
    app: AppHandle,
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
    stdin_mgr: State<'_, StdinManager>,
) -> Result<WechatStatusResponse, String> {
    let account = state_store().load_account()?;
    if account.is_some() {
        if start_polling_task_if_desktop_session(
            runtime.inner(),
            polling_task.inner(),
            stdin_mgr.inner(),
            Some(app),
        )
        .await
        {
            notify_lifecycle(WechatLifecycleEffect::NotifyStart, &state_store()).await;
        }
    }
    Ok(WechatStatusResponse {
        connected: account.is_some(),
        account: account.map(WechatAccountInfo::from),
    })
}

#[tauri::command]
pub async fn wechat_start_qr_login() -> Result<WechatQrStartResponse, String> {
    let client = IlinkApiClient::new(None);
    let response: QrCodeResponse = client.execute_json(client.qr_code_request()).await?;
    let parsed = parse_qr_code_response(response)?;
    Ok(WechatQrStartResponse {
        qrcode_id: parsed.qrcode_id,
        qrcode_image: normalize_qr_image(&parsed.qrcode_image),
    })
}

#[tauri::command]
pub async fn wechat_poll_qr_login(
    app: AppHandle,
    qrcode_id: String,
    verify_code: Option<String>,
    base_url: Option<String>,
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
    stdin_mgr: State<'_, StdinManager>,
) -> Result<WechatQrPollResponse, String> {
    let client = match base_url.filter(|value| !value.trim().is_empty()) {
        Some(base_url) => IlinkApiClient::with_base_url(None, base_url),
        None => IlinkApiClient::new(None),
    };
    let response: QrStatusResponse = client
        .execute_json(client.qr_status_request(&qrcode_id, verify_code.as_deref()))
        .await?;
    match parse_qr_status_response(response, now_ms())? {
        WechatQrPoll::Connected { account } => {
            let store = state_store();
            store.save_account(&account)?;
            notify_lifecycle(WechatLifecycleEffect::NotifyStart, &store).await;
            start_polling_task_if_desktop_session(
                runtime.inner(),
                polling_task.inner(),
                stdin_mgr.inner(),
                Some(app),
            )
            .await;
            Ok(WechatQrPollResponse {
                status: "connected".into(),
                connected: true,
                message: None,
                account: Some(WechatAccountInfo::from(account)),
                redirect_base_url: None,
            })
        }
        WechatQrPoll::Pending {
            status,
            message,
            redirect_base_url,
        } => Ok(WechatQrPollResponse {
            status,
            connected: false,
            message,
            account: None,
            redirect_base_url,
        }),
        WechatQrPoll::Failed { status, message } => Ok(WechatQrPollResponse {
            status,
            connected: false,
            message: Some(message),
            account: None,
            redirect_base_url: None,
        }),
    }
}

#[tauri::command]
pub async fn wechat_disconnect(
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
) -> Result<(), String> {
    polling_task.stop().await;
    let effects = runtime.disconnect().await;
    let store = runtime.state_store().await;
    for effect in &effects {
        if let Err(err) = execute_wechat_effect(effect, &store).await {
            eprintln!("[WeChat] disconnect effect failed: {err}");
        }
    }
    notify_lifecycle(WechatLifecycleEffect::NotifyStop, &store).await;
    store.clear_account()
}

#[tauri::command]
pub async fn wechat_start_polling(
    app: AppHandle,
    session_id: String,
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
    stdin_mgr: State<'_, StdinManager>,
) -> Result<(), String> {
    set_desktop_session(runtime.inner(), Some(session_id)).await;
    if start_polling_task(
        runtime.inner(),
        polling_task.inner(),
        stdin_mgr.inner(),
        Some(app),
    )
    .await
    {
        let store = runtime.state_store().await;
        notify_lifecycle(WechatLifecycleEffect::NotifyStart, &store).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn wechat_stop_polling(
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
) -> Result<(), String> {
    if polling_task.stop().await {
        let store = runtime.state_store().await;
        notify_lifecycle(WechatLifecycleEffect::NotifyStop, &store).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn wechat_set_desktop_session(
    session_id: Option<String>,
    runtime: State<'_, WechatRuntimeHandle>,
) -> Result<(), String> {
    set_desktop_session(runtime.inner(), session_id).await;
    Ok(())
}

async fn start_polling_task(
    runtime: &WechatRuntimeHandle,
    polling_task: &WechatPollingTask,
    stdin_mgr: &StdinManager,
    app: Option<AppHandle>,
) -> bool {
    polling_task
        .start(runtime.clone(), stdin_mgr.clone(), app)
        .await
}

async fn start_polling_task_if_desktop_session(
    runtime: &WechatRuntimeHandle,
    polling_task: &WechatPollingTask,
    stdin_mgr: &StdinManager,
    app: Option<AppHandle>,
) -> bool {
    if runtime.desktop_session_id().await.is_some() {
        start_polling_task(runtime, polling_task, stdin_mgr, app).await
    } else {
        false
    }
}

async fn set_desktop_session(runtime: &WechatRuntimeHandle, session_id: Option<String>) {
    match session_id.filter(|value| !value.trim().is_empty()) {
        Some(session_id) => runtime.set_desktop_session(session_id).await,
        None => runtime.clear_desktop_session().await,
    }
}

async fn notify_lifecycle(effect: WechatLifecycleEffect, store: &WechatStateStore) {
    if let Err(err) = execute_wechat_lifecycle_effect(effect, store).await {
        eprintln!("[WeChat] lifecycle notify effect failed: {err}");
    }
}

fn state_store() -> WechatStateStore {
    default_wechat_state_store()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub fn normalize_qr_image(value: &str) -> String {
    let value = value.trim();
    if value.starts_with("data:image/") {
        return value.into();
    }
    if let Some(data_url) = raw_base64_image_data_url(value) {
        return data_url;
    }

    qr_svg_data_url(value).unwrap_or_else(|_| value.into())
}

fn raw_base64_image_data_url(value: &str) -> Option<String> {
    let compact: String = value.chars().filter(|ch| !ch.is_whitespace()).collect();
    let bytes = BASE64_STANDARD.decode(compact.as_bytes()).ok()?;
    let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes
        .iter()
        .copied()
        .skip_while(u8::is_ascii_whitespace)
        .take(4)
        .eq(b"<svg".iter().copied())
    {
        "image/svg+xml"
    } else {
        return None;
    };

    Some(format!("data:{media_type};base64,{compact}"))
}

fn qr_svg_data_url(value: &str) -> Result<String, String> {
    let code = QrCode::new(value.as_bytes()).map_err(|err| format!("build QR SVG: {err}"))?;
    let image = code
        .render::<svg::Color>()
        .min_dimensions(256, 256)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        BASE64_STANDARD.encode(image.as_bytes())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_desktop_session_updates_shared_runtime_route() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = WechatRuntimeHandle::new(WechatStateStore::new(dir.path().to_path_buf()));

        set_desktop_session(&runtime, Some("stdin-1".into())).await;

        assert_eq!(runtime.desktop_session_id().await, Some("stdin-1".into()));

        set_desktop_session(&runtime, None).await;

        assert_eq!(runtime.desktop_session_id().await, None);
    }

    #[tokio::test]
    async fn start_polling_task_if_desktop_session_only_starts_with_route() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = WechatRuntimeHandle::new(WechatStateStore::new(dir.path().to_path_buf()));
        let polling_task = WechatPollingTask::default();
        let stdin_mgr = StdinManager::new();

        assert!(
            !start_polling_task_if_desktop_session(&runtime, &polling_task, &stdin_mgr, None).await
        );

        runtime.set_desktop_session("stdin-1".into()).await;

        assert!(
            start_polling_task_if_desktop_session(&runtime, &polling_task, &stdin_mgr, None).await
        );
        assert!(polling_task.stop().await);
    }

    #[test]
    fn normalizes_qr_payloads_for_frontend() {
        let generated = normalize_qr_image("weixin://scan/login?token=abc123");
        assert!(generated.starts_with("data:image/svg+xml;base64,"));
        assert!(!generated.contains("weixin://scan/login"));

        let generated_from_url =
            normalize_qr_image("https://liteapp.weixin.qq.com/q/abc?qrcode=qr-1&bot_type=3");
        assert!(generated_from_url.starts_with("data:image/svg+xml;base64,"));
        assert!(!generated_from_url.contains("liteapp.weixin.qq.com"));

        assert_eq!(
            normalize_qr_image("iVBORw0KGgo="),
            "data:image/png;base64,iVBORw0KGgo=".to_string()
        );

        assert_eq!(
            normalize_qr_image("data:image/png;base64,abc123"),
            "data:image/png;base64,abc123".to_string()
        );
    }
}
