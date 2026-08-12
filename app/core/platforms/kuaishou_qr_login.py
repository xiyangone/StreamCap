"""快手扫码登录，用于获取 live.kuaishou.com 可用的 Cookie。

与快手网页端一致的四步流程：

    1. qr/start        取二维码，参数必须表单编码且 sid 必填
    2. qr/scanResult   长轮询，手机扫码后返回，二维码过期时返回 707
    3. qr/acceptResult 手机点确认后返回 qrToken
    4. login/qr/callback  用 qrToken 换取账号级主凭证 passToken
    5. login/passToken    用 passToken 换取 live.kuaishou.com 的会话令牌

第 5 步不能省。直播站只认 kuaishou.live.web_st，而它仅在响应体里返回，
不走 Set-Cookie；只带 passToken 的 Cookie 在直播站是未登录状态。

二维码有效期只有 60 秒，过期必须重新取码。
"""

import asyncio
import base64
import io
import time
from dataclasses import dataclass

import httpx
from PIL import Image

from ...utils.logger import logger

QR_START_URL = "https://id.kuaishou.com/rest/c/infra/ks/qr/start"
QR_SCAN_URL = "https://id.kuaishou.com/rest/c/infra/ks/qr/scanResult"
QR_ACCEPT_URL = "https://id.kuaishou.com/rest/c/infra/ks/qr/acceptResult"
QR_CALLBACK_URL = "https://id.kuaishou.com/pass/kuaishou/login/qr/callback"
PASS_TOKEN_URL = "https://id.kuaishou.com/pass/kuaishou/login/passToken"
LIVE_HOME_URL = "https://live.kuaishou.com/"
USER_INFO_URL = "https://live.kuaishou.com/live_api/baseuser/userinfo/byid?__NS_hxfalcon=&caver=2&principalId="

# live.kuaishou.com 的登录态 sid，对应 kuaishou.live.web_st / _ph 两个 Cookie
DEFAULT_SID = "kuaishou.live.web"
QR_EXPIRED_RESULT = 707
RESULT_OK = 1

USER_AGENT = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36 Edg/145.0.0.0"
)


class QrLoginError(Exception):
    """扫码登录过程中的可预期失败，消息直接面向用户展示。"""


class QrExpiredError(QrLoginError):
    """二维码已过期，需要重新取码。"""


@dataclass
class QrCode:
    token: str
    signature: str
    image_base64: str
    expire_at: float

    @property
    def seconds_left(self) -> int:
        return max(0, int(self.expire_at - time.time()))


def _upscale_qr(raw_png: bytes, size: int = 260) -> str:
    """把 125x125 的原图按最近邻放大，避免界面缩放糊掉导致扫不出来。"""
    try:
        with Image.open(io.BytesIO(raw_png)) as img:
            enlarged = img.convert("L").resize((size, size), Image.Resampling.NEAREST)
            buffer = io.BytesIO()
            enlarged.save(buffer, format="PNG")
            return base64.b64encode(buffer.getvalue()).decode()
    except Exception as e:
        logger.warning(f"Failed to upscale Kuaishou QR image, use original: {e}")
        return base64.b64encode(raw_png).decode()


