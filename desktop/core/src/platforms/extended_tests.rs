use super::*;
use std::{collections::VecDeque, sync::Mutex};
pub(super) struct Reply {
    pub url: &'static str,
    pub body: Vec<u8>,
    pub status: u16,
    pub cookie: Option<&'static str>,
}
impl Reply {
    fn json(url: &'static str, body: Value) -> Self {
        Self {
            url,
            body: serde_json::to_vec(&body).unwrap(),
            status: 200,
            cookie: None,
        }
    }
    fn text(url: &'static str, body: &str) -> Self {
        Self {
            url,
            body: body.as_bytes().to_vec(),
            status: 200,
            cookie: None,
        }
    }
}
pub(super) fn respond(
    replies: &Mutex<VecDeque<Reply>>,
    jar: &reqwest::cookie::Jar,
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, String> {
    let request = request.build().map_err(|_| "fixture request invalid")?;
    let reply = replies.lock().unwrap().pop_front().ok_or_else(|| {
        format!(
            "unexpected fixture request: {} {}",
            request.method(),
            request.url()
        )
    })?;
    assert!(
        request.url().as_str().contains(reply.url),
        "expected {} but requested {}",
        reply.url,
        request.url()
    );
    let mut response = axum::http::Response::builder().status(reply.status);
    if let Some(cookie) = reply.cookie {
        jar.add_cookie_str(cookie, request.url());
        response = response.header("set-cookie", cookie);
    }
    Ok(reqwest::Response::from(response.body(reply.body).unwrap()))
}
fn request(platform: &str, url: &str) -> ResolveRequest {
    ResolveRequest {
        account: None,
        url: url.into(),
        quality: Some("OD".into()),
        proxy: None,
        cookie: None,
        platform: Some(platform.into()),
    }
}
async fn fixture(platform: &str, url: &str, replies: Vec<Reply>) -> Result<StreamInfo, String> {
    fixture_request(request(platform, url), replies).await
}
async fn fixture_request(
    request: ResolveRequest,
    replies: Vec<Reply>,
) -> Result<StreamInfo, String> {
    let platform = request.platform.as_deref().unwrap();
    let mut c = Context::new(&request, catalog::by_key(platform).unwrap()).unwrap();
    c.test_responses = Some(Mutex::new(replies.into()));
    prepare_short_link(&mut c).await?;
    let result = resolve_context(&c).await;
    assert!(
        c.test_responses
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .is_empty(),
        "unused responses for {platform}"
    );
    eprintln!(
        "[platform-fixture] {platform} {}",
        match &result {
            Ok(info) if info.is_live => "live",
            Ok(_) => "offline",
            Err(_) => "rejected",
        }
    );
    result
}
#[tokio::test]
async fn bigo_live_fixture() {
    let info=fixture("bigo","https://www.bigo.tv/123",vec![Reply::json("getInternalStudioInfo",json!({"data":{"alive":1,"nick_name":"Fixture","hls_src":"https://media.example.test/live.m3u8"}}))]).await.unwrap();
    assert!(info.is_live);
    assert_eq!(info.anchor_name, "Fixture");
}

struct SimpleCase {
    key: &'static str,
    url: &'static str,
    endpoint: &'static str,
    body: Value,
    status: &'static str,
    offline: Value,
}
fn simple_cases() -> Vec<SimpleCase> {
    vec![
        SimpleCase {
            key: "bigo",
            url: "https://www.bigo.tv/123",
            endpoint: "getInternalStudioInfo",
            body: json!({"data":{"alive":1,"nick_name":"Fixture","hls_src":"https://media.example.test/live.m3u8"}}),
            status: "/data/alive",
            offline: json!(0),
        },
        SimpleCase {
            key: "inke",
            url: "https://www.inke.cn/123?uid=123&id=456",
            endpoint: "live_share_pc",
            body: json!({"data":{"status":1,"media_info":{"nick":"Fixture"},"live_addr":[{"hls_stream_addr":"https://media.example.test/live.m3u8","stream_addr":"https://media.example.test/live.flv"}]}}),
            status: "/data/status",
            offline: json!(0),
        },
        SimpleCase {
            key: "langlive",
            url: "https://www.lang.live/123",
            endpoint: "room/liveinfo",
            body: json!({"data":{"live_info":{"live_status":1,"nickname":"Fixture","liveurl_hls":"https://media.example.test/live.m3u8"}}}),
            status: "/data/live_info/live_status",
            offline: json!(0),
        },
        SimpleCase {
            key: "lianjie",
            url: "https://www.lailianjie.com/123",
            endpoint: "live/getRoomInfo",
            body: json!({"data":{"isonline":1,"nickname":"Fixture","videoUrl":"https://media.example.test/live.flv"}}),
            status: "/data/isonline",
            offline: json!(0),
        },
        SimpleCase {
            key: "maoerfm",
            url: "https://fm.missevan.com/123",
            endpoint: "api/v2/live/123",
            body: json!({"info":{"creator":{"username":"Fixture"},"room":{"status":{"broadcasting":true},"channel":{"hls_pull_url":"https://media.example.test/live.m3u8"}}}}),
            status: "/info/room/status/broadcasting",
            offline: json!(false),
        },
        SimpleCase {
            key: "picarto",
            url: "https://picarto.tv/Fixture",
            endpoint: "channel/detail/Fixture",
            body: json!({"channel":{"online":true,"name":"Fixture"}}),
            status: "/channel/online",
            offline: json!(false),
        },
        SimpleCase {
            key: "chzzk",
            url: "https://chzzk.naver.com/live/123",
            endpoint: "channels/123/live-detail",
            body: json!({"content":{"status":"OPEN","channel":{"channelName":"Fixture"},"livePlaybackJson":json!({"media":[{"path":"https://media.example.test/live.m3u8"}]}).to_string()}}),
            status: "/content/status",
            offline: json!("CLOSE"),
        },
        SimpleCase {
            key: "piaopiao",
            url: "https://www.weimipopo.com/live?anchorUid=123",
            endpoint: "live/preview",
            body: json!({"data":{"living":true,"name":"Fixture","pullUrl":"https://media.example.test/live.m3u8"}}),
            status: "/data/living",
            offline: json!(false),
        },
        SimpleCase {
            key: "huamao",
            url: "https://www.catshow168.com/live?anchorUid=123",
            endpoint: "live/preview",
            body: json!({"data":{"living":true,"name":"Fixture","pullUrl":"https://media.example.test/live.m3u8"}}),
            status: "/data/living",
            offline: json!(false),
        },
        SimpleCase {
            key: "weibo",
            url: "https://weibo.com/l/wblive/p/show/123",
            endpoint: "anchor/live",
            body: json!({"data":{"item":{"status":1,"stream_info":{"pull":{"live_origin_flv_url":"https://media.example.test/live.flv"}}},"user_info":{"name":"Fixture"}}}),
            status: "/data/item/status",
            offline: json!(0),
        },
        SimpleCase {
            key: "laixiu",
            url: "https://www.imkktv.com/live?roomId=123",
            endpoint: "getShareLiveVideo",
            body: json!({"data":{"playStatus":0,"nickname":"Fixture","playUrl":"https://media.example.test/live.flv"}}),
            status: "/data/playStatus",
            offline: json!(1),
        },
    ]
}
#[tokio::test]
async fn explicit_api_platforms_distinguish_live_offline_and_missing_state() {
    for case in simple_cases() {
        for mode in 0..3 {
            let mut body = case.body.clone();
            if mode == 1 {
                *body.pointer_mut(case.status).unwrap() = case.offline.clone();
            } else if mode == 2 {
                *body.pointer_mut(case.status).unwrap() = Value::Null;
            }
            let info = fixture(case.key, case.url, vec![Reply::json(case.endpoint, body)]).await;
            if mode == 2 {
                assert!(info.is_err(), "{} missing state was accepted", case.key);
                continue;
            }
            let info = info.unwrap_or_else(|error| panic!("{} mode={mode}: {error}", case.key));
            assert_eq!(info.anchor_name, "Fixture", "{}", case.key);
            assert_eq!(info.is_live, mode == 0);
            if info.is_live {
                assert!(!info.record_url.is_empty(), "{}", case.key);
            } else {
                assert!(
                    info.record_url.is_empty()
                        && info.m3u8_url.is_empty()
                        && info.flv_url.is_empty()
                );
            }
        }
    }
}
#[test]
fn platform_api_host_policy_cannot_be_bypassed_by_lookalikes() {
    for platform in catalog::PLATFORMS {
        for domain in platform.domains {
            assert!(allowed_host(platform, domain));
            assert!(!allowed_host(platform, &format!("{domain}.example.test")));
        }
    }
}

fn page(marker: &str, body: &Value) -> String {
    format!("<script>{marker}{body}</script>")
}
#[tokio::test]
async fn embedded_state_platforms_have_live_and_offline_contracts() {
    for on in [true, false] {
        let stream = "https://media.example.test/live.flv";
        for key in ["qiandurebo", "xindongrebo"] {
            let url = format!("https://{}/123", catalog::by_key(key).unwrap().domains[0]);
            let mut html = page(
                "var user = ",
                &json!({"zb_nickname":"Fixture","play_url":if on{stream}else{""}}),
            );
            if !on {
                html.push_str("common-text-center\" style=\"display:block");
            }
            let info = fixture(key, &url, vec![Reply::text("/123", &html)])
                .await
                .unwrap();
            assert_eq!(info.is_live, on);
            assert_eq!(info.anchor_name, "Fixture");
        }
        let payload = json!({"userInfo":{"onLive":on,"name":"Fixture"},"liveInfo":{"liveUrl":"https://media.example.test/live.m3u8"}});
        let html = format!(
            "decodeURIComponent(\"{}\")),window.Promise",
            escaped(&payload.to_string())
        );
        assert_eq!(
            fixture(
                "blued",
                "https://www.blued.cn/123",
                vec![Reply::text("/123", &html)]
            )
            .await
            .unwrap()
            .is_live,
            on
        );
        let payload = json!({"props":{"pageProps":{"roomInfoInitData":{"live":{"status":if on{1}else{0},"nickname":"Fixture","quickplay":{"resolution":{"blueray":{"cdn":{"a":stream}}}}}}}}});
        let html = format!("<script id=\"__NEXT_DATA__\">{payload}</script>");
        assert_eq!(
            fixture(
                "netease",
                "https://cc.163.com/123",
                vec![Reply::text("/123", &html)]
            )
            .await
            .unwrap()
            .is_live,
            on
        );
        let payload = json!({"initialState":{"theater":{"theaters":{"123":{"drama":{"status":if on{1}else{2},"playInfo":{"playUrl":stream}},"actor":{"name":"Fixture"}}}}}});
        let html = format!("<script id=\"js-initialData\">{payload}</script>");
        assert_eq!(
            fixture(
                "zhihu",
                "https://www.zhihu.com/theater/123",
                vec![Reply::text("/123", &html)]
            )
            .await
            .unwrap()
            .is_live,
            on
        );
        let payload = json!({"videoDetails":{"author":"Fixture","isLive":on},"streamingData":{"hlsManifestUrl":"https://media.example.test/live.m3u8"}});
        assert_eq!(
            fixture(
                "youtube",
                "https://www.youtube.com/watch?v=123",
                vec![Reply::text(
                    "/watch",
                    &page("var ytInitialPlayerResponse = ", &payload)
                )]
            )
            .await
            .unwrap()
            .is_live,
            on
        );
        let payload = json!({"LiveRoom":{"liveRoomUserInfo":{"user":{"status":if on{2}else{0},"nickname":"Fixture"},"liveRoom":{"streamData":{"pull_data":{"stream_data":json!({"data":{"hd":{"main":{"sdk_params":json!({"vbitrate":1000}).to_string(),"flv":stream}}}}).to_string()}}}}}});
        let html = format!("<script id=\"SIGI_STATE\">{payload}</script>");
        assert_eq!(
            fixture(
                "tiktok",
                "https://www.tiktok.com/@fixture/live",
                vec![Reply::text("/@fixture/live", &html)]
            )
            .await
            .unwrap()
            .is_live,
            on
        );
        let deeplink = query_url(
            "xhsdiscover://live",
            &[
                ("host_nickname", "Fixture".into()),
                ("flvUrl", if on { stream.into() } else { String::new() }),
            ],
        )
        .unwrap();
        let payload = json!({"liveStream":{"liveStatus":"success","roomData":{"roomInfo":{"deeplink":deeplink,"roomTitle":if on{"Fixture live"}else{"Fixture 回放"}}}}});
        assert_eq!(
            fixture(
                "rednote",
                "https://www.xiaohongshu.com/live/123",
                vec![Reply::text(
                    "/123",
                    &page("window.__INITIAL_STATE__ = ", &payload)
                )]
            )
            .await
            .unwrap()
            .is_live,
            on
        );
    }
}

#[tokio::test]
async fn multi_request_platforms_keep_identity_and_state() {
    for on in [true, false] {
        let stream = "https://media.example.test/live.flv";
        let hls = "https://media.example.test/live.m3u8";
        let mut replies = vec![
            Reply::json(
                "room_init",
                json!({"data":{"live_status":if on{1}else{0},"room_id":123,"uid":456}}),
            ),
            Reply::json("Master/info", json!({"data":{"info":{"uname":"Fixture"}}})),
        ];
        if on {
            replies.extend([
                Reply::json(
                    "getH5InfoByRoom",
                    json!({"data":{"room_info":{"title":"Fixture live"}}}),
                ),
                Reply::json("Room/playUrl", json!({"data":{"durl":[{"url":stream}]}})),
            ]);
        }
        let info = fixture("bilibili", "https://live.bilibili.com/123", replies)
            .await
            .unwrap();
        assert_eq!(info.is_live, on);
        assert_eq!(info.anchor_name, "Fixture");
        let info=fixture("17live","https://17.live/live/123",vec![Reply::json("user/room/123",json!({"displayName":"Fixture"})),Reply::json("viewers/alive",json!({"status":if on{2}else{0},"pullURLsInfo":{"rtmpURLs":[{"urlHighQuality":stream}]}}))]).await.unwrap();
        assert_eq!(info.is_live, on);
        let mut replies = vec![Reply::json(
            "live/live_info",
            json!({"room_name":"Fixture","live_status":if on{2}else{0}}),
        )];
        if on {
            replies.push(Reply::json(
                "live/streaming_url",
                json!({"streaming_url_list":[{"type":"hls","url":hls}]}),
            ));
        }
        assert_eq!(
            fixture(
                "showroom",
                "https://www.showroom-live.com/r/fixture?room_id=123",
                replies
            )
            .await
            .unwrap()
            .is_live,
            on
        );
        assert_eq!(fixture("sixroom","https://v.6.cn/123",vec![Reply::text("v.6.cn/123","rid: '456'"),Reply::json("coop/mobile/index.php",json!({"content":{"roominfo":{"alias":"Fixture"},"liveinfo":{"flvtitle":if on{"fixture-stream"}else{""}}}}))]).await.unwrap().is_live,on);
        let mut replies = vec![Reply::json(
            "getEnterRoomInfo",
            json!({"data":{"liveType":if on{0}else{-1},"normalRoomInfo":{"nickName":"Fixture"}}}),
        )];
        if on {
            replies.push(Reply::json(
                "mutiline/streamaddr",
                json!({"data":{"lines":[{"streamProfiles":[{"httpsFlv":[stream]}]}]}}),
            ));
        }
        assert_eq!(
            fixture("kugou", "https://fanxing.kugou.com/123", replies)
                .await
                .unwrap()
                .is_live,
            on
        );
        for key in ["yinbo", "changliao"] {
            let url = format!("https://{}/123", catalog::by_key(key).unwrap().domains[0]);
            let mut replies = vec![Reply::json(
                "live.ashx",
                json!({"data":{"roomInfo":{"live_stat":if on{1}else{0},"nickname":"Fixture","liveID":"fixture"}}}),
            )];
            if on {
                replies.push(Reply::text("/123",&page("var config = ",&json!({"domainpullstream_hls":"https://media.example.test/hls","domainpullstream_flv":"https://media.example.test/flv"}))));
            }
            assert_eq!(fixture(key, &url, replies).await.unwrap().is_live, on);
        }
        let mut replies = vec![Reply::json(
            "getRoomData.do",
            json!({"status":if on{100}else{0},"nickName":"Fixture","videoUrl":if on{hls}else{""}}),
        )];
        if !on {
            replies.push(Reply::json(
                "captain/banner",
                json!({"data":{"anchorName":"Fixture"}}),
            ));
        }
        assert_eq!(
            fixture(
                "vvxq",
                "https://h5webcdn-pro.vvxqiu.com/live?roomId=123",
                replies
            )
            .await
            .unwrap()
            .is_live,
            on
        );
        assert_eq!(fixture("baidu","https://live.baidu.com/room?room_id=123",vec![Reply::json("mbd.baidu.com/searchbox",json!({"data":{"123":{"status":if on{0}else{1},"host":{"name":"Fixture"},"video":{"url_clarity_list":[{"urls":{"flv":stream}}]}}}}))]).await.unwrap().is_live,on);
        assert_eq!(fixture("huya","https://www.huya.com/123",vec![Reply::json("mp.huya.com/cache.php",json!({"data":{"realLiveStatus":if on{"ON"}else{"OFF"},"profileInfo":{"nick":"Fixture"},"stream":{"baseSteamInfoList":[{"sCdnType":"TX","sStreamName":"fixture","sFlvUrl":"https://media.example.test","sFlvAntiCode":"ratio=264_1000","sHlsUrl":"https://media.example.test","sHlsAntiCode":"fixture=1"}]}}}))]).await.unwrap().is_live,on);
        let mut replies = vec![Reply::json(
            "douyu.com/betard/123",
            json!({"room":{"show_status":if on{1}else{2},"nickname":"Fixture"}}),
        )];
        if on {
            replies.extend([Reply::json("getEncryption",json!({"error":0,"data":{"enc_time":2,"key":"fixture","rand_str":"fixture","is_special":false,"enc_data":"fixture"}})),Reply::json("getH5PlayV1/123",json!({"error":0,"data":{"rtmp_url":"https://media.example.test","rtmp_live":"fixture.flv"}}))]);
        }
        assert_eq!(
            fixture("douyu", "https://www.douyu.com/123", replies)
                .await
                .unwrap()
                .is_live,
            on
        );
        for key in ["pandatv", "winktv"] {
            let url = format!(
                "https://{}/fixture",
                catalog::by_key(key).unwrap().domains[0]
            );
            let mut replies = vec![Reply::json(
                "v1/member/bj",
                json!({"bjInfo":{"nick":"Fixture"},"media":if on{json!({"code":"fixture"})}else{Value::Null}}),
            )];
            if on {
                replies.push(Reply::json(
                    "v1/live/play",
                    json!({"PlayList":{"hls":[{"url":hls}]}}),
                ));
            }
            assert_eq!(fixture(key, &url, replies).await.unwrap().is_live, on);
        }
    }
}

#[tokio::test]
async fn signed_and_authenticated_platforms_use_native_protocols() {
    let hls = "https://media.example.test/live.m3u8";
    let flv = "https://media.example.test/live.flv";
    for on in [true, false] {
        let mut replies = vec![Reply::json(
            "talent_head_findTalentMsg",
            json!({"result":{"talentName":"Fixture","livingRoomJump":{"params":{"id":if on{"123"}else{""}}}}}),
        )];
        if on {
            replies.push(Reply::json(
                "client.action",
                json!({"data":{"status":1,"h5VideoUrl":hls}}),
            ));
        }
        assert_eq!(
            fixture("jd", "https://www.jd.com/live?authorId=456", replies)
                .await
                .unwrap()
                .is_live,
            on
        );
        let mut replies = vec![Reply::json(
            "user/userInfo",
            json!({"profile":{"name":"Fixture","liveId":if on{json!("123")}else{Value::Null}}}),
        )];
        if on {
            replies.extend([Reply::json("visitor/login",json!({"userId":456,"acfun.api.visitor_st":"fixture-token"})),Reply::json("web/startPlay",json!({"data":{"caption":"Fixture title","videoPlayRes":json!({"liveAdaptiveManifest":[{"adaptationSet":{"representation":[{"bitrate":1000,"url":flv}]}}]}).to_string()}}))]);
        }
        assert_eq!(
            fixture("acfun", "https://live.acfun.cn/live/123", replies)
                .await
                .unwrap()
                .is_live,
            on
        );
        assert_eq!(fixture("look","https://look.163.com/live?id=123",vec![Reply::json("livestream/room/get/v3",json!({"data":{"liveStatus":if on{1}else{0},"anchor":{"nickName":"Fixture"},"roomInfo":{"liveUrl":{"hlsPullUrl":hls}}}}))]).await.unwrap().is_live,on);
        for key in ["haixiu", "lehai"] {
            let url = format!("https://{}/123", catalog::by_key(key).unwrap().domains[0]);
            let replies = vec![
                Reply::text("/123", "accessToken: 'fixture-token'"),
                Reply::json(
                    "advanceInfoRoom",
                    json!({"data":{"live_status":if on{1}else{0},"nickname":"Fixture","media_url_web":flv}}),
                ),
            ];
            assert_eq!(fixture(key, &url, replies).await.unwrap().is_live, on);
        }
        assert_eq!(fixture("liveme","https://www.liveme.com/live/123/index.html",vec![Reply::json("queryinfosimple",json!({"data":{"video_info":{"status":if on{0}else{1},"uname":"Fixture","videosource":flv}}}))]).await.unwrap().is_live,on);
        assert_eq!(fixture("huajiao","https://www.huajiao.com/user?author=123",vec![Reply::json("getUserFeeds",json!({"data":{"feeds":[{"author":{"nickname":"Fixture"},"feed":{"rtop":if on{"直播中"}else{"回放"},"pull_url":flv}}]}}))]).await.unwrap().is_live,on);
        assert_eq!(fixture("yy","https://www.yy.com/123",vec![Reply::text("yy.com/123","nick: \"Fixture\", sid: \"123\""),Reply::json("channel/streams",json!({"avp_info_res":{"stream_line_addr":if on{json!({"line1":{"cdn_info":{"url":flv}}})}else{json!({})}}})),Reply::json("live/detail",json!({"data":{"roomName":"Fixture"}}))]).await.unwrap().is_live,on);
        let mut replies = vec![
            Reply::json(
                "get_station_status.php",
                json!({"DATA":{"user_nick":"Fixture"}}),
            ),
            Reply::json(
                "player_live_api.php",
                json!({"CHANNEL":{"RESULT":1,"VIEWPRESET":if on{"fixture"}else{""},"BNO":"123"}}),
            ),
        ];
        if on {
            replies.extend([
                Reply::json("broad_stream_assign.html", json!({"view_url":hls})),
                Reply::json(
                    "player_live_api.php",
                    json!({"CHANNEL":{"AID":"fixture-token"}}),
                ),
            ]);
        }
        assert_eq!(
            fixture("soop", "https://play.sooplive.com/fixture", replies)
                .await
                .unwrap()
                .is_live,
            on
        );
        assert_eq!(fixture("taobao","https://live.taobao.com/live?id=123",vec![Reply::json("livedetail/4.0",json!({"ret":["SUCCESS::成功"],"data":{"streamStatus":if on{"1"}else{"0"},"broadCaster":{"accountName":"Fixture"},"liveUrlList":[{"definition":"ud","hlsUrl":hls}]}}))]).await.unwrap().is_live,on);
        let html =
            format!("data-is-onlive=\"{on}\" <meta name=\"twitter:title\" content=\"Fixture\">");
        let mut replies = vec![Reply::text("twitcasting.tv/fixture", &html)];
        if on {
            replies.push(Reply::json(
                "streamserver.php",
                json!({"tc-hls":{"streams":{"high":hls}}}),
            ));
        }
        assert_eq!(
            fixture("twitcasting", "https://twitcasting.tv/fixture", replies)
                .await
                .unwrap()
                .is_live,
            on
        );
    }
    let source=fixture("shopee","https://live.shopee.tw/share?session=123",vec![Reply::json("play_param/session",json!({"data":{"play_param_list":[{"session":{"nickname":"Fixture","username":"fixture"},"play_param":{"play_url_list":[flv]}}]}}))]).await.unwrap();
    assert!(source.is_live);
    let payload = json!({"props":{"pageProps":{"channelStream":{"channel":{"owner":{"nickname":"Fixture"}}}}}});
    let html = format!("<script id=\"__NEXT_DATA__\">{payload}</script>");
    assert!(
        fixture(
            "flextv",
            "https://www.ttinglive.com/channels/fixture/live",
            vec![
                Reply::text("channels/fixture/live", &html),
                Reply::json(
                    "api/channels/fixture/stream",
                    json!({"sources":[{"url":hls}]})
                )
            ]
        )
        .await
        .unwrap()
        .is_live
    );
}

fn twitch_replies(on: bool) -> Vec<Reply> {
    let mut replies = vec![
        Reply::text(
            "www.twitch.tv",
            &format!("clientId: \"{}\"", "a".repeat(30)),
        ),
        Reply::json(
            "gql.twitch.tv/gql",
            json!({"data":{"user":{"displayName":"Fixture","stream":if on{json!({"id":"123"})}else{Value::Null}},"streamPlaybackAccessToken":{"signature":"fixture-signature","value":"fixture-playback-token","authorization":{"isForbidden":false}}}}),
        ),
    ];
    if on {
        replies.push(Reply::text("usher.ttvnw.net", "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=2000,RESOLUTION=1920x1080\nhttps://media.example.test/1080.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=1000,RESOLUTION=1280x720\nhttps://media.example.test/720.m3u8\n"));
    }
    replies
}
#[tokio::test]
async fn twitch_faceit_and_popkon_identity_auth_contracts() {
    for on in [true, false] {
        let info = fixture(
            "twitch",
            "https://www.twitch.tv/fixture",
            twitch_replies(on),
        )
        .await
        .unwrap();
        assert_eq!(info.is_live, on);
        if on {
            assert!(info.m3u8_url.ends_with("1080.m3u8"));
        }
        let mut replies = vec![
            Reply::json(
                "nicknames/fixture",
                json!({"payload":{"id":"fixture-user"}}),
            ),
            Reply::json(
                "streamings",
                json!({"payload":if on{json!([{"platform":"twitch","platformId":"fixture"}])}else{json!([])}}),
            ),
        ];
        if on {
            replies.extend(twitch_replies(true));
        }
        assert_eq!(
            fixture(
                "faceit",
                "https://www.faceit.com/en/players/fixture",
                replies
            )
            .await
            .unwrap()
            .is_live,
            on
        );
    }
    let offline = json!({"props":{"pageProps":{}}});
    let html = format!("<script id=\"__NEXT_DATA__\">{offline}</script>");
    assert!(
        !fixture(
            "popkontv",
            "https://www.popkontv.com/live/view?castId=fixture",
            vec![
                Reply::text("live/view", &html),
                Reply::text("channel/notices", "\"mcNickName\":\"Fixture\"")
            ]
        )
        .await
        .unwrap()
        .is_live
    );
    let active = json!({"props":{"pageProps":{"mcData":{"data":{"mc_nickName":"Fixture","mc_isPrivate":0,"mc_signId":"fixture","mc_castStartDate":"123","castType":1}}}}});
    let html = format!("<script id=\"__NEXT_DATA__\">{active}</script>");
    let mut req = request(
        "popkontv",
        "https://www.popkontv.com/live/view?castId=fixture",
    );
    req.account = Some(crate::config::PlatformAccount {
        username: "fixture-user".into(),
        password: "fixture-password".into(),
        ..Default::default()
    });
    let result=fixture_request(req,vec![Reply::text("live/view",&html),Reply::text("www.popkontv.com",&format!("Client {}","A".repeat(40))),Reply::text("www.popkontv.com",&format!("Basic {}","B".repeat(40))),Reply::json("member/v1/login",json!({"statusCd":"S2000","data":{"token":"fixture-access-token","partnerCode":"P-00001"}})),Reply::json("castwatchonoffguest",json!({"statusCd":"L0000","data":{"castHlsUrl":"https://media.example.test/live.m3u8"}}))]).await.unwrap();
    assert!(result.is_live);
    assert_eq!(result.new_token, "fixture-access-token");
}
fn protocol_wasm() -> Vec<u8> {
    wat::parse_str(r#"(module
    (memory (export "d") 1 2)
    (global $heap (mut i32) (i32.const 1024))
    (func (export "u") (param $size i32) (result i32) (local $old i32) global.get $heap local.set $old global.get $heap local.get $size i32.add global.set $heap local.get $old)
    (func (export "m") (result i32) i32.const 0)
    (func $accept (param i32 i32 i32) (result i32) i32.const 0)
    (export "h" (func $accept)) (export "q" (func $accept)) (export "p" (func $accept)) (export "j" (func $accept)) (export "r" (func $accept)) (export "o" (func $accept)) (export "i" (func $accept)) (export "n" (func $accept))
    (func (export "t") (param i32 i32 i32 i32 i32) (result i32) i32.const 0)
    (func (export "k") (param i32 i32 i32) (result i32) local.get 1 i32.const 6513249 i32.store i32.const 0)
)"#).unwrap()
}
#[tokio::test]
async fn migu_protocol_module_is_bounded_and_used_in_process() {
    assert!(
        !fixture(
            "migu",
            "https://www.miguvideo.com/match/123",
            vec![Reply::json(
                "basic-data/123/miguvideo",
                json!({"body":{"title":"Fixture","pId":""}})
            )]
        )
        .await
        .unwrap()
        .is_live
    );
    let module = protocol_wasm();
    assert_eq!(
        super::super::migu_wasm::sign(&module, "https://media.example.test/live.m3u8").unwrap(),
        "abc"
    );
    let info=fixture("migu","https://www.miguvideo.com/match/123",vec![Reply::json("basic-data/123/miguvideo",json!({"body":{"title":"Fixture","pId":"456"}})),Reply::json("v3/play/playurl",json!({"body":{"content":{"currentLive":1},"urlInfo":{"url":"https://media.example.test/live.m3u8?userid=fixture"}}})),Reply::json("settings/H5_DetailPage",json!({"body":{"paramValue":json!({"playerVersion":"fixture"}).to_string()}})),Reply{url:"mgprtcl.wasm",body:module,status:200,cookie:None}]).await.unwrap();
    assert!(info.is_live);
    assert!(info.m3u8_url.contains("ddCalcu=abc"));
    let forbidden = wat::parse_str(
        r#"(module (import "wasi_snapshot_preview1" "proc_exit" (func (param i32))))"#,
    )
    .unwrap();
    assert!(
        super::super::migu_wasm::sign(&forbidden, "https://media.example.test")
            .unwrap_err()
            .contains("不允许")
    );
}
