use crate::wechat::{
    api::{QrCodeResponse, QrStatusResponse, DEFAULT_BASE_URL},
    store::WechatAccount,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WechatQrCode {
    pub qrcode_id: String,
    pub qrcode_image: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WechatQrPoll {
    Pending {
        status: String,
        message: Option<String>,
        redirect_base_url: Option<String>,
    },
    Connected {
        account: WechatAccount,
    },
    Failed {
        status: String,
        message: String,
    },
}

pub fn parse_qr_code_response(response: QrCodeResponse) -> Result<WechatQrCode, String> {
    ensure_ret_ok(response.ret, response.errmsg.as_deref())?;
    let qrcode_id = response
        .qrcode
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "iLink QR response missing qrcode".to_string())?;
    let qrcode_image = response
        .qrcode_img_content
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "iLink QR response missing qrcode image".to_string())?;

    Ok(WechatQrCode {
        qrcode_id,
        qrcode_image,
    })
}

pub fn parse_qr_status_response(
    response: QrStatusResponse,
    now_ms: u64,
) -> Result<WechatQrPoll, String> {
    ensure_ret_ok(response.ret, response.retmsg.as_deref())?;

    let status = response.status.unwrap_or_else(|| "wait".into());
    match status.as_str() {
        "confirmed" => Ok(WechatQrPoll::Connected {
            account: WechatAccount {
                bot_token: required(response.bot_token, "bot_token")?,
                account_id: required(response.ilink_bot_id, "ilink_bot_id")?,
                base_url: response
                    .baseurl
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| DEFAULT_BASE_URL.into()),
                user_id: required(response.ilink_user_id, "ilink_user_id")?,
                created_at_ms: now_ms,
            },
        }),
        "scaned_but_redirect" => {
            let redirect_base_url = redirect_base_url_from_host(response.redirect_host.as_deref());
            Ok(WechatQrPoll::Pending {
                status,
                message: response.retmsg.or(response.redirect_host),
                redirect_base_url,
            })
        }
        "wait" | "scaned" | "need_verifycode" => Ok(WechatQrPoll::Pending {
            status,
            message: response.retmsg,
            redirect_base_url: None,
        }),
        "expired" => Ok(WechatQrPoll::Failed {
            status,
            message: response.retmsg.unwrap_or_else(|| "QR code expired".into()),
        }),
        _ => Ok(WechatQrPoll::Failed {
            status: status.clone(),
            message: response.retmsg.unwrap_or(status),
        }),
    }
}

fn ensure_ret_ok(ret: Option<i32>, message: Option<&str>) -> Result<(), String> {
    let ret = ret.unwrap_or_default();
    if ret == 0 {
        Ok(())
    } else {
        Err(format!(
            "iLink QR response ret={ret}: {}",
            message.unwrap_or("unknown error")
        ))
    }
}

fn required(value: Option<String>, field: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("confirmed iLink QR status missing {field}"))
}

fn redirect_base_url_from_host(redirect_host: Option<&str>) -> Option<String> {
    let host = redirect_host?.trim().trim_end_matches('/');
    if host.is_empty() {
        return None;
    }

    let without_scheme = host
        .strip_prefix("https://")
        .or_else(|| host.strip_prefix("http://"))
        .unwrap_or(host);
    let candidate = format!("https://{without_scheme}");
    let url = reqwest::Url::parse(&candidate).ok()?;
    let host = url.host_str()?;
    let allowed = host == "weixin.qq.com"
        || host.ends_with(".weixin.qq.com")
        || host == "wechat.com"
        || host.ends_with(".wechat.com");
    if allowed {
        Some(format!("https://{host}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wechat::api::{QrCodeResponse, QrStatusResponse};

    #[test]
    fn parses_qr_code_response() {
        let parsed = parse_qr_code_response(QrCodeResponse {
            ret: Some(0),
            errmsg: None,
            qrcode: Some("qr-1".into()),
            qrcode_img_content: Some("data:image/png;base64,abc".into()),
        })
        .unwrap();

        assert_eq!(parsed.qrcode_id, "qr-1");
        assert_eq!(parsed.qrcode_image, "data:image/png;base64,abc");
    }

    #[test]
    fn rejects_incomplete_qr_code_response() {
        let err = parse_qr_code_response(QrCodeResponse {
            ret: Some(0),
            errmsg: None,
            qrcode: None,
            qrcode_img_content: Some("abc".into()),
        })
        .unwrap_err();

        assert!(err.contains("missing qrcode"));
    }

    #[test]
    fn maps_confirmed_qr_status_to_account() {
        let poll = parse_qr_status_response(
            QrStatusResponse {
                ret: Some(0),
                status: Some("confirmed".into()),
                retmsg: None,
                bot_token: Some("bot-token".into()),
                ilink_bot_id: Some("bot-id".into()),
                baseurl: None,
                ilink_user_id: Some("user-1".into()),
                redirect_host: None,
            },
            123,
        )
        .unwrap();

        assert_eq!(
            poll,
            WechatQrPoll::Connected {
                account: WechatAccount {
                    bot_token: "bot-token".into(),
                    account_id: "bot-id".into(),
                    base_url: "https://ilinkai.weixin.qq.com".into(),
                    user_id: "user-1".into(),
                    created_at_ms: 123,
                },
            }
        );
    }

    #[test]
    fn keeps_wait_and_scaned_as_pending_states() {
        assert_eq!(
            parse_qr_status_response(
                QrStatusResponse {
                    ret: Some(0),
                    status: Some("wait".into()),
                    retmsg: None,
                    bot_token: None,
                    ilink_bot_id: None,
                    baseurl: None,
                    ilink_user_id: None,
                    redirect_host: None,
                },
                123,
            )
            .unwrap(),
            WechatQrPoll::Pending {
                status: "wait".into(),
                message: None,
                redirect_base_url: None,
            }
        );

        assert_eq!(
            parse_qr_status_response(
                QrStatusResponse {
                    ret: Some(0),
                    status: Some("scaned".into()),
                    retmsg: Some("scanned".into()),
                    bot_token: None,
                    ilink_bot_id: None,
                    baseurl: None,
                    ilink_user_id: None,
                    redirect_host: None,
                },
                123,
            )
            .unwrap(),
            WechatQrPoll::Pending {
                status: "scaned".into(),
                message: Some("scanned".into()),
                redirect_base_url: None,
            }
        );
    }

    #[test]
    fn scaned_redirect_exposes_next_polling_base_url() {
        assert_eq!(
            parse_qr_status_response(
                QrStatusResponse {
                    ret: Some(0),
                    status: Some("scaned_but_redirect".into()),
                    retmsg: None,
                    bot_token: None,
                    ilink_bot_id: None,
                    baseurl: None,
                    ilink_user_id: None,
                    redirect_host: Some("hk.weixin.qq.com".into()),
                },
                123,
            )
            .unwrap(),
            WechatQrPoll::Pending {
                status: "scaned_but_redirect".into(),
                message: Some("hk.weixin.qq.com".into()),
                redirect_base_url: Some("https://hk.weixin.qq.com".into()),
            }
        );
    }
}
