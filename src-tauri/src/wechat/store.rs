use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::wechat::inbound::{InboundWechatMedia, InboundWechatMediaKind};

type StoreResult<T> = Result<T, String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WechatAccount {
    pub bot_token: String,
    pub account_id: String,
    pub base_url: String,
    pub user_id: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct WechatStateStore {
    dir: PathBuf,
}

impl WechatStateStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn save_account(&self, account: &WechatAccount) -> StoreResult<()> {
        write_json(&self.account_path(), account)
    }

    pub fn load_account(&self) -> StoreResult<Option<WechatAccount>> {
        read_json(&self.account_path())
    }

    pub fn clear_account(&self) -> StoreResult<()> {
        remove_if_exists(&self.account_path())
    }

    pub fn save_sync_buf(&self, sync_buf: &str) -> StoreResult<()> {
        write_text(&self.sync_buf_path(), sync_buf)
    }

    pub fn load_sync_buf(&self) -> StoreResult<Option<String>> {
        match fs::read_to_string(self.sync_buf_path()) {
            Ok(value) => Ok(Some(value)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("read sync buf: {err}")),
        }
    }

    pub fn save_context_token(&self, user_id: &str, context_token: &str) -> StoreResult<()> {
        let mut tokens = self.load_context_tokens()?;
        tokens.insert(user_id.into(), context_token.into());
        write_json(&self.context_tokens_path(), &tokens)
    }

    pub fn load_context_token(&self, user_id: &str) -> StoreResult<Option<String>> {
        Ok(self.load_context_tokens()?.get(user_id).cloned())
    }

    pub fn save_inbound_media(
        &self,
        media: &InboundWechatMedia,
        bytes: &[u8],
    ) -> StoreResult<PathBuf> {
        let dir = self.dir.join("inbound-media");
        fs::create_dir_all(&dir).map_err(|err| format!("create {}: {err}", dir.display()))?;
        let file_name = inbound_media_file_name(media);
        let path = dir.join(file_name);
        fs::write(&path, bytes).map_err(|err| format!("write {}: {err}", path.display()))?;
        Ok(path)
    }

    fn load_context_tokens(&self) -> StoreResult<BTreeMap<String, String>> {
        Ok(read_json(&self.context_tokens_path())?.unwrap_or_default())
    }

    fn account_path(&self) -> PathBuf {
        self.dir.join("account.json")
    }

    fn sync_buf_path(&self) -> PathBuf {
        self.dir.join("get_updates.buf")
    }

    fn context_tokens_path(&self) -> PathBuf {
        self.dir.join("context_tokens.json")
    }
}

fn inbound_media_file_name(media: &InboundWechatMedia) -> String {
    let default_name = match media.kind {
        InboundWechatMediaKind::Image => format!("wechat-image-{}.jpg", media.message_id),
        InboundWechatMediaKind::File => format!("wechat-file-{}.bin", media.message_id),
    };
    let raw_name = media
        .file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&default_name);
    let sanitized_name = sanitize_file_name(raw_name);
    format!(
        "{}-{}",
        sanitize_file_name(&media.message_id),
        sanitized_name
    )
}

fn sanitize_file_name(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '_' => ch,
            _ => '_',
        })
        .collect();
    let sanitized = sanitized.trim_matches(|ch| ch == '.' || ch == '_');
    if sanitized.is_empty() {
        "media".into()
    } else {
        sanitized.chars().take(120).collect()
    }
}

pub fn default_wechat_state_store() -> WechatStateStore {
    WechatStateStore::new(default_wechat_state_dir())
}

pub fn default_wechat_state_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".tokenicode")
        .join("wechat")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> StoreResult<()> {
    let data = serde_json::to_vec_pretty(value).map_err(|err| format!("serialize json: {err}"))?;
    write_bytes(path, &data)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> StoreResult<Option<T>> {
    match fs::read(path) {
        Ok(data) => serde_json::from_slice(&data)
            .map(Some)
            .map_err(|err| format!("parse {}: {err}", path.display())),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("read {}: {err}", path.display())),
    }
}

fn write_text(path: &Path, value: &str) -> StoreResult<()> {
    write_bytes(path, value.as_bytes())
}

fn write_bytes(path: &Path, value: &[u8]) -> StoreResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, value).map_err(|err| format!("write {}: {err}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .map_err(|err| format!("rename {} to {}: {err}", tmp_path.display(), path.display()))
}

fn remove_if_exists(path: &Path) -> StoreResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("remove {}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wechat::inbound::{InboundWechatCdnMedia, InboundWechatMediaKind};

    #[test]
    fn persists_account_sync_buf_and_context_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        let account = WechatAccount {
            bot_token: "bot-token".into(),
            account_id: "bot-id".into(),
            base_url: "https://ilinkai.weixin.qq.com".into(),
            user_id: "user-1".into(),
            created_at_ms: 123,
        };

        store.save_account(&account).unwrap();
        store.save_sync_buf("cursor-1").unwrap();
        store.save_context_token("user-1", "ctx-1").unwrap();

        let reloaded = WechatStateStore::new(dir.path().to_path_buf());
        assert_eq!(reloaded.load_account().unwrap(), Some(account));
        assert_eq!(reloaded.load_sync_buf().unwrap(), Some("cursor-1".into()));
        assert_eq!(
            reloaded.load_context_token("user-1").unwrap(),
            Some("ctx-1".into())
        );
    }

    #[test]
    fn clear_account_leaves_runtime_cursors_available() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());
        store
            .save_account(&WechatAccount {
                bot_token: "bot-token".into(),
                account_id: "bot-id".into(),
                base_url: "https://ilinkai.weixin.qq.com".into(),
                user_id: "user-1".into(),
                created_at_ms: 123,
            })
            .unwrap();
        store.save_sync_buf("cursor-1").unwrap();
        store.save_context_token("user-1", "ctx-1").unwrap();

        store.clear_account().unwrap();

        assert_eq!(store.load_account().unwrap(), None);
        assert_eq!(store.load_sync_buf().unwrap(), Some("cursor-1".into()));
        assert_eq!(
            store.load_context_token("user-1").unwrap(),
            Some("ctx-1".into())
        );
    }

    #[test]
    fn save_inbound_media_sanitizes_untrusted_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = WechatStateStore::new(dir.path().to_path_buf());

        let path = store
            .save_inbound_media(
                &InboundWechatMedia {
                    message_id: "../msg-1".into(),
                    from_user_id: "user-1".into(),
                    context_token: "ctx-1".into(),
                    received_at_ms: 1_000,
                    kind: InboundWechatMediaKind::File,
                    file_name: Some("../../secret.pdf".into()),
                    size_hint: None,
                    cdn: InboundWechatCdnMedia {
                        encrypt_query_param: Some("query".into()),
                        aes_key: "key".into(),
                        encrypt_type: Some(1),
                        full_url: None,
                    },
                },
                b"file bytes",
            )
            .unwrap();

        assert!(path.starts_with(dir.path().join("inbound-media")));
        assert_eq!(path.file_name().unwrap(), "msg-1-secret.pdf");
        assert_eq!(std::fs::read(path).unwrap(), b"file bytes");
    }
}
