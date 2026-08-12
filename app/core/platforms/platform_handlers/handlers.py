import json
import time

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


class TikTokHandler(PlatformHandler):
    platform = "tiktok"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.TikTokLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.TikTokLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class _KwaiLiveStreamHardened(streamget.KwaiLiveStream):
    """快手加固版解析器。

    针对上游 KwaiLiveStream 的三处问题：

    1. _get_pc_headers 丢弃了用户 Cookie，主页面请求始终是匿名的；
    2. get_user_info 假定 /u/ 后面是数字 principalId，遇到用户名形式的地址
       或平台下发验证码挑战时会直接 KeyError，把整次检测拖崩——配了 Cookie
       反而比不配更容易失败；
    3. 只有网页抓取一条通道，而网页端按 IP 做严格突发限流。

    手机分享接口（与 DouyinLiveRecorder v4.0.7 主通道一致）限流宽松，
    因此优先走它，网页抓取兜底；该接口被限流时进入冷却，避免每次检测
    都白打一次请求反而加重限流。
    """

    APP_API = "https://livev.m.chenzhongtech.com/rest/k/live/byUser?kpn=GAME_ZONE&captchaToken="
    APP_HEADERS = {
        "user-agent": "ios/7.830 (ios 17.0; ; iPhone 15 (A2846/A3089/A3090/A3092))",
        "accept-language": "zh-CN,zh;q=0.8,zh-TW;q=0.7,zh-HK;q=0.5,en-US;q=0.3,en;q=0.2",
        "referer": "https://www.kuaishou.com/short-video/3x224rwabjmuc9y?fid=1712760877&cc=share_copylink",
        "content-type": "application/json",
    }
    APP_API_COOLDOWN = 600.0

    def __init__(self, proxy_addr: str | None = None, cookies: str | None = None) -> None:
        super().__init__(proxy_addr=proxy_addr, cookies=cookies)
        self._app_api_ready_at = 0.0

    def _get_pc_headers(self) -> dict:
        headers = super()._get_pc_headers()
        if self.cookies and self.cookies.strip():
            headers["cookie"] = self.cookies
        return headers

    async def get_user_info(self, url: str):
        """探测失败即放行，交给网页解析判断，不让一个辅助接口决定成败。

        平台下发验证码挑战时响应里没有 userInfo，上游会 KeyError；
        真正的账号风控（RuntimeError）仍然向上抛出，供上层归类展示。
        """
        try:
            return await super().get_user_info(url)
        except RuntimeError:
            raise
        except Exception as e:
            logger.info(f"Kuaishou user info probe unavailable, fall back to page parsing: {e}")
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
        self._app_api_ready_at = time.monotonic() + self.APP_API_COOLDOWN
        logger.info(
            f"Kuaishou app API unavailable ({reason}), use page parsing for the next "
            f"{int(self.APP_API_COOLDOWN // 60)} minutes"
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
        try:
            json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        except Exception as e:
            logger.info(f"Kuaishou app API failed, fallback to web page: {e}")
        if not json_data:
            json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        if not json_data:
            raise Exception(f"Failed to fetch Kuaishou stream data from {live_url}")
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class HuyaHandler(PlatformHandler):
    platform = "huya"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.HuyaLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.HuyaLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class DouyuHandler(PlatformHandler):
    platform = "douyu"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.DouyuLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.DouyuLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class YYHandler(PlatformHandler):
    platform = "YY"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.YYLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.YYLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class BilibiliHandler(PlatformHandler):
    platform = "bilibili"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.BilibiliLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.BilibiliLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class RedNoteHandler(PlatformHandler):
    platform = "rednote"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.RedNoteLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.RedNoteLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class BigoHandler(PlatformHandler):
    platform = "bigo"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.BigoLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.BigoLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class BluedHandler(PlatformHandler):
    platform = "blued"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.BluedLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.BluedLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


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


class NeteaseHandler(PlatformHandler):
    platform = "netease"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.NeteaseLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.NeteaseLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


@deprecated(reason="Live platform has been shut down")
class QiandureboHandler(PlatformHandler):
    platform = "qiandurebo"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.QiandureboLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.QiandureboLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class PamdaTVHandler(PlatformHandler):
    platform = "pandatv"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.PandaLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.PandaLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class MaoerFMHandler(PlatformHandler):
    platform = "maoerfm"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.MaoerLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.MaoerLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class LookHandler(PlatformHandler):
    platform = "look"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.LookLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.LookLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


@deprecated(reason="Live platform has been shut down")
class WinkTVHandler(PlatformHandler):
    platform = "winktv"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.WinkTVLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.WinkTVLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


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


class BaiduHandler(PlatformHandler):
    platform = "baidu"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.BaiduLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.BaiduLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class WeiboHandler(PlatformHandler):
    platform = "weibo"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.WeiboLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.WeiboLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class KugouHandler(PlatformHandler):
    platform = "kugou"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.KugouLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.KugouLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class TwitchHandler(PlatformHandler):
    platform = "twitch"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.TwitchLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.TwitchLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class LivemeHandler(PlatformHandler):
    platform = "liveme"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.LiveMeLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.LiveMeLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class HuajiaoHandler(PlatformHandler):
    platform = "huajiao"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.HuajiaoLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.HuajiaoLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class ShowRoomHandlerHandler(PlatformHandler):
    platform = "showroom"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.ShowRoomLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.ShowRoomLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class AcfunHandler(PlatformHandler):
    platform = "acfun"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.AcfunLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.AcfunLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class InkeHandler(PlatformHandler):
    platform = "inke"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.InkeLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.InkeLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


@deprecated(reason="Live platform has been shut down")
class YinboHandler(PlatformHandler):
    platform = "yinbo"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.YinboLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.YinboLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class ChangliaoHandler(PlatformHandler):
    platform = "changliao"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.ChangliaoLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.ChangliaoLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class ZhihuHandler(PlatformHandler):
    platform = "zhihu"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.ZhihuLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.ZhihuLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class ChzzkHandler(PlatformHandler):
    platform = "chzzk"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.ChzzkLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.ChzzkLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class HaixiuHandler(PlatformHandler):
    platform = "haixiu"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.HaixiuLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.HaixiuLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


@deprecated(reason="Live stream acquisition has been shut down")
class VVXQHandler(PlatformHandler):
    platform = "vvxqiu"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.VVXQLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.VVXQLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class YiqiLiveHandler(PlatformHandler):
    platform = "17live"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.YiqiLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.YiqiLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class LangLiveHandler(PlatformHandler):
    platform = "langlive"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.LangLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.LangLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


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


class SixRoomHandler(PlatformHandler):
    platform = "sixroom"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.SixRoomLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.SixRoomLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class LehaiHandler(PlatformHandler):
    platform = "lehai"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.LehaiLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.LehaiLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class HuamaoHandler(PlatformHandler):
    platform = "huamao"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.HuamaoLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.HuamaoLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class ShopeeHandler(PlatformHandler):
    platform = "shopee"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.ShopeeLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.ShopeeLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class YoutubeHandler(PlatformHandler):
    platform = "youtube"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.YoutubeLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.YoutubeLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class TaobaoHandler(PlatformHandler):
    platform = "taobao"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.TaobaoLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.TaobaoLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class JDHandler(PlatformHandler):
    platform = "jd"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.JDLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.JDLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class FaceitHandler(PlatformHandler):
    platform = "faceit"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.FaceitLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.FaceitLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class LianJieHandler(PlatformHandler):
    platform = "lianjie"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.LianJieLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.LianJieLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


@deprecated(reason="Live stream acquisition has been shut down")
class MiguHandler(PlatformHandler):
    platform = "migu"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.MiguLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.MiguLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class LaixiuHandler(PlatformHandler):
    platform = "laixiu"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.LaixiuLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.LaixiuLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class PicartoHandler(PlatformHandler):
    platform = "picarto"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.PicartoLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.PicartoLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


class XindongreboHandler(PlatformHandler):
    platform = "xindongrebo"

    def __init__(
        self,
        proxy: str | None = None,
        cookies: str | None = None,
        record_quality: str | None = None,
        platform: str | None = None,
    ) -> None:
        super().__init__(proxy, cookies, record_quality, platform)
        self.live_stream: streamget.XindongreboLiveStream | None = None

    @trace_error_decorator
    async def get_stream_info(self, live_url: str) -> StreamData:
        if not self.live_stream:
            self.live_stream = streamget.XindongreboLiveStream(proxy_addr=self.proxy, cookies=self.cookies)
        json_data = await self.live_stream.fetch_web_stream_data(url=live_url)
        return await self.live_stream.fetch_stream_url(json_data, self.record_quality)


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
