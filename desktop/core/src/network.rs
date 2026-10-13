//! Bounded reusable transports; credentials are scoped and never appear in cache diagnostics.
use reqwest::{cookie::Jar, Client};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const CAPACITY: usize = 32;
const TTL: Duration = Duration::from_secs(300);
#[derive(Clone, Copy)]
pub enum CookiePolicy {
    Send,
    IdentityOnly,
}
#[derive(Clone)]
pub struct SessionClient {
    pub client: Client,
    pub jar: Arc<Jar>,
}
struct Entry {
    identity: [u8; 32],
    used: Instant,
    transport: SessionClient,
}
fn pool() -> &'static Mutex<VecDeque<Entry>> {
    static POOL: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    POOL.get_or_init(Mutex::default)
}

/// Different identities, proxies, scopes and redirect policies never share a cookie jar.
pub fn platform_client(
    domain: &'static str,
    proxy: Option<&str>,
    cookie: Option<&str>,
    cookie_policy: CookiePolicy,
    account: Option<&crate::config::PlatformAccount>,
    timeout: Duration,
    redirects: bool,
) -> Result<SessionClient, String> {
    let cookie = cookie.unwrap_or("").trim();
    if !cookie.is_empty() && (cookie.contains(['\r', '\n']) || !cookie.contains('=')) {
        return Err("Cookie 格式无效，未发送匿名请求".into());
    }
    let proxy = proxy.unwrap_or("").trim();
    let mut hash = Sha256::new();
    for part in [
        domain.as_bytes(),
        proxy.as_bytes(),
        cookie.as_bytes(),
        &[cookie_policy as u8],
        &timeout.as_nanos().to_le_bytes(),
        &[u8::from(redirects)],
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.update([u8::from(account.is_some())]);
    if let Some(account) = account {
        for part in [
            &account.username,
            &account.password,
            &account.access_token,
            &account.account_type,
        ] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
    }
    let identity: [u8; 32] = hash.finalize().into();
    let mut cache = pool().lock().map_err(|_| "平台连接池不可用")?;
    cache.retain(|entry| entry.used.elapsed() < TTL);
    if let Some(index) = cache.iter().position(|entry| entry.identity == identity) {
        let mut entry = cache.remove(index).expect("cached client");
        entry.used = Instant::now();
        let transport = entry.transport.clone();
        cache.push_back(entry);
        return Ok(transport);
    }
    let jar = Arc::new(Jar::default());
    let origin = reqwest::Url::parse(&format!("https://{domain}/")).map_err(|_| "平台域名无效")?;
    if matches!(cookie_policy, CookiePolicy::Send) {
        for pair in cookie.split(';').map(str::trim).filter(|s| s.contains('=')) {
            jar.add_cookie_str(
                &format!("{pair}; Domain=.{domain}; Path=/; Secure"),
                &origin,
            );
        }
    }
    let policy = if redirects {
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("重定向次数超过限制")
            } else if crate::platforms::http::trusted_redirect(attempt.url(), domain, false) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    let mut builder = Client::builder()
        .no_proxy()
        .user_agent(crate::platforms::http::USER_AGENT)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(2)
        .cookie_provider(jar.clone())
        .redirect(policy);
    if !proxy.is_empty() {
        builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| "代理地址无效")?);
    }
    let transport = SessionClient {
        client: builder.build().map_err(|_| "无法初始化平台 HTTP 客户端")?,
        jar,
    };
    cache.push_back(Entry {
        identity,
        used: Instant::now(),
        transport: transport.clone(),
    });
    while cache.len() > CAPACITY {
        cache.pop_front();
    }
    Ok(transport)
}

/// FFmpeg must use only the explicit application proxy or the operating system route (including TUN).
pub fn isolate_media_proxy(command: &mut tokio::process::Command) {
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        command.env_remove(name);
    }
}
pub fn prune_idle_clients() {
    if let Ok(mut cache) = pool().lock() {
        cache.retain(|entry| entry.used.elapsed() < TTL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_reuses_identity_but_separates_credentials_and_policy() {
        use reqwest::cookie::CookieStore;
        let a = platform_client(
            "fixture.example",
            None,
            Some("sid=one"),
            CookiePolicy::Send,
            None,
            Duration::from_secs(9),
            false,
        )
        .unwrap();
        let b = platform_client(
            "fixture.example",
            None,
            Some("sid=one"),
            CookiePolicy::Send,
            None,
            Duration::from_secs(9),
            false,
        )
        .unwrap();
        assert!(Arc::ptr_eq(&a.jar, &b.jar));
        let withheld = platform_client(
            "fixture.example",
            None,
            Some("sid=one"),
            CookiePolicy::IdentityOnly,
            None,
            Duration::from_secs(9),
            false,
        )
        .unwrap();
        assert!(!Arc::ptr_eq(&a.jar, &withheld.jar));
        assert!(withheld
            .jar
            .cookies(&reqwest::Url::parse("https://fixture.example/").unwrap())
            .is_none());
        for (cookie, proxy, policy) in [
            ("sid=two", None, false),
            ("sid=one", Some("http://127.0.0.1:9"), false),
            ("sid=one", None, true),
        ] {
            let other = platform_client(
                "fixture.example",
                proxy,
                Some(cookie),
                CookiePolicy::Send,
                None,
                Duration::from_secs(9),
                policy,
            )
            .unwrap();
            assert!(!Arc::ptr_eq(&a.jar, &other.jar));
        }
        assert!(platform_client(
            "fixture.example",
            None,
            Some("sid=a\nInjected: true"),
            CookiePolicy::Send,
            None,
            Duration::from_secs(9),
            false
        )
        .is_err());
    }
    #[test]
    fn every_media_proxy_environment_key_is_removed() {
        let mut command = tokio::process::Command::new("fixture");
        isolate_media_proxy(&mut command);
        let removed: Vec<_> = command
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .collect();
        assert!(removed
            .iter()
            .any(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("http_proxy")));
        assert!(removed
            .iter()
            .any(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("all_proxy")));
    }
}
