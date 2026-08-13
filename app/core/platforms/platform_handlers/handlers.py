import json
import time
from typing import ClassVar

import streamget
from deprecated import deprecated
from streamget.requests.async_http import async_req

from ....utils.logger import logger
from ....utils.utils import trace_error_decorator
from .base import PlatformHandler, StreamData


class CustomHandler(PlatformHandler):
    platform = "custom"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        stream_data = StreamData(platform="Custom", anchor_name="CustomLive", is_live=True, record_url=live_url)
        if ".flv" in live_url:
            stream_data.flv_url = live_url
        if ".m3u8" in live_url:
            stream_data.flv_url = live_url
        return stream_data


class DouyinHandler(PlatformHandler):
    platform = "douyin"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.DouyinLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        """
        Fetch stream information for a Douyin live URL.
        """
        if not self.live_stream:
            self.live_stream = streamget.DouyinLiveStream(proxy_addr=self.proxy, cookies=self.cookies)

        if "v.douyin.com" in live_url or "www.douyin.com/user" in live_url:
            json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        else:
            json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class _TemplateHandler(PlatformHandler):
    """流程完全一致的平台的共享实现。

    这些平台的差异只有两点：streamget 解析类是哪一个、取流走 web 还是 app
    接口。子类用两个类属性声明差异即可；流程有任何偏差（额外凭证、多通道、
    URL 分支）的平台不要继承它，保持独立实现。
    """

    stream_class_name: ClassVar[str]
    fetch_method: ClassVar[str] = "fetch_web_stream_data"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            stream_class = getattr(streamget, self.stream_class_name)
            self.live_stream = stream_class(proxy_addr=self.proxy, cookies=self.cookies)
        fetch_stream_data = getattr(self.live_stream, self.fetch_method)
        json_data = await fetch_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class TikTokHandler(_TemplateHandler):
    platform = "tiktok"
    stream_class_name = "TikTokLiveStream"


