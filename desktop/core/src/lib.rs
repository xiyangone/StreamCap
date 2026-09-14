//! StreamCap Rust 后端核心。
//!
//! 阶段一范围：配置存储、任务模型与持久化、监控调度、ffmpeg 录制引擎、HTTP/SSE API。
//! 平台解析在进程内执行；当前支持抖音、快手和媒体直链。

pub mod api;
pub mod automation;
pub mod config;
pub mod direct;
pub mod engine;
pub mod media_safety;
pub mod model;
pub mod notifications;
pub(crate) mod owned_process;
pub mod pacing;
pub mod paths;
pub mod platforms;
pub mod postprocess;
pub mod preview;
pub mod resolver;
pub mod schedule;
pub mod scheduler;
pub mod security;
pub mod service;
pub mod storage;
pub mod store;
pub mod subtitles;
pub mod tools;

pub use api::ApiState;
pub use config::ConfigStore;
pub use engine::Engine;
pub use model::Recording;
pub use paths::Workspace;
pub use resolver::Resolver;
pub use scheduler::Scheduler;
pub use store::Store;

pub const GATEWAY_PORT: u16 = 6059;
