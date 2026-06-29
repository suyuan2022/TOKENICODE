use std::{future::Future, sync::Arc, time::Duration};
use tokio::{
    sync::{oneshot, Mutex},
    task::JoinHandle,
};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::{
    commands::StdinManager,
    events::emit_to_frontend,
    wechat::{
        api::{GetUpdatesResponse, IlinkApiClient, IlinkHttpRequest},
        executor::{
            execute_turn_effects_with, WechatDesktopClearConversation, WechatDesktopStop,
            WechatDesktopUserMessage,
        },
        monitor::MonitorStatus,
        now_ms,
        runtime::WechatRuntimeHandle,
    },
};

const NO_ACCOUNT_RETRY_MS: u64 = 5_000;
const ERROR_RETRY_MS: u64 = 5_000;

#[derive(Debug, Default, Clone)]
pub struct WechatPollingTask {
    inner: Arc<Mutex<PollingTaskState>>,
}

#[derive(Debug, Default)]
struct PollingTaskState {
    stop_tx: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl WechatPollingTask {
    pub async fn start(
        &self,
        runtime: WechatRuntimeHandle,
        stdin_mgr: StdinManager,
        app: Option<AppHandle>,
    ) -> bool {
        let mut state = self.inner.lock().await;
        if state
            .handle
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
        {
            return false;
        }

        if let Some(handle) = state.handle.take() {
            handle.abort();
        }

        let (stop_tx, stop_rx) = oneshot::channel();
        state.stop_tx = Some(stop_tx);
        state.handle = Some(tokio::spawn(run_poll_loop(
            runtime, stdin_mgr, app, stop_rx,
        )));
        true
    }

    pub async fn stop(&self) -> bool {
        let mut state = self.inner.lock().await;
        let had_task = state.stop_tx.is_some() || state.handle.is_some();

        if let Some(stop_tx) = state.stop_tx.take() {
            let _ = stop_tx.send(());
        }
        if let Some(handle) = state.handle.take() {
            handle.abort();
        }

        had_task
    }

    pub async fn is_running(&self) -> bool {
        self.inner
            .lock()
            .await
            .handle
            .as_ref()
            .map(|handle| !handle.is_finished())
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WechatPollIteration {
    pub polled: bool,
    pub status: Option<MonitorStatus>,
    pub status_event: Option<WechatStatusEvent>,
    pub inbound_text_count: usize,
    pub claude_effect_count: usize,
    pub wechat_effect_count: usize,
    pub desktop_user_messages: Vec<WechatDesktopUserMessage>,
    pub desktop_clear_conversations: Vec<WechatDesktopClearConversation>,
    pub desktop_stops: Vec<WechatDesktopStop>,
    pub next_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WechatStatusEvent {
    pub status: String,
    pub connected: bool,
    pub message: String,
    pub retry_after_ms: Option<u64>,
}

pub async fn poll_once(
    runtime: &WechatRuntimeHandle,
    stdin_mgr: &StdinManager,
) -> Result<WechatPollIteration, String> {
    poll_once_with_executors(
        runtime,
        stdin_mgr,
        now_ms(),
        |request| async move { IlinkApiClient::new(None).execute_json(request).await },
        |request| async move {
            IlinkApiClient::new(None)
                .execute_json::<Value>(request)
                .await
        },
    )
    .await
}

pub async fn poll_once_with<F, Fut>(
    runtime: &WechatRuntimeHandle,
    stdin_mgr: &StdinManager,
    received_at_ms: u64,
    execute_updates: F,
) -> Result<WechatPollIteration, String>
where
    F: FnOnce(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<GetUpdatesResponse, String>>,
{
    poll_once_with_executors(
        runtime,
        stdin_mgr,
        received_at_ms,
        execute_updates,
        |_request| async { Ok(json!({ "ret": 0 })) },
    )
    .await
}

pub async fn poll_once_with_executors<F, Fut, G, Gut>(
    runtime: &WechatRuntimeHandle,
    stdin_mgr: &StdinManager,
    received_at_ms: u64,
    execute_updates: F,
    mut execute_wechat_request: G,
) -> Result<WechatPollIteration, String>
where
    F: FnOnce(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<GetUpdatesResponse, String>>,
    G: FnMut(IlinkHttpRequest) -> Gut,
    Gut: Future<Output = Result<Value, String>>,
{
    let Some(request) = runtime.next_get_updates_request().await? else {
        return Ok(WechatPollIteration {
            polled: false,
            status: None,
            status_event: None,
            inbound_text_count: 0,
            claude_effect_count: 0,
            wechat_effect_count: 0,
            desktop_user_messages: Vec::new(),
            desktop_clear_conversations: Vec::new(),
            desktop_stops: Vec::new(),
            next_timeout_ms: NO_ACCOUNT_RETRY_MS,
        });
    };

    runtime.connect().await;
    let response = execute_updates(request).await?;
    let outcome = runtime
        .process_updates_response(response, received_at_ms)
        .await?;

    let store = runtime.state_store().await;
    let dispatch = execute_turn_effects_with(
        stdin_mgr,
        &store,
        &outcome.effects,
        &mut execute_wechat_request,
    )
    .await?;

    if outcome.inbound_text_count > 0
        || !outcome.effects.is_empty()
        || dispatch.claude_effect_count > 0
        || dispatch.wechat_effect_count > 0
        || !dispatch.desktop_stops.is_empty()
    {
        eprintln!(
            "[WeChat] poll dispatch: inbound_text={} effects={} claude_effects={} wechat_effects={} desktop_messages={} desktop_clears={} desktop_stops={}",
            outcome.inbound_text_count,
            outcome.effects.len(),
            dispatch.claude_effect_count,
            dispatch.wechat_effect_count,
            dispatch.desktop_user_messages.len(),
            dispatch.desktop_clear_conversations.len(),
            dispatch.desktop_stops.len(),
        );
    }

    Ok(WechatPollIteration {
        polled: true,
        status: Some(outcome.status),
        status_event: status_event_for_poll_outcome(outcome.status, outcome.next_timeout_ms),
        inbound_text_count: outcome.inbound_text_count,
        claude_effect_count: dispatch.claude_effect_count,
        wechat_effect_count: dispatch.wechat_effect_count,
        desktop_user_messages: dispatch.desktop_user_messages,
        desktop_clear_conversations: dispatch.desktop_clear_conversations,
        desktop_stops: dispatch.desktop_stops,
        next_timeout_ms: outcome.next_timeout_ms,
    })
}

fn status_event_for_poll_outcome(
    status: MonitorStatus,
    next_timeout_ms: u64,
) -> Option<WechatStatusEvent> {
    match status {
        MonitorStatus::Active => None,
        MonitorStatus::SessionExpired => Some(WechatStatusEvent {
            status: "sessionExpired".into(),
            connected: false,
            message: "WeChat session expired. Scan QR again to reconnect.".into(),
            retry_after_ms: Some(next_timeout_ms),
        }),
    }
}

async fn run_poll_loop(
    runtime: WechatRuntimeHandle,
    stdin_mgr: StdinManager,
    app: Option<AppHandle>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let mut retry_delay_ms = 0;
    loop {
        if retry_delay_ms > 0 {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = tokio::time::sleep(Duration::from_millis(retry_delay_ms)) => {}
            }
        }

        let iteration = tokio::select! {
            _ = &mut stop_rx => break,
            result = poll_once(&runtime, &stdin_mgr) => result,
        };

        retry_delay_ms = match iteration {
            Ok(iteration) if iteration.polled => {
                emit_wechat_status_event(app.as_ref(), iteration.status_event.as_ref());
                emit_each(
                    app.as_ref(),
                    "wechat:desktop_user_message",
                    &iteration.desktop_user_messages,
                    "desktop user message",
                );
                emit_each(
                    app.as_ref(),
                    "wechat:clear_desktop_conversation",
                    &iteration.desktop_clear_conversations,
                    "desktop clear conversation",
                );
                emit_each(
                    app.as_ref(),
                    "wechat:desktop_stop",
                    &iteration.desktop_stops,
                    "desktop stop",
                );
                if iteration.status == Some(MonitorStatus::SessionExpired) {
                    iteration.next_timeout_ms
                } else {
                    0
                }
            }
            Ok(_) => NO_ACCOUNT_RETRY_MS,
            Err(err) => {
                eprintln!("[WeChat] polling failed: {err}");
                ERROR_RETRY_MS
            }
        };
    }
}

fn emit_wechat_status_event(app: Option<&AppHandle>, event: Option<&WechatStatusEvent>) {
    let (Some(app), Some(event)) = (app, event) else {
        return;
    };

    if let Err(err) = emit_to_frontend(app, "wechat:status", event) {
        eprintln!("[WeChat] status emit failed: {err}");
    }
}

fn emit_each<T: Serialize>(app: Option<&AppHandle>, channel: &str, items: &[T], label: &str) {
    let Some(app) = app else {
        return;
    };

    for item in items {
        if let Err(err) = emit_to_frontend(app, channel, item) {
            eprintln!("[WeChat] {label} emit failed: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::sync::Arc;
    use parking_lot::Mutex as StdMutex;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::{Child, Command};

    use crate::{
        commands::StdinManager,
        wechat::{
            api::{
                GetUpdatesResponse, IlinkHttpRequest, MessageItem, MessageItemType, MessageType,
                TextItem, WechatMessage,
            },
            executor::execute_turn_effects_with,
            monitor::MonitorStatus,
            poller::{poll_once_with, poll_once_with_executors, WechatPollingTask},
            runtime::WechatRuntimeHandle,
            store::{WechatAccount, WechatStateStore},
        },
    };

    #[tokio::test]
    async fn polling_task_starts_once_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = WechatRuntimeHandle::new(WechatStateStore::new(dir.path().to_path_buf()));
        let stdin_mgr = StdinManager::new();
        let task = WechatPollingTask::default();

        assert!(task.start(runtime.clone(), stdin_mgr.clone(), None).await);
        assert!(!task.start(runtime, stdin_mgr, None).await);
        assert!(task.is_running().await);

        assert!(task.stop().await);
        assert!(!task.is_running().await);
    }

    #[tokio::test]
    async fn poll_once_routes_inbound_text_to_current_desktop_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let runtime = WechatRuntimeHandle::new(store);
        runtime.set_desktop_session("stdin-1".into()).await;

        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        let iteration = poll_once_with(&runtime, &stdin_mgr, 1_000, |request| async move {
            assert_eq!(
                request.url,
                "https://ilinkai.weixin.qq.com/ilink/bot/getupdates"
            );
            assert_eq!(
                request.headers.get("Authorization"),
                Some(&"Bearer bot-token".into())
            );
            Ok(GetUpdatesResponse {
                ret: Some(0),
                errcode: None,
                errmsg: None,
                msgs: vec![text_message(7, "user-1", "ctx-1", "hello from WeChat")],
                get_updates_buf: Some("cursor-1".into()),
                longpolling_timeout_ms: Some(25_000),
            })
        })
        .await
        .unwrap();

        assert!(iteration.polled);
        assert_eq!(iteration.inbound_text_count, 1);
        assert_eq!(iteration.claude_effect_count, 1);
        assert_eq!(iteration.desktop_user_messages.len(), 1);
        assert_eq!(
            iteration.desktop_user_messages[0].desktop_session_id,
            "stdin-1"
        );
        assert_eq!(
            iteration.desktop_user_messages[0].content,
            "hello from WeChat"
        );
        assert!(iteration.desktop_user_messages[0].attachments.is_empty());
        assert_eq!(iteration.next_timeout_ms, 25_000);

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
        assert!(content.ends_with("hello from WeChat"));

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn poll_once_dispatches_wechat_outbound_effects() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let runtime = WechatRuntimeHandle::new(store);
        runtime.set_desktop_session("stdin-1".into()).await;

        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;
        let outbound_requests = Arc::new(StdMutex::new(Vec::<IlinkHttpRequest>::new()));
        let captured = outbound_requests.clone();

        let iteration = poll_once_with_executors(
            &runtime,
            &stdin_mgr,
            1_000,
            |_request| async {
                Ok(GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "hello from WeChat")],
                    get_updates_buf: Some("cursor-1".into()),
                    longpolling_timeout_ms: Some(25_000),
                })
            },
            move |request| {
                let captured = captured.clone();
                async move {
                    let mut requests = captured.lock();
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

        assert_eq!(iteration.claude_effect_count, 1);
        assert_eq!(iteration.wechat_effect_count, 1);
        let line = lines.next_line().await.unwrap().unwrap();
        assert!(line.contains("hello from WeChat"));

        let outbound_requests = outbound_requests.lock();
        assert_eq!(outbound_requests.len(), 2);
        assert_eq!(
            outbound_requests[0].url,
            "https://ilinkai.weixin.qq.com/ilink/bot/getconfig"
        );
        assert_eq!(
            outbound_requests[1].url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendtyping"
        );

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn queued_wechat_message_discards_as_stale_through_outbound_executor() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let runtime = WechatRuntimeHandle::new(store.clone());
        runtime.set_desktop_session("stdin-1".into()).await;
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        let first_iteration = poll_once_with_executors(
            &runtime,
            &stdin_mgr,
            1_000,
            |_request| async {
                Ok(GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "first")],
                    get_updates_buf: Some("cursor-1".into()),
                    longpolling_timeout_ms: None,
                })
            },
            |_request| async { Ok(json!({ "ret": 0, "typing_ticket": "ticket-1" })) },
        )
        .await
        .unwrap();

        assert_eq!(first_iteration.claude_effect_count, 1);
        assert_eq!(first_iteration.wechat_effect_count, 1);
        assert!(lines.next_line().await.unwrap().unwrap().contains("first"));

        let second_iteration = poll_once_with_executors(
            &runtime,
            &stdin_mgr,
            10_000,
            |_request| async {
                Ok(GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(8, "user-1", "ctx-2", "second")],
                    get_updates_buf: Some("cursor-2".into()),
                    longpolling_timeout_ms: None,
                })
            },
            |_request| async { Ok(json!({ "ret": 0 })) },
        )
        .await
        .unwrap();

        assert_eq!(second_iteration.claude_effect_count, 0);
        assert_eq!(second_iteration.wechat_effect_count, 0);

        let effects = runtime
            .process_stream_event(
                &json!({
                    "type": "result",
                    "subtype": "success",
                    "result": "answer one",
                }),
                71_001,
            )
            .await;
        let outbound_requests = Arc::new(StdMutex::new(Vec::<IlinkHttpRequest>::new()));
        let captured = outbound_requests.clone();
        let dispatch = execute_turn_effects_with(&stdin_mgr, &store, &effects, move |request| {
            let captured = captured.clone();
            async move {
                captured.lock().push(request);
                Ok(json!({ "ret": 0, "typing_ticket": "ticket-1" }))
            }
        })
        .await
        .unwrap();

        assert_eq!(dispatch.wechat_effect_count, 3);
        let send_texts: Vec<String> = outbound_requests
            .lock()
            .iter()
            .filter(|request| request.url.ends_with("/ilink/bot/sendmessage"))
            .filter_map(|request| {
                request.body["msg"]["item_list"][0]["text_item"]["text"]
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .collect();
        assert_eq!(
            send_texts,
            vec!["answer one", "这条消息排队超过 60 秒，请重新发送。"]
        );

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn poll_once_reports_wechat_stop_to_frontend_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let runtime = WechatRuntimeHandle::new(store);
        runtime.set_desktop_session("stdin-1".into()).await;
        let stdin_mgr = StdinManager::new();
        let (mut child, mut lines) = spawn_echo_session(&stdin_mgr, "stdin-1").await;

        let first_iteration = poll_once_with_executors(
            &runtime,
            &stdin_mgr,
            1_000,
            |_request| async {
                Ok(GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "first")],
                    get_updates_buf: Some("cursor-1".into()),
                    longpolling_timeout_ms: None,
                })
            },
            |_request| async { Ok(json!({ "ret": 0, "typing_ticket": "ticket-1" })) },
        )
        .await
        .unwrap();

        assert_eq!(first_iteration.claude_effect_count, 1);
        assert!(lines.next_line().await.unwrap().unwrap().contains("first"));

        let stop_iteration = poll_once_with_executors(
            &runtime,
            &stdin_mgr,
            2_000,
            |_request| async {
                Ok(GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(8, "user-1", "ctx-2", "/stop")],
                    get_updates_buf: Some("cursor-2".into()),
                    longpolling_timeout_ms: None,
                })
            },
            |_request| async { Ok(json!({ "ret": 0, "typing_ticket": "ticket-1" })) },
        )
        .await
        .unwrap();

        assert_eq!(stop_iteration.claude_effect_count, 1);
        assert_eq!(stop_iteration.desktop_user_messages.len(), 0);
        assert_eq!(stop_iteration.desktop_stops.len(), 1);
        assert_eq!(
            stop_iteration.desktop_stops[0].desktop_session_id,
            "stdin-1"
        );
        assert_eq!(stop_iteration.desktop_stops[0].source, "wechat");

        let line = tokio::time::timeout(Duration::from_millis(200), lines.next_line())
            .await
            .expect("stop command wrote no interrupt request")
            .unwrap()
            .unwrap();
        let actual: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(actual["type"], "control_request");
        assert_eq!(actual["request"]["subtype"], "interrupt");

        stdin_mgr.remove("stdin-1").await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn poll_once_reports_session_expired_status_event_and_pause() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        let runtime = WechatRuntimeHandle::new(store);
        runtime.set_desktop_session("stdin-1".into()).await;
        let stdin_mgr = StdinManager::new();

        let iteration = poll_once_with(&runtime, &stdin_mgr, 1_000, |_request| async {
            Ok(GetUpdatesResponse {
                ret: Some(1),
                errcode: Some(-14),
                errmsg: Some("session expired".into()),
                msgs: vec![text_message(7, "user-1", "ctx-1", "hello from WeChat")],
                get_updates_buf: Some("cursor-1".into()),
                longpolling_timeout_ms: None,
            })
        })
        .await
        .unwrap();

        assert_eq!(iteration.status, Some(MonitorStatus::SessionExpired));
        assert_eq!(iteration.next_timeout_ms, 3_600_000);
        let status_event = iteration.status_event.as_ref().unwrap();
        assert_eq!(status_event.status, "sessionExpired");
        assert!(!status_event.connected);
        assert_eq!(status_event.retry_after_ms, Some(3_600_000));
        assert!(status_event.message.contains("expired"));
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

    fn text_message(
        message_id: i64,
        from_user_id: &str,
        context_token: &str,
        text: &str,
    ) -> WechatMessage {
        WechatMessage {
            message_id: Some(message_id),
            from_user_id: Some(from_user_id.into()),
            message_type: Some(MessageType::User as i32),
            item_list: vec![MessageItem {
                item_type: Some(MessageItemType::Text as i32),
                text_item: Some(TextItem {
                    text: Some(text.into()),
                }),
                ..MessageItem::default()
            }],
            context_token: Some(context_token.into()),
            ..WechatMessage::default()
        }
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
}