class _KwaiLiveStreamHardened(streamget.KwaiLiveStream):
    """快手加固版解析器。

    针对上游 KwaiLiveStream 的三处问题：

    1. _get_pc_headers 丢弃了用户 Cookie，主页面请求始终是匿名的；
    2. Cookie 非空时会先打 userinfo 辅助接口，该接口限流时返回语义虚假的
       占位响应且不抛异常，导致网页通道被完全短路（详见 get_user_info）；
    3. 只有网页抓取一条通道，而网页端按 IP 做严格突发限流。

    手机分享接口（与 DouyinLiveRecorder v4.0.7 主通道一致）优先，网页抓取兜底；
    该接口被限流时进入冷却，避免每次检测都白打一次请求反而加重限流。
    冷却原因会通过 last_fetch_error 上报，供上层区分"平台限流"与"普通检测失败"。

    实测记录（2026-08-13，同一 IP、同一房间）：
    - 手机接口的限流维度既不是 User-Agent（换 5 个 iOS/Android/浏览器 UA
      全部返回「操作太快了」），也不是账号（匿名请求同样被限流）；
      社区 DouyinLiveRecorder #1058 的「换 UA 可绕过」在此接口上不成立。
    - 网页通道另有独立且宽松的配额（1.5 秒间隔连打 12 次未触发限流），
      与 #862「6000 秒轮询间隔也照样被封」互为印证：触发条件不是请求频率。
    因此该接口一旦配额耗尽可能长时间不可用，冷却采用指数退避而非固定值。
    """

    APP_API = "https://livev.m.chenzhongtech.com/rest/k/live/byUser?kpn=GAME_ZONE&captchaToken="
    APP_HEADERS = {
        "user-agent": "ios/7.830 (ios 17.0; ; iPhone 15 (A2846/A3089/A3090/A3092))",
        "accept-language": "zh-CN,zh;q=0.8,zh-TW;q=0.7,zh-HK;q=0.5,en-US;q=0.3,en;q=0.2",
        "referer": "https://www.kuaishou.com/short-video/3x224rwabjmuc9y?fid=1712760877&cc=share_copylink",
        "content-type": "application/json",
    }
    APP_API_COOLDOWN = 600.0
    # 连续失败时冷却翻倍的上限。配额耗尽后该接口可能整天不可用，
    # 固定 10 分钟重试等于每天白打上百次请求
    APP_API_COOLDOWN_MAX = 14400.0

    def __init__(self, proxy_addr: str | None = None, cookies: str | None = None) -> None:
        super().__init__(proxy_addr=proxy_addr, cookies=cookies)
        self._app_api_ready_at = 0.0
        self._block_reason: str | None = None
        self._app_api_failures = 0

    @property
    def pending_block_reason(self) -> str | None:
        """手机接口当前是否正因限流处于冷却期，是则返回平台给出的原因。

        以冷却截止时间为准而非一次性标志：handler 实例被 _instances 缓存复用，
        冷却期内每次检测都直接跳过请求，原因不会被重新赋值。绑定截止时间可以
        让原因随冷却自然过期，不需要手动清理。
        """
        if time.monotonic() < self._app_api_ready_at:
            return self._block_reason
        return None

    def _get_pc_headers(self) -> dict:
        headers = super()._get_pc_headers()
        if self.cookies and self.cookies.strip():
            headers["cookie"] = self.cookies
        return headers

    async def get_user_info(self, url: str):
        """废弃 userinfo 前置探测，恒定放行，一律交给网页解析判断。

        上游 fetch_web_stream_data 在 Cookie 非空时会先打这个辅助接口，
        拿到 living 为假就直接 early return，网页通道一次都不请求。

        但该接口在限流时会返回结构完整、语义虚假的占位响应——实测同一房间
        三分钟内在 result=1（name 正常）、result=2（name 为 None）、
        result=400002（userInfo 整体缺失）之间跳变。占位响应不抛异常，
        干净地返回 (None, False)，于是整次检测被判成未开播，
        表现为卡片"检测失败"。配了 Cookie 反而比不配更容易失败。

        返回 (None, True) 让上游跳过 early return 分支；不再发这次请求，
        同时也少一个被限流的接口面。

        取舍：一并放弃了上游基于该接口的账号风控识别
        （result==2 且 name 存在时抛 RuntimeError）。该判据本身不可靠——
        限流占位响应同样是 result=2。真实的房间级异常仍由网页通道的
        errorType 与 "IP banned" 分支反馈。
        """
        return None, True

    async def fetch_app_stream_data(self, url: str) -> dict | None:
        if "/u/" not in url or time.monotonic() < self._app_api_ready_at:
            return None
        eid = url.split("/u/")[1].split("?")[0].strip("/").strip()
        if not eid:
            return None

        headers = dict(self.APP_HEADERS)
        if self.cookies and self.cookies.strip():
            headers["cookie"] = self.cookies
        body = {"source": 5, "eid": eid, "shareMethod": "card", "clientType": "WEB_OUTSIDE_SHARE_H5"}
        # 必须走 json_data：该接口对表单编码的请求返回空响应体
        raw = await async_req(url=self.APP_API, proxy_addr=self.proxy_addr, headers=headers, json_data=body)
        if not isinstance(raw, str) or not raw.strip():
            self._cool_down_app_api("empty response")
            return None

        json_data = json.loads(raw)
        live_stream = json_data.get("liveStream")
        if not live_stream:
            self._cool_down_app_api(json_data.get("error_msg") or f"result={json_data.get('result')}")
            return None

        anchor_name = (live_stream.get("user") or {}).get("user_name")
        if not anchor_name:
            return None

        # 取到真实数据说明配额已恢复，退避重新从最短冷却起算
        self._app_api_failures = 0
        result = {"type": 2, "anchor_name": anchor_name, "is_live": False, "live_url": url}
        if live_stream.get("living"):
            if "multiResolutionHlsPlayUrls" in live_stream:
                result["m3u8_url_list"] = live_stream["multiResolutionHlsPlayUrls"][0]["urls"]
            if "multiResolutionPlayUrls" in live_stream:
                result["flv_url_list"] = live_stream["multiResolutionPlayUrls"][0]["urls"]
            elif live_stream.get("playUrls"):
                result["flv_url_list"] = live_stream["playUrls"]
            if "flv_url_list" not in result and "m3u8_url_list" not in result:
                # 在播但拿不到直播源地址，交给网页兜底解析
                return None
            result["is_live"] = True
        return result

    def _cool_down_app_api(self, reason: str) -> None:
        self._app_api_failures += 1
        # 指数退避：10 分钟起，每次连续失败翻倍，封顶 4 小时
        cooldown = min(self.APP_API_COOLDOWN * 2 ** (self._app_api_failures - 1), self.APP_API_COOLDOWN_MAX)
        self._app_api_ready_at = time.monotonic() + cooldown
        self._block_reason = reason
        logger.info(
            f"Kuaishou app API unavailable ({reason}), consecutive failures="
            f"{self._app_api_failures}, use page parsing for the next {int(cooldown // 60)} minutes"
        )


