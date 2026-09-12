//! Bounded, TLS-verified platform transport. Errors never include credential-bearing URLs.
use reqwest::{cookie::Jar, Client, Response, Url};
use std::{sync::Arc, time::Duration};

pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";
pub const MAX_RESPONSE: usize = 4 * 1024 * 1024;

pub struct PlatformHttp {
    pub client: Client,
    pub jar: Arc<Jar>,
}
impl PlatformHttp {
    pub fn new(
        domain: &'static str,
        proxy: Option<&str>,
        cookie: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, String> {
        Self::build(domain, proxy, cookie, timeout, false)
    }
    pub fn for_loopback() -> Result<Self, String> {
        Self::build("localhost", None, None, Duration::from_secs(3), true)
    }
    fn build(
        domain: &'static str,
        proxy: Option<&str>,
        cookie: Option<&str>,
        timeout: Duration,
        loopback: bool,
    ) -> Result<Self, String> {
        let jar = Arc::new(Jar::default());
        let mut builder = Client::builder()
            .no_proxy()
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            .cookie_provider(jar.clone())
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if !trusted_redirect(attempt.url(), domain, loopback) {
                    attempt.stop()
                } else if attempt.previous().len() >= 5 {
                    attempt.error("重定向次数超过限制")
                } else {
                    attempt.follow()
                }
            }));
        if let Some(proxy) = proxy.filter(|s| !s.trim().is_empty()) {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| "代理地址无效")?);
        }
        if let Some(cookie) = cookie.filter(|s| !s.trim().is_empty()) {
            if cookie.contains(['\r', '\n']) {
                return Err("Cookie 包含非法换行".into());
            }
            let origin = Url::parse(&format!("https://{domain}/")).map_err(|_| "平台域名无效")?;
            for pair in cookie.split(';').map(str::trim).filter(|s| s.contains('=')) {
                jar.add_cookie_str(
                    &format!("{pair}; Domain=.{domain}; Path=/; Secure"),
                    &origin,
                );
            }
        }
        let client = builder.build().map_err(|_| "无法初始化平台 HTTP 客户端")?;
        Ok(Self { client, jar })
    }
    pub async fn get(&self, url: Url, referer: &str) -> Result<String, String> {
        let response = self
            .client
            .get(url)
            .header("Referer", referer)
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.6")
            .send()
            .await
            .map_err(transport_error)?;
        body(response).await
    }
}
pub fn transport_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "平台请求超时".into()
    } else if error.is_connect() {
        "无法连接平台（请检查网络或代理）".into()
    } else {
        "平台请求失败（传输或 TLS 错误）".into()
    }
}
pub async fn body(mut response: Response) -> Result<String, String> {
    if !response.status().is_success() {
        return Err(format!("平台返回 HTTP {}", response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE as u64)
    {
        return Err("平台响应超过大小限制".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err("平台响应超过大小限制".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Err("平台返回空响应（未验证直播状态）".into());
    }
    String::from_utf8(bytes).map_err(|_| "平台响应不是有效的 UTF-8".into())
}
pub fn validate_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "直播间地址无效")?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("仅支持不含用户凭证的 HTTP/HTTPS 地址".into());
    }
    Ok(url)
}

fn trusted_redirect(url: &Url, domain: &str, loopback: bool) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let host = url.host_str().unwrap_or_default();
    if loopback {
        matches!(url.scheme(), "http" | "https")
            && matches!(host, "127.0.0.1" | "localhost" | "[::1]")
    } else {
        url.scheme() == "https" && (host == domain || host.ends_with(&format!(".{domain}")))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redirects_never_leak_cookies_or_downgrade_tls() {
        for value in [
            "https://evil.invalid/",
            "https://kuaishou.com.evil.invalid/",
            "http://live.kuaishou.com/",
            "https://user:pass@live.kuaishou.com/",
        ] {
            assert!(!trusted_redirect(
                &Url::parse(value).unwrap(),
                "kuaishou.com",
                false
            ));
        }
        assert!(trusted_redirect(
            &Url::parse("https://id.kuaishou.com/").unwrap(),
            "kuaishou.com",
            false
        ));
        assert!(trusted_redirect(
            &Url::parse("http://[::1]:1234/").unwrap(),
            "localhost",
            true
        ));
    }
}
