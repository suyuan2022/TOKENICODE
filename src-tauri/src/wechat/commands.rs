use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::{
    commands::StdinManager,
    wechat::{
        api::{IlinkApiClient, QrCodeResponse, QrStatusResponse},
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
}

#[tauri::command]
pub async fn wechat_get_status(
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
    stdin_mgr: State<'_, StdinManager>,
) -> Result<WechatStatusResponse, String> {
    let account = state_store().load_account()?;
    if account.is_some() {
        start_polling_task_if_desktop_session(
            runtime.inner(),
            polling_task.inner(),
            stdin_mgr.inner(),
        )
        .await;
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
    qrcode_id: String,
    verify_code: Option<String>,
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
    stdin_mgr: State<'_, StdinManager>,
) -> Result<WechatQrPollResponse, String> {
    let client = IlinkApiClient::new(None);
    let response: QrStatusResponse = client
        .execute_json(client.qr_status_request(&qrcode_id, verify_code.as_deref()))
        .await?;
    match parse_qr_status_response(response, now_ms())? {
        WechatQrPoll::Connected { account } => {
            state_store().save_account(&account)?;
            start_polling_task_if_desktop_session(
                runtime.inner(),
                polling_task.inner(),
                stdin_mgr.inner(),
            )
            .await;
            Ok(WechatQrPollResponse {
                status: "connected".into(),
                connected: true,
                message: None,
                account: Some(WechatAccountInfo::from(account)),
            })
        }
        WechatQrPoll::Pending { status, message } => Ok(WechatQrPollResponse {
            status,
            connected: false,
            message,
            account: None,
        }),
        WechatQrPoll::Failed { status, message } => Ok(WechatQrPollResponse {
            status,
            connected: false,
            message: Some(message),
            account: None,
        }),
    }
}

#[tauri::command]
pub async fn wechat_disconnect(polling_task: State<'_, WechatPollingTask>) -> Result<(), String> {
    polling_task.stop().await;
    state_store().clear_account()
}

#[tauri::command]
pub async fn wechat_start_polling(
    session_id: String,
    runtime: State<'_, WechatRuntimeHandle>,
    polling_task: State<'_, WechatPollingTask>,
    stdin_mgr: State<'_, StdinManager>,
) -> Result<(), String> {
    set_desktop_session(runtime.inner(), Some(session_id)).await;
    start_polling_task(runtime.inner(), polling_task.inner(), stdin_mgr.inner()).await;
    Ok(())
}

#[tauri::command]
pub async fn wechat_stop_polling(polling_task: State<'_, WechatPollingTask>) -> Result<(), String> {
    polling_task.stop().await;
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
) {
    polling_task.start(runtime.clone(), stdin_mgr.clone()).await;
}

async fn start_polling_task_if_desktop_session(
    runtime: &WechatRuntimeHandle,
    polling_task: &WechatPollingTask,
    stdin_mgr: &StdinManager,
) {
    if runtime.desktop_session_id().await.is_some() {
        start_polling_task(runtime, polling_task, stdin_mgr).await;
    }
}

async fn set_desktop_session(runtime: &WechatRuntimeHandle, session_id: Option<String>) {
    match session_id.filter(|value| !value.trim().is_empty()) {
        Some(session_id) => runtime.set_desktop_session(session_id).await,
        None => runtime.clear_desktop_session().await,
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
    if value.starts_with("data:") || value.starts_with("http://") || value.starts_with("https://") {
        value.into()
    } else {
        format!("data:image/png;base64,{value}")
    }
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

    #[test]
    fn normalizes_raw_base64_qr_images_for_frontend() {
        assert_eq!(
            normalize_qr_image("abc123"),
            "data:image/png;base64,abc123".to_string()
        );
        assert_eq!(
            normalize_qr_image("https://example.com/qr.png"),
            "https://example.com/qr.png".to_string()
        );
        assert_eq!(
            normalize_qr_image("data:image/png;base64,abc123"),
            "data:image/png;base64,abc123".to_string()
        );
    }
}
