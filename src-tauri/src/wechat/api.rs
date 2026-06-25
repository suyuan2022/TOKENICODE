use std::collections::BTreeMap;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
pub const DEFAULT_LONG_POLL_TIMEOUT_MS: u64 = 35_000;
pub const DEFAULT_API_TIMEOUT_MS: u64 = 15_000;
pub const DEFAULT_CONFIG_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IlinkHttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IlinkHttpRequest {
    pub method: IlinkHttpMethod,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct IlinkApiClient {
    base_url: String,
    token: Option<String>,
    route_tag: Option<String>,
}

impl IlinkApiClient {
    pub fn new(token: Option<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            token: token.filter(|value| !value.trim().is_empty()),
            route_tag: None,
        }
    }

    pub fn with_base_url(token: Option<String>, base_url: impl Into<String>) -> Self {
        let mut client = Self::new(token);
        client.base_url = sanitize_base_url(&base_url.into());
        client
    }

    pub fn with_route_tag(mut self, route_tag: Option<String>) -> Self {
        self.route_tag = route_tag.filter(|value| !value.trim().is_empty());
        self
    }

    pub fn qr_code_request(&self) -> IlinkHttpRequest {
        self.get_request(
            "ilink/bot/get_bot_qrcode?bot_type=3",
            DEFAULT_CONFIG_TIMEOUT_MS,
        )
    }

    pub fn qr_status_request(&self, qrcode: &str, verify_code: Option<&str>) -> IlinkHttpRequest {
        let mut endpoint = format!(
            "ilink/bot/get_qrcode_status?qrcode={}",
            percent_encode_query_value(qrcode)
        );
        if let Some(verify_code) = verify_code.filter(|value| !value.trim().is_empty()) {
            endpoint.push_str("&verify_code=");
            endpoint.push_str(&percent_encode_query_value(verify_code));
        }
        self.get_request(&endpoint, DEFAULT_LONG_POLL_TIMEOUT_MS)
    }

    pub fn get_updates_request(&self, get_updates_buf: Option<String>) -> IlinkHttpRequest {
        self.post_request(
            "ilink/bot/getupdates",
            json!({
                "get_updates_buf": get_updates_buf.unwrap_or_default(),
                "base_info": base_info_value(),
            }),
            DEFAULT_LONG_POLL_TIMEOUT_MS,
        )
    }

    pub fn send_text_request(
        &self,
        to_user_id: &str,
        context_token: &str,
        text: &str,
        client_id: &str,
    ) -> IlinkHttpRequest {
        self.post_request(
            "ilink/bot/sendmessage",
            json!({
                "msg": {
                    "from_user_id": "",
                    "to_user_id": to_user_id,
                    "client_id": client_id,
                    "message_type": MessageType::Bot as i32,
                    "message_state": MessageState::Finish as i32,
                    "item_list": [
                        {
                            "type": MessageItemType::Text as i32,
                            "text_item": { "text": text },
                        }
                    ],
                    "context_token": context_token,
                },
                "base_info": base_info_value(),
            }),
            DEFAULT_API_TIMEOUT_MS,
        )
    }

    pub fn get_config_request(
        &self,
        ilink_user_id: &str,
        context_token: Option<&str>,
    ) -> IlinkHttpRequest {
        self.post_request(
            "ilink/bot/getconfig",
            json!({
                "ilink_user_id": ilink_user_id,
                "context_token": context_token,
                "base_info": base_info_value(),
            }),
            DEFAULT_CONFIG_TIMEOUT_MS,
        )
    }

    pub fn send_typing_request(
        &self,
        ilink_user_id: &str,
        typing_ticket: &str,
        status: TypingStatus,
    ) -> IlinkHttpRequest {
        self.post_request(
            "ilink/bot/sendtyping",
            json!({
                "ilink_user_id": ilink_user_id,
                "typing_ticket": typing_ticket,
                "status": status as i32,
                "base_info": base_info_value(),
            }),
            DEFAULT_CONFIG_TIMEOUT_MS,
        )
    }

    pub fn notify_start_request(&self) -> IlinkHttpRequest {
        self.post_request(
            "ilink/bot/msg/notifystart",
            json!({ "base_info": base_info_value() }),
            DEFAULT_CONFIG_TIMEOUT_MS,
        )
    }

    pub fn notify_stop_request(&self) -> IlinkHttpRequest {
        self.post_request(
            "ilink/bot/msg/notifystop",
            json!({ "base_info": base_info_value() }),
            DEFAULT_CONFIG_TIMEOUT_MS,
        )
    }

    pub async fn execute_json<T: DeserializeOwned>(
        &self,
        request: IlinkHttpRequest,
    ) -> Result<T, String> {
        let method = match request.method {
            IlinkHttpMethod::Get => reqwest::Method::GET,
            IlinkHttpMethod::Post => reqwest::Method::POST,
        };
        let client = reqwest::Client::new();
        let mut builder = client
            .request(method, &request.url)
            .timeout(Duration::from_millis(request.timeout_ms));
        for (key, value) in &request.headers {
            builder = builder.header(key, value);
        }
        if request.method == IlinkHttpMethod::Post {
            builder = builder.json(&request.body);
        }

        let response = builder
            .send()
            .await
            .map_err(|err| format!("iLink request failed: {err}"))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| format!("iLink response read failed: {err}"))?;
        if !status.is_success() {
            return Err(format!(
                "iLink HTTP {status}: {}",
                truncate_for_error(&body)
            ));
        }
        serde_json::from_str(&body).map_err(|err| {
            format!(
                "iLink response JSON parse failed: {err}: {}",
                truncate_for_error(&body)
            )
        })
    }

    fn get_request(&self, endpoint: &str, timeout_ms: u64) -> IlinkHttpRequest {
        IlinkHttpRequest {
            method: IlinkHttpMethod::Get,
            url: self.url(endpoint),
            headers: build_common_headers(self.route_tag.as_deref()),
            body: Value::Null,
            timeout_ms,
        }
    }

    fn post_request(&self, endpoint: &str, body: Value, timeout_ms: u64) -> IlinkHttpRequest {
        IlinkHttpRequest {
            method: IlinkHttpMethod::Post,
            url: self.url(endpoint),
            headers: build_auth_headers(
                self.token.as_deref(),
                self.route_tag.as_deref(),
                rand::random::<u32>(),
            ),
            body,
            timeout_ms,
        }
    }

    fn url(&self, endpoint: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            endpoint.trim_start_matches('/')
        )
    }
}

