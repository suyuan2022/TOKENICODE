use std::{
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::commands::StdinManager;
use serde::de::DeserializeOwned;
use serde_json::json;
use serde_json::Value;

use super::{
    api::{
        classify_send_response, GetConfigResponse, IlinkApiClient, IlinkHttpRequest,
        SendMessageResponse, SendResponseClass, TypingStatus,
    },
    store::WechatStateStore,
    turn::WechatTurnEffect,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WechatEffectDispatch {
    pub claude_effect_count: usize,
    pub wechat_effect_count: usize,
}

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
                    "content": text,
                },
            });
            stdin_mgr
                .send(desktop_session_id, &payload.to_string())
                .await?;
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
        _ => Ok(false),
    }
}

pub async fn execute_turn_effects(
    stdin_mgr: &StdinManager,
    store: &WechatStateStore,
    effects: &[WechatTurnEffect],
) -> Result<WechatEffectDispatch, String> {
    execute_turn_effects_with(stdin_mgr, store, effects, |request| async move {
        IlinkApiClient::new(None)
            .execute_json::<Value>(request)
            .await
    })
    .await
}

pub async fn execute_turn_effects_with<F, Fut>(
    stdin_mgr: &StdinManager,
    store: &WechatStateStore,
    effects: &[WechatTurnEffect],
    mut execute_wechat_request: F,
) -> Result<WechatEffectDispatch, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let mut dispatch = WechatEffectDispatch::default();
    for effect in effects {
        if execute_claude_effect(stdin_mgr, effect).await? {
            dispatch.claude_effect_count += 1;
        }
        if execute_wechat_effect_with(effect, store, &mut execute_wechat_request).await? {
            dispatch.wechat_effect_count += 1;
        }
    }
    Ok(dispatch)
}

pub async fn execute_wechat_effect(
    effect: &WechatTurnEffect,
    store: &WechatStateStore,
) -> Result<bool, String> {
    execute_wechat_effect_with(effect, store, |request| async move {
        IlinkApiClient::new(None)
            .execute_json::<Value>(request)
            .await
    })
    .await
}

pub async fn execute_wechat_effect_with<F, Fut>(
    effect: &WechatTurnEffect,
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

    match effect {
        WechatTurnEffect::SendWeChatText {
            to_user_id,
            context_token,
            text,
        } => {
            let request =
                client.send_text_request(to_user_id, context_token, text, &new_client_id());
            let response: SendMessageResponse = parse_response(execute_request(request).await?)?;
            match classify_send_response(&response) {
                SendResponseClass::Ok => Ok(true),
                SendResponseClass::RateLimited => Err("WeChat sendmessage rate limited".into()),
                SendResponseClass::StaleSession => {
                    Err("WeChat sendmessage stale session; reconnect required".into())
                }
                SendResponseClass::Failed => Err(format!(
                    "WeChat sendmessage failed: ret={:?} errcode={:?} errmsg={:?}",
                    response.ret, response.errcode, response.errmsg
                )),
            }
        }
        WechatTurnEffect::StartTyping {
            to_user_id,
            context_token,
        } => {
            execute_typing_effect(
                &client,
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
            execute_typing_effect(
                &client,
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

async fn execute_typing_effect<F, Fut>(
    client: &IlinkApiClient,
    to_user_id: &str,
    context_token: &str,
    status: TypingStatus,
    execute_request: &mut F,
) -> Result<bool, String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let config_request = client.get_config_request(to_user_id, Some(context_token));
    let config_value = match execute_request(config_request).await {
        Ok(value) => value,
        Err(err) => {
            eprintln!("[WeChat] getconfig for typing failed: {err}");
            return Ok(true);
        }
    };
    let config: GetConfigResponse = match parse_response(config_value) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("[WeChat] parse getconfig for typing failed: {err}");
            return Ok(true);
        }
    };
    let Some(ticket) = config
        .typing_ticket
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|_| config.ret.unwrap_or_default() == 0)
    else {
        return Ok(true);
    };

    let typing_request = client.send_typing_request(to_user_id, &ticket, status);
    if let Err(err) = execute_request(typing_request).await {
        eprintln!("[WeChat] sendtyping failed: {err}");
    }
    Ok(true)
}

fn parse_response<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|err| format!("parse iLink response: {err}"))
}

fn new_client_id() -> String {
    format!("tc-{}-{}", now_ms(), rand::random::<u32>())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::{Child, Command};

    use crate::wechat::store::{WechatAccount, WechatStateStore};

    #[tokio::test]
    async fn send_to_claude_effect_writes_user_ndjson_to_existing_stdin_manager() {
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