class KuaishouHandler(PlatformHandler):
    platform = "kuaishou"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: _KwaiLiveStreamHardened | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = _KwaiLiveStreamHardened(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = None
        app_exception: Exception | None = None
        try:
            json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        except Exception as e:
            app_exception = e
            logger.info(f"Kuaishou app API failed, fallback to web page: {e}")
        if not json_data:
            json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        if not json_data:
            # 两条通道都失败时才判断平台级限流：手机接口正处于冷却，且有限流原因
            if reason := self.live_stream.pending_block_reason:
                raise Exception(f"Kuaishou channels blocked: {reason}")
            # 否则是普通失败：手机接口抛异常（网络错误、解析错等）+ 网页空
            if app_exception:
                raise Exception(f"App API: {app_exception}; web page also failed") from app_exception
            raise Exception(f"Failed to fetch Kuaishou stream data from {live_url}")
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class HuyaHandler(_TemplateHandler):
    platform = "huya"
    stream_class_name = "HuyaLiveStream"
    fetch_method = "fetch_app_stream_data"


class DouyuHandler(_TemplateHandler):
    platform = "douyu"
    stream_class_name = "DouyuLiveStream"


class YYHandler(_TemplateHandler):
    platform = "YY"
    stream_class_name = "YYLiveStream"


class BilibiliHandler(_TemplateHandler):
    platform = "bilibili"
    stream_class_name = "BilibiliLiveStream"


class RedNoteHandler(_TemplateHandler):
    platform = "rednote"
    stream_class_name = "RedNoteLiveStream"
    fetch_method = "fetch_app_stream_data"


class BigoHandler(_TemplateHandler):
    platform = "bigo"
    stream_class_name = "BigoLiveStream"


class BluedHandler(_TemplateHandler):
    platform = "blued"
    stream_class_name = "BluedLiveStream"


class SoopHandler(PlatformHandler):
    platform = "soop"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
        username: str | None = None,
        password: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform, username, password)
        self.live_stream: streamget.SoopLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.SoopLiveStream(
                proxy_addr=self.proxy, cookies=self.cookies, username=self.username, password=self.password
            )
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class NeteaseHandler(_TemplateHandler):
    platform = "netease"
    stream_class_name = "NeteaseLiveStream"


@deprecated(reason="Live platform has been shut down")
class QiandureboHandler(_TemplateHandler):
    platform = "qiandurebo"
    stream_class_name = "QiandureboLiveStream"


class PamdaTVHandler(_TemplateHandler):
    platform = "pandatv"
    stream_class_name = "PandaLiveStream"


class MaoerFMHandler(_TemplateHandler):
    platform = "maoerfm"
    stream_class_name = "MaoerLiveStream"


class LookHandler(_TemplateHandler):
    platform = "look"
    stream_class_name = "LookLiveStream"


@deprecated(reason="Live platform has been shut down")
class WinkTVHandler(_TemplateHandler):
    platform = "winktv"
    stream_class_name = "WinkTVLiveStream"


