//! WeChat outbound file/image send: manifest expansion, per-file encrypt +
//! CDN upload, and the image-vs-file send dispatch.
//!
//! Extracted from `executor.rs`, which mixes effect dispatch with this
//! file-send pipeline. The shared send plumbing it leans on
//! (`execute_send_message_request`, the circuit breaker, `new_client_id`,
//! `parse_response`) stays in `executor` because the text path uses it too;
//! only the file-specific stages live here.

use std::future::Future;
use std::path::Path;

use serde_json::Value;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use rand::RngCore;

use super::api::{GetUploadUrlResponse, IlinkApiClient, IlinkHttpRequest, UploadMediaType};
use super::executor::{execute_send_message_request, new_client_id, parse_response};
use super::media::{
    aes_128_ecb_pkcs7_padded_size, build_cdn_upload_request, encrypt_aes_128_ecb_pkcs7,
    WechatCdnUploadRequest,
};
use super::store::WechatStateStore;
use super::text::filter_wechat_markdown;

const MAX_WECHAT_FILE_BYTES: u64 = 25 * 1024 * 1024;
pub(super) const WECHAT_SEND_MANIFEST_BASENAME: &str = "tokenicode-wechat-send-files.json";
const MAX_WECHAT_SEND_MANIFEST_FILES: usize = 10;

pub(super) async fn execute_wechat_file_effect<F, Fut, H, Hut>(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    to_user_id: &str,
    context_token: &str,
    path: &str,
    caption: Option<&str>,
    execute_request: &mut F,
    upload_media: &mut H,
) -> Result<(), String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
    H: FnMut(WechatCdnUploadRequest) -> Hut,
    Hut: Future<Output = Result<String, String>>,
{
    let path = Path::new(path);
    if let Some(paths) = wechat_send_manifest_paths(path)? {
        eprintln!(
            "[WeChat] sending {} file(s) listed in manifest {}",
            paths.len(),
            path.display()
        );
        for path in paths {
            send_single_wechat_file(
                client,
                store,
                to_user_id,
                context_token,
                Path::new(&path),
                caption,
                execute_request,
                upload_media,
            )
            .await?;
        }
        return Ok(());
    }

    send_single_wechat_file(
        client,
        store,
        to_user_id,
        context_token,
        path,
        caption,
        execute_request,
        upload_media,
    )
    .await
}

async fn send_single_wechat_file<F, Fut, H, Hut>(
    client: &IlinkApiClient,
    store: &WechatStateStore,
    to_user_id: &str,
    context_token: &str,
    path: &Path,
    caption: Option<&str>,
    execute_request: &mut F,
    upload_media: &mut H,
) -> Result<(), String>
where
    F: FnMut(IlinkHttpRequest) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
    H: FnMut(WechatCdnUploadRequest) -> Hut,
    Hut: Future<Output = Result<String, String>>,
{
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("WeChat file send stat failed for {}: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "WeChat file send path is not a file: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_WECHAT_FILE_BYTES {
        return Err(format!(
            "WeChat file too large: {} bytes, max {} bytes",
            metadata.len(),
            MAX_WECHAT_FILE_BYTES
        ));
    }

    let plaintext = std::fs::read(path)
        .map_err(|err| format!("WeChat file send read failed for {}: {err}", path.display()))?;
    let raw_size = plaintext.len() as u64;
    let encrypted_size = aes_128_ecb_pkcs7_padded_size(plaintext.len()) as u64;
    let raw_md5 = format!("{:x}", md5::compute(&plaintext));
    let filekey = random_hex_16();
    let aes_key = random_bytes_16();
    let aeskey_hex = lower_hex(&aes_key);
    let is_image = is_wechat_image_path(path);
    let media_type = if is_image {
        UploadMediaType::Image
    } else {
        UploadMediaType::File
    };
    eprintln!(
        "[WeChat] uploading {} as {} to WeChat",
        path.display(),
        if is_image { "image" } else { "file" }
    );

    let upload_url_request = client.get_upload_url_request(
        &filekey,
        media_type,
        to_user_id,
        raw_size,
        &raw_md5,
        encrypted_size,
        &aeskey_hex,
    );
    let upload_url_response: GetUploadUrlResponse =
        parse_response(execute_request(upload_url_request).await?)?;
    if upload_url_response.ret.unwrap_or_default() != 0 {
        return Err(format!(
            "WeChat getuploadurl failed: ret={:?} errcode={:?} errmsg={:?}",
            upload_url_response.ret, upload_url_response.errcode, upload_url_response.errmsg
        ));
    }

    let encrypted = encrypt_aes_128_ecb_pkcs7(&plaintext, &aes_key)?;
    let upload_request = build_cdn_upload_request(
        upload_url_response.upload_param.as_deref(),
        upload_url_response.upload_full_url.as_deref(),
        &filekey,
        encrypted,
    )?;
    let encrypt_query_param = upload_media(upload_request).await?;
    let aes_key_base64 = BASE64_STANDARD.encode(aeskey_hex.as_bytes());
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "file".into());
    let caption = caption.map(filter_wechat_markdown);
    let caption = caption.as_deref();
    let send_request = if is_image {
        client.send_image_request(
            to_user_id,
            context_token,
            &encrypt_query_param,
            &aes_key_base64,
            encrypted_size,
            caption,
            &new_client_id(),
        )
    } else {
        client.send_file_request(
            to_user_id,
            context_token,
            &encrypt_query_param,
            &aes_key_base64,
            &file_name,
            raw_size,
            caption,
            &new_client_id(),
        )
    };

    execute_send_message_request(store, send_request, execute_request).await
}

fn wechat_send_manifest_paths(path: &Path) -> Result<Option<Vec<String>>, String> {
    let is_manifest = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value == WECHAT_SEND_MANIFEST_BASENAME)
        .unwrap_or(false);
    if !is_manifest {
        return Ok(None);
    }

    let content = std::fs::read_to_string(path).map_err(|err| {
        format!(
            "WeChat send manifest read failed for {}: {err}",
            path.display()
        )
    })?;
    let value: Value = serde_json::from_str(&content).map_err(|err| {
        format!(
            "WeChat send manifest must be JSON at {}: {err}",
            path.display()
        )
    })?;
    let paths_value = value
        .get("send_files")
        .or_else(|| value.get("paths"))
        .unwrap_or(&value);
    let Some(paths) = paths_value.as_array() else {
        return Err(format!(
            "WeChat send manifest must contain a send_files array: {}",
            path.display()
        ));
    };

    let paths: Vec<String> = paths
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(MAX_WECHAT_SEND_MANIFEST_FILES)
        .map(ToOwned::to_owned)
        .collect();
    if paths.is_empty() {
        return Err(format!(
            "WeChat send manifest has no file paths: {}",
            path.display()
        ));
    }

    Ok(Some(paths))
}

fn is_wechat_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "jpe" | "jfif" | "gif" | "webp" | "bmp" | "svg" | "ico"
            )
        })
        .unwrap_or(false)
}

fn random_bytes_16() -> [u8; 16] {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes
}

fn random_hex_16() -> String {
    lower_hex(&random_bytes_16())
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
