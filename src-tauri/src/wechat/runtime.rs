use crate::wechat::{
    api::{GetUpdatesResponse, IlinkApiClient, IlinkHttpRequest},
    inbound::parse_inbound_text,
    monitor::{MonitorStatus, WechatMonitorState},
    store::WechatStateStore,
    turn::{WechatTurnEffect, WechatTurnManager},
};

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

    pub fn set_desktop_session(&mut self, session_id: String) {
        self.turn_manager.set_desktop_session(session_id);
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
            let Some(inbound) = parse_inbound_text(message, received_at_ms)? else {
                continue;
            };
            self.store
                .save_context_token(&inbound.from_user_id, &inbound.context_token)?;
            inbound_text_count += 1;
            effects.extend(self.turn_manager.receive_text(inbound));
        }

        Ok(WechatPollOutcome {
            status: tick.status,
            next_timeout_ms: tick.next_timeout_ms,
            inbound_text_count,
            effects,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::wechat::{
        api::{
            GetUpdatesResponse, MessageItem, MessageItemType, MessageType, TextItem, WechatMessage,
        },
        monitor::MonitorStatus,
        store::{WechatAccount, WechatStateStore},
        turn::WechatTurnEffect,
    };

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
}