class FlexTVHandler(PlatformHandler):
    platform = "flextv"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
        username: str | None = None,
        password: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform, username, password)
        self.live_stream: streamget.FlexTVLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.FlexTVLiveStream(
                proxy_addr=self.proxy, cookies=self.cookies, username=self.username, password=self.password
            )
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class PopkonTVHandler(PlatformHandler):
    platform = "popkontv"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
        username: str | None = None,
        password: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform, username, password)
        self.live_stream: streamget.PopkonTVLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.PopkonTVLiveStream(
                proxy_addr=self.proxy, cookies=self.cookies, username=self.username, password=self.password
            )
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class TwitcastingHandler(PlatformHandler):
    platform = "twitcasting"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
        username: str | None = None,
        password: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform, username, password)
        self.live_stream: streamget.TwitCastingLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.TwitCastingLiveStream(
                proxy_addr=self.proxy, cookies=self.cookies, username=self.username, password=self.password
            )
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class BaiduHandler(_TemplateHandler):
    platform = "baidu"
    stream_class_name = "BaiduLiveStream"


class WeiboHandler(_TemplateHandler):
    platform = "weibo"
    stream_class_name = "WeiboLiveStream"


class KugouHandler(_TemplateHandler):
    platform = "kugou"
    stream_class_name = "KugouLiveStream"


class TwitchHandler(_TemplateHandler):
    platform = "twitch"
    stream_class_name = "TwitchLiveStream"


class LivemeHandler(_TemplateHandler):
    platform = "liveme"
    stream_class_name = "LiveMeLiveStream"


class HuajiaoHandler(_TemplateHandler):
    platform = "huajiao"
    stream_class_name = "HuajiaoLiveStream"
    fetch_method = "fetch_app_stream_data"


class ShowRoomHandlerHandler(_TemplateHandler):
    platform = "showroom"
    stream_class_name = "ShowRoomLiveStream"


class AcfunHandler(_TemplateHandler):
    platform = "acfun"
    stream_class_name = "AcfunLiveStream"


class InkeHandler(_TemplateHandler):
    platform = "inke"
    stream_class_name = "InkeLiveStream"


@deprecated(reason="Live platform has been shut down")
class YinboHandler(_TemplateHandler):
    platform = "yinbo"
    stream_class_name = "YinboLiveStream"


class ChangliaoHandler(_TemplateHandler):
    platform = "changliao"
    stream_class_name = "ChangliaoLiveStream"


class ZhihuHandler(_TemplateHandler):
    platform = "zhihu"
    stream_class_name = "ZhihuLiveStream"


class ChzzkHandler(_TemplateHandler):
    platform = "chzzk"
    stream_class_name = "ChzzkLiveStream"


class HaixiuHandler(_TemplateHandler):
    platform = "haixiu"
    stream_class_name = "HaixiuLiveStream"


@deprecated(reason="Live stream acquisition has been shut down")
class VVXQHandler(_TemplateHandler):
    platform = "vvxqiu"
    stream_class_name = "VVXQLiveStream"


class YiqiLiveHandler(_TemplateHandler):
    platform = "17live"
    stream_class_name = "YiqiLiveStream"


class LangLiveHandler(_TemplateHandler):
    platform = "langlive"
    stream_class_name = "LangLiveStream"


@deprecated(reason="Live stream acquisition has been shut down")
class PiaopiaoHandler(PlatformHandler):
    platform = "piaopiao"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.PiaopaioLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.PiaopaioLiveStream(proxy_addr=self.proxy, cookies=self.cookies)

        if "preview.html" not in live_url:
            json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        else:
            json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class SixRoomHandler(_TemplateHandler):
    platform = "sixroom"
    stream_class_name = "SixRoomLiveStream"


class LehaiHandler(_TemplateHandler):
    platform = "lehai"
    stream_class_name = "LehaiLiveStream"


class HuamaoHandler(_TemplateHandler):
    platform = "huamao"
    stream_class_name = "HuamaoLiveStream"


class ShopeeHandler(_TemplateHandler):
    platform = "shopee"
    stream_class_name = "ShopeeLiveStream"
    fetch_method = "fetch_app_stream_data"


class YoutubeHandler(_TemplateHandler):
    platform = "youtube"
    stream_class_name = "YoutubeLiveStream"


class TaobaoHandler(_TemplateHandler):
    platform = "taobao"
    stream_class_name = "TaobaoLiveStream"


class JDHandler(_TemplateHandler):
    platform = "jd"
    stream_class_name = "JDLiveStream"


class FaceitHandler(_TemplateHandler):
    platform = "faceit"
    stream_class_name = "FaceitLiveStream"


class LianJieHandler(_TemplateHandler):
    platform = "lianjie"
    stream_class_name = "LianJieLiveStream"


