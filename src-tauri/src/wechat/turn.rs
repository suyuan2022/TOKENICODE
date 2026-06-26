use std::collections::{HashMap, VecDeque};

use crate::wechat::inbound::InboundWechatMedia;
use serde_json::Value;

const STALE_QUEUE_AFTER_MS: u64 = 60_000;
const STALE_QUEUE_NOTICE: &str = "这条消息排队超过 60 秒，请重新发送。";
const STOP_CONFIRM_NOTICE: &str = "已停止当前任务，并清空排队消息。";
const STOP_IDLE_NOTICE: &str = "当前没有正在运行的任务。";
const CLEAR_CONTEXT_NOTICE: &str = "已清空当前「微信接入」上下文。";
const CLEAR_WHILE_RUNNING_NOTICE: &str = "当前任务正在运行，请先发送 /stop，再发送 /clear。";
const HELP_NOTICE: &str = "可用命令：\n/help 查看帮助\n/status 查看微信远程状态\n/stop 停止当前任务并清空排队消息\n/clear 或 /new 清空当前「微信接入」上下文\n\n权限请求时，回复 approve/deny 或 同意/拒绝。";
const NO_DESKTOP_SESSION_NOTICE: &str =
    "微信已连接，但还没有绑定 TOKENICODE 的「微信接入」专用窗口。请先在桌面左侧打开「微信接入」并启动一次会话。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BasicRemoteCommand {
    Help,
    Status,
    ClearContext,
}

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
    SendClaudeSlashCommand {
        desktop_session_id: String,
        command: String,
    },
    ClearDesktopConversation {
        desktop_session_id: String,
    },
    DownloadMediaToClaude {
        desktop_session_id: String,
        media: InboundWechatMedia,
    },
    SendWeChatText {
        to_user_id: String,
        context_token: String,
        text: String,
    },
    SendWeChatFile {
        to_user_id: String,
        context_token: String,
        path: String,
        caption: Option<String>,
    },
    StartTyping {
        to_user_id: String,
        context_token: String,
    },
    StopTyping {
        to_user_id: String,
        context_token: String,
    },
    InterruptClaude {
        desktop_session_id: String,
    },
    RespondPermission {
        desktop_session_id: String,
        request_id: String,
        allow: bool,
        tool_use_id: Option<String>,
        updated_input: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InboundWechatTurn {
    Text(InboundWechatText),
    Media(InboundWechatMedia),
}

impl InboundWechatTurn {
    fn message_id(&self) -> &str {
        match self {
            Self::Text(turn) => &turn.message_id,
            Self::Media(turn) => &turn.message_id,
        }
    }

    fn from_user_id(&self) -> &str {
        match self {
            Self::Text(turn) => &turn.from_user_id,
            Self::Media(turn) => &turn.from_user_id,
        }
    }

    fn context_token(&self) -> &str {
        match self {
            Self::Text(turn) => &turn.context_token,
            Self::Media(turn) => &turn.context_token,
        }
    }

    fn received_at_ms(&self) -> u64 {
        match self {
            Self::Text(turn) => turn.received_at_ms,
            Self::Media(turn) => turn.received_at_ms,
        }
    }
}

#[derive(Debug, Default)]
pub struct WechatTurnManager {
    connected: bool,
    desktop_session_id: Option<String>,
    active_turn: Option<InboundWechatTurn>,
    queue: VecDeque<InboundWechatTurn>,
    pending_permission: Option<WechatPermissionRequest>,
    pending_generated_files: HashMap<String, String>,
}

impl WechatTurnManager {
    pub fn connect(&mut self) {
        self.connected = true;
    }

    pub fn disconnect(&mut self) -> Vec<WechatTurnEffect> {
        self.connected = false;
        self.queue.clear();
        self.pending_permission = None;
        self.pending_generated_files.clear();

        let effects = self
            .active_turn
            .take()
            .map(|active_turn| {
                vec![WechatTurnEffect::StopTyping {
                    to_user_id: active_turn.from_user_id().to_string(),
                    context_token: active_turn.context_token().to_string(),
                }]
            })
            .unwrap_or_default();

        effects
    }

    pub fn set_desktop_session(&mut self, session_id: String) {
        self.desktop_session_id = Some(session_id);
    }

    pub fn clear_desktop_session(&mut self) {
        self.desktop_session_id = None;
    }

    pub fn receive_text(&mut self, message: InboundWechatText) -> Vec<WechatTurnEffect> {
        if parse_stop_command(&message.text) {
            return self.stop_current_turn(message);
        }
        if let Some(command) = parse_basic_remote_command(&message.text) {
            return self.reply_to_basic_command(command, message);
        }

        let mut effects = self.discard_stale(message.received_at_ms);
        if self.pending_permission.is_some() {
            if let Some(allow) = parse_permission_decision(&message.text) {
                effects.extend(self.answer_permission(allow));
                return effects;
            }
        }
        if self.desktop_session_id.is_none() {
            effects.push(WechatTurnEffect::SendWeChatText {
                to_user_id: message.from_user_id,
                context_token: message.context_token,
                text: NO_DESKTOP_SESSION_NOTICE.into(),
            });
            return effects;
        }
        self.queue.push_back(InboundWechatTurn::Text(message));
        effects.extend(self.start_next_turn());
        effects
    }

    pub fn receive_media(&mut self, media: InboundWechatMedia) -> Vec<WechatTurnEffect> {
        let mut effects = self.discard_stale(media.received_at_ms);
        if self.desktop_session_id.is_none() {
            effects.push(WechatTurnEffect::SendWeChatText {
                to_user_id: media.from_user_id,
                context_token: media.context_token,
                text: NO_DESKTOP_SESSION_NOTICE.into(),
            });
            return effects;
        }
        self.queue.push_back(InboundWechatTurn::Media(media));
        effects.extend(self.start_next_turn());
        effects
    }

    pub fn finish_turn(&mut self, text: String, finished_at_ms: u64) -> Vec<WechatTurnEffect> {
        let Some(active_turn) = self.active_turn.take() else {
            return Vec::new();
        };
        self.pending_permission = None;
        self.pending_generated_files.clear();

        let mut effects = vec![
            WechatTurnEffect::SendWeChatText {
                to_user_id: active_turn.from_user_id().to_string(),
                context_token: active_turn.context_token().to_string(),
                text,
            },
            WechatTurnEffect::StopTyping {
                to_user_id: active_turn.from_user_id().to_string(),
                context_token: active_turn.context_token().to_string(),
            },
        ];
        effects.extend(self.discard_stale(finished_at_ms));
        effects.extend(self.start_next_turn());
        effects
    }

    pub fn request_permission(
        &mut self,
        request: WechatPermissionRequest,
    ) -> Vec<WechatTurnEffect> {
        let Some(active_turn) = self.active_turn.as_ref() else {
            return Vec::new();
        };

        let text = format!(
            "权限请求：{}\n{}\n回复 approve 或 deny。",
            request.tool_name, request.input_preview
        );
        self.pending_permission = Some(request);
        vec![WechatTurnEffect::SendWeChatText {
            to_user_id: active_turn.from_user_id().to_string(),
            context_token: active_turn.context_token().to_string(),
            text,
        }]
    }

    pub(crate) fn remember_generated_file(&mut self, tool_use_id: String, path: String) {
        if self.active_turn.is_some() {
            self.pending_generated_files.insert(tool_use_id, path);
        }
    }

    pub(crate) fn complete_generated_file(
        &mut self,
        tool_use_id: &str,
        is_error: bool,
    ) -> Vec<WechatTurnEffect> {
        let Some(path) = self.pending_generated_files.remove(tool_use_id) else {
            return Vec::new();
        };
        if is_error {
            return Vec::new();
        }
        let Some(active_turn) = self.active_turn.as_ref() else {
            return Vec::new();
        };

        vec![WechatTurnEffect::SendWeChatFile {
            to_user_id: active_turn.from_user_id().to_string(),
            context_token: active_turn.context_token().to_string(),
            path,
            caption: None,
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
        self.active_turn.as_ref().map(InboundWechatTurn::message_id)
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

    fn stop_current_turn(&mut self, command: InboundWechatText) -> Vec<WechatTurnEffect> {
        let active_turn = self.active_turn.take();
        let had_work =
            active_turn.is_some() || !self.queue.is_empty() || self.pending_permission.is_some();
        self.queue.clear();
        self.pending_permission = None;
        self.pending_generated_files.clear();

        let mut effects = Vec::new();
        if let Some(active_turn) = active_turn {
            if let Some(desktop_session_id) = self.desktop_session_id.clone() {
                effects.push(WechatTurnEffect::InterruptClaude { desktop_session_id });
            }
            effects.push(WechatTurnEffect::StopTyping {
                to_user_id: active_turn.from_user_id().to_string(),
                context_token: active_turn.context_token().to_string(),
            });
        }

        effects.push(WechatTurnEffect::SendWeChatText {
            to_user_id: command.from_user_id,
            context_token: command.context_token,
            text: if had_work {
                STOP_CONFIRM_NOTICE.into()
            } else {
                STOP_IDLE_NOTICE.into()
            },
        });
        effects
    }

    fn reply_to_basic_command(
        &self,
        command: BasicRemoteCommand,
        message: InboundWechatText,
    ) -> Vec<WechatTurnEffect> {
        let text = match command {
            BasicRemoteCommand::Help => HELP_NOTICE.into(),
            BasicRemoteCommand::Status => self.status_notice(),
            BasicRemoteCommand::ClearContext => {
                return self.clear_current_context(message);
            }
        };

        vec![WechatTurnEffect::SendWeChatText {
            to_user_id: message.from_user_id,
            context_token: message.context_token,
            text,
        }]
    }

    fn clear_current_context(&self, message: InboundWechatText) -> Vec<WechatTurnEffect> {
        if self.active_turn.is_some() || !self.queue.is_empty() || self.pending_permission.is_some()
        {
            return vec![WechatTurnEffect::SendWeChatText {
                to_user_id: message.from_user_id,
                context_token: message.context_token,
                text: CLEAR_WHILE_RUNNING_NOTICE.into(),
            }];
        }
        let Some(desktop_session_id) = self.desktop_session_id.clone() else {
            return vec![WechatTurnEffect::SendWeChatText {
                to_user_id: message.from_user_id,
                context_token: message.context_token,
                text: NO_DESKTOP_SESSION_NOTICE.into(),
            }];
        };

        vec![
            WechatTurnEffect::ClearDesktopConversation {
                desktop_session_id: desktop_session_id.clone(),
            },
            WechatTurnEffect::SendClaudeSlashCommand {
                desktop_session_id,
                command: "/clear".into(),
            },
            WechatTurnEffect::SendWeChatText {
                to_user_id: message.from_user_id,
                context_token: message.context_token,
                text: CLEAR_CONTEXT_NOTICE.into(),
            },
        ]
    }

    fn status_notice(&self) -> String {
        let mut lines = vec![
            format!(
                "微信远程状态：{}",
                if self.connected {
                    "已连接"
                } else {
                    "未连接"
                }
            ),
            format!(
                "桌面会话：{}",
                if self.desktop_session_id.is_some() {
                    "已绑定"
                } else {
                    "未绑定"
                }
            ),
            format!(
                "当前任务：{}",
                if self.active_turn.is_some() {
                    "处理中"
                } else {
                    "空闲"
                }
            ),
            format!("排队消息：{}", self.queue.len()),
        ];
        if self.pending_permission.is_some() {
            lines.push("权限请求：等待回复 approve/deny".into());
        }
        lines.join("\n")
    }

    fn discard_stale(&mut self, now_ms: u64) -> Vec<WechatTurnEffect> {
        let mut fresh = VecDeque::new();
        let mut effects = Vec::new();

        while let Some(item) = self.queue.pop_front() {
            if now_ms.saturating_sub(item.received_at_ms()) > STALE_QUEUE_AFTER_MS {
                effects.push(WechatTurnEffect::SendWeChatText {
                    to_user_id: item.from_user_id().to_string(),
                    context_token: item.context_token().to_string(),
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

        let mut effects = vec![match &next_turn {
            InboundWechatTurn::Text(turn) => WechatTurnEffect::SendToClaude {
                desktop_session_id,
                text: turn.text.clone(),
            },
            InboundWechatTurn::Media(turn) => WechatTurnEffect::DownloadMediaToClaude {
                desktop_session_id,
                media: turn.clone(),
            },
        }];
        effects.push(WechatTurnEffect::StartTyping {
            to_user_id: next_turn.from_user_id().to_string(),
            context_token: next_turn.context_token().to_string(),
        });
        self.active_turn = Some(next_turn);
        effects
    }
}

fn parse_basic_remote_command(text: &str) -> Option<BasicRemoteCommand> {
    match text.trim().to_ascii_lowercase().as_str() {
        "/help" | "help" | "帮助" | "幫助" => Some(BasicRemoteCommand::Help),
        "/status" | "status" | "状态" | "狀態" => Some(BasicRemoteCommand::Status),
        "/clear" | "clear" | "/new" | "new" | "清空" | "清除上下文" | "新会话" => {
            Some(BasicRemoteCommand::ClearContext)
        }
        _ => None,
    }
}

fn parse_permission_decision(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "approve" | "allow" | "yes" | "y" | "同意" | "允许" => Some(true),
        "deny" | "reject" | "no" | "n" | "拒绝" | "不允许" => Some(false),
        _ => None,
    }
}

fn parse_stop_command(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "/stop" | "stop" | "/cancel" | "cancel" | "停止" | "中止" | "终止" | "取消"
    )
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
    fn unbound_text_gets_notice_instead_of_entering_current_desktop_session() {
        let mut manager = WechatTurnManager::default();
        manager.connect();

        let effects = manager.receive_text(text_message("msg-1", "hello", 0));

        assert_eq!(
            effects,
            vec![WechatTurnEffect::SendWeChatText {
                to_user_id: "user@im.wechat".into(),
                context_token: "ctx-msg-1".into(),
                text: NO_DESKTOP_SESSION_NOTICE.into(),
            }],
        );
        assert_eq!(manager.active_turn_message_id(), None);
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
    fn permission_decision_message_is_not_sent_to_claude() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());
        manager.receive_text(text_message("msg-1", "first", 0));
        manager.request_permission(WechatPermissionRequest {
            request_id: "perm-1".into(),
            tool_name: "Bash".into(),
            input_preview: "{}".into(),
            tool_use_id: Some("toolu-1".into()),
            updated_input: serde_json::json!({ "cmd": "pnpm test" }),
        });

        let effects = manager.receive_text(text_message("msg-2", "approve", 1_000));

        assert_eq!(
            effects,
            vec![WechatTurnEffect::RespondPermission {
                desktop_session_id: "stdin-1".into(),
                request_id: "perm-1".into(),
                allow: true,
                tool_use_id: Some("toolu-1".into()),
                updated_input: serde_json::json!({ "cmd": "pnpm test" }),
            }],
        );
        assert_eq!(manager.pending_permission_request_id(), None);
        assert_eq!(manager.active_turn_message_id(), Some("msg-1"));
        assert_eq!(manager.queued_len(), 0);
    }

    #[test]
    fn permission_deny_message_maps_to_denial_response() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());
        manager.receive_text(text_message("msg-1", "first", 0));
        manager.request_permission(WechatPermissionRequest {
            request_id: "perm-1".into(),
            tool_name: "Bash".into(),
            input_preview: "{}".into(),
            tool_use_id: Some("toolu-1".into()),
            updated_input: serde_json::json!({ "cmd": "pnpm test" }),
        });

        let effects = manager.receive_text(text_message("msg-2", "deny", 1_000));

        assert_eq!(
            effects,
            vec![WechatTurnEffect::RespondPermission {
                desktop_session_id: "stdin-1".into(),
                request_id: "perm-1".into(),
                allow: false,
                tool_use_id: Some("toolu-1".into()),
                updated_input: serde_json::json!({ "cmd": "pnpm test" }),
            }],
        );
        assert_eq!(manager.pending_permission_request_id(), None);
        assert_eq!(manager.queued_len(), 0);
    }

    #[test]
    fn stop_command_interrupts_active_session_and_clears_wechat_work() {
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

        let effects = manager.receive_text(text_message("msg-3", "/stop", 2_000));

        assert_eq!(
            effects,
            vec![
                WechatTurnEffect::InterruptClaude {
                    desktop_session_id: "stdin-1".into(),
                },
                WechatTurnEffect::StopTyping {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-1".into(),
                },
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-3".into(),
                    text: "已停止当前任务，并清空排队消息。".into(),
                },
            ],
        );
        assert_eq!(manager.active_turn_message_id(), None);
        assert_eq!(manager.queued_len(), 0);
        assert_eq!(manager.pending_permission_request_id(), None);
        assert!(manager.finish_turn("late answer".into(), 3_000).is_empty());
    }

    #[test]
    fn help_command_replies_without_starting_a_claude_turn() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());

        let effects = manager.receive_text(text_message("msg-1", "/help", 0));

        assert_eq!(
            effects,
            vec![WechatTurnEffect::SendWeChatText {
                to_user_id: "user@im.wechat".into(),
                context_token: "ctx-msg-1".into(),
                text: "可用命令：\n/help 查看帮助\n/status 查看微信远程状态\n/stop 停止当前任务并清空排队消息\n/clear 或 /new 清空当前「微信接入」上下文\n\n权限请求时，回复 approve/deny 或 同意/拒绝。"
                    .into(),
            }],
        );
        assert_eq!(manager.active_turn_message_id(), None);
        assert_eq!(manager.queued_len(), 0);
    }

    #[test]
    fn status_command_reports_remote_state_without_interrupting_work() {
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

        let effects = manager.receive_text(text_message("msg-3", "/status", 2_000));

        assert_eq!(
            effects,
            vec![WechatTurnEffect::SendWeChatText {
                to_user_id: "user@im.wechat".into(),
                context_token: "ctx-msg-3".into(),
                text: "微信远程状态：已连接\n桌面会话：已绑定\n当前任务：处理中\n排队消息：1\n权限请求：等待回复 approve/deny".into(),
            }],
        );
        assert_eq!(manager.active_turn_message_id(), Some("msg-1"));
        assert_eq!(manager.queued_len(), 1);
        assert_eq!(manager.pending_permission_request_id(), Some("perm-1"));
    }

    #[test]
    fn clear_command_clears_desktop_context_without_starting_a_turn() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());

        let effects = manager.receive_text(text_message("msg-1", "/clear", 0));

        assert_eq!(
            effects,
            vec![
                WechatTurnEffect::ClearDesktopConversation {
                    desktop_session_id: "stdin-1".into(),
                },
                WechatTurnEffect::SendClaudeSlashCommand {
                    desktop_session_id: "stdin-1".into(),
                    command: "/clear".into(),
                },
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-1".into(),
                    text: "已清空当前「微信接入」上下文。".into(),
                },
            ],
        );
        assert_eq!(manager.active_turn_message_id(), None);
        assert_eq!(manager.queued_len(), 0);
    }

    #[test]
    fn new_command_uses_clear_context_behavior() {
        let mut manager = WechatTurnManager::default();
        manager.connect();
        manager.set_desktop_session("stdin-1".into());

        let effects = manager.receive_text(text_message("msg-1", "/new", 0));

        assert_eq!(
            effects,
            vec![
                WechatTurnEffect::ClearDesktopConversation {
                    desktop_session_id: "stdin-1".into(),
                },
                WechatTurnEffect::SendClaudeSlashCommand {
                    desktop_session_id: "stdin-1".into(),
                    command: "/clear".into(),
                },
                WechatTurnEffect::SendWeChatText {
                    to_user_id: "user@im.wechat".into(),
                    context_token: "ctx-msg-1".into(),
                    text: "已清空当前「微信接入」上下文。".into(),
                },
            ],
        );
    }

    #[test]
    fn clear_command_requires_a_bound_desktop_session() {
        let mut manager = WechatTurnManager::default();
        manager.connect();

        let effects = manager.receive_text(text_message("msg-1", "/clear", 0));

        assert_eq!(
            effects,
            vec![WechatTurnEffect::SendWeChatText {
                to_user_id: "user@im.wechat".into(),
                context_token: "ctx-msg-1".into(),
                text: NO_DESKTOP_SESSION_NOTICE.into(),
            }],
        );
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
