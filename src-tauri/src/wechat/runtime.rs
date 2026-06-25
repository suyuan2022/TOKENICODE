use crate::wechat::{
    api::{GetUpdatesResponse, IlinkApiClient, IlinkHttpRequest},
    inbound::{parse_inbound_message, ParsedInboundWechatMessage},
    monitor::{MonitorStatus, WechatMonitorState},
    store::WechatStateStore,
    turn::{WechatPermissionRequest, WechatTurnEffect, WechatTurnManager},
};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

const UNSUPPORTED_VOICE_NOTICE: &str = "不支持语音，请发文字";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WechatPollOutcome {
    pub status: MonitorStatus,
    pub next_timeout_ms: u64,
    pub inbound_text_count: usize,
    pub effects: Vec<WechatTurnEffect>,
}

#[derive(Debug)]
pub struct WechatRuntime {
    store: WechatStateStore,
    monitor: WechatMonitorState,
    turn_manager: WechatTurnManager,
}

#[derive(Debug, Clone)]
pub struct WechatRuntimeHandle {
    inner: Arc<Mutex<WechatRuntime>>,
}

impl WechatRuntimeHandle {
    pub fn new(store: WechatStateStore) -> Self {
        Self {
            inner: Arc::new(Mutex::new(WechatRuntime::new(store))),
        }
    }

    pub async fn set_desktop_session(&self, session_id: String) {
        self.inner.lock().await.set_desktop_session(session_id);
    }

    pub async fn connect(&self) {
        self.inner.lock().await.connect();
    }

    pub async fn disconnect(&self) -> Vec<WechatTurnEffect> {
        self.inner.lock().await.disconnect()
    }

    pub async fn clear_desktop_session(&self) {
        self.inner.lock().await.clear_desktop_session();
    }

    pub async fn desktop_session_id(&self) -> Option<String> {
        self.inner
            .lock()
            .await
            .desktop_session_id()
            .map(ToOwned::to_owned)
    }

    pub async fn state_store(&self) -> WechatStateStore {
        self.inner.lock().await.store.clone()
    }

    pub async fn next_get_updates_request(&self) -> Result<Option<IlinkHttpRequest>, String> {
        self.inner.lock().await.next_get_updates_request()
    }

    pub async fn process_updates_response(
        &self,
        response: GetUpdatesResponse,
        received_at_ms: u64,
    ) -> Result<WechatPollOutcome, String> {
        self.inner
            .lock()
            .await
            .process_updates_response(response, received_at_ms)
    }

    pub async fn process_stream_event(
        &self,
        event: &Value,
        received_at_ms: u64,
    ) -> Vec<WechatTurnEffect> {
        self.inner
            .lock()
            .await
            .process_stream_event(event, received_at_ms)
    }
}

impl WechatRuntime {
    pub fn new(store: WechatStateStore) -> Self {
        Self {
            store,
            monitor: WechatMonitorState::default(),
            turn_manager: WechatTurnManager::default(),
        }
    }

    pub fn connect(&mut self) {
        self.turn_manager.connect();
    }

    pub fn disconnect(&mut self) -> Vec<WechatTurnEffect> {
        self.turn_manager.disconnect()
    }

    pub fn set_desktop_session(&mut self, session_id: String) {
        self.turn_manager.set_desktop_session(session_id);
    }

    pub fn clear_desktop_session(&mut self) {
        self.turn_manager.clear_desktop_session();
    }

    pub fn desktop_session_id(&self) -> Option<&str> {
        self.turn_manager.desktop_session_id()
    }

    pub fn next_get_updates_request(&self) -> Result<Option<IlinkHttpRequest>, String> {
        let Some(account) = self.store.load_account()? else {
            return Ok(None);
        };
        let sync_buf = self.store.load_sync_buf()?;
        let client = IlinkApiClient::with_base_url(Some(account.bot_token), account.base_url);

        Ok(Some(client.get_updates_request(sync_buf)))
    }

