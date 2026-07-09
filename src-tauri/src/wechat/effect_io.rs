//! `WechatEffectIo`: the three outbound IO seams — iLink HTTP request, CDN media
//! download, CDN media upload — behind one trait, replacing the per-call
//! `FnMut` closure ladder that executor's dispatch functions used to thread
//! through six generic parameters.
//!
//! `LiveIo` is the production implementation. `FakeIo` (test-only, added
//! alongside) records calls and returns injected responses. Methods return
//! `impl Future + Send` (RPITIT) rather than `async fn` in trait because the
//! polling loop dispatches effects on a spawned multi-thread tokio task, so the
//! futures must be `Send`.

use std::future::Future;

use serde_json::Value;

use super::api::{IlinkApiClient, IlinkHttpRequest};
use super::executor::{download_cdn_media_bytes, upload_cdn_media_bytes};
use super::media::{WechatCdnDownloadRequest, WechatCdnUploadRequest};

pub(crate) trait WechatEffectIo {
    fn execute_request(
        &mut self,
        request: IlinkHttpRequest,
    ) -> impl Future<Output = Result<Value, String>> + Send;

    fn download_media(
        &mut self,
        request: WechatCdnDownloadRequest,
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send;

    fn upload_media(
        &mut self,
        request: WechatCdnUploadRequest,
    ) -> impl Future<Output = Result<String, String>> + Send;
}

/// Production IO: real iLink HTTP client and real CDN transfers.
pub(crate) struct LiveIo;

impl WechatEffectIo for LiveIo {
    fn execute_request(
        &mut self,
        request: IlinkHttpRequest,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        async move {
            IlinkApiClient::new(None)
                .execute_json::<Value>(request)
                .await
        }
    }

    fn download_media(
        &mut self,
        request: WechatCdnDownloadRequest,
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send {
        download_cdn_media_bytes(request)
    }

    fn upload_media(
        &mut self,
        request: WechatCdnUploadRequest,
    ) -> impl Future<Output = Result<String, String>> + Send {
        upload_cdn_media_bytes(request)
    }
}

/// Test IO: each seam answers via an injected responder closure and records the
/// requests it received, so a test can assert on the exact request sequence
/// afterwards. Because dispatch borrows `&mut FakeIo`, the recordings need no
/// `Arc<Mutex<_>>` — the test reads `io.requests` directly once dispatch returns.
#[cfg(test)]
pub(crate) struct FakeIo {
    execute_responder: Box<dyn FnMut(&IlinkHttpRequest) -> Result<Value, String> + Send>,
    download_responder: Box<dyn FnMut(&WechatCdnDownloadRequest) -> Result<Vec<u8>, String> + Send>,
    upload_responder: Box<dyn FnMut(&WechatCdnUploadRequest) -> Result<String, String> + Send>,
    pub requests: Vec<IlinkHttpRequest>,
    pub downloads: Vec<WechatCdnDownloadRequest>,
    pub uploads: Vec<WechatCdnUploadRequest>,
}

#[cfg(test)]
impl FakeIo {
    /// Default: every iLink request gets `{ "ret": 0 }`; media transfers panic
    /// unless a test opts into them via `on_download` / `on_upload`.
    pub fn new() -> Self {
        Self {
            execute_responder: Box::new(|_| Ok(serde_json::json!({ "ret": 0 }))),
            download_responder: Box::new(|_| panic!("FakeIo: unexpected download_media call")),
            upload_responder: Box::new(|_| panic!("FakeIo: unexpected upload_media call")),
            requests: Vec::new(),
            downloads: Vec::new(),
            uploads: Vec::new(),
        }
    }

    pub fn on_request(
        mut self,
        responder: impl FnMut(&IlinkHttpRequest) -> Result<Value, String> + Send + 'static,
    ) -> Self {
        self.execute_responder = Box::new(responder);
        self
    }

    pub fn on_download(
        mut self,
        responder: impl FnMut(&WechatCdnDownloadRequest) -> Result<Vec<u8>, String> + Send + 'static,
    ) -> Self {
        self.download_responder = Box::new(responder);
        self
    }

    pub fn on_upload(
        mut self,
        responder: impl FnMut(&WechatCdnUploadRequest) -> Result<String, String> + Send + 'static,
    ) -> Self {
        self.upload_responder = Box::new(responder);
        self
    }
}

#[cfg(test)]
impl WechatEffectIo for FakeIo {
    fn execute_request(
        &mut self,
        request: IlinkHttpRequest,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        let result = (self.execute_responder)(&request);
        self.requests.push(request);
        async move { result }
    }

    fn download_media(
        &mut self,
        request: WechatCdnDownloadRequest,
    ) -> impl Future<Output = Result<Vec<u8>, String>> + Send {
        let result = (self.download_responder)(&request);
        self.downloads.push(request);
        async move { result }
    }

    fn upload_media(
        &mut self,
        request: WechatCdnUploadRequest,
    ) -> impl Future<Output = Result<String, String>> + Send {
        let result = (self.upload_responder)(&request);
        self.uploads.push(request);
        async move { result }
    }
}