pub fn wechat_channel_version() -> String {
    format!("TOKENICODE/{}", env!("CARGO_PKG_VERSION"))
}

pub fn build_auth_headers(
    token: Option<&str>,
    route_tag: Option<&str>,
    random_uin_source: u32,
) -> BTreeMap<String, String> {
    let mut headers = build_common_headers(route_tag);
    headers.insert("Content-Type".into(), "application/json".into());
    headers.insert("AuthorizationType".into(), "ilink_bot_token".into());
    headers.insert(
        "X-WECHAT-UIN".into(),
        wechat_uin_from_u32(random_uin_source),
    );
    if let Some(token) = token.filter(|value| !value.trim().is_empty()) {
        headers.insert("Authorization".into(), format!("Bearer {}", token.trim()));
    }
    headers
}

pub fn classify_send_response(response: &SendMessageResponse) -> SendResponseClass {
    match response.ret.unwrap_or_default() {
        0 => SendResponseClass::Ok,
        -2 if response
            .errmsg
            .as_deref()
            .map(|errmsg| errmsg.trim().eq_ignore_ascii_case("unknown error"))
            .unwrap_or(false) =>
        {
            SendResponseClass::StaleSession
        }
        -2 => SendResponseClass::RateLimited,
        _ => SendResponseClass::Failed,
    }
}

fn base_info_value() -> Value {
    json!({ "channel_version": wechat_channel_version() })
}