    pub fn process_updates_response(
        &mut self,
        response: GetUpdatesResponse,
        received_at_ms: u64,
    ) -> Result<WechatPollOutcome, String> {
        let tick = self.monitor.ingest(response);
        if tick.status == MonitorStatus::SessionExpired {
            return Ok(WechatPollOutcome {
                status: tick.status,
                next_timeout_ms: tick.next_timeout_ms,
                inbound_text_count: 0,
                effects: Vec::new(),
            });
        }

        if let Some(sync_buf) = tick.next_sync_buf.as_deref() {
            self.store.save_sync_buf(sync_buf)?;
        }

        let mut effects = Vec::new();
        let mut inbound_text_count = 0;
        for message in tick.messages {
            match parse_inbound_message(message, received_at_ms)? {
                ParsedInboundWechatMessage::Text(inbound) => {
                    self.store
                        .save_context_token(&inbound.from_user_id, &inbound.context_token)?;
                    inbound_text_count += 1;
                    effects.extend(self.turn_manager.receive_text(inbound));
                }
                ParsedInboundWechatMessage::UnsupportedVoice {
                    from_user_id,
                    context_token,
                } => {
                    self.store
                        .save_context_token(&from_user_id, &context_token)?;
                    effects.push(WechatTurnEffect::SendWeChatText {
                        to_user_id: from_user_id,
                        context_token,
                        text: UNSUPPORTED_VOICE_NOTICE.into(),
                    });
                }
                ParsedInboundWechatMessage::Media(_) => {}
                ParsedInboundWechatMessage::Ignore => {}
            }
        }

        Ok(WechatPollOutcome {
            status: tick.status,
            next_timeout_ms: tick.next_timeout_ms,
            inbound_text_count,
            effects,
        })
    }

