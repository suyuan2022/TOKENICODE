pub mod api;
pub mod commands;
pub mod executor;
pub mod file_send;
pub mod inbound;
pub mod login;
pub mod media;
pub mod monitor;
pub mod poller;
pub mod runtime;
pub mod store;
pub mod text;
pub mod typing;
pub mod turn;

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
