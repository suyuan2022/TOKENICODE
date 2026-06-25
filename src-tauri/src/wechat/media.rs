use crate::wechat::inbound::InboundWechatCdnMedia;
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};
use aes::Aes128;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};

pub const DEFAULT_CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
pub const CDN_DOWNLOAD_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WechatCdnDownloadRequest {
    pub url: String,
    pub timeout_ms: u64,
}

pub fn build_cdn_download_request(
    cdn: &InboundWechatCdnMedia,
) -> Result<WechatCdnDownloadRequest, String> {
    let url = if let Some(full_url) = non_empty(cdn.full_url.as_deref()) {
        if !full_url.starts_with("https://") {
            return Err("WeChat CDN full_url must use https".into());
        }
        full_url.to_string()
    } else {
        let encrypt_query_param =
            non_empty(cdn.encrypt_query_param.as_deref()).ok_or_else(|| {
                "WeChat CDN media missing encrypt_query_param or full_url".to_string()
            })?;
        format!(
            "{}/download?encrypted_query_param={}",
            DEFAULT_CDN_BASE_URL,
            percent_encode_query_value(encrypt_query_param)
        )
    };

    Ok(WechatCdnDownloadRequest {
        url,
        timeout_ms: CDN_DOWNLOAD_TIMEOUT_MS,
    })
}

pub fn parse_cdn_aes_key(aes_key_base64: &str) -> Result<[u8; 16], String> {
    let decoded = BASE64_STANDARD
        .decode(aes_key_base64.trim())
        .map_err(|err| format!("WeChat CDN aes_key base64 decode failed: {err}"))?;

    if decoded.len() == 16 {
        return decoded
            .try_into()
            .map_err(|_| "WeChat CDN aes_key must decode to 16 bytes".to_string());
    }

    if decoded.len() == 32 {
        let hex_key = std::str::from_utf8(&decoded)
            .map_err(|_| "WeChat CDN aes_key hex form must be ASCII".to_string())?;
        return decode_hex_aes_key(hex_key);
    }

    Err(format!(
        "WeChat CDN aes_key must decode to 16 raw bytes or 32 hex bytes, got {} bytes",
        decoded.len()
    ))
}

pub fn decrypt_aes_128_ecb_pkcs7(encrypted: &[u8], aes_key: &[u8; 16]) -> Result<Vec<u8>, String> {
    if encrypted.is_empty() || encrypted.len() % 16 != 0 {
        return Err("WeChat CDN encrypted media length must be a non-empty multiple of 16".into());
    }

    let cipher = Aes128::new_from_slice(aes_key)
        .map_err(|_| "WeChat CDN AES-128 key must be 16 bytes".to_string())?;
    let mut decrypted = encrypted.to_vec();
    for chunk in decrypted.chunks_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }

    remove_pkcs7_padding(&mut decrypted)?;
    Ok(decrypted)
}

fn remove_pkcs7_padding(buffer: &mut Vec<u8>) -> Result<(), String> {
    let padding = buffer
        .last()
        .copied()
        .ok_or_else(|| "WeChat CDN decrypted media is empty".to_string())?
        as usize;
    if padding == 0 || padding > 16 || padding > buffer.len() {
        return Err("WeChat CDN decrypted media has invalid PKCS#7 padding".into());
    }
    if !buffer[buffer.len() - padding..]
        .iter()
        .all(|byte| *byte as usize == padding)
    {
        return Err("WeChat CDN decrypted media has invalid PKCS#7 padding".into());
    }
    buffer.truncate(buffer.len() - padding);
    Ok(())
}

fn decode_hex_aes_key(hex_key: &str) -> Result<[u8; 16], String> {
    let hex_key = hex_key.trim();
    if hex_key.len() != 32 {
        return Err("WeChat CDN hex aes_key must be 32 characters".into());
    }

    let mut bytes = [0_u8; 16];
    for (index, pair) in hex_key.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).map_err(|_| "WeChat CDN hex aes_key must be ASCII")?;
        bytes[index] = u8::from_str_radix(pair, 16)
            .map_err(|_| "WeChat CDN hex aes_key must contain only hex digits".to_string())?;
    }
    Ok(bytes)
}

fn percent_encode_query_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            byte => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_cdn_download_request_from_full_url_or_encrypted_query_param() {
        let direct = build_cdn_download_request(&InboundWechatCdnMedia {
            encrypt_query_param: Some("ignored-query".into()),
            aes_key: "key".into(),
            encrypt_type: Some(1),
            full_url: Some("https://novac2c.cdn.weixin.qq.com/c2c/direct?id=1".into()),
        })
        .unwrap();

        assert_eq!(
            direct,
            WechatCdnDownloadRequest {
                url: "https://novac2c.cdn.weixin.qq.com/c2c/direct?id=1".into(),
                timeout_ms: CDN_DOWNLOAD_TIMEOUT_MS,
            }
        );

        let fallback = build_cdn_download_request(&InboundWechatCdnMedia {
            encrypt_query_param: Some("a=b&x=hello world?".into()),
            aes_key: "key".into(),
            encrypt_type: Some(1),
            full_url: None,
        })
        .unwrap();

        assert_eq!(
            fallback.url,
            "https://novac2c.cdn.weixin.qq.com/c2c/download?encrypted_query_param=a%3Db%26x%3Dhello%20world%3F"
        );
        assert_eq!(fallback.timeout_ms, CDN_DOWNLOAD_TIMEOUT_MS);
    }

    #[test]
    fn parses_aes_key_from_base64_raw_bytes_or_base64_hex_string() {
        assert_eq!(
            parse_cdn_aes_key("MDEyMzQ1Njc4OWFiY2RlZg==").unwrap(),
            *b"0123456789abcdef"
        );
        assert_eq!(
            parse_cdn_aes_key("MzAzMTMyMzMzNDM1MzYzNzM4Mzk2MTYyNjM2NDY1NjY=").unwrap(),
            *b"0123456789abcdef"
        );
    }

    #[test]
    fn decrypts_aes_128_ecb_pkcs7_media_bytes() {
        let encrypted = BASE64_STANDARD
            .decode("xhoD0c7E8emien3r349dx0yFRk8xSm9+WOSTOn8Wjn4=")
            .unwrap();
        let key = parse_cdn_aes_key("MDEyMzQ1Njc4OWFiY2RlZg==").unwrap();

        let decrypted = decrypt_aes_128_ecb_pkcs7(&encrypted, &key).unwrap();

        assert_eq!(decrypted, b"hello wechat media");
    }
}
