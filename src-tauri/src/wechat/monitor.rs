use std::collections::HashSet;

use crate::wechat::api::{GetUpdatesResponse, WechatMessage};

pub const DEFAULT_MONITOR_TIMEOUT_MS: u64 = 35_000;
pub const SESSION_EXPIRED_PAUSE_MS: u64 = 60 * 60 * 1_000;
const SESSION_EXPIRED_ERRCODE: i32 = -14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorStatus {
    Active,
    SessionExpired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorTick {
    pub status: MonitorStatus,
    pub messages: Vec<WechatMessage>,
    pub next_sync_buf: Option<String>,
    pub next_timeout_ms: u64,
}

#[derive(Debug, Default)]
pub struct WechatMonitorState {
    seen_message_keys: HashSet<String>,
    sync_buf: Option<String>,
}

impl WechatMonitorState {
    pub fn ingest(&mut self, response: GetUpdatesResponse) -> MonitorTick {
        if response.errcode == Some(SESSION_EXPIRED_ERRCODE) {
            return MonitorTick {
                status: MonitorStatus::SessionExpired,
                messages: Vec::new(),
                next_sync_buf: None,
                next_timeout_ms: SESSION_EXPIRED_PAUSE_MS,
            };
        }

        if let Some(sync_buf) = response.get_updates_buf.filter(|value| !value.is_empty()) {
            self.sync_buf = Some(sync_buf);
        }

        let mut messages = Vec::new();
        for message in response.msgs {
            let key = message_dedup_key(&message);
            if self.seen_message_keys.insert(key) {
                messages.push(message);
            }
        }

        MonitorTick {
            status: MonitorStatus::Active,
            messages,
            next_sync_buf: self.sync_buf.clone(),
            next_timeout_ms: response
                .longpolling_timeout_ms
                .filter(|timeout| *timeout > 0)
                .unwrap_or(DEFAULT_MONITOR_TIMEOUT_MS),
        }
    }
}

fn message_dedup_key(message: &WechatMessage) -> String {
    if let Some(message_id) = message.message_id {
        return format!("id:{message_id}");
    }

    format!(
        "fallback:{}:{}:{}",
        message.from_user_id.as_deref().unwrap_or_default(),
        message.context_token.as_deref().unwrap_or_default(),
        message.create_time_ms.unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wechat::api::{GetUpdatesResponse, WechatMessage};

    #[test]
    fn updates_sync_buf_and_deduplicates_messages() {
        let mut state = WechatMonitorState::default();

        let first = state.ingest(GetUpdatesResponse {
            ret: Some(0),
            errcode: None,
            errmsg: None,
            msgs: vec![message(7, "user-1", "ctx-1"), message(7, "user-1", "ctx-1")],
            get_updates_buf: Some("cursor-2".into()),
            longpolling_timeout_ms: Some(27_000),
        });

        assert_eq!(first.status, MonitorStatus::Active);
        assert_eq!(first.messages.len(), 1);
        assert_eq!(first.next_sync_buf.as_deref(), Some("cursor-2"));
        assert_eq!(first.next_timeout_ms, 27_000);

        let second = state.ingest(GetUpdatesResponse {
            ret: Some(0),
            errcode: None,
            errmsg: None,
            msgs: vec![message(7, "user-1", "ctx-1"), message(8, "user-1", "ctx-2")],
            get_updates_buf: Some("cursor-3".into()),
            longpolling_timeout_ms: None,
        });

        assert_eq!(second.messages.len(), 1);
        assert_eq!(second.messages[0].message_id, Some(8));
        assert_eq!(second.next_sync_buf.as_deref(), Some("cursor-3"));
        assert_eq!(second.next_timeout_ms, 35_000);
    }

    #[test]
    fn marks_session_expired_without_delivering_messages() {
        let mut state = WechatMonitorState::default();

        let tick = state.ingest(GetUpdatesResponse {
            ret: Some(1),
            errcode: Some(-14),
            errmsg: Some("session expired".into()),
            msgs: vec![message(7, "user-1", "ctx-1")],
            get_updates_buf: Some("cursor-2".into()),
            longpolling_timeout_ms: None,
        });

        assert_eq!(tick.status, MonitorStatus::SessionExpired);
        assert!(tick.messages.is_empty());
        assert_eq!(tick.next_sync_buf, None);
        assert_eq!(tick.next_timeout_ms, 3_600_000);
    }

    fn message(message_id: i64, from_user_id: &str, context_token: &str) -> WechatMessage {
        WechatMessage {
            message_id: Some(message_id),
            from_user_id: Some(from_user_id.into()),
            context_token: Some(context_token.into()),
            ..WechatMessage::default()
        }
    }
}
