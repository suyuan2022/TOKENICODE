use std::{
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{oneshot, Mutex},
    task::JoinHandle,
};

use serde_json::{json, Value};

use crate::{
    commands::StdinManager,
    wechat::{
        api::{GetUpdatesResponse, IlinkApiClient, IlinkHttpRequest},
        executor::{execute_claude_effect, execute_wechat_effect_with},
        monitor::MonitorStatus,
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
    pub async fn start(&self, runtime: WechatRuntimeHandle, stdin_mgr: StdinManager) -> bool {
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
        state.handle = Some(tokio::spawn(run_poll_loop(runtime, stdin_mgr, stop_rx)));
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
    pub inbound_text_count: usize,
    pub claude_effect_count: usize,
    pub wechat_effect_count: usize,
    pub next_timeout_ms: u64,
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
            inbound_text_count: 0,
            claude_effect_count: 0,
            wechat_effect_count: 0,
            next_timeout_ms: NO_ACCOUNT_RETRY_MS,
        });
    };

    runtime.connect().await;
    let response = execute_updates(request).await?;
    let outcome = runtime
        .process_updates_response(response, received_at_ms)
        .await?;

    let store = runtime.state_store().await;
    let mut claude_effect_count = 0;
    let mut wechat_effect_count = 0;
    for effect in &outcome.effects {
        if execute_claude_effect(stdin_mgr, effect).await? {
            claude_effect_count += 1;
        }
        if execute_wechat_effect_with(effect, &store, &mut execute_wechat_request).await? {
            wechat_effect_count += 1;
        }
    }

    Ok(WechatPollIteration {
        polled: true,
        status: Some(outcome.status),
        inbound_text_count: outcome.inbound_text_count,
        claude_effect_count,
        wechat_effect_count,
        next_timeout_ms: outcome.next_timeout_ms,
    })
}

async fn run_poll_loop(
    runtime: WechatRuntimeHandle,
    stdin_mgr: StdinManager,
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
            Ok(iteration) if iteration.status == Some(MonitorStatus::SessionExpired) => {
                iteration.next_timeout_ms
            }
            Ok(iteration) if iteration.polled => 0,
            Ok(_) => NO_ACCOUNT_RETRY_MS,
            Err(err) => {
                eprintln!("[WeChat] polling failed: {err}");
                ERROR_RETRY_MS
            }
        };
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::process::Stdio;
    use std::sync::{Arc, Mutex as StdMutex};

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

        assert!(task.start(runtime.clone(), stdin_mgr.clone()).await);
        assert!(!task.start(runtime, stdin_mgr).await);
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
        assert_eq!(iteration.next_timeout_ms, 25_000);

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

        assert_eq!(iteration.claude_effect_count, 1);
        assert_eq!(iteration.wechat_effect_count, 1);
        let line = lines.next_line().await.unwrap().unwrap();
        assert!(line.contains("hello from WeChat"));

        let outbound_requests = outbound_requests.lock().unwrap();
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
