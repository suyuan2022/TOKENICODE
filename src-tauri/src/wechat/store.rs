use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

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
}
