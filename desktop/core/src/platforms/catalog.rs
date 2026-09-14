//! Single platform identity registry shared by detection and native resolver dispatch.
#[derive(Clone, Copy, serde::Serialize)]
pub struct Platform {
    pub key: &'static str,
    pub name: &'static str,
    pub domains: &'static [&'static str],
}
pub const PLATFORMS: &[Platform] = &[
    Platform {
        key: "douyin",
        name: "抖音直播",
        domains: &["douyin.com"],
    },
    Platform {
        key: "kuaishou",
        name: "快手直播",
        domains: &["kuaishou.com"],
    },
    Platform {
        key: "bilibili",
        name: "哔哩哔哩",
        domains: &["bilibili.com"],
    },
    Platform {
        key: "huya",
        name: "虎牙直播",
        domains: &["huya.com"],
    },
    Platform {
        key: "douyu",
        name: "斗鱼直播",
        domains: &["douyu.com"],
    },
    Platform {
        key: "tiktok",
        name: "TikTok",
        domains: &["tiktok.com"],
    },
    Platform {
        key: "yy",
        name: "YY直播",
        domains: &["yy.com"],
    },
    Platform {
        key: "rednote",
        name: "小红书",
        domains: &["xiaohongshu.com", "xhslink.com"],
    },
    Platform {
        key: "bigo",
        name: "Bigo",
        domains: &["bigo.tv", "bigo.sg"],
    },
    Platform {
        key: "blued",
        name: "Blued",
        domains: &["blued.cn"],
    },
    Platform {
        key: "soop",
        name: "SOOP",
        domains: &["sooplive.co.kr", "sooplive.com", "afreecatv.com"],
    },
    Platform {
        key: "netease",
        name: "网易CC",
        domains: &["cc.163.com"],
    },
    Platform {
        key: "qiandurebo",
        name: "千度热播",
        domains: &["qiandurebo.com"],
    },
    Platform {
        key: "pandatv",
        name: "PandaTV",
        domains: &["pandalive.co.kr"],
    },
    Platform {
        key: "maoerfm",
        name: "猫耳FM",
        domains: &["missevan.com"],
    },
    Platform {
        key: "look",
        name: "LOOK",
        domains: &["look.163.com"],
    },
    Platform {
        key: "winktv",
        name: "WinkTV",
        domains: &["winktv.co.kr"],
    },
    Platform {
        key: "flextv",
        name: "FlexTV",
        domains: &["flextv.co.kr", "ttinglive.com"],
    },
    Platform {
        key: "popkontv",
        name: "PopkonTV",
        domains: &["popkontv.com"],
    },
    Platform {
        key: "twitcasting",
        name: "TwitCasting",
        domains: &["twitcasting.tv"],
    },
    Platform {
        key: "baidu",
        name: "百度直播",
        domains: &["live.baidu.com"],
    },
    Platform {
        key: "weibo",
        name: "微博直播",
        domains: &["weibo.com"],
    },
    Platform {
        key: "kugou",
        name: "酷狗直播",
        domains: &["kugou.com"],
    },
    Platform {
        key: "twitch",
        name: "Twitch",
        domains: &["twitch.tv"],
    },
    Platform {
        key: "liveme",
        name: "LiveMe",
        domains: &["liveme.com"],
    },
    Platform {
        key: "huajiao",
        name: "花椒直播",
        domains: &["huajiao.com"],
    },
    Platform {
        key: "showroom",
        name: "SHOWROOM",
        domains: &["showroom-live.com"],
    },
    Platform {
        key: "acfun",
        name: "AcFun",
        domains: &["acfun.cn"],
    },
    Platform {
        key: "inke",
        name: "映客直播",
        domains: &["inke.cn"],
    },
    Platform {
        key: "yinbo",
        name: "音播直播",
        domains: &["ybw1666.com"],
    },
    Platform {
        key: "changliao",
        name: "畅聊直播",
        domains: &["tlclw.com"],
    },
    Platform {
        key: "zhihu",
        name: "知乎直播",
        domains: &["zhihu.com"],
    },
    Platform {
        key: "chzzk",
        name: "CHZZK",
        domains: &["chzzk.naver.com"],
    },
    Platform {
        key: "haixiu",
        name: "嗨秀直播",
        domains: &["haixiutv.com"],
    },
    Platform {
        key: "vvxq",
        name: "VV星球",
        domains: &["vvxqiu.com"],
    },
    Platform {
        key: "17live",
        name: "17Live",
        domains: &["17.live"],
    },
    Platform {
        key: "langlive",
        name: "浪Live",
        domains: &["lang.live"],
    },
    Platform {
        key: "piaopiao",
        name: "漂漂直播",
        domains: &["weimipopo.com"],
    },
    Platform {
        key: "sixroom",
        name: "六间房",
        domains: &["6.cn"],
    },
    Platform {
        key: "lehai",
        name: "乐嗨直播",
        domains: &["lehaitv.com"],
    },
    Platform {
        key: "huamao",
        name: "花猫直播",
        domains: &["catshow168.com"],
    },
    Platform {
        key: "shopee",
        name: "Shopee",
        domains: &[
            "shopee.tw",
            "shopee.co.th",
            "shopee.com.my",
            "shopee.sg",
            "shopee.ph",
            "shopee.co.id",
            "shopee.vn",
            "shopee.com.br",
            "shp.ee",
        ],
    },
    Platform {
        key: "youtube",
        name: "YouTube",
        domains: &["youtube.com", "youtu.be"],
    },
    Platform {
        key: "taobao",
        name: "淘宝直播",
        domains: &["taobao.com", "tb.cn"],
    },
    Platform {
        key: "jd",
        name: "京东直播",
        domains: &["jd.com", "3.cn"],
    },
    Platform {
        key: "faceit",
        name: "FACEIT",
        domains: &["faceit.com"],
    },
    Platform {
        key: "lianjie",
        name: "连接直播",
        domains: &["lailianjie.com"],
    },
    Platform {
        key: "migu",
        name: "咪咕直播",
        domains: &["miguvideo.com"],
    },
    Platform {
        key: "laixiu",
        name: "来秀直播",
        domains: &["imkktv.com"],
    },
    Platform {
        key: "picarto",
        name: "Picarto",
        domains: &["picarto.tv"],
    },
    Platform {
        key: "xindongrebo",
        name: "心动热播",
        domains: &["xcqrkj.com"],
    },
];
pub fn detect(value: &str) -> Option<&'static Platform> {
    let url = reqwest::Url::parse(value).ok()?;
    let host = url.host_str()?;
    PLATFORMS.iter().find(|p| {
        p.domains
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
    })
}
pub fn by_key(key: &str) -> Option<&'static Platform> {
    PLATFORMS.iter().find(|p| p.key == canonical_key(key))
}
pub fn canonical_key(key: &str) -> &str {
    match key {
        "xhs" | "xiaohongshu" => "rednote",
        "sooplive" => "soop",
        "pandalive" => "pandatv",
        "lang" => "langlive",
        "vvxqiu" => "vvxq",
        "6room" => "sixroom",
        "catshow" => "huamao",
        "yingbo" => "yinbo",
        "YY" => "yy",
        _ => key,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_matching_does_not_accept_lookalike_domains() {
        assert_eq!(
            detect("https://live.bilibili.com/1").unwrap().key,
            "bilibili"
        );
        assert!(detect("https://bilibili.com.evil.test/1").is_none());
        assert_eq!(PLATFORMS.len(), 51);
    }
}