@deprecated(reason="Live stream acquisition has been shut down")
class MiguHandler(_TemplateHandler):
    platform = "migu"
    stream_class_name = "MiguLiveStream"


class LaixiuHandler(_TemplateHandler):
    platform = "laixiu"
    stream_class_name = "LaixiuLiveStream"


class PicartoHandler(_TemplateHandler):
    platform = "picarto"
    stream_class_name = "PicartoLiveStream"


class XindongreboHandler(_TemplateHandler):
    platform = "xindongrebo"
    stream_class_name = "XindongreboLiveStream"


CustomHandler.register(r"https?://.*\.(?:flv|m3u8)(\?.*)?$")
DouyinHandler.register(r"https://.*\.douyin\.com/")
TikTokHandler.register(r"https://.*\.tiktok\.com/")
KuaishouHandler.register(r"https://live\.kuaishou\.com/")
HuyaHandler.register(r"https://.*\.huya\.com/")
DouyuHandler.register(r"https://.*\.douyu\.com/")
YYHandler.register(r"https://.*\.yy\.com/")
BilibiliHandler.register(r"https://live\.bilibili\.com/")
RedNoteHandler.register(r"www\.xiaohongshu\.com/", r"xhslink\.com/")
BigoHandler.register(r"https://www\.bigo\.tv/", r"https://slink\.bigovideo\.tv/")
BluedHandler.register(r"https://app\.blued\.cn/")
SoopHandler.register(r"sooplive\.co\.kr/", r"sooplive\.com/")
NeteaseHandler.register(r"cc\.163\.com/")
QiandureboHandler.register(r"qiandurebo.com/")
PamdaTVHandler.register(r".*\.pandalive.co.kr/")
MaoerFMHandler.register(r"fm.missevan.com/")
LookHandler.register(r"look.163.com/")
WinkTVHandler.register(r"www.winktv.co.kr/")
FlexTVHandler.register(r"www\.flextv\.co\.kr/", r"www\.ttinglive\.com/")
PopkonTVHandler.register(r"www\.popkontv\.com/")
TwitcastingHandler.register(r"twitcasting\.tv")
BaiduHandler.register(r".*\.baidu\.com")
WeiboHandler.register(r"weibo\.com/")
KugouHandler.register(r".*\.kugou\.com")
TwitchHandler.register(r"https://.*\.twitch\.tv/")
LivemeHandler.register(r"https://.*\.liveme\.com/")
HuajiaoHandler.register(r".*\.huajiao\.com/")
ShowRoomHandlerHandler.register(r".*\.showroom-live\.com")
AcfunHandler.register(r"live.acfun.cn/")
InkeHandler.register(r"https://.*\.inke\.cn/")
YinboHandler.register(r"live.ybw1666.com")
ChangliaoHandler.register(r".*\.tlclw\.com")
ZhihuHandler.register(r"https://.*\.zhihu\.com/")
ChzzkHandler.register(r"chzzk\.naver\.com/")
HaixiuHandler.register(r"https://.*\.haixiutv\.com/")
VVXQHandler.register(r".*\.vvxqiu\.com")
YiqiLiveHandler.register(r"17\.live")
LangLiveHandler.register(r"https://.*\.lang\.live/")
PiaopiaoHandler.register(r".*\.weimipopo.com/")
SixRoomHandler.register(r"v.6.cn/")
LehaiHandler.register(r"https://.*\.lehaitv\.com/")
HuamaoHandler.register(r"h.catshow168.com")
ShopeeHandler.register(r".*.shp.ee/")
YoutubeHandler.register(r".*\.youtube\.com/")
TaobaoHandler.register(r".*\.tb\.cn/", r".*\.taobao\.com/")
JDHandler.register(r"3\.cn/")
FaceitHandler.register(r"https://.*\.faceit\.com/")
LianJieHandler.register(r"https://.*\.lailianjie\.com/")
MiguHandler.register(r"https://.*\.miguvideo\.com/")
LaixiuHandler.register(r"https://.*\.imkktv\.com/")
PicartoHandler.register(r"https://.*\.picarto\.tv/")
XindongreboHandler.register(r"xcqrkj.com")
