use std::collections::VecDeque;

use serde_json::Value;

const STALE_QUEUE_AFTER_MS: u64 = 60_000;
const STALE_QUEUE_NOTICE: &str = "这条消息排队超过 60 秒，请重新发送。";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundWechatText {
    pub message_id: String,
    pub from_user_id: String,
    pub context_token: String,
    pub text: String,
    pub received_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WechatPermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub input_preview: String,
    pub tool_use_id: Option<String>,
    pub updated_input: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WechatTurnEffect {
    SendToClaude {
        desktop_session_id: String,
        text: String,
    },
    SendWeChatText {
        to_user_id: String,
        context_token: String,
        text: String,
    },
    StartTyping {
        to_user_id: String,
        context_token: String,
    },
    StopTyping {
        to_user_id: String,
        context_token: String,
    },
    RespondPermission {
        desktop_session_id: String,
        request_id: String,
        allow: bool,
        tool_use_id: Option<String>,
        updated_input: Value,
    },
}

#[derive(Debug, Default)]
pub struct WechatTurnManager {
    connected: bool,
    desktop_session_id: Option<String>,
    active_turn: Option<InboundWechatText>,
    queue: VecDeque<InboundWechatText>,
    pending_permission: Option<WechatPermissionRequest>,
}

impl WechatTurnManager {
    pub fn connect(&mut self) {
        self.connected = true;
    }

    pub fn disconnect(&mut self) -> Vec<WechatTurnEffect> {
        self.connected = false;
        self.queue.clear();
        self.pending_permission = None;

        let effects = self
            .active_turn
            .take()
            .map(|active_turn| {
                vec![WechatTurnEffect::StopTyping {
                    to_user_id: active_turn.from_user_id,
                    context_token: active_turn.context_token,
                }]
            })
            .unwrap_or_default();

        effects
    }

    pub fn set_desktop_session(&mut self, session_id: String) {
        self.desktop_session_id = Some(session_id);
    }

    pub fn receive_text(&mut self, message: InboundWechatText) -> Vec<WechatTurnEffect> {
        let mut effects = self.discard_stale(message.received_at_ms);
        self.queue.push_back(message);
        effects.extend(self.start_next_turn());
        effects
    }

    pub fn finish_turn(&mut self, text: String, finished_at_ms: u64) -> Vec<WechatTurnEffect> {
        let Some(active_turn) = self.active_turn.take() else {
            return Vec::new();
        };
        self.pending_permission = None;

        let mut effects = vec![
            WechatTurnEffect::SendWeChatText {
                to_user_id: active_turn.from_user_id.clone(),
                context_token: active_turn.context_token.clone(),
                text,
            },
            WechatTurnEffect::StopTyping {
                to_user_id: active_turn.from_user_id,
                context_token: active_turn.context_token,
            },
        ];
        effects.extend(self.discard_stale(finished_at_ms));
        effects.extend(self.start_next_turn());
        effects
    }

    pub fn request_permission(&mut self, request: WechatPermissionRequest) -> Vec<WechatTurnEffect> {
        let Some(active_turn) = self.active_turn.as_ref() else {
            return Vec::new();
        };

        let text = format!(
            "权限请求：{}\n{}\n回复 approve 或 deny。",
            request.tool_name, request.input_preview
        );
        self.pending_permission = Some(request);
        vec![WechatTurnEffect::SendWeChatText {
            to_user_id: active_turn.from_user_id.clone(),
            context_token: active_turn.context_token.clone(),
            text,
        }]
    }

    pub fn answer_permission(&mut self, allow: bool) -> Vec<WechatTurnEffect> {
        let Some(request) = self.pending_permission.take() else {
            return Vec::new();
        };
        let Some(desktop_session_id) = self.desktop_session_id.clone() else {
            return Vec::new();
        };

        vec![WechatTurnEffect::RespondPermission {
            desktop_session_id,
            request_id: request.request_id,
            allow,
            tool_use_id: request.tool_use_id,
            updated_input: request.updated_input,
        }]
    }

    pub fn active_turn_message_id(&self) -> Option<&str> {
        self.active_turn
            .as_ref()
            .map(|turn| turn.message_id.as_str())
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn desktop_session_id(&self) -> Option<&str> {
        self.desktop_session_id.as_deref()
    }

    pub fn queued_len(&self) -> usize {
        self.queue.len()
    }

    pub fn pending_permission_request_id(&self) -> Option<&str> {
        self.pending_permission
            .as_ref()
            .map(|request| request.request_id.as_str())
    }

    fn discard_stale(&mut self, now_ms: u64) -> Vec<WechatTurnEffect> {
        let mut fresh = VecDeque::new();
        let mut effects = Vec::new();

        while let Some(item) = self.queue.pop_front() {
            if now_ms.saturating_sub(item.received_at_ms) > STALE_QUEUE_AFTER_MS {
                effects.push(WechatTurnEffect::SendWeChatText {
                    to_user_id: item.from_user_id,
                    context_token: item.context_token,
                    text: STALE_QUEUE_NOTICE.into(),
                });
            } else {
                fresh.push_back(item);
            }
        }

        self.queue = fresh;
        effects
    }

    fn start_next_turn(&mut self) -> Vec<WechatTurnEffect> {
        if !self.connected || self.active_turn.is_some() {
            return Vec::new();
        }
        let Some(desktop_session_id) = self.desktop_session_id.clone() else {
            return Vec::new();
        };
        let Some(next_turn) = self.queue.pop_front() else {
            return Vec::new();
        };

        let effects = vec![
            WechatTurnEffect::SendToClaude {
                desktop_session_id,
                text: next_turn.text.clone(),
            },
            WechatTurnEffect::StartTyping {
                to_user_id: next_turn.from_user_id.clone(),
                context_token: next_turn.context_token.clone(),
            },
        ];
        self.active_turn = Some(next_turn);
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_first_fresh_message_on_current_desktop_session() {
        let mut manager = WechatTurnManager::default();

        manager.connect();
        manager.set_desktop_session("stdin-1".into());
        let effects = manager.receive_text(InboundWechatText {
            message_id: "msg-1".into(),
            from_user_id: "user@im.wechat".into(),
            context_token: "ctx-1".into(),
            text: "hello".into(),
            received_at_ms: 0,
        });

        assert_eq!(
            effects,
            vec![
                WechatTurnEffect::SendToClaude {
                    desktop_session_id: "stdin-1".into(),
                    text: "hello".into(),
                },
                WechatTurnEffect::StartTyping {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-1".into(),
                },
            ],
        );
        assert_eq!(manager.active_turn_message_id(), Some("msg-1"));
        assert_eq!(manager.queued_len(), 0);
    }

    #[test]
    fn queues_later_messages_while_a_turn_is_active() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());

        manager.receive_text(text_message("msg-1", "first", 0));
        let effects = manager.receive_text(text_message("msg-2", "second", 10_000));

        assert!(effects.is_empty());
        assert_eq!(manager.active_turn_message_id(), Some("msg-1"));
        assert_eq!(manager.queued_len(), 1);
    }

    #[test]
    fn finishing_a_turn_replies_stops_typing_and_starts_next_message() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());
        manager.receive_text(text_message("msg-1", "first", 0));
        manager.receive_text(text_message("msg-2", "second", 10_000));

        let effects = manager.finish_turn("answer one".into(), 20_000);

        assert_eq!(
            effects,
            vec![
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-1".into(),
                    text: "answer one".into(),
                },
                WechatTurnEffect::StopTyping {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-1".into(),
                },
                WechatTurnEffect::SendToClaude {
                    desktop_session_id: "stdin-1".into(),
                    text: "second".into(),
                },
                WechatTurnEffect::StartTyping {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-2".into(),
                },
            ],
        );
        assert_eq!(manager.active_turn_message_id(), Some("msg-2"));
        assert_eq!(manager.queued_len(), 0);
    }

    #[test]
    fn discards_queued_messages_waiting_more_than_sixty_seconds() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());
        manager.receive_text(text_message("msg-1", "first", 0));
        manager.receive_text(text_message("msg-2", "second", 10_000));

        let effects = manager.finish_turn("answer one".into(), 71_000);

        assert_eq!(
            effects,
            vec![
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-1".into(),
                    text: "answer one".into(),
                },
                WechatTurnEffect::StopTyping {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-1".into(),
                },
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-2".into(),
                    text: "这条消息排队超过 60 秒，请重新发送。".into(),
                },
            ],
        );
        assert_eq!(manager.active_turn_message_id(), None);
        assert_eq!(manager.queued_len(), 0);
    }

    #[test]
    fn forwards_permission_request_and_maps_decision_to_desktop_session() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());
        manager.receive_text(text_message("msg-1", "first", 0));

        let forward_effects = manager.request_permission(WechatPermissionRequest {
            request_id: "perm-1".into(),
            tool_name: "Bash".into(),
            input_preview: "{ \"cmd\": \"pnpm test\" }".into(),
            tool_use_id: Some("toolu-1".into()),
            updated_input: serde_json::json!({ "cmd": "pnpm test" }),
        });

        assert_eq!(
            forward_effects,
            vec![WechatTurnEffect::SendWeChatText {
                to_user_id: "user@im.wechat".into(),
                context_token: "ctx-msg-1".into(),
                text: "权限请求：Bash\n{ \"cmd\": \"pnpm test\" }\n回复 approve 或 deny。".into(),
            }],
        );
        assert_eq!(manager.pending_permission_request_id(), Some("perm-1"));

        let decision_effects = manager.answer_permission(true);

        assert_eq!(
            decision_effects,
            vec![WechatTurnEffect::RespondPermission {
                desktop_session_id: "stdin-1".into(),
                request_id: "perm-1".into(),
                allow: true,
                tool_use_id: Some("toolu-1".into()),
                updated_input: serde_json::json!({ "cmd": "pnpm test" }),
            }],
        );
        assert_eq!(manager.pending_permission_request_id(), None);
    }

    #[test]
    fn disconnect_clears_wechat_state_without_touching_desktop_session() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());
        manager.receive_text(text_message("msg-1", "first", 0));
        manager.receive_text(text_message("msg-2", "second", 1_000));
        manager.request_permission(WechatPermissionRequest {
            request_id: "perm-1".into(),
            tool_name: "Bash".into(),
            input_preview: "{}".into(),
            tool_use_id: None,
            updated_input: serde_json::json!({}),
        });

        let effects = manager.disconnect();

        assert_eq!(
            effects,
            vec![WechatTurnEffect::StopTyping {
                to_user_id: "user@im.wechat".into(),
                context_token: "ctx-msg-1".into(),
            }],
        );
        assert!(!manager.is_connected());
        assert_eq!(manager.desktop_session_id(), Some("stdin-1"));
        assert_eq!(manager.active_turn_message_id(), None);
        assert_eq!(manager.queued_len(), 0);
        assert_eq!(manager.pending_permission_request_id(), None);
    }

    fn text_message(message_id: &str, text: &str, received_at_ms: u64) -> InboundWechatText {
        InboundWechatText {
            message_id: message_id.into(),
            from_user_id: "user@im.wechat".into(),
            context_token: format!("ctx-{message_id}"),
            text: text.into(),
            received_at_ms,
        }
    }
}