    pub fn process_stream_event(
        &mut self,
        event: &Value,
        received_at_ms: u64,
    ) -> Vec<WechatTurnEffect> {
        match event.get("type").and_then(Value::as_str) {
            Some("result") => self.process_result_event(event, received_at_ms),
            Some("tokenicode_permission_request") => permission_request_from_event(event)
                .map(|request| self.turn_manager.request_permission(request))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn process_result_event(
        &mut self,
        event: &Value,
        received_at_ms: u64,
    ) -> Vec<WechatTurnEffect> {
        if event
            .get("parent_tool_use_id")
            .or_else(|| event.get("parentToolUseId"))
            .is_some()
        {
            return Vec::new();
        }
        let Some(text) = event
            .get("result")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Vec::new();
        };
        self.turn_manager
            .finish_turn(text.to_string(), received_at_ms)
    }
}

fn permission_request_from_event(event: &Value) -> Option<WechatPermissionRequest> {
    let request_id = event
        .get("request_id")
        .or_else(|| event.get("requestId"))
        .and_then(Value::as_str)?
        .to_string();
    let tool_name = event
        .get("tool_name")
        .or_else(|| event.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    let updated_input = event.get("input").cloned().unwrap_or(Value::Null);
    let input_preview =
        serde_json::to_string_pretty(&updated_input).unwrap_or_else(|_| updated_input.to_string());
    let tool_use_id = event
        .get("tool_use_id")
        .or_else(|| event.get("toolUseId"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Some(WechatPermissionRequest {
        request_id,
        tool_name,
        input_preview,
        tool_use_id,
        updated_input,
    })
}

#[cfg(test)]
mod handle_tests {
    use crate::wechat::{runtime::WechatRuntimeHandle, store::WechatStateStore};

    #[tokio::test]
    async fn stores_and_clears_current_desktop_session() {
        let dir = tempfile::tempdir().unwrap();
        let handle = WechatRuntimeHandle::new(WechatStateStore::new(dir.path().to_path_buf()));

        handle.set_desktop_session("stdin-1".into()).await;

        assert_eq!(handle.desktop_session_id().await, Some("stdin-1".into()));

        handle.clear_desktop_session().await;

        assert_eq!(handle.desktop_session_id().await, None);
    }
}

#[cfg(test)]
mod tests {
    use crate::wechat::{
        api::{
            GetUpdatesResponse, MessageItem, MessageItemType, MessageType, TextItem, VoiceItem,
            WechatMessage,
        },
        monitor::MonitorStatus,
        store::{WechatAccount, WechatStateStore},
        turn::WechatTurnEffect,
    };
    use serde_json::json;

    use super::*;

    #[test]
    fn next_get_updates_request_returns_none_without_saved_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let runtime = WechatRuntime::new(store);

        assert_eq!(runtime.next_get_updates_request().unwrap(), None);
    }

    #[test]
    fn next_get_updates_request_uses_saved_account_and_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store.save_account(&account()).unwrap();
        store.save_sync_buf("cursor-1").unwrap();
        let runtime = WechatRuntime::new(store);

        let request = runtime.next_get_updates_request().unwrap().unwrap();

        assert_eq!(
            request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/getupdates"
        );
        assert_eq!(
            request.headers.get("Authorization"),
            Some(&"Bearer bot-token".into())
        );
        assert_eq!(request.body["get_updates_buf"], "cursor-1");
    }

    #[test]
    fn processing_updates_persists_cursor_context_token_and_starts_desktop_turn() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let mut runtime = WechatRuntime::new(store.clone());
        runtime.connect();
        runtime.set_desktop_session("stdin-1".into());

        let outcome = runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "hello")],
                    get_updates_buf: Some("cursor-2".into()),
                    longpolling_timeout_ms: Some(12_000),
                },
                1_000,
            )
            .unwrap();

        assert_eq!(outcome.status, MonitorStatus::Active);
        assert_eq!(outcome.next_timeout_ms, 12_000);
        assert_eq!(outcome.inbound_text_count, 1);
        assert_eq!(store.load_sync_buf().unwrap(), Some("cursor-2".into()));
        assert_eq!(
            store.load_context_token("user-1").unwrap(),
            Some("ctx-1".into())
        );
        assert_eq!(
            outcome.effects,
            vec![
                WechatTurnEffect::SendToClaude {
                    desktop_session_id: "stdin-1".into(),
                    text: "hello".into(),
                },
                WechatTurnEffect::StartTyping {
                    to_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                },
            ]
        );
    }

    #[test]
    fn raw_voice_without_transcript_replies_with_unsupported_notice_without_starting_turn() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let mut runtime = WechatRuntime::new(store.clone());
        runtime.connect();
        runtime.set_desktop_session("stdin-1".into());

        let outcome = runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![raw_voice_message(8, "user-1", "ctx-voice")],
                    get_updates_buf: Some("cursor-voice".into()),
                    longpolling_timeout_ms: None,
                },
                1_000,
            )
            .unwrap();

        assert_eq!(outcome.inbound_text_count, 0);
        assert_eq!(
            store.load_context_token("user-1").unwrap(),
            Some("ctx-voice".into())
        );
        assert_eq!(
            outcome.effects,
            vec![WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-voice".into(),
                text: "不支持语音，请发文字".into(),
            }]
        );
        assert!(runtime
            .process_stream_event(
                &json!({
                    "type": "result",
                    "subtype": "success",
                    "result": "answer from Claude"
                }),
                2_000,
            )
            .is_empty());
    }

    #[test]
    fn session_expired_response_does_not_persist_cursor_or_deliver_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let mut runtime = WechatRuntime::new(store.clone());
        runtime.connect();
        runtime.set_desktop_session("stdin-1".into());

        let outcome = runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(1),
                    errcode: Some(-14),
                    errmsg: Some("session expired".into()),
                    msgs: vec![text_message(7, "user-1", "ctx-1", "hello")],
                    get_updates_buf: Some("cursor-2".into()),
                    longpolling_timeout_ms: None,
                },
                1_000,
            )
            .unwrap();

        assert_eq!(outcome.status, MonitorStatus::SessionExpired);
        assert_eq!(outcome.inbound_text_count, 0);
        assert!(outcome.effects.is_empty());
        assert_eq!(store.load_sync_buf().unwrap(), None);
        assert_eq!(store.load_context_token("user-1").unwrap(), None);
    }

    #[test]
    fn main_result_event_finishes_active_wechat_turn() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let mut runtime = WechatRuntime::new(store);
        runtime.connect();
        runtime.set_desktop_session("stdin-1".into());
        runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "hello")],
                    get_updates_buf: None,
                    longpolling_timeout_ms: None,
                },
                1_000,
            )
            .unwrap();

        let effects = runtime.process_stream_event(
            &json!({
                "type": "result",
                "subtype": "success",
                "result": "answer from Claude"
            }),
            2_000,
        );

        assert_eq!(
            effects,
            vec![
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                    text: "answer from Claude".into(),
                },
                WechatTurnEffect::StopTyping {
                    to_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                },
            ]
        );
    }

    #[test]
    fn subagent_result_event_does_not_finish_active_wechat_turn() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let mut runtime = WechatRuntime::new(store);
        runtime.connect();
        runtime.set_desktop_session("stdin-1".into());
        runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "hello")],
                    get_updates_buf: None,
                    longpolling_timeout_ms: None,
                },
                1_000,
            )
            .unwrap();

        let subagent_effects = runtime.process_stream_event(
            &json!({
                "type": "result",
                "subtype": "success",
                "parent_tool_use_id": "toolu-parent",
                "result": "sub-agent answer"
            }),
            2_000,
        );
        let main_effects = runtime.process_stream_event(
            &json!({
                "type": "result",
                "subtype": "success",
                "result": "main answer"
            }),
            3_000,
        );

        assert!(subagent_effects.is_empty());
        assert_eq!(
            main_effects,
            vec![
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                    text: "main answer".into(),
                },
                WechatTurnEffect::StopTyping {
                    to_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                },
            ]
        );
    }

    #[test]
    fn permission_event_forwards_request_to_active_wechat_turn() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let mut runtime = WechatRuntime::new(store);
        runtime.connect();
        runtime.set_desktop_session("stdin-1".into());
        runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "hello")],
                    get_updates_buf: None,
                    longpolling_timeout_ms: None,
                },
                1_000,
            )
            .unwrap();

        let effects = runtime.process_stream_event(
            &json!({
                "type": "tokenicode_permission_request",
                "request_id": "perm-1",
                "tool_name": "Bash",
                "input": { "cmd": "pnpm test" },
                "tool_use_id": "toolu-1"
            }),
            2_000,
        );

        assert_eq!(
            effects,
            vec![WechatTurnEffect::SendWeChatText {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
                text: "权限请求：Bash\n{\n  \"cmd\": \"pnpm test\"\n}\n回复 approve 或 deny。"
                    .into(),
            }]
        );
    }

    #[test]
    fn disconnect_clears_wechat_turn_state_and_preserves_desktop_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let mut runtime = WechatRuntime::new(store);
        runtime.connect();
        runtime.set_desktop_session("stdin-1".into());
        runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(7, "user-1", "ctx-1", "first")],
                    get_updates_buf: None,
                    longpolling_timeout_ms: None,
                },
                1_000,
            )
            .unwrap();
        runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(8, "user-1", "ctx-2", "queued")],
                    get_updates_buf: None,
                    longpolling_timeout_ms: None,
                },
                2_000,
            )
            .unwrap();
        runtime.process_stream_event(
            &json!({
                "type": "tokenicode_permission_request",
                "request_id": "perm-1",
                "tool_name": "Bash",
                "input": { "cmd": "pnpm test" }
            }),
            3_000,
        );

        let effects = runtime.disconnect();

        assert_eq!(
            effects,
            vec![WechatTurnEffect::StopTyping {
                to_user_id: "user-1".into(),
                context_token: "ctx-1".into(),
            }]
        );
        assert_eq!(runtime.desktop_session_id(), Some("stdin-1"));
        assert!(runtime
            .process_stream_event(
                &json!({
                    "type": "result",
                    "subtype": "success",
                    "result": "late answer"
                }),
                4_000,
            )
            .is_empty());

        runtime.connect();
        let outcome = runtime
            .process_updates_response(
                GetUpdatesResponse {
                    ret: Some(0),
                    errcode: None,
                    errmsg: None,
                    msgs: vec![text_message(9, "user-1", "ctx-3", "fresh")],
                    get_updates_buf: None,
                    longpolling_timeout_ms: None,
                },
                5_000,
            )
            .unwrap();

        assert_eq!(
            outcome.effects,
            vec![
                WechatTurnEffect::SendToClaude {
                    desktop_session_id: "stdin-1".into(),
                    text: "fresh".into(),
                },
                WechatTurnEffect::StartTyping {
                    to_user_id: "user-1".into(),
                    context_token: "ctx-3".into(),
                },
            ]
        );
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

    fn raw_voice_message(
        message_id: i64,
        from_user_id: &str,
        context_token: &str,
    ) -> WechatMessage {
        WechatMessage {
            message_id: Some(message_id),
            from_user_id: Some(from_user_id.into()),
            message_type: Some(MessageType::User as i32),
            item_list: vec![MessageItem {
                item_type: Some(MessageItemType::Voice as i32),
                voice_item: Some(VoiceItem::default()),
                ..MessageItem::default()
            }],
            context_token: Some(context_token.into()),
            ..WechatMessage::default()
        }
    }
}