fn build_common_headers(route_tag: Option<&str>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("iLink-App-Id".into(), "wxeb7ec651dd0aefa9".into());
    headers.insert("iLink-App-ClientVersion".into(), "100".into());
    if let Some(route_tag) = route_tag.filter(|value| !value.trim().is_empty()) {
        headers.insert("SKRouteTag".into(), route_tag.trim().into());
    }
    headers
}

fn wechat_uin_from_u32(value: u32) -> String {
    BASE64_STANDARD.encode(value.to_string())
}

fn sanitize_base_url(input: &str) -> String {
    let parsed = reqwest::Url::parse(input.trim());
    let Ok(url) = parsed else {
        return DEFAULT_BASE_URL.into();
    };
    let Some(host) = url.host_str() else {
        return DEFAULT_BASE_URL.into();
    };
    let allowed = url.scheme() == "https"
        && (host == "weixin.qq.com"
            || host.ends_with(".weixin.qq.com")
            || host == "wechat.com"
            || host.ends_with(".wechat.com"));
    if allowed {
        input.trim().trim_end_matches('/').into()
    } else {
        DEFAULT_BASE_URL.into()
    }
}

fn percent_encode_query_value(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn truncate_for_error(value: &str) -> String {
    const MAX: usize = 512;
    if value.len() <= MAX {
        value.into()
    } else {
        format!("{}...", &value[..MAX])
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    User = 1,
    Bot = 2,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageItemType {
    Text = 1,
    Image = 2,
    Voice = 3,
    File = 4,
    Video = 5,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageState {
    Generating = 1,
    Finish = 2,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingStatus {
    Start = 1,
    Stop = 2,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseInfo {
    pub channel_version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextItem {
    pub text: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CdnMedia {
    pub encrypt_query_param: Option<String>,
    pub aes_key: Option<String>,
    pub encrypt_type: Option<i32>,
    pub full_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageItem {
    pub media: Option<CdnMedia>,
    pub thumb_media: Option<CdnMedia>,
    pub aeskey: Option<String>,
    pub mid_size: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceItem {
    pub media: Option<CdnMedia>,
    pub text: Option<String>,
    pub encode_type: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileItem {
    pub media: Option<CdnMedia>,
    pub file_name: Option<String>,
    pub len: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageItem {
    #[serde(rename = "type")]
    pub item_type: Option<i32>,
    pub text_item: Option<TextItem>,
    pub ref_msg: Option<RefMessage>,
    pub image_item: Option<ImageItem>,
    pub voice_item: Option<VoiceItem>,
    pub file_item: Option<FileItem>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefMessage {
    pub message_item: Option<Box<MessageItem>>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WechatMessage {
    pub seq: Option<i64>,
    pub message_id: Option<i64>,
    pub from_user_id: Option<String>,
    pub to_user_id: Option<String>,
    pub client_id: Option<String>,
    pub create_time_ms: Option<i64>,
    pub session_id: Option<String>,
    pub message_type: Option<i32>,
    pub message_state: Option<i32>,
    #[serde(default)]
    pub item_list: Vec<MessageItem>,
    pub context_token: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUpdatesResponse {
    pub ret: Option<i32>,
    pub errcode: Option<i32>,
    pub errmsg: Option<String>,
    #[serde(default)]
    pub msgs: Vec<WechatMessage>,
    pub get_updates_buf: Option<String>,
    pub longpolling_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessageResponse {
    pub ret: Option<i32>,
    pub errcode: Option<i32>,
    pub errmsg: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetConfigResponse {
    pub ret: Option<i32>,
    pub errcode: Option<i32>,
    pub errmsg: Option<String>,
    pub typing_ticket: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrCodeResponse {
    pub ret: Option<i32>,
    pub errmsg: Option<String>,
    pub qrcode: Option<String>,
    pub qrcode_img_content: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QrStatusResponse {
    pub ret: Option<i32>,
    pub status: Option<String>,
    pub retmsg: Option<String>,
    pub bot_token: Option<String>,
    pub ilink_bot_id: Option<String>,
    pub baseurl: Option<String>,
    pub ilink_user_id: Option<String>,
    pub redirect_host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendResponseClass {
    Ok,
    RateLimited,
    StaleSession,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_auth_headers_with_random_wechat_uin_shape() {
        let headers = build_auth_headers(Some("bot-token"), Some("route-a"), 42);

        assert_eq!(
            headers.get("Authorization"),
            Some(&"Bearer bot-token".into())
        );
        assert_eq!(
            headers.get("AuthorizationType"),
            Some(&"ilink_bot_token".into())
        );
        assert_eq!(headers.get("SKRouteTag"), Some(&"route-a".into()));
        assert_eq!(headers.get("X-WECHAT-UIN"), Some(&"NDI=".into()));
    }

    #[test]
    fn builds_get_updates_request_with_cursor_and_base_info() {
        let client = IlinkApiClient::new(Some("bot-token".into()));

        let request = client.get_updates_request(Some("sync-cursor".into()));

        assert_eq!(request.method, IlinkHttpMethod::Post);
        assert_eq!(
            request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/getupdates"
        );
        assert_eq!(request.timeout_ms, 35_000);
        assert_eq!(
            request.body["get_updates_buf"],
            serde_json::Value::String("sync-cursor".into())
        );
        assert_eq!(
            request.body["base_info"]["channel_version"],
            serde_json::Value::String(wechat_channel_version())
        );
    }

    #[test]
    fn builds_qr_login_requests_without_bearer_token() {
        let client = IlinkApiClient::new(None);

        let qr = client.qr_code_request();
        let poll = client.qr_status_request("qr id/needs escaping", None);

        assert_eq!(qr.method, IlinkHttpMethod::Get);
        assert_eq!(
            qr.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/get_bot_qrcode?bot_type=3"
        );
        assert!(qr.headers.get("Authorization").is_none());
        assert_eq!(poll.method, IlinkHttpMethod::Get);
        assert_eq!(
            poll.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/get_qrcode_status?qrcode=qr%20id%2Fneeds%20escaping"
        );
        assert_eq!(poll.timeout_ms, 35_000);
    }

    #[test]
    fn builds_send_text_with_context_token() {
        let client = IlinkApiClient::new(Some("bot-token".into()));

        let request = client.send_text_request("user-1", "ctx-1", "hello", "client-1");

        assert_eq!(request.method, IlinkHttpMethod::Post);
        assert_eq!(
            request.url,
            "https://ilinkai.weixin.qq.com/ilink/bot/sendmessage"
        );
        assert_eq!(request.timeout_ms, 15_000);
        assert_eq!(request.body["msg"]["to_user_id"], "user-1");
        assert_eq!(request.body["msg"]["context_token"], "ctx-1");
        assert_eq!(request.body["msg"]["client_id"], "client-1");
        assert_eq!(request.body["msg"]["message_type"], 2);
        assert_eq!(request.body["msg"]["message_state"], 2);
        assert_eq!(request.body["msg"]["item_list"][0]["type"], 1);
        assert_eq!(
            request.body["msg"]["item_list"][0]["text_item"]["text"],
            "hello"
        );
        assert_eq!(
            request.body["base_info"]["channel_version"],
            serde_json::Value::String(wechat_channel_version())
        );
    }

    #[test]
    fn classifies_ilink_send_response_errors() {
        assert_eq!(
            classify_send_response(&SendMessageResponse {
                ret: Some(-2),
                errcode: None,
                errmsg: Some("unknown error".into()),
            }),
            SendResponseClass::StaleSession
        );
        assert_eq!(
            classify_send_response(&SendMessageResponse {
                ret: Some(-2),
                errcode: None,
                errmsg: Some("too frequent".into()),
            }),
            SendResponseClass::RateLimited
        );
        assert_eq!(
            classify_send_response(&SendMessageResponse {
                ret: Some(0),
                errcode: None,
                errmsg: None,
            }),
            SendResponseClass::Ok
        );
    }
}
