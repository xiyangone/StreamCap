//! Native in-process platform resolver. Python/Node fallback is deliberately absent.
use crate::platforms::{
    self,
    http::validate_url,
    kuaishou_login::{LoginEndpoints, LoginManager},
};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// 统一平台解析结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StreamInfo {
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub anchor_name: String,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub quality: String,
    #[serde(default)]
    pub m3u8_url: String,
    #[serde(default)]
    pub flv_url: String,
    #[serde(default)]
    pub record_url: String,
    #[serde(default)]
    pub new_cookies: String,
    #[serde(default)]
    pub new_token: String,
    #[serde(default)]
    pub error: Option<String>,
}

impl StreamInfo {
    /// 按质量档位与源偏好挑选最终录制地址。
    /// 顺序与 Python `_select_source_url` 的取向一致：录制地址 > m3u8 > flv。
    pub fn pick_record_url(&self, prefer_flv: bool) -> Option<String> {
        let candidates = if prefer_flv {
            [&self.flv_url, &self.record_url, &self.m3u8_url]
        } else {
            [&self.record_url, &self.m3u8_url, &self.flv_url]
        };
        candidates
            .into_iter()
            .find(|url| !url.trim().is_empty())
            .cloned()
    }
}

#[derive(Clone, Serialize)]
pub struct ResolveRequest {
    #[serde(skip_serializing)]
    pub account: Option<crate::config::PlatformAccount>,
    pub url: String,
    #[serde(default)]
    pub quality: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub cookie: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
}

#[derive(Clone)]
pub struct Resolver {
    stop: CancellationToken,
    slots: Arc<Semaphore>,
    login: LoginManager,
    kuaishou: platforms::kuaishou::KuaishouResolver,
}
impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}
impl Resolver {
    pub fn new() -> Self {
        Self::with_login_endpoints(LoginEndpoints::default())
    }
    pub fn with_login_endpoints(endpoints: LoginEndpoints) -> Self {
        let stop = CancellationToken::new();
        Self {
            login: LoginManager::with_endpoints(stop.clone(), endpoints),
            stop,
            slots: Arc::new(Semaphore::new(3)),
            kuaishou: platforms::kuaishou::KuaishouResolver::default(),
        }
    }
    pub fn cancellation(&self) -> CancellationToken {
        self.stop.clone()
    }
    pub fn begin_shutdown(&self) {
        self.stop.cancel();
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.login.shutdown().await;
    }
    pub async fn kuaishou_session_changed(&self) {
        self.kuaishou.reset_session().await;
    }
    pub async fn kuaishou_begin_page_check(
        &self,
        saved: Option<&str>,
        interval: Duration,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        self.kuaishou.begin_page_check(saved, interval).await
    }
    pub async fn kuaishou_page_problem(&self, error: &str) {
        self.kuaishou.page_problem(error).await;
    }
    pub async fn kuaishou_page_session(
        &self,
        saved: Option<&str>,
        page: &str,
    ) -> Result<(), String> {
        self.kuaishou.use_page_session(saved, page).await
    }
    pub async fn healthy(&self) -> bool {
        !self.stop.is_cancelled()
    }
    pub fn supported_platforms() -> &'static [&'static str] {
        static KEYS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
        KEYS.get_or_init(|| {
            platforms::catalog::PLATFORMS
                .iter()
                .map(|p| p.key)
                .chain(std::iter::once("custom"))
                .collect()
        })
        .as_slice()
    }
    pub async fn resolve(&self, request: ResolveRequest) -> Result<StreamInfo, String> {
        let operation = async {
            let _slot = self.slots.acquire().await.map_err(|_| "解析已停止")?;
            let url = validate_url(&request.url)?;
            let detected = platforms::catalog::detect(url.as_str());
            let platform = detected.map(|p| p.key).unwrap_or("custom");
            if let Some(expected) = request.platform.as_deref().filter(|s| !s.is_empty()) {
                let expected = platforms::catalog::canonical_key(expected);
                if !Self::supported_platforms().contains(&expected) {
                    return Err(format!("{expected} 平台不受支持"));
                }
                if expected != platform {
                    return Err("平台标识与直播间域名不匹配".into());
                }
            }
            platforms::quality_index(request.quality.as_deref())?;
            match platform {
                "douyin" => platforms::douyin::resolve(&request).await,
                "kuaishou" => self.kuaishou.resolve(&request).await,
                "custom" => platforms::custom::resolve(&request),
                _ => platforms::extended::resolve(&request, detected.ok_or("平台未识别")?).await,
            }
        };
        tokio::select! {biased;
            _=self.stop.cancelled()=>Err("应用正在退出，解析已取消".into()),
            outcome=tokio::time::timeout(Duration::from_secs(45),operation)=>outcome.map_err(|_|"平台解析超时".to_string())?,
        }
    }
    pub async fn qr_start(&self, proxy: Option<&str>) -> Result<QrSnapshot, String> {
        self.login.start(proxy).await
    }
    pub async fn qr_status(&self, id: &str) -> Result<QrSnapshot, String> {
        self.login.status(id).await
    }
    pub async fn qr_cancel(&self, id: &str) -> Result<(), String> {
        self.login.cancel(id).await
    }
}
/// 进程内扫码会话快照；完成登录不自动保存凭证。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QrSnapshot {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub image_base64: String,
    #[serde(default)]
    pub seconds_left: i64,
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

impl QrSnapshot {
    /// 终态判定：到达这些状态后前端应停止轮询。
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state.as_str(),
            "success" | "error" | "expired" | "cancelled"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_record_url_by_default() {
        let info = StreamInfo {
            record_url: "https://a/record.m3u8".into(),
            m3u8_url: "https://a/x.m3u8".into(),
            flv_url: "https://a/x.flv".into(),
            ..Default::default()
        };
        assert_eq!(
            info.pick_record_url(false).unwrap(),
            "https://a/record.m3u8"
        );
    }

    #[test]
    fn prefers_flv_when_direct_download_enabled() {
        let info = StreamInfo {
            record_url: "https://a/record.m3u8".into(),
            flv_url: "https://a/x.flv".into(),
            ..Default::default()
        };
        assert_eq!(info.pick_record_url(true).unwrap(), "https://a/x.flv");
    }

    #[test]
    fn falls_back_to_any_available_source() {
        let info = StreamInfo {
            m3u8_url: "https://a/only.m3u8".into(),
            ..Default::default()
        };
        assert_eq!(info.pick_record_url(false).unwrap(), "https://a/only.m3u8");
    }

    #[test]
    fn no_source_returns_none() {
        assert!(StreamInfo::default().pick_record_url(false).is_none());
    }

    #[test]
    fn deserializes_stream_payload() {
        let raw = r#"{
            "platform":"抖音","anchor_name":"主播A","is_live":true,"title":"标题",
            "quality":"OD","m3u8_url":"https://a/x.m3u8","flv_url":"","record_url":"",
            "new_cookies":"","new_token":"","error":null
        }"#;
        let info: StreamInfo = serde_json::from_str(raw).unwrap();
        assert!(info.is_live);
        assert_eq!(info.anchor_name, "主播A");
        assert_eq!(info.pick_record_url(false).unwrap(), "https://a/x.m3u8");
    }
}