class KuaishouQrLogin:
    """一次扫码登录会话。整个流程共用一个 client，Cookie 逐步累积在同一个 jar 里。"""

    def __init__(self, sid: str = DEFAULT_SID, proxy: str | None = None):
        self.sid = sid
        self.proxy = proxy
        self.client: httpx.AsyncClient | None = None
        self.user_id: str | None = None

    async def __aenter__(self) -> "KuaishouQrLogin":
        self.client = httpx.AsyncClient(
            timeout=httpx.Timeout(75.0, connect=15.0),
            verify=False,
            follow_redirects=True,
            proxy=self.proxy or None,
            headers={"User-Agent": USER_AGENT, "Referer": "https://www.kuaishou.com/"},
        )
        return self

    async def __aexit__(self, *_exc) -> None:
        if self.client is not None:
            await self.client.aclose()
            self.client = None

    def _require_client(self) -> httpx.AsyncClient:
        if self.client is None:
            raise QrLoginError("登录会话未启动")
        return self.client

    @staticmethod
    def _parse(response: httpx.Response, step: str) -> dict:
        try:
            data = response.json()
        except Exception as e:
            raise QrLoginError(f"{step}: 返回内容无法解析 ({e})") from e
        if not isinstance(data, dict):
            raise QrLoginError(f"{step}: 返回格式异常")
        if data.get("result") == QR_EXPIRED_RESULT:
            raise QrExpiredError(data.get("error_msg") or "登录二维码已过期")
        return data

    async def create_qr(self) -> QrCode:
        client = self._require_client()
        response = await client.post(QR_START_URL, data={"sid": self.sid})
        data = self._parse(response, "获取二维码")
        if data.get("result") != RESULT_OK or not data.get("imageData"):
            raise QrLoginError(f"获取二维码失败: {data.get('error_msg') or data.get('result')}")

        expire_ms = data.get("expireTime")
        expire_at = expire_ms / 1000 if isinstance(expire_ms, (int, float)) else time.time() + 60
        qr = QrCode(
            token=data["qrLoginToken"],
            signature=data["qrLoginSignature"],
            image_base64=_upscale_qr(base64.b64decode(data["imageData"])),
            expire_at=expire_at,
        )
        logger.info(f"Kuaishou QR created, valid for {qr.seconds_left}s")
        return qr

    async def wait_for_scan(self, qr: QrCode) -> None:
        """长轮询等待扫码。服务端会挂住连接直到扫码或二维码过期。"""
        client = self._require_client()
        payload = {"qrLoginToken": qr.token, "qrLoginSignature": qr.signature}
        try:
            response = await client.post(QR_SCAN_URL, data=payload)
        except httpx.TimeoutException as e:
            raise QrExpiredError("等待扫码超时，请重新获取二维码") from e
        data = self._parse(response, "等待扫码")
        if data.get("result") != RESULT_OK:
            raise QrLoginError(f"扫码失败: {data.get('error_msg') or data.get('result')}")
        logger.info("Kuaishou QR scanned, waiting for confirmation")

    async def wait_for_accept(self, qr: QrCode) -> str:
        """等待手机端点确认，返回 qrToken。"""
        client = self._require_client()
        payload = {"qrLoginToken": qr.token, "qrLoginSignature": qr.signature, "sid": self.sid}
        deadline = time.time() + 120
        while time.time() < deadline:
            response = await client.post(QR_ACCEPT_URL, data=payload)
            data = self._parse(response, "等待确认")
            if data.get("result") == RESULT_OK and data.get("qrToken"):
                logger.info("Kuaishou QR confirmed on phone")
                return data["qrToken"]
            await asyncio.sleep(1.5)
        raise QrLoginError("等待手机确认超时")

    async def exchange_cookies(self, qr_token: str) -> str:
        """qrToken -> passToken -> 会话令牌，最后组装成请求头可用的 Cookie 字符串。"""
        client = self._require_client()

        # 用 qrToken 换账号级主凭证，passToken 会写进 jar
        response = await client.post(QR_CALLBACK_URL, data={"qrToken": qr_token, "sid": self.sid})
        self._parse(response, "换取登录凭证")

        # 访问一次直播站，补齐 did / client_key / kpn 等仅由该域下发的 Cookie
        try:
            await client.get(LIVE_HOME_URL, headers={"Referer": LIVE_HOME_URL})
        except Exception as e:
            logger.warning(f"Failed to warm up live.kuaishou.com cookies: {e}")

        # 换取直播站会话令牌。它只在响应体里返回，必须手动并入 Cookie
        token_response = await client.post(PASS_TOKEN_URL, data={"sid": self.sid})
        token_data = self._parse(token_response, "换取会话令牌")
        if token_data.get("result") != RESULT_OK:
            raise QrLoginError(f"换取会话令牌失败: {token_data.get('error_msg') or token_data.get('result')}")

        session_token = token_data.get(f"{self.sid}_st")
        if not session_token:
            available = ", ".join(sorted(token_data)) or "空"
            raise QrLoginError(f"未取到会话令牌，返回字段: {available}")
        self.user_id = str(token_data.get("userId") or "") or None

        cookies = self._collect_cookies({f"{self.sid}_st": session_token})
        if not cookies:
            raise QrLoginError("登录成功但未取到 Cookie")
        logger.info(f"Kuaishou login cookies acquired: {[c.split('=')[0] for c in cookies.split('; ')]}")
        return cookies

    def _collect_cookies(self, extra: dict[str, str] | None = None) -> str:
        """汇总 jar 中所有 kuaishou 域的 Cookie，同名以最后写入的为准。"""
        client = self._require_client()
        merged: dict[str, str] = {}
        for cookie in client.cookies.jar:
            if cookie.name and cookie.value and "kuaishou" in (cookie.domain or ""):
                merged[cookie.name] = cookie.value
        merged.update(extra or {})
        return "; ".join(f"{k}={v}" for k, v in merged.items())

    async def verify_cookies(self, cookies: str) -> str | None:
        """确认登录态真的被直播站认可。

        用户信息接口对匿名请求也照常返回，无法用来判断登录态；
        实测可靠的判据是直播站首页会把已登录用户的 userId 写进页面。
        """
        client = self._require_client()
        if not self.user_id:
            return None
        try:
            response = await client.get(LIVE_HOME_URL, headers={"Cookie": cookies, "Referer": LIVE_HOME_URL})
            if self.user_id not in response.text:
                logger.warning("Kuaishou cookies acquired but live site does not report a logged-in state")
                return None
        except Exception as e:
            logger.warning(f"Kuaishou cookie verification request failed: {e}")
            return None

        try:
            info = await client.get(
                f"{USER_INFO_URL}{self.user_id}",
                headers={"Cookie": cookies, "Referer": LIVE_HOME_URL},
            )
            name = ((info.json().get("data") or {}).get("userInfo") or {}).get("name")
        except Exception:
            name = None
        return name or self.user_id
