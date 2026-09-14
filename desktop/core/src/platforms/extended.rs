//! Native ports of explicit streamget platform protocols. Missing business fields are errors, not "offline".
use super::{catalog, http, quality_index, quality_name};
use crate::resolver::{ResolveRequest, StreamInfo};
use reqwest::Url;
use serde_json::{json, Value};
use std::time::Duration;
struct Context<'a> {
    request: &'a ResolveRequest,
    platform: &'static catalog::Platform,
    client: reqwest::Client,
    jar: std::sync::Arc<reqwest::cookie::Jar>,
    url: Url,
    initial_page: std::sync::Mutex<Option<(Url, String)>>,
    #[cfg(test)]
    test_responses: Option<std::sync::Mutex<std::collections::VecDeque<fixture_tests::Reply>>>,
}
impl<'a> Context<'a> {
    fn new(
        request: &'a ResolveRequest,
        platform: &'static catalog::Platform,
    ) -> Result<Self, String> {
        let jar = std::sync::Arc::new(reqwest::cookie::Jar::default());
        if let Some(cookie) = request
            .cookie
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            if cookie.contains(['\r', '\n']) || !cookie.contains('=') {
                return Err("Cookie 格式无效，未发送匿名请求".into());
            }
            let domain = if platform.key == "flextv" {
                "ttinglive.com"
            } else {
                platform.domains[0]
            };
            let origin = Url::parse(&format!("https://{domain}/")).map_err(|_| "平台域名无效")?;
            for pair in cookie.split(';').map(str::trim).filter(|s| s.contains('=')) {
                jar.add_cookie_str(
                    &format!("{pair}; Domain=.{domain}; Path=/; Secure"),
                    &origin,
                );
            }
        }
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .cookie_provider(jar.clone())
            .user_agent(http::USER_AGENT)
            .timeout(Duration::from_secs(18))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(proxy) = request.proxy.as_deref().filter(|p| !p.is_empty()) {
            client = client.proxy(reqwest::Proxy::all(proxy).map_err(|_| "代理地址无效")?);
        }
        Ok(Self {
            request,
            #[cfg(test)]
            test_responses: None,
            jar,
            platform,
            client: client.build().map_err(|_| "平台客户端初始化失败")?,
            url: http::validate_url(&request.url)?,
            initial_page: std::sync::Mutex::new(None),
        })
    }
    fn id(&self) -> Result<String, String> {
        self.url
            .path_segments()
            .and_then(|mut p| p.rfind(|s| !s.is_empty()))
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
            .ok_or("直播间地址缺少标识".into())
    }
    fn query(&self, key: &str) -> Result<String, String> {
        self.url
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .ok_or_else(|| format!("直播间地址缺少 {key}"))
    }
    fn request(
        &self,
        method: reqwest::Method,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, String> {
        let parsed = http::validate_url(url)?;
        let host = parsed.host_str().ok_or("平台地址无效")?;
        if !allowed_host(self.platform, host) {
            return Err("平台请求指向未授权域名".into());
        }
        let builder = self
            .client
            .request(method, parsed.clone())
            .header("Referer", self.url.as_str());

        Ok(builder)
    }
    async fn send_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, String> {
        #[cfg(test)]
        if let Some(replies) = &self.test_responses {
            return fixture_tests::respond(replies, &self.jar, request);
        }
        request.send().await.map_err(http::transport_error)
    }
    async fn fetch_page(&self, url: &str) -> Result<(Url, String), String> {
        let mut url = http::validate_url(url)?;
        {
            let mut cached = self.initial_page.lock().expect("initial page");
            if cached.as_ref().is_some_and(|(key, _)| key == &url) {
                return Ok(cached.take().expect("cached page"));
            }
        }
        for _ in 0..5 {
            let response = self
                .send_request(self.request(reqwest::Method::GET, url.as_str())?)
                .await?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("平台跳转缺少地址")?;
                let target = url.join(location).map_err(|_| "平台跳转地址无效")?;
                if url.scheme() == "https" && target.scheme() != "https" {
                    return Err("平台跳转降级被拒绝".into());
                }
                url = target;
                continue;
            }
            return Ok((url, http::body(response).await?));
        }
        Err("平台跳转次数超出限制".into())
    }
    async fn text(&self, url: &str) -> Result<String, String> {
        Ok(self.fetch_page(url).await?.1)
    }
    fn cookie_value(&self, url: &str, name: &str) -> Option<String> {
        use reqwest::cookie::CookieStore;
        self.jar
            .cookies(&Url::parse(url).ok()?)?
            .to_str()
            .ok()?
            .split(';')
            .filter_map(|v| v.trim().split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
    }
    async fn get(&self, url: &str) -> Result<Value, String> {
        json_body(&self.text(url).await?)
    }
    async fn form(&self, url: &str, fields: &[(&str, String)]) -> Result<Value, String> {
        let response = self
            .send_request(self.request(reqwest::Method::POST, url)?.form(fields))
            .await?;
        json_body(&http::body(response).await?)
    }
    async fn post(&self, url: &str, body: Value) -> Result<Value, String> {
        let response = self
            .send_request(self.request(reqwest::Method::POST, url)?.json(&body))
            .await?;
        json_body(&http::body(response).await?)
    }
    fn result(
        &self,
        name: String,
        live: bool,
        title: String,
        hls: String,
        flv: String,
    ) -> Result<StreamInfo, String> {
        if name.trim().is_empty() {
            return Err("平台响应未包含当前主播身份，未判定直播状态".into());
        }
        for value in [&hls, &flv].into_iter().filter(|v| !v.is_empty()) {
            let url = Url::parse(value).map_err(|_| "播放地址无效")?;
            if !matches!(url.scheme(), "http" | "https" | "rtmp" | "rtmps")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("播放地址协议或凭证无效".into());
            }
        }
        if live && hls.is_empty() && flv.is_empty() {
            return Err("平台确认开播但未返回可用播放地址，请检查登录或地区限制".into());
        }
        Ok(StreamInfo {
            platform: self.platform.name.into(),
            anchor_name: name,
            is_live: live,
            title,
            quality: quality_name(quality_index(self.request.quality.as_deref())?).into(),
            record_url: if flv.is_empty() {
                hls.clone()
            } else {
                flv.clone()
            },
            m3u8_url: hls,
            flv_url: flv,
            ..Default::default()
        })
    }
}
fn allowed_host(platform: &catalog::Platform, host: &str) -> bool {
    platform
        .domains
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        || extra_domains(platform.key)
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}
fn extra_domains(key: &str) -> &'static [&'static str] {
    match key {
        "douyu" => &["douyucdn.cn"],
        "baidu" => &["baidu.com"],
        "17live" => &["17app.co"],
        "chzzk" => &["api.chzzk.naver.com"],
        "huajiao" => &["huajiao.com"],
        "look" => &["music.163.com"],
        "netease" => &["cc.163.com"],
        "acfun" => &["acfun.com", "kuaishouzt.com"],
        "liveme" => &["ksmobile.net", "liveme.com"],
        "migu" => &["miguvideo.com"],
        "jd" => &["jd.com"],
        "taobao" => &["taobao.com", "taobao.org"],
        "twitch" | "faceit" => &["twitch.tv", "static.twitchcdn.net", "ttvnw.net"],
        "shopee" => &[
            "shopee.tw",
            "shopee.co.th",
            "shopee.com.my",
            "shopee.sg",
            "shopee.ph",
            "shopee.co.id",
            "shopee.vn",
            "shopee.com.br",
        ],
        _ => &[],
    }
}
fn json_body(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|_| "平台未返回有效 JSON（可能需要登录或验证）".into())
}
fn s(value: &Value, pointer: &str) -> String {
    value.pointer(pointer).map(super::text).unwrap_or_default()
}
fn field<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value, String> {
    value
        .pointer(pointer)
        .filter(|v| !v.is_null())
        .ok_or_else(|| format!("平台响应缺少字段 {pointer}，未判定为下播"))
}
fn live(value: &Value, pointer: &str, yes: &[&str], no: &[&str]) -> Result<bool, String> {
    let v = field(value, pointer)?;
    let text = if v == &Value::Bool(true) {
        "true".into()
    } else if v == &Value::Bool(false) {
        "false".into()
    } else {
        super::text(v)
    };
    if yes.contains(&text.as_str()) {
        Ok(true)
    } else if no.contains(&text.as_str()) {
        Ok(false)
    } else {
        Err(format!("平台直播状态字段 {pointer} 无法识别"))
    }
}
fn capture(pattern: &str, text: &str) -> Result<String, String> {
    regex::Regex::new(pattern)
        .map_err(|_| "解析模式无效")?
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
        .ok_or("页面未包含当前直播间数据".into())
}
fn escaped(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
fn query_url(base: &str, params: &[(&str, String)]) -> Result<String, String> {
    let mut url = Url::parse(base).map_err(|_| "请求地址无效")?;
    url.query_pairs_mut()
        .extend_pairs(params.iter().map(|(k, v)| (*k, v)));
    Ok(url.to_string())
}
pub async fn resolve(
    request: &ResolveRequest,
    platform: &'static catalog::Platform,
) -> Result<StreamInfo, String> {
    let mut c = Context::new(request, platform)?;
    prepare_short_link(&mut c).await?;
    resolve_context(&c).await
}
async fn prepare_short_link(c: &mut Context<'_>) -> Result<(), String> {
    if c.url.host_str().is_some_and(|h| {
        ["xhslink.com", "shp.ee", "tb.cn", "3.cn", "youtu.be"]
            .iter()
            .any(|d| h == *d || h.ends_with(&format!(".{d}")))
    }) {
        let (url, page) = c.fetch_page(c.url.as_str()).await?;
        c.url = url.clone();
        *c.initial_page.lock().expect("initial page") = Some((url, page));
    }
    Ok(())
}
async fn resolve_context(c: &Context<'_>) -> Result<StreamInfo, String> {
    let id = c.id()?;
    match c.platform.key {
        "bigo" => {
            let v = c
                .form(
                    "https://ta.bigo.tv/official_website/studio/getInternalStudioInfo",
                    &[("siteId", c.query("h").unwrap_or(id))],
                )
                .await?;
            let on = live(&v, "/data/alive", &["1"], &["0"])?;
            c.result(
                s(&v, "/data/nick_name"),
                on,
                s(&v, "/data/roomTopic"),
                if on {
                    s(&v, "/data/hls_src")
                } else {
                    String::new()
                },
                String::new(),
            )
        }
        "inke" => {
            let url = query_url(
                "https://webapi.busi.inke.cn/web/live_share_pc",
                &[("uid", c.query("uid")?), ("id", c.query("id")?)],
            )?;
            let v = c.get(&url).await?;
            let on = live(&v, "/data/status", &["1"], &["0", "2"])?;
            c.result(
                s(&v, "/data/media_info/nick"),
                on,
                String::new(),
                if on {
                    s(&v, "/data/live_addr/0/hls_stream_addr")
                } else {
                    String::new()
                },
                if on {
                    s(&v, "/data/live_addr/0/stream_addr")
                } else {
                    String::new()
                },
            )
        }
        "langlive" => {
            let v = c
                .get(&query_url(
                    "https://api.lang.live/langweb/v1/room/liveinfo",
                    &[("room_id", id)],
                )?)
                .await?;
            let on = live(&v, "/data/live_info/live_status", &["1"], &["0"])?;
            c.result(
                s(&v, "/data/live_info/nickname"),
                on,
                String::new(),
                if on {
                    s(&v, "/data/live_info/liveurl_hls")
                } else {
                    String::new()
                },
                if on {
                    s(&v, "/data/live_info/liveurl")
                } else {
                    String::new()
                },
            )
        }
        "lianjie" => {
            let v = c
                .get(&query_url(
                    "https://api.lailianjie.com/ApiServices/service/live/getRoomInfo",
                    &[
                        ("_$t", String::new()),
                        ("_sign", String::new()),
                        ("roomNumber", id),
                    ],
                )?)
                .await?;
            let on = live(&v, "/data/isonline", &["1"], &["0"])?;
            c.result(
                s(&v, "/data/nickname"),
                on,
                s(&v, "/data/defaultRoomTitle"),
                String::new(),
                if on {
                    s(&v, "/data/videoUrl")
                } else {
                    String::new()
                },
            )
        }
        "maoerfm" => {
            let v = c
                .get(&format!(
                    "https://fm.missevan.com/api/v2/live/{}",
                    escaped(&id)
                ))
                .await?;
            let on = live(
                &v,
                "/info/room/status/broadcasting",
                &["true", "1"],
                &["false", "0"],
            )?;
            c.result(
                s(&v, "/info/creator/username"),
                on,
                s(&v, "/info/room/name"),
                if on {
                    s(&v, "/info/room/channel/hls_pull_url")
                } else {
                    String::new()
                },
                if on {
                    s(&v, "/info/room/channel/flv_pull_url")
                } else {
                    String::new()
                },
            )
        }
        "picarto" => {
            let v = c
                .get(&format!(
                    "https://ptvintern.picarto.tv/api/channel/detail/{}",
                    escaped(&id)
                ))
                .await?;
            let on = live(&v, "/channel/online", &["true", "1"], &["false", "0"])?;
            let name = s(&v, "/channel/name");
            let hls = if on {
                format!(
                    "https://1-edge1-us-newyork.picarto.tv/stream/hls/golive+{}/index.m3u8",
                    escaped(&name)
                )
            } else {
                String::new()
            };
            c.result(name, on, s(&v, "/channel/title"), hls, String::new())
        }
        "chzzk" => {
            let v = c
                .get(&format!(
                    "https://api.chzzk.naver.com/service/v3/channels/{}/live-detail",
                    escaped(&id)
                ))
                .await?;
            let on = live(&v, "/content/status", &["OPEN"], &["CLOSE", "CLOSED"])?;
            let hls = if on {
                s(
                    &json_body(&s(&v, "/content/livePlaybackJson"))?,
                    "/media/0/path",
                )
            } else {
                String::new()
            };
            c.result(
                s(&v, "/content/channel/channelName"),
                on,
                s(&v, "/content/liveTitle"),
                hls,
                String::new(),
            )
        }
        "blued" => {
            let text = c.text(c.url.as_str()).await?;
            let encoded = capture(r#"decodeURIComponent\("(.*?)"\)\),window\.Promise"#, &text)?;
            let decoded = percent_encoding::percent_decode_str(&encoded)
                .decode_utf8()
                .map_err(|_| "页面编码无效")?;
            let v = json_body(&decoded)?;
            let on = live(&v, "/userInfo/onLive", &["true", "1"], &["false", "0"])?;
            c.result(
                s(&v, "/userInfo/name"),
                on,
                String::new(),
                if on {
                    s(&v, "/liveInfo/liveUrl")
                } else {
                    String::new()
                },
                String::new(),
            )
        }
        "17live" => {
            let identity = c
                .get(&format!(
                    "https://wap-api.17app.co/api/v1/user/room/{}",
                    escaped(&id)
                ))
                .await?;
            let v = c
                .post(
                    &format!(
                        "https://wap-api.17app.co/api/v1/lives/{}/viewers/alive",
                        escaped(&id)
                    ),
                    json!({"liveStreamID":id}),
                )
                .await?;
            let on = live(&v, "/status", &["2"], &["0", "1", "3"])?;
            c.result(
                s(&identity, "/displayName"),
                on,
                String::new(),
                String::new(),
                if on {
                    s(&v, "/pullURLsInfo/rtmpURLs/0/urlHighQuality")
                } else {
                    String::new()
                },
            )
        }
        "qiandurebo" | "xindongrebo" => {
            let page = c.text(c.url.as_str()).await?;
            let v = super::json_after(&page, "var user = ").ok_or("页面未包含直播间对象")?;
            let offline = page.contains("common-text-center\" style=\"display:block");
            let stream = s(&v, "/play_url");
            if stream.is_empty() && !offline {
                return Err("页面未提供可验证的直播状态".into());
            }
            c.result(
                s(&v, "/zb_nickname"),
                !offline,
                String::new(),
                String::new(),
                if offline { String::new() } else { stream },
            )
        }
        _ => resolve_other(c).await,
    }
}
async fn resolve_other(c: &Context<'_>) -> Result<StreamInfo, String> {
    let id = c.id()?;
    let q = quality_index(c.request.quality.as_deref())?;
    match c.platform.key {
        "bilibili" => {
            let init = c
                .get(&query_url(
                    "https://api.live.bilibili.com/room/v1/Room/room_init",
                    &[("id", id)],
                )?)
                .await?;
            let on = live(&init, "/data/live_status", &["1"], &["0", "2"])?;
            let rid = s(&init, "/data/room_id");
            let profile = c
                .get(&query_url(
                    "https://api.live.bilibili.com/live_user/v1/Master/info",
                    &[("uid", s(&init, "/data/uid"))],
                )?)
                .await?;
            let name = s(&profile, "/data/info/uname");
            if !on {
                return c.result(name, false, String::new(), String::new(), String::new());
            }
            let details = c
                .get(&query_url(
                    "https://api.live.bilibili.com/xlive/web-room/v1/index/getH5InfoByRoom",
                    &[("room_id", rid.clone())],
                )?)
                .await?;
            let play = c
                .get(&query_url(
                    "https://api.live.bilibili.com/room/v1/Room/playUrl",
                    &[
                        ("cid", rid),
                        ("qn", ["10000", "400", "250", "150", "80"][q].into()),
                        ("platform", "web".into()),
                    ],
                )?)
                .await?;
            let streams = field(&play, "/data/durl")?
                .as_array()
                .ok_or("播放地址列表无效")?;
            let stream = streams.last().map(|v| s(v, "/url")).unwrap_or_default();
            let hls = if stream.contains(".m3u8") {
                stream.clone()
            } else {
                String::new()
            };
            c.result(
                name,
                true,
                s(&details, "/data/room_info/title"),
                hls,
                if stream.contains(".m3u8") {
                    String::new()
                } else {
                    stream
                },
            )
        }
        "netease" => {
            let page = c.text(c.url.as_str()).await?;
            let v = json_body(&capture(
                r#"(?s)<script[^>]*id="__NEXT_DATA__"[^>]*>(.*?)</script>"#,
                &page,
            )?)?;
            let room = field(&v, "/props/pageProps/roomInfoInitData")?;
            let on = live(room, "/live/status", &["1"], &["0", "2"])?;
            let name = if s(room, "/live/nickname").is_empty() {
                s(room, "/nickname")
            } else {
                s(room, "/live/nickname")
            };
            let mut urls = Vec::new();
            for quality in ["blueray", "ultra", "high", "standard"] {
                if let Some(cdns) = room
                    .pointer(&format!("/live/quickplay/resolution/{quality}/cdn"))
                    .and_then(Value::as_object)
                {
                    if let Some(url) = cdns.values().find_map(Value::as_str) {
                        urls.push(url.to_string());
                    }
                }
            }
            c.result(
                name,
                on,
                s(room, "/live/title"),
                if on {
                    s(room, "/live/sharefile")
                } else {
                    String::new()
                },
                if on { choose(&urls, q) } else { String::new() },
            )
        }
        "showroom" => {
            let rid = match c.query("room_id") {
                Ok(id) => id,
                Err(_) => capture(
                    r#"href="/room/profile\?room_id=(\d+)"#,
                    &c.text(c.url.as_str()).await?,
                )?,
            };
            let v = c
                .get(&query_url(
                    "https://www.showroom-live.com/api/live/live_info",
                    &[("room_id", rid.clone())],
                )?)
                .await?;
            let on = live(&v, "/live_status", &["2"], &["0", "1", "3"])?;
            let mut hls = String::new();
            if on {
                let streams = c
                    .get(&query_url(
                        "https://www.showroom-live.com/api/live/streaming_url",
                        &[("room_id", rid), ("abr_available", "1".into())],
                    )?)
                    .await?;
                hls = field(&streams, "/streaming_url_list")?
                    .as_array()
                    .and_then(|a| {
                        a.iter()
                            .find(|v| v["type"] == "hls_all" || v["type"] == "hls")
                    })
                    .map(|v| s(v, "/url"))
                    .unwrap_or_default();
            }
            c.result(s(&v, "/room_name"), on, String::new(), hls, String::new())
        }
        "sixroom" => {
            let page = c.text(&format!("https://v.6.cn/{}", escaped(&id))).await?;
            let rid = capture(r#"rid:\s*'(\d+)'"#, &page)?;
            let v = c
                .form(
                    "https://v.6.cn/coop/mobile/index.php?padapi=coop-mobile-inroom.php",
                    &[
                        ("av", "3.1".into()),
                        ("encpass", String::new()),
                        ("logiuid", String::new()),
                        ("project", "v6iphone".into()),
                        ("rate", "1".into()),
                        ("rid", String::new()),
                        ("ruid", rid),
                    ],
                )
                .await?;
            let title = field(&v, "/content/liveinfo/flvtitle")?
                .as_str()
                .ok_or("直播状态无效")?;
            c.result(
                s(&v, "/content/roominfo/alias"),
                !title.is_empty(),
                String::new(),
                String::new(),
                if title.is_empty() {
                    String::new()
                } else {
                    format!("https://wlive.6rooms.com/httpflv/{}.flv", escaped(title))
                },
            )
        }
        "kugou" => {
            let rid = c.query("roomId").unwrap_or(id);
            let v = c
                .get(&query_url(
                    "https://service2.fanxing.kugou.com/roomcen/room/web/cdn/getEnterRoomInfo",
                    &[("roomId", rid.clone())],
                )?)
                .await?;
            let status = field(&v, "/data/liveType")?
                .as_i64()
                .ok_or("直播状态无效")?;
            let on = status >= 0;
            let mut flv = String::new();
            if on {
                let play = c
                    .get(&query_url(
                        "https://fx1.service.kugou.com/video/pc/live/pull/mutiline/streamaddr",
                        &[
                            ("std_rid", rid),
                            ("std_plat", "7".into()),
                            ("std_kid", "0".into()),
                            ("streamType", "1-2-4-5-8".into()),
                            ("ua", "fx-flash".into()),
                            ("targetLiveTypes", "1-5-6".into()),
                            ("version", "1000".into()),
                            ("supportEncryptMode", "1".into()),
                            ("appid", "1010".into()),
                        ],
                    )?)
                    .await?;
                flv = field(&play, "/data/lines")?
                    .as_array()
                    .and_then(|a| a.last())
                    .map(|v| s(v, "/streamProfiles/0/httpsFlv/0"))
                    .unwrap_or_default();
            }
            c.result(
                s(&v, "/data/normalRoomInfo/nickName"),
                on,
                String::new(),
                String::new(),
                flv,
            )
        }
        "yinbo" | "changliao" => {
            let domain = if c.platform.key == "yinbo" {
                "ybw1666.com"
            } else {
                "tlclw.com"
            };
            let v = c
                .get(&query_url(
                    &format!("https://wap.{domain}/api/ui/room/v1.0.0/live.ashx"),
                    &[
                        ("roomidx", id.clone()),
                        ("currentUrl", format!("https://wap.{domain}/{id}")),
                    ],
                )?)
                .await?;
            let on = live(&v, "/data/roomInfo/live_stat", &["1"], &["0", "2"])?;
            let (mut hls, mut flv) = (String::new(), String::new());
            if on {
                let page = c.text(c.url.as_str()).await?;
                let config =
                    super::json_after(&page, "var config = ").ok_or("页面缺少播放域名配置")?;
                let live_id = s(&v, "/data/roomInfo/liveID");
                hls = format!(
                    "{}/{}.m3u8",
                    s(&config, "/domainpullstream_hls").trim_end_matches('/'),
                    escaped(&live_id)
                );
                flv = format!(
                    "{}/{}.flv",
                    s(&config, "/domainpullstream_flv").trim_end_matches('/'),
                    escaped(&live_id)
                );
            }
            c.result(
                s(&v, "/data/roomInfo/nickname"),
                on,
                String::new(),
                hls,
                flv,
            )
        }
        "piaopiao" | "huamao" => {
            let endpoint = if c.platform.key == "huamao" {
                "https://api.catshow168.com/live/preview"
            } else {
                "https://api.pp.weimipopo.com/live/preview"
            };
            let v = c
                .post(
                    endpoint,
                    json!({"inviteUuid":"","anchorUuid":c.query("anchorUid")?}),
                )
                .await?;
            let on = live(&v, "/data/living", &["true", "1"], &["false", "0"])?;
            c.result(
                s(&v, "/data/name"),
                on,
                String::new(),
                if on {
                    s(&v, "/data/pullUrl")
                } else {
                    String::new()
                },
                String::new(),
            )
        }
        "zhihu" => {
            if !c.url.path().contains("/theater/") {
                return Err("请使用知乎直播 theater 地址".into());
            }
            let page = c.text(c.url.as_str()).await?;
            let v = json_body(&capture(
                r#"(?s)<script[^>]*id="js-initialData"[^>]*>(.*?)</script>"#,
                &page,
            )?)?;
            let data = field(&v, &format!("/initialState/theater/theaters/{id}"))?;
            let on = live(data, "/drama/status", &["1"], &["0", "2", "3"])?;
            c.result(
                s(data, "/actor/name"),
                on,
                s(data, "/theme"),
                if on {
                    s(data, "/drama/playInfo/hlsUrl")
                } else {
                    String::new()
                },
                if on {
                    s(data, "/drama/playInfo/playUrl")
                } else {
                    String::new()
                },
            )
        }
        "weibo" => {
            let mut name = String::new();
            let rid = if c.url.path().contains("show/") {
                id
            } else {
                let list = c
                    .get(&query_url(
                        "https://weibo.com/ajax/statuses/mymblog",
                        &[("uid", id), ("page", "1".into()), ("feature", "0".into())],
                    )?)
                    .await?;
                let items = field(&list, "/data/list")?
                    .as_array()
                    .ok_or("微博列表无效")?;
                name = items
                    .first()
                    .map(|v| s(v, "/user/screen_name"))
                    .unwrap_or_default();
                items
                    .iter()
                    .find(|v| v["page_info"]["object_type"] == "live")
                    .map(|v| s(v, "/page_info/object_id"))
                    .unwrap_or_default()
            };
            if rid.is_empty() {
                return c.result(name, false, String::new(), String::new(), String::new());
            }
            let v = c
                .get(&query_url(
                    "https://weibo.com/l/pc/anchor/live",
                    &[("live_id", rid)],
                )?)
                .await?;
            let on = live(&v, "/data/item/status", &["1"], &["0", "2", "3"])?;
            c.result(
                s(&v, "/data/user_info/name"),
                on,
                s(&v, "/data/item/desc"),
                if on {
                    s(&v, "/data/item/stream_info/pull/live_origin_hls_url")
                } else {
                    String::new()
                },
                if on {
                    s(&v, "/data/item/stream_info/pull/live_origin_flv_url")
                } else {
                    String::new()
                },
            )
        }
        "vvxq" => {
            let rid = c.query("roomId")?;
            let v = c
                .get(&query_url(
                    "https://h5p.vvxqiu.com/room/video/getRoomData.do",
                    &[("roomId", rid.clone())],
                )?)
                .await?;
            let status = field(&v, "/status")?.as_i64().ok_or("直播状态无效")?;
            let on = status == 100 && !s(&v, "/videoUrl").is_empty();
            let name = if on {
                s(&v, "/nickName")
            } else {
                s(
                    &c.get(&query_url(
                        "https://h5p.vvxqiu.com/activity-center/fanclub/activity/captain/banner",
                        &[("roomId", rid), ("product", "vvstar".into())],
                    )?)
                    .await?,
                    "/data/anchorName",
                )
            };
            c.result(
                name,
                on,
                String::new(),
                if on {
                    s(&v, "/videoUrl")
                } else {
                    String::new()
                },
                String::new(),
            )
        }
        "baidu" => {
            let rid = c.query("room_id")?;
            let uid = format!("h5-{}", uuid::Uuid::new_v4().simple());
            let v=c.get(&query_url("https://mbd.baidu.com/searchbox",&[("cmd","371".into()),("action","star".into()),("service","bdbox".into()),("osname","baiduboxapp".into()),("data",json!({"data":{"room_id":rid,"device_id":uid,"source_type":0,"osname":"baiduboxapp"},"replay_slice":0}).to_string()),("ua","360_740_ANDROID_0".into()),("uid",uid)])?).await?;
            let rooms = field(&v, "/data")?.as_object().ok_or("百度直播数据无效")?;
            let data = rooms
                .get(&rid)
                .or_else(|| (rooms.len() == 1).then(|| rooms.values().next()).flatten())
                .ok_or("百度返回了不明确的直播间数据")?;
            let on = live(data, "/status", &["0"], &["1", "2", "3"])?;
            let urls = data
                .pointer("/video/url_clarity_list")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|v| s(v, "/urls/flv"))
                        .filter(|u| !u.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            c.result(
                s(data, "/host/name"),
                on,
                s(data, "/video/title"),
                String::new(),
                if on { choose(&urls, q) } else { String::new() },
            )
        }
        _ => resolve_remaining(c).await,
    }
}
fn choose(values: &[String], q: usize) -> String {
    values
        .get(q.min(values.len().saturating_sub(1)))
        .cloned()
        .unwrap_or_default()
}
async fn resolve_remaining(c: &Context<'_>) -> Result<StreamInfo, String> {
    let id = c.id()?;
    let q = quality_index(c.request.quality.as_deref())?;
    match c.platform.key {
        "youtube" => {
            let page = c.text(c.url.as_str()).await?;
            let v = super::json_after(&page, "var ytInitialPlayerResponse = ")
                .or_else(|| super::json_after(&page, "ytInitialPlayerResponse="))
                .ok_or("YouTube 页面未提供播放数据，请检查登录或地区限制")?;
            let on = if v.pointer("/videoDetails/isLive").is_some() {
                live(&v, "/videoDetails/isLive", &["true"], &["false"])?
            } else {
                live(
                    &v,
                    "/microformat/playerMicroformatRenderer/liveBroadcastDetails/isLiveNow",
                    &["true"],
                    &["false"],
                )?
            };
            c.result(
                s(&v, "/videoDetails/author"),
                on,
                s(&v, "/videoDetails/title"),
                if on {
                    s(&v, "/streamingData/hlsManifestUrl")
                } else {
                    String::new()
                },
                String::new(),
            )
        }
        "tiktok" => {
            let page = c.text(c.url.as_str()).await?;
            let v = json_body(&capture(
                r#"(?s)<script[^>]*id="SIGI_STATE"[^>]*>(.*?)</script>"#,
                &page,
            )?)?;
            let room = field(&v, "/LiveRoom/liveRoomUserInfo")?;
            let on = live(room, "/user/status", &["2"], &["0", "1", "3", "4"])?;
            let (mut hls, mut flv) = (String::new(), String::new());
            if on {
                let stream = json_body(&s(room, "/liveRoom/streamData/pull_data/stream_data"))?;
                let variants = field(&stream, "/data")?
                    .as_object()
                    .ok_or("TikTok 画质列表无效")?;
                let mut ranked = Vec::new();
                for variant in variants.values() {
                    let params = json_body(&s(variant, "/main/sdk_params"))?;
                    let rate = params["vbitrate"]
                        .as_u64()
                        .or_else(|| params["vbitrate"].as_str().and_then(|s| s.parse().ok()))
                        .unwrap_or(0);
                    ranked.push((rate, s(variant, "/main/hls"), s(variant, "/main/flv")));
                }
                ranked.sort_by_key(|v| std::cmp::Reverse(v.0));
                if let Some(choice) = ranked.get(q.min(ranked.len().saturating_sub(1))) {
                    hls = choice.1.clone();
                    flv = choice.2.clone();
                }
            }
            c.result(
                s(room, "/user/nickname"),
                on,
                s(room, "/liveRoom/title"),
                hls,
                flv,
            )
        }
        "rednote" => {
            let page = c.text(c.url.as_str()).await?;
            let raw = capture(
                r#"(?s)window\.__INITIAL_STATE__\s*=\s*(.*?)</script>"#,
                &page,
            )?;
            let v = json_body(&raw.replace(":undefined", ":null"))?;
            let room = field(&v, "/liveStream")?;
            let status = s(room, "/liveStatus");
            if status != "success" {
                return Err("小红书未提供可验证的直播数据，请检查登录或直播分享地址".into());
            }
            let room = field(room, "/roomData/roomInfo")?;
            let link = Url::parse(&s(room, "/deeplink")).map_err(|_| "直播分享数据无效")?;
            let name = link
                .query_pairs()
                .find(|(k, _)| k == "host_nickname")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            let flv = link
                .query_pairs()
                .find(|(k, _)| k == "flvUrl")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            let title = s(room, "/roomTitle");
            let on = !title.contains("回放");
            if on && flv.is_empty() {
                return Err("小红书直播数据缺少播放地址，未判定下播".into());
            }
            c.result(
                name,
                on,
                title,
                String::new(),
                if on { flv } else { String::new() },
            )
        }
        "pandatv" | "winktv" => {
            let domain = if c.platform.key == "winktv" {
                "winktv.co.kr"
            } else {
                "pandalive.co.kr"
            };
            let v = c
                .form(
                    &format!("https://api.{domain}/v1/member/bj"),
                    &[("userId", id.clone()), ("info", "media fanGrade".into())],
                )
                .await?;
            field(&v, "/bjInfo")?;
            let on = v.get("media").is_some_and(|v| !v.is_null());
            let mut hls = String::new();
            if on {
                let play = c
                    .form(
                        &format!("https://api.{domain}/v1/live/play"),
                        &[
                            ("action", "watch".into()),
                            ("userId", id),
                            ("password", c.query("pwd").unwrap_or_default()),
                            ("shareLinkType", String::new()),
                        ],
                    )
                    .await?;
                if play.get("errorData").is_some() {
                    return Err("该直播间要求有效登录或访问权限，请在平台完成验证".into());
                }
                hls = s(&play, "/PlayList/hls/0/url");
            }
            c.result(s(&v, "/bjInfo/nick"), on, String::new(), hls, String::new())
        }
        "huya" => {
            let rid = if id.chars().all(|c| c.is_ascii_digit()) {
                id
            } else {
                capture(
                    r#"ProfileRoom"\s*:\s*(\d+)"#,
                    &c.text(c.url.as_str()).await?,
                )?
            };
            let v = c
                .get(&query_url(
                    "https://mp.huya.com/cache.php",
                    &[
                        ("m", "Live".into()),
                        ("do", "profileRoom".into()),
                        ("roomid", rid),
                        ("showSecret", "1".into()),
                    ],
                )?)
                .await?;
            let on = live(&v, "/data/realLiveStatus", &["ON"], &["OFF"])?;
            let (mut hls, mut flv) = (String::new(), String::new());
            if on {
                let list = field(&v, "/data/stream/baseSteamInfoList")?
                    .as_array()
                    .ok_or("虎牙 CDN 列表无效")?;
                let stream = list
                    .iter()
                    .find(|v| v["sCdnType"] == "TX")
                    .or_else(|| list.first())
                    .ok_or("虎牙未返回播放线路")?;
                let name = s(stream, "/sStreamName");
                flv = format!(
                    "{}/{}.flv?{}",
                    s(stream, "/sFlvUrl"),
                    name,
                    s(stream, "/sFlvAntiCode")
                );
                hls = format!(
                    "{}/{}.m3u8?{}",
                    s(stream, "/sHlsUrl"),
                    name,
                    s(stream, "/sHlsAntiCode")
                );
                if q > 0 {
                    let rates = regex::Regex::new(r"264_(\d+)")
                        .expect("static regex")
                        .captures_iter(&s(stream, "/sFlvAntiCode"))
                        .filter_map(|c| c[1].parse::<u64>().ok())
                        .collect::<Vec<_>>();
                    let mut rates = rates;
                    rates.sort_unstable_by(|a, b| b.cmp(a));
                    if let Some(ratio) = rates.get((q - 1).min(rates.len().saturating_sub(1))) {
                        flv.push_str(&format!("&ratio={ratio}"));
                        hls.push_str(&format!("&ratio={ratio}"));
                    }
                }
            }
            c.result(
                s(&v, "/data/profileInfo/nick"),
                on,
                s(&v, "/data/liveData/introduction"),
                hls,
                flv,
            )
        }
        "douyu" => {
            let rid = if id.chars().all(|c| c.is_ascii_digit()) {
                id
            } else {
                capture(
                    r#""rid"\s*:\s*(\d+)"#,
                    &c.text(&format!("https://m.douyu.com/{}", escaped(&id)))
                        .await?,
                )?
            };
            let v = c
                .get(&format!("https://www.douyu.com/betard/{}", escaped(&rid)))
                .await?;
            let on = live(&v, "/room/show_status", &["1"], &["0", "2"])?;
            let mut flv = String::new();
            if on {
                let did = uuid::Uuid::new_v4().simple().to_string();
                let white = c
                    .get(&query_url(
                        "https://www.douyu.com/wgapi/livenc/liveweb/websec/getEncryption",
                        &[("did", did.clone())],
                    )?)
                    .await?;
                if white["error"] != 0 {
                    return Err("斗鱼未提供播放签名参数".into());
                }
                let white = field(&white, "/data")?;
                let count = field(white, "/enc_time")?
                    .as_u64()
                    .filter(|n| *n <= 32)
                    .ok_or("斗鱼签名参数无效")?;
                let key = s(white, "/key");
                let mut secret = s(white, "/rand_str");
                for _ in 0..count {
                    secret = format!("{:x}", md5::compute(format!("{secret}{key}")));
                }
                let now = chrono::Utc::now().timestamp().to_string();
                let salt = if white["is_special"] == true || white["is_special"] == 1 {
                    String::new()
                } else {
                    format!("{rid}{now}")
                };
                let auth = format!("{:x}", md5::compute(format!("{secret}{key}{salt}")));
                let play = c
                    .form(
                        &format!(
                            "https://playweb.douyucdn.cn/lapi/live/getH5PlayV1/{}",
                            escaped(&rid)
                        ),
                        &[
                            ("rate", ["0", "3", "2", "1", "1"][q].into()),
                            ("ver", "219032101".into()),
                            ("iar", "0".into()),
                            ("ive", "0".into()),
                            ("rid", rid),
                            ("hevc", "0".into()),
                            ("fa", "0".into()),
                            ("sov", "0".into()),
                            ("enc_data", s(white, "/enc_data")),
                            ("tt", now),
                            ("did", did),
                            ("auth", auth),
                        ],
                    )
                    .await?;
                if play["error"] != 0 {
                    return Err("斗鱼播放接口拒绝请求，请稍后重试".into());
                }
                flv = format!(
                    "{}/{}",
                    s(&play, "/data/rtmp_url"),
                    s(&play, "/data/rtmp_live")
                );
            }
            c.result(
                s(&v, "/room/nickname"),
                on,
                s(&v, "/room/room_name"),
                String::new(),
                flv,
            )
        }
        "jd" => {
            let author = c.query("authorId").ok();
            let (name, rid) = if let Some(author) = author {
                let v = c
                    .form(
                        "https://api.m.jd.com/talent_head_findTalentMsg",
                        &[
                            ("functionId", "talent_head_findTalentMsg".into()),
                            ("appid", "dr_detail".into()),
                            (
                                "body",
                                json!({"authorId":author,"monitorSource":"1","userId":""})
                                    .to_string(),
                            ),
                        ],
                    )
                    .await?;
                let name = s(&v, "/result/talentName");
                let rid = s(&v, "/result/livingRoomJump/params/id");
                if rid.is_empty() {
                    return c.result(name, false, String::new(), String::new(), String::new());
                }
                (name, rid)
            } else {
                let fragment = c.url.fragment().ok_or("京东地址缺少 authorId 或直播标识")?;
                let rid = fragment
                    .trim_start_matches('/')
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .to_string();
                (format!("京东直播 {rid}"), rid)
            };
            let v = c
                .get(&query_url(
                    "https://api.m.jd.com/client.action",
                    &[
                        ("body", json!({"liveId":rid}).to_string()),
                        ("functionId", "getImmediatePlayToM".into()),
                        ("appid", "h5-live".into()),
                    ],
                )?)
                .await?;
            let on = live(&v, "/data/status", &["1"], &["0", "2", "3"])?;
            c.result(
                name,
                on,
                String::new(),
                if on {
                    s(&v, "/data/h5VideoUrl")
                } else {
                    String::new()
                },
                if on {
                    s(&v, "/data/videoUrl")
                } else {
                    String::new()
                },
            )
        }
        "shopee" => {
            let session = c
                .query("session")?
                .parse::<u64>()
                .map_err(|_| "Shopee session 无效")?;
            let domain = c
                .platform
                .domains
                .iter()
                .find(|d| {
                    c.url
                        .host_str()
                        .is_some_and(|h| h == **d || h.ends_with(&format!(".{d}")))
                })
                .ok_or("Shopee 地区地址无效")?;
            let v = c
                .post(
                    &format!("https://live.{domain}/api/v1/play_param/session"),
                    json!({"extra":"{}","quality_level_id":q,"session_ids":[session]}),
                )
                .await?;
            let item = field(&v, "/data/play_param_list/0")?;
            let name = format!(
                "{}_{}",
                s(item, "/session/nickname"),
                s(item, "/session/username")
            );
            let mpd = s(item, "/play_param/las_param/mpd");
            let flv = if mpd.is_empty() {
                s(item, "/play_param/play_url_list/0")
            } else {
                let mpd = json_body(&mpd)?;
                let mut list = field(&mpd, "/adaptationSet/0/representation")?
                    .as_array()
                    .ok_or("Shopee 画质列表无效")?
                    .clone();
                list.sort_by_key(|v| std::cmp::Reverse(v["maxBitrate"].as_u64().unwrap_or(0)));
                list.get(q.min(list.len().saturating_sub(1)))
                    .map(|v| s(v, "/url"))
                    .unwrap_or_default()
            };
            c.result(name, true, String::new(), String::new(), flv)
        }
        "acfun" => {
            let identity = c
                .get(&query_url(
                    "https://live.acfun.cn/rest/pc-direct/user/userInfo",
                    &[("userId", id.clone())],
                )?)
                .await?;
            let profile = field(&identity, "/profile")?;
            let live_id = profile.get("liveId").ok_or("AcFun 响应缺少直播状态")?;
            let on = !live_id.is_null() && live_id != &json!("");
            let mut flv = String::new();
            let mut title = String::new();
            if on {
                let did = format!("web_{}", uuid::Uuid::new_v4().simple());
                let response = c
                    .send_request(
                        c.request(
                            reqwest::Method::POST,
                            "https://id.app.acfun.cn/rest/app/visitor/login",
                        )?
                        .header("Cookie", format!("_did={did}"))
                        .form(&[("sid", "acfun.api.visitor")]),
                    )
                    .await?;
                let visitor = json_body(&http::body(response).await?)?;
                let token = s(&visitor, "/acfun.api.visitor_st");
                if token.is_empty() {
                    return Err("AcFun 访客凭证不可用".into());
                }
                let play = c
                    .form(
                        &query_url(
                            "https://api.kuaishouzt.com/rest/zt/live/web/startPlay",
                            &[
                                ("subBiz", "mainApp".into()),
                                ("kpn", "ACFUN_APP".into()),
                                ("kpf", "PC_WEB".into()),
                                ("userId", s(&visitor, "/userId")),
                                ("did", did),
                                ("acfun.api.visitor_st", token),
                            ],
                        )?,
                        &[("authorId", id), ("pullStreamType", "FLV".into())],
                    )
                    .await?;
                title = s(&play, "/data/caption");
                let streams = json_body(&s(&play, "/data/videoPlayRes"))?;
                let mut list = field(
                    &streams,
                    "/liveAdaptiveManifest/0/adaptationSet/representation",
                )?
                .as_array()
                .ok_or("AcFun 画质列表无效")?
                .clone();
                list.sort_by_key(|v| std::cmp::Reverse(v["bitrate"].as_u64().unwrap_or(0)));
                flv = list
                    .get(q.min(list.len().saturating_sub(1)))
                    .map(|v| s(v, "/url"))
                    .unwrap_or_default();
            }
            c.result(s(profile, "/name"), on, title, String::new(), flv)
        }
        _ => resolve_specialized(c).await,
    }
}
async fn resolve_specialized(c: &Context<'_>) -> Result<StreamInfo, String> {
    let id = c.id()?;
    let q = quality_index(c.request.quality.as_deref())?;
    match c.platform.key {
        "look" => {
            let (params, key) =
                super::signatures::look_params(&json!({"liveRoomNo":c.query("id")?}))?;
            let v = c
                .form(
                    "https://api.look.163.com/weapi/livestream/room/get/v3",
                    &[("params", params), ("encSecKey", key)],
                )
                .await?;
            let on = live(&v, "/data/liveStatus", &["1"], &["0", "2"])?;
            c.result(
                s(&v, "/data/anchor/nickName"),
                on,
                s(&v, "/data/roomInfo/title"),
                if on {
                    s(&v, "/data/roomInfo/liveUrl/hlsPullUrl")
                } else {
                    String::new()
                },
                if on {
                    s(&v, "/data/roomInfo/liveUrl/httpPullUrl")
                } else {
                    String::new()
                },
            )
        }
        "laixiu" => {
            let timestamp = chrono::Utc::now().timestamp_millis().to_string();
            let imei = uuid::Uuid::new_v4().simple().to_string();
            let signature = format!(
                "{:x}",
                md5::compute(format!(
                    "web{imei}{timestamp}kk792f28d6ff1f34ec702c08626d454b39pro"
                ))
            );
            let rid = c.query("roomId").or_else(|_| c.query("anchorId"))?;
            let mut request = c.request(
                reqwest::Method::GET,
                &query_url(
                    "https://api.imkktv.com/liveroom/getShareLiveVideo",
                    &[("roomId", rid)],
                )?,
            )?;
            for (k, v) in [
                ("mobileModel", "web"),
                ("timestamp", timestamp.as_str()),
                ("loginType", "2"),
                ("versionCode", "10003"),
                ("imei", imei.as_str()),
                ("requestId", signature.as_str()),
                ("channel", "9"),
                ("version", "1.0.0"),
                ("os", "web"),
                ("platform", "WEB"),
            ] {
                request = request.header(k, v);
            }
            let v = json_body(&http::body(c.send_request(request).await?).await?)?;
            let on = live(&v, "/data/playStatus", &["0"], &["1", "2"])?;
            c.result(
                s(&v, "/data/nickname"),
                on,
                String::new(),
                String::new(),
                if on {
                    s(&v, "/data/playUrl")
                } else {
                    String::new()
                },
            )
        }
        "haixiu" | "lehai" => {
            let token = if let Some(token) = c
                .request
                .account
                .as_ref()
                .map(|a| a.access_token.clone())
                .filter(|s| !s.is_empty())
            {
                token
            } else {
                let page = c.text(c.url.as_str()).await?;
                capture(r#"accessToken\s*[:=]\s*["']([^"']+)"#, &page)
                    .map_err(|_| "该平台需要有效 accessToken，请在账号设置中配置")?
            };
            let time = chrono::Utc::now().timestamp_millis();
            let token_encoded = escaped(&escaped(&token));
            let params = json!({"accessToken":token_encoded,"tku":"3000006","c":"10138100100000","_st1":time});
            let signature = super::signatures::haixiu(&params)?;
            let domain = if c.platform.key == "haixiu" {
                "haixiutv.com"
            } else {
                "lehaitv.com"
            };
            let v = c
                .get(&query_url(
                    &format!(
                        "https://service.{domain}/v2/room/{}/media/advanceInfoRoom",
                        escaped(&id)
                    ),
                    &[
                        ("accessToken", token),
                        ("tku", "3000006".into()),
                        ("c", "10138100100000".into()),
                        ("_st1", time.to_string()),
                        ("_ajaxData1", signature),
                        ("_", time.to_string()),
                    ],
                )?)
                .await?;
            let on = live(&v, "/data/live_status", &["1"], &["0", "2"])?;
            c.result(
                s(&v, "/data/nickname"),
                on,
                String::new(),
                String::new(),
                if on {
                    s(&v, "/data/media_url_web")
                } else {
                    String::new()
                },
            )
        }
        "liveme" => {
            let rid = if c.url.path().contains("/index.html") {
                c.url
                    .path()
                    .trim_end_matches("/index.html")
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_string()
            } else {
                let page = c.text(c.url.as_str()).await?;
                let original = Url::parse(&capture(
                    r#"<meta property="og:url" content="([^"]+)"#,
                    &page,
                )?)
                .map_err(|_| "LiveMe 分享地址无效")?;
                original
                    .path()
                    .trim_end_matches("/index.html")
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_string()
            };
            let mut signature = super::signatures::liveme(&rid)?;
            let sign = s(&signature, "/lm_s_sign");
            let map = signature.as_object_mut().ok_or("LiveMe 签名无效")?;
            map.remove("lm_s_sign");
            let mut params = Vec::new();
            for key in ["alias", "tongdun_black_box", "os"] {
                if let Some(value) = map.remove(key) {
                    params.push((key, super::text(&value)));
                }
            }
            let fields = map
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if v.is_number() {
                            v.to_string()
                        } else {
                            super::text(v)
                        },
                    )
                })
                .collect::<Vec<_>>();
            let response = c
                .send_request(
                    c.request(
                        reqwest::Method::POST,
                        &query_url("https://live.liveme.com/live/queryinfosimple", &params)?,
                    )?
                    .header("lm-s-sign", sign)
                    .form(&fields),
                )
                .await?;
            let v = json_body(&http::body(response).await?)?;
            let on = live(&v, "/data/video_info/status", &["0"], &["1", "2", "3"])?;
            c.result(
                s(&v, "/data/video_info/uname"),
                on,
                String::new(),
                if on {
                    s(&v, "/data/video_info/hlsvideosource")
                } else {
                    String::new()
                },
                if on {
                    s(&v, "/data/video_info/videosource")
                } else {
                    String::new()
                },
            )
        }
        "huajiao" => {
            if let Ok(author) = c.query("author") {
                let v = c
                    .get(&query_url(
                        "https://live.huajiao.com/feed/getUserFeeds",
                        &[
                            ("channel", "Apple".into()),
                            ("userid", author.clone()),
                            ("uid", author),
                        ],
                    )?)
                    .await?;
                let room = field(&v, "/data/feeds/0")?;
                let status = s(room, "/feed/rtop");
                let on = if status == "直播中" {
                    true
                } else if status.contains("回放") {
                    false
                } else {
                    return Err("花椒未返回可验证的直播状态".into());
                };
                return c.result(
                    s(room, "/author/nickname"),
                    on,
                    s(room, "/feed/title"),
                    String::new(),
                    if on {
                        s(room, "/feed/pull_url")
                    } else {
                        String::new()
                    },
                );
            }
            let rid = c.query("liveid")?;
            let v = c
                .get(&query_url(
                    "https://live.huajiao.com/feed/getFeedInfo",
                    &[("relateid", rid)],
                )?)
                .await?;
            let data = field(&v, "/data")?;
            let play = c
                .get(&query_url(
                    "https://live.huajiao.com/live/substream",
                    &[
                        ("time", chrono::Utc::now().timestamp_millis().to_string()),
                        ("version", "1.0.0".into()),
                        ("sn", s(data, "/feed/sn")),
                        ("liveid", s(data, "/feed/relateid")),
                        ("uid", s(data, "/author/uid")),
                        ("encode", "h264".into()),
                    ],
                )?)
                .await?;
            c.result(
                s(data, "/author/nickname"),
                true,
                s(data, "/feed/title"),
                s(&play, "/data/pull_m3u8"),
                s(&play, "/data/h264_url"),
            )
        }
        "yy" => {
            let page = c.text(c.url.as_str()).await?;
            let name = capture(r#"nick:\s*"([^"]+)"#, &page)?;
            let cid = capture(r#"sid\s*:\s*"(\d+)"#, &page)?;
            let now = chrono::Utc::now().timestamp_millis();
            let payload = json!({"head":{"seq":now,"appidstr":"0","bidstr":"121","cidstr":cid,"sidstr":cid,"uid64":0,"client_type":108,"client_ver":"5.17.0","stream_sys_ver":1,"app":"yylive_web","playersdk_ver":"5.17.0","thundersdk_ver":"0","streamsdk_ver":"5.17.0"},"client_attribute":{"client":"web","model":"web0","os":"chrome","osversion":"0","client_type":8,"h265":0},"avp_parameter":{"version":1,"client_type":8,"service_type":0,"imsi":0,"send_time":now/1000,"line_seq":-1,"gear":4-q.min(3),"ssl":1,"stream_format":0}});
            let v = c
                .post(
                    &query_url(
                        "https://stream-manager.yy.com/v3/channel/streams",
                        &[
                            ("uid", "0".into()),
                            ("cid", cid.clone()),
                            ("sid", cid.clone()),
                            ("appid", "0".into()),
                            ("sequence", now.to_string()),
                            ("encode", "json".into()),
                        ],
                    )?,
                    payload,
                )
                .await?;
            let list = field(&v, "/avp_info_res/stream_line_addr")?
                .as_object()
                .ok_or("YY 播放线路无效")?;
            let flv = list
                .values()
                .next()
                .map(|v| s(v, "/cdn_info/url"))
                .unwrap_or_default();
            let detail = c
                .get(&query_url(
                    "https://www.yy.com/live/detail",
                    &[("uid", String::new()), ("sid", cid.clone()), ("ssid", cid)],
                )?)
                .await?;
            c.result(
                name,
                !flv.is_empty(),
                s(&detail, "/data/roomName"),
                String::new(),
                flv,
            )
        }
        "flextv" => {
            let user = c
                .url
                .path()
                .trim_end_matches("/live")
                .rsplit('/')
                .next()
                .unwrap_or("");
            let url = format!("https://www.ttinglive.com/channels/{}/live", escaped(user));
            let mut page = c.text(&url).await?;
            let mut v = json_body(&capture(
                r#"(?s)<script[^>]*id="__NEXT_DATA__"[^>]*>(.*?)</script>"#,
                &page,
            )?)?;
            if s(&v, "/props/pageProps/channelStream/channel/message").contains("로그인") {
                let account = credentials(c)?;
                c.post("https://api.ttinglive.com/v2/api/auth/signin",json!({"loginId":account.username,"password":account.password,"loginKeep":true,"saveId":true,"device":"PCWEB"})).await?;
                page = c.text(&url).await?;
                v = json_body(&capture(
                    r#"(?s)<script[^>]*id="__NEXT_DATA__"[^>]*>(.*?)</script>"#,
                    &page,
                )?)?;
            }
            let channel = v
                .pointer("/props/pageProps/channelStream/channel")
                .or_else(|| v.pointer("/props/pageProps/channel"))
                .ok_or("FlexTV 未返回频道数据")?;
            let name = s(channel, "/owner/nickname");
            if channel.get("message").is_some() {
                return Err("FlexTV 当前需要有效登录或频道权限".into());
            }
            let play = c
                .get(&format!(
                    "https://api.ttinglive.com/api/channels/{}/stream?option=all",
                    escaped(user)
                ))
                .await?;
            let url = s(&play, "/sources/0/url");
            c.result(
                name,
                true,
                String::new(),
                if url.contains(".m3u8") {
                    url.clone()
                } else {
                    String::new()
                },
                if url.contains(".m3u8") {
                    String::new()
                } else {
                    url
                },
            )
        }
        "soop" => {
            let user = c
                .url
                .path_segments()
                .and_then(|mut p| p.find(|s| !s.is_empty()))
                .ok_or("SOOP 地址缺少主播标识")?;
            let identity = c
                .get(&query_url(
                    "https://st.sooplive.com/api/get_station_status.php",
                    &[("szBjId", user.into())],
                )?)
                .await?;
            let name = s(&identity, "/DATA/user_nick");
            let endpoint = query_url(
                "https://live.sooplive.com/afreeca/player_live_api.php",
                &[("bjid", user.into())],
            )?;
            let mut fields = vec![
                ("bid", user.into()),
                ("bno", String::new()),
                ("type", String::new()),
                ("pwd", c.query("pwd").unwrap_or_default()),
                ("player_type", "html5".into()),
                ("stream_type", "common".into()),
                ("quality", "master".into()),
                ("mode", "landing".into()),
                ("from_api", "0".into()),
                ("is_revive", "false".into()),
            ];
            let mut v = c.form(&endpoint, &fields).await?;
            if !matches!(
                v.pointer("/CHANNEL/RESULT").and_then(Value::as_i64),
                Some(0 | 1)
            ) {
                let account = credentials(c)?;
                c.form(
                    "https://login.sooplive.com/app/LoginAction.php",
                    &[
                        ("szWork", "login".into()),
                        ("szType", "json".into()),
                        ("szUid", account.username.clone()),
                        ("szPassword", account.password.clone()),
                        ("isLoginRetain", "Y".into()),
                    ],
                )
                .await?;
                v = c.form(&endpoint, &fields).await?;
            }
            let result = field(&v, "/CHANNEL/RESULT")?
                .as_i64()
                .ok_or("SOOP 状态无效")?;
            if !matches!(result, 0 | 1) {
                return Err("SOOP 拒绝播放，请在平台完成账号验证".into());
            }
            let on = v
                .pointer("/CHANNEL/VIEWPRESET")
                .is_some_and(|v| !v.is_null() && v != &json!(""));
            if !on {
                return c.result(name, false, String::new(), String::new(), String::new());
            }
            let broad = s(&v, "/CHANNEL/BNO");
            let cdn = c
                .get(&query_url(
                    "https://livestream-manager.sooplive.com/broad_stream_assign.html",
                    &[
                        ("return_type", "gcp_cdn".into()),
                        ("use_cors", "false".into()),
                        ("cors_origin_url", "play.sooplive.com".into()),
                        ("broad_key", format!("{broad}-common-master-hls")),
                    ],
                )?)
                .await?;
            fields
                .iter_mut()
                .find(|(k, _)| *k == "type")
                .expect("type field")
                .1 = "aid".into();
            let token = c.form(&endpoint, &fields).await?;
            let hls = query_url(&s(&cdn, "/view_url"), &[("aid", s(&token, "/CHANNEL/AID"))])?;
            c.result(name, true, s(&v, "/CHANNEL/TITLE"), hls, String::new())
        }
        "taobao" => {
            let rid = match c.query("id").or_else(|_| c.query("liveId")) {
                Ok(id) => id,
                Err(_) => {
                    let page = c.text(c.url.as_str()).await?;
                    let link = Url::parse(&capture(r#"var url = '([^']+)'"#, &page)?)
                        .map_err(|_| "淘宝分享地址无效")?;
                    link.query_pairs()
                        .find(|(k, _)| k == "id" || k == "liveId")
                        .map(|(_, v)| v.into_owned())
                        .ok_or("淘宝分享地址缺少直播标识")?
                }
            };
            let endpoint = "https://h5api.m.taobao.com/h5/mtop.mediaplatform.live.livedetail/4.0/";
            let body = json!({"liveId":rid,"creatorId":null}).to_string();
            for _ in 0..2 {
                let time = chrono::Utc::now().timestamp_millis().to_string();
                let token = c.cookie_value(endpoint, "_m_h5_tk").unwrap_or_default();
                let token = token.split('_').next().unwrap_or("");
                let sign = format!(
                    "{:x}",
                    md5::compute(format!("{token}&{time}&12574478&{body}"))
                );
                let page = c
                    .text(&query_url(
                        endpoint,
                        &[
                            ("jsv", "2.7.0".into()),
                            ("appKey", "12574478".into()),
                            ("t", time),
                            ("sign", sign),
                            ("api", "mtop.mediaplatform.live.livedetail".into()),
                            ("v", "4.0".into()),
                            ("type", "originaljson".into()),
                            ("dataType", "json".into()),
                            ("data", body.clone()),
                        ],
                    )?)
                    .await?;
                let v = json_body(&page)?;
                if v["ret"].as_array().is_some_and(|a| {
                    a.iter()
                        .any(|v| v.as_str().is_some_and(|s| s.starts_with("SUCCESS")))
                }) {
                    let on = live(&v, "/data/streamStatus", &["1"], &["0", "2", "3"])?;
                    let list = v
                        .pointer("/data/liveUrlList")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let mut list = list;
                    list.sort_by_key(|v| {
                        std::cmp::Reverse(match s(v, "/definition").as_str() {
                            "ud" => 4,
                            "hd" => 3,
                            "md" => 2,
                            "ld" => 1,
                            _ => 0,
                        })
                    });
                    let choice = list
                        .get(q.min(list.len().saturating_sub(1)))
                        .unwrap_or(&Value::Null)
                        .clone();
                    return c.result(
                        s(&v, "/data/broadCaster/accountName"),
                        on,
                        s(&v, "/data/title"),
                        if on {
                            s(&choice, "/hlsUrl")
                        } else {
                            String::new()
                        },
                        if on {
                            s(&choice, "/flvUrl")
                        } else {
                            String::new()
                        },
                    );
                }
            }
            Err("淘宝登录或播放签名不可用，请更新 Cookie".into())
        }

        "twitcasting" => {
            let channel = c
                .url
                .path_segments()
                .and_then(|mut s| s.next())
                .ok_or("TwitCasting 地址无效")?;
            let mut page = c.text(c.url.as_str()).await?;
            if !page.contains("data-is-onlive=") {
                let account = credentials(c)?;
                let endpoint = if account.account_type.eq_ignore_ascii_case("twitter") {
                    "https://twitcasting.tv/indexpasswordlogin.php"
                } else {
                    "https://twitcasting.tv/indexcaslogin.php"
                };
                let login = c.text(endpoint).await?;
                let csrf = capture(r#"name="cs_session_id" value="([^"]+)"#, &login)?;
                let response = c
                    .send_request(c.request(reqwest::Method::POST, endpoint)?.form(&[
                        ("username", account.username.clone()),
                        ("password", account.password.clone()),
                        ("action", "login".into()),
                        ("cs_session_id", csrf),
                    ]))
                    .await?;
                if !response.status().is_success() && !response.status().is_redirection() {
                    return Err("TwitCasting 登录失败，请在平台完成验证".into());
                }
                page = c.text(c.url.as_str()).await?;
            }
            let status = capture(r#"data-is-onlive="([^"]+)"#, &page)?;
            let on = match status.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err("TwitCasting 直播状态无效".into()),
            };
            let title = capture(r#"<meta name="twitter:title" content="([^"]*)"#, &page)
                .unwrap_or_default();
            let hls = if on {
                let v = c
                    .get(&query_url(
                        "https://twitcasting.tv/streamserver.php",
                        &[
                            ("target", channel.into()),
                            ("mode", "client".into()),
                            ("player", "pc_web".into()),
                        ],
                    )?)
                    .await?;
                let streams = field(&v, "/tc-hls/streams")?;
                let urls = ["high", "medium", "low"]
                    .iter()
                    .map(|key| s(streams, &format!("/{key}")))
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>();
                choose(&urls, q)
            } else {
                String::new()
            };
            c.result(channel.to_string(), on, title, hls, String::new())
        }
        "twitch" => twitch(c, &id).await,
        "faceit" => {
            let name = c
                .url
                .path_segments()
                .and_then(|mut p| p.find(|s| *s == "players").and_then(|_| p.next()))
                .ok_or("FACEIT 地址无效")?;
            let user = c
                .get(&format!(
                    "https://www.faceit.com/api/users/v1/nicknames/{}",
                    escaped(name)
                ))
                .await?;
            let v = c
                .get(&query_url(
                    "https://www.faceit.com/api/stream/v1/streamings",
                    &[("userId", s(&user, "/payload/id"))],
                )?)
                .await?;
            let list = field(&v, "/payload")?.as_array().ok_or("FACEIT 数据无效")?;
            let Some(stream) = list.first() else {
                return c.result(
                    name.into(),
                    false,
                    String::new(),
                    String::new(),
                    String::new(),
                );
            };
            if stream["platform"] != "twitch" {
                return Err("FACEIT 返回了不支持的关联直播来源".into());
            }
            let target = s(stream, "/platformId");
            if target.is_empty() {
                return Err("FACEIT 未返回关联直播频道".into());
            }
            twitch(c, &target).await
        }
        "popkontv" => popkontv(c).await,
        "migu" => migu(c).await,
        _ => Err("未知的平台标识".into()),
    }
}
fn credentials<'a>(c: &'a Context<'_>) -> Result<&'a crate::config::PlatformAccount, String> {
    c.request
        .account
        .as_ref()
        .filter(|a| !a.username.is_empty() && !a.password.is_empty())
        .ok_or("该平台需要有效账号或 Cookie，请先在账号设置中配置".into())
}
async fn public_identifier(
    c: &Context<'_>,
    page_url: &str,
    pattern: &str,
) -> Result<String, String> {
    let page = c.text(page_url).await?;
    if let Ok(value) = capture(pattern, &page) {
        return Ok(value);
    }
    let scripts = regex::Regex::new(r#"<script[^>]*src="([^"]+\.js[^"]*)"#).expect("static regex");
    for script in scripts.captures_iter(&page).take(8) {
        let url = Url::parse(page_url)
            .map_err(|_| "平台地址无效")?
            .join(&script[1])
            .map_err(|_| "脚本地址无效")?;
        if !allowed_host(c.platform, url.host_str().unwrap_or("")) {
            continue;
        }
        let source = c.text(url.as_str()).await?;
        if let Ok(value) = capture(pattern, &source) {
            return Ok(value);
        }
    }
    Err("平台公开客户端标识已变化，未使用硬编码凭证".into())
}
fn hls_variant(base: &str, text: &str, quality: usize) -> Result<String, String> {
    if !text.trim_start().starts_with("#EXTM3U") {
        return Err("平台未返回有效 HLS 播放列表".into());
    }
    let base = Url::parse(base).map_err(|_| "HLS 地址无效")?;
    let mut pending = None;
    let mut values = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(attributes) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let video = attributes.contains("RESOLUTION=")
                || attributes.contains("avc")
                || attributes.contains("hvc")
                || attributes.contains("av01");
            let bitrate = attributes
                .split(',')
                .find_map(|field| field.strip_prefix("BANDWIDTH="))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            pending = video.then_some(bitrate);
        } else if !line.starts_with('#') && !line.is_empty() {
            if let Some(rate) = pending.take() {
                values.push((
                    rate,
                    base.join(line).map_err(|_| "HLS 分支地址无效")?.to_string(),
                ));
            }
        }
    }
    values.sort_by_key(|(rate, _)| std::cmp::Reverse(*rate));
    if values.is_empty() {
        if text.contains("#EXTINF:") {
            Ok(base.to_string())
        } else {
            Err("HLS 播放列表没有可用视频分支".into())
        }
    } else {
        Ok(values[quality.min(values.len() - 1)].1.clone())
    }
}

async fn twitch(c: &Context<'_>, channel: &str) -> Result<StreamInfo, String> {
    let client = public_identifier(
        c,
        "https://www.twitch.tv/",
        r#"(?:clientId|client_id|clientID)["']?\s*[:=]\s*["']([a-z0-9]{20,40})"#,
    )
    .await?;
    let query="query($login:String!){user(login:$login){displayName stream{id} broadcastSettings{title}} streamPlaybackAccessToken(channelName:$login,params:{platform:\"web\",playerBackend:\"mediaplayer\",playerType:\"site\"}){value signature authorization{isForbidden}}}";
    let response = c
        .send_request(
            c.request(reqwest::Method::POST, "https://gql.twitch.tv/gql")?
                .header("Client-ID", client)
                .json(&json!({"query":query,"variables":{"login":channel}})),
        )
        .await?;
    let v = json_body(&http::body(response).await?)?;
    let user = field(&v, "/data/user")?;
    field(user, "/displayName")?;
    let on = !user
        .get("stream")
        .ok_or("Twitch 响应缺少直播状态")?
        .is_null();
    let mut hls = String::new();
    if on {
        let token = field(&v, "/data/streamPlaybackAccessToken")?;
        if token["authorization"]["isForbidden"] == true {
            return Err("Twitch 拒绝当前账号播放权限".into());
        }
        if s(token, "/signature").is_empty() || s(token, "/value").is_empty() {
            return Err("Twitch 未提供有效播放授权".into());
        }
        hls = query_url(
            &format!(
                "https://usher.ttvnw.net/api/channel/hls/{}.m3u8",
                escaped(channel)
            ),
            &[
                ("allow_source", "true".into()),
                ("allow_audio_only", "true".into()),
                ("sig", s(token, "/signature")),
                ("token", s(token, "/value")),
                ("player_backend", "mediaplayer".into()),
            ],
        )?;
        let manifest = c.text(&hls).await?;
        hls = hls_variant(
            &hls,
            &manifest,
            quality_index(c.request.quality.as_deref())?,
        )?;
    }
    c.result(
        s(user, "/displayName"),
        on,
        s(user, "/broadcastSettings/title"),
        hls,
        String::new(),
    )
}
async fn popkontv(c: &Context<'_>) -> Result<StreamInfo, String> {
    let id = c.query("castId").or_else(|_| c.query("mcid"))?;
    let partner = c
        .query("partnerCode")
        .or_else(|_| c.query("mcPartnerCode"))
        .unwrap_or_else(|_| "P-00001".into());
    let url = query_url(
        "https://www.popkontv.com/live/view",
        &[("castId", id.clone()), ("partnerCode", partner.clone())],
    )?;
    let page = c.text(&url).await?;
    let v = json_body(&capture(
        r#"(?s)<script[^>]*id="__NEXT_DATA__"[^>]*>(.*?)</script>"#,
        &page,
    )?)?;
    let page_props = field(&v, "/props/pageProps")?;
    let data = page_props.pointer("/mcData/data");
    let name = match data
        .map(|data| s(data, "/mc_nickName"))
        .filter(|name| !name.is_empty())
    {
        Some(name) => name,
        None => {
            let notices = c
                .text(&query_url(
                    "https://www.popkontv.com/channel/notices",
                    &[("mcid", id.clone()), ("mcPartnerCode", partner.clone())],
                )?)
                .await?;
            capture(r#""mcNickName"\s*:\s*"([^"]+)""#, &notices)?
        }
    };
    let Some(data) = data else {
        return c.result(name, false, String::new(), String::new(), String::new());
    };
    let private = field(data, "/mc_isPrivate")?;
    let password = c.query("pwd").unwrap_or_default();
    if private != &json!(0) && private != &json!("0") && password.is_empty() {
        return Err("该直播间需要房间密码，未尝试绕过".into());
    }
    let client = public_identifier(
        c,
        "https://www.popkontv.com",
        r#"(Client [A-Za-z0-9+/=]{32,128})"#,
    )
    .await?;
    let mut account = c.request.account.clone().unwrap_or_default();
    let mut account_partner = partner.clone();
    let mut new_token = String::new();
    if account.access_token.is_empty()
        && (!account.username.is_empty() || !account.password.is_empty())
    {
        if account.username.is_empty() || account.password.is_empty() {
            return Err("PopkonTV 账号与密码需要完整填写".into());
        }
        let authorization = public_identifier(
            c,
            "https://www.popkontv.com",
            r#"(Basic [A-Za-z0-9+/=]{32,128})"#,
        )
        .await?;
        let response=c.send_request(c.request(reqwest::Method::POST,"https://www.popkontv.com/api/proxy/member/v1/login")?.header("Authorization",authorization).json(&json!({"partnerCode":partner,"signId":account.username,"signPwd":account.password}))).await?;
        let result = json_body(&http::body(response).await?)?;
        if result["statusCd"] != "S2000" {
            return Err("PopkonTV 登录未通过，请检查账号或在平台完成验证".into());
        }
        new_token = s(&result, "/data/token");
        if new_token.is_empty() {
            return Err("PopkonTV 登录未返回访问令牌".into());
        }
        account.access_token = new_token.clone();
        let code = s(&result, "/data/partnerCode");
        if !code.is_empty() {
            account_partner = code;
        }
    }
    let payload = json!({"androidStore":0,"castCode":format!("{}-{}",s(data,"/mc_signId"),s(data,"/mc_castStartDate")),"castPartnerCode":partner,"castSignId":s(data,"/mc_signId"),"castType":data["castType"],"commandType":0,"exePath":5,"isSecret":private,"partnerCode":account_partner,"password":password,"signId":account.username,"version":"4.6.2"});
    let mut request = c
        .request(
            reqwest::Method::POST,
            "https://www.popkontv.com/api/proxy/broadcast/v1/castwatchonoffguest",
        )?
        .header("clientKey", client)
        .json(&payload);
    if !account.access_token.is_empty() {
        request = request.bearer_auth(&account.access_token);
    }
    let response = c.send_request(request).await?;
    let result = json_body(&http::body(response).await?)?;
    if !matches!(result["statusCd"].as_str(), Some("L0000" | "L0001")) {
        return Err("PopkonTV 要求有效登录或账号验证，请在平台完成验证后配置 accessToken".into());
    }
    let mut info = c.result(
        name,
        true,
        String::new(),
        s(&result, "/data/castHlsUrl"),
        String::new(),
    )?;
    info.new_token = new_token;
    Ok(info)
}
async fn migu(c: &Context<'_>) -> Result<StreamInfo, String> {
    let id = c.id()?;
    let v = c
        .get(&format!(
            "https://vms-sc.miguvideo.com/vms-match/v6/staticcache/basic/basic-data/{}/miguvideo",
            escaped(&id)
        ))
        .await?;
    let name = s(&v, "/body/title");
    let content = s(&v, "/body/pId");
    if content.is_empty() {
        return c.result(name, false, String::new(), String::new(), String::new());
    }
    let play = c
        .get(&query_url(
            "https://webapi.miguvideo.com/gateway/playurl/v3/play/playurl",
            &[
                ("contId", content),
                (
                    "rateType",
                    ["3", "3", "2", "1", "1"][quality_index(c.request.quality.as_deref())?].into(),
                ),
                ("clientId", uuid::Uuid::new_v4().to_string()),
                (
                    "timestamp",
                    chrono::Utc::now().timestamp_millis().to_string(),
                ),
                ("flvEnable", "true".into()),
                ("xh265", "false".into()),
                ("chip", "mgwww".into()),
                ("channelId", String::new()),
            ],
        )?)
        .await?;
    let on = live(&play, "/body/content/currentLive", &["1"], &["0"])?;
    if !on {
        return c.result(name, false, String::new(), String::new(), String::new());
    }
    let url = s(&play, "/body/urlInfo/url");
    let settings = c
        .get("https://app-sc.miguvideo.com/common/v1/settings/H5_DetailPage")
        .await?;
    let parameters = json_body(&s(&settings, "/body/paramValue"))?;
    let version = s(&parameters, "/playerVersion");
    if version.len() > 80
        || version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("咪咕播放器版本无效".into());
    }
    let address = format!("https://www.miguvideo.com/mgs/player/prd/{version}/dist/mgprtcl.wasm");
    let mut response = c
        .send_request(c.request(reqwest::Method::GET, &address)?)
        .await?
        .error_for_status()
        .map_err(http::transport_error)?;
    let mut module = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(http::transport_error)? {
        if module.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err("咪咕签名模块过大".into());
        }
        module.extend_from_slice(&chunk);
    }
    let source = url.clone();
    let signature = tokio::task::spawn_blocking(move || super::migu_wasm::sign(&module, &source))
        .await
        .map_err(|_| "咪咕签名任务异常")??;
    let mut url = Url::parse(&url).map_err(|_| "咪咕播放地址无效")?;
    url.query_pairs_mut()
        .append_pair("ddCalcu", &signature)
        .append_pair("sv", "10010");
    let hls = url.path().contains(".m3u8");
    c.result(
        name,
        true,
        s(&v, "/body/detailPageTitle"),
        if hls { url.to_string() } else { String::new() },
        if hls { String::new() } else { url.to_string() },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_or_missing_live_state_is_not_offline() {
        assert!(live(&json!({}), "/status", &["1"], &["0"]).is_err());
        assert!(live(&json!({"status":5}), "/status", &["1"], &["0"]).is_err());
        assert!(!live(&json!({"status":0}), "/status", &["1"], &["0"]).unwrap());
    }
}

#[cfg(test)]
#[path = "extended_tests.rs"]
mod fixture_tests;
