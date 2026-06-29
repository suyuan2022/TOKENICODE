//! WeChat typing-indicator effect: ticket resolution, best-effort send, and
//! the keepalive loop that re-asserts the "typing" status until a turn ends.
//!
//! Extracted from `executor.rs`, which mixes effect execution (stdin, iLink
//! HTTP, circuit breaking) with this typing-specific layer. The ticket cache,
//! keepalive task registry, and send retries all live here so they can evolve
//! and be tested apart from the text/file/media send paths.

use std::future::Future;
use std::time::Duration;

use serde_json::Value;

use super::api::{GetConfigResponse, IlinkApiClient, TypingStatus};
use super::effect_io::WechatEffectIo;
use super::executor::parse_response;
use super::now_ms;
use super::store::WechatStateStore;

#[cfg(not(test))]
use std::{collections::HashMap, sync::LazyLock};
#[cfg(not(test))]
use parking_lot::Mutex as StdMutex;
#[cfg(not(test))]
use tokio::task::JoinHandle;

const TYPING_TICKET_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
#[cfg(not(test))]
const TYPING_KEEPALIVE_INTERVAL_MS: u64 = 5_000;

#[cfg(not(test))]
static TYPING_KEEPALIVE_TASKS: LazyLock<StdMutex<HashMap<String, JoinHandle<()>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

pub(super) async fn execute_typing_effect(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    keepalive_key: &str,
    to_user_id: &str,
    context_token: &str,
    status: TypingStatus,
    io: &mut impl WechatEffectIo,
) -> Result<bool, String> {
    if status == TypingStatus::Stop {
        stop_typing_keepalive(keepalive_key);
    }

    let Some(ticket) =
        resolve_typing_ticket(client, store, to_user_id, context_token, io).await
    else {
        return Ok(true);
    };

    let sent = send_typing_best_effort(client, to_user_id, &ticket, status, io).await;
    if status == TypingStatus::Start && sent {
        start_typing_keepalive(
            keepalive_key.to_string(),
            client.clone(),
            to_user_id.to_string(),
            ticket,
        );
    }
    Ok(true)
}

async fn resolve_typing_ticket(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    to_user_id: &str,
    context_token: &str,
    io: &mut impl WechatEffectIo,
) -> Option<String> {
    match store.load_typing_ticket(to_user_id) {
        Ok(Some(cached))
            if !cached.ticket.trim().is_empty()
                && now_ms().saturating_sub(cached.fetched_at_ms) < TYPING_TICKET_TTL_MS =>
        {
            return Some(cached.ticket.trim().to_string());
        }
        Ok(_) => {}
        Err(err) => eprintln!("[WeChat] load typing ticket cache failed: {err}"),
    }

    let config_request = client.get_config_request(to_user_id, Some(context_token));
    let config_value = match io.execute_request(config_request).await {
        Ok(value) => value,
        Err(err) => {
            eprintln!("[WeChat] getconfig for typing failed: {err}");
            return None;
        }
    };
    let config: GetConfigResponse = match parse_response(config_value) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("[WeChat] parse getconfig for typing failed: {err}");
            return None;
        }
    };
    if config.ret.unwrap_or_default() != 0 {
        return None;
    }
    let ticket = config
        .typing_ticket
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;

    if let Err(err) = store.save_typing_ticket(to_user_id, &ticket, now_ms()) {
        eprintln!("[WeChat] save typing ticket cache failed: {err}");
    }
    Some(ticket)
}

async fn send_typing_best_effort(
    client: &IlinkApiClient,
    to_user_id: &str,
    ticket: &str,
    status: TypingStatus,
    io: &mut impl WechatEffectIo,
) -> bool {
    let typing_request = client.send_typing_request(to_user_id, ticket, status);
    match io.execute_request(typing_request).await {
        Ok(value) => {
            let ret = value.get("ret").and_then(Value::as_i64).unwrap_or_default();
            if ret != 0 {
                let errmsg = value
                    .get("errmsg")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                eprintln!("[WeChat] sendtyping returned ret={ret} errmsg={errmsg}");
                return false;
            }
            true
        }
        Err(err) => {
            eprintln!("[WeChat] sendtyping failed: {err}");
            false
        }
    }
}

pub(super) fn typing_keepalive_key(account_id: &str, to_user_id: &str) -> String {
    format!("{account_id}:{to_user_id}")
}

#[cfg(not(test))]
fn start_typing_keepalive(key: String, client: IlinkApiClient, to_user_id: String, ticket: String) {
    stop_typing_keepalive(&key);
    let handle = tokio::spawn(async move {
        run_typing_keepalive_loop(TYPING_KEEPALIVE_INTERVAL_MS, || {
            let client = client.clone();
            let request = client.send_typing_request(&to_user_id, &ticket, TypingStatus::Start);
            async move {
                let value = client
                    .execute_json::<Value>(request)
                    .await
                    .map_err(|err| format!("sendtyping keepalive failed: {err}"))?;
                let ret = value.get("ret").and_then(Value::as_i64).unwrap_or_default();
                if ret == 0 {
                    Ok(())
                } else {
                    let errmsg = value
                        .get("errmsg")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    Err(format!(
                        "sendtyping keepalive returned ret={ret} errmsg={errmsg}"
                    ))
                }
            }
        })
        .await;
    });
    TYPING_KEEPALIVE_TASKS.lock().insert(key, handle);
}

#[cfg(test)]
fn start_typing_keepalive(
    _key: String,
    _client: IlinkApiClient,
    _to_user_id: String,
    _ticket: String,
) {
}

#[cfg(not(test))]
fn stop_typing_keepalive(key: &str) {
    if let Some(handle) = TYPING_KEEPALIVE_TASKS.lock().remove(key) {
        handle.abort();
    }
}

#[cfg(test)]
fn stop_typing_keepalive(_key: &str) {}

async fn run_typing_keepalive_loop<F, Fut>(interval_ms: u64, mut send_start: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    loop {
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        if let Err(err) = send_start().await {
            eprintln!("[WeChat] {err}");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use parking_lot::Mutex;

    #[tokio::test]
    async fn typing_keepalive_loop_repeats_until_send_fails() {
        let sends = Arc::new(Mutex::new(0usize));
        let captured = sends.clone();

        run_typing_keepalive_loop(1, move || {
            let captured = captured.clone();
            async move {
                let mut sends = captured.lock();
                *sends += 1;
                if *sends >= 3 {
                    Err("stop keepalive".into())
                } else {
                    Ok(())
                }
            }
        })
        .await;

        assert_eq!(*sends.lock(), 3);
    }
}
