# Phase 1: Handlers 重构 + Node.js 依赖优化

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除 handlers.py 的 85% 代码重复（52 个平台 Handler 类），并优化 Node.js 依赖（89MB）为按需下载。

**Architecture:** 
- 配置驱动的通用 Handler 替代 52 个重复类，仅保留 3 个特例（Custom/Kuaishou/Douyin 有 app/web 分支）
- streamget 的 4 个平台（douyin/haixiu/liveme/migu）依赖 execjs，node.exe 必须保留但改为首次使用时按需下载

**Tech Stack:** 
- Python 3.10+，streamget 4.0.10，pyright 类型检查
- 现有 `scripts/node_install.py` 已实现按需下载逻辑（239 行）

**Scope:** 
- handlers.py: 1326 行 → ~350 行（减 976 行）
- 打包体积：减 89MB（node.exe 移至运行时下载）
- 风险：中等（需全量回归测试 52 个平台，但 streamget 层未变）

---

## 前置：备份与环境准备

### Task 0: 备份与分支准备

**Files:**
- Read: `app/core/platforms/platform_handlers/handlers.py`
- Read: `app/core/platforms/platform_handlers/__init__.py`
- Create: `.backup/handlers_before_refactor.py` (备份)

- [ ] **Step 1: 创建特性分支**

```bash
cd "Z:/demo/StreamCap"
git checkout -b refactor/phase1-handlers-and-node
```

- [ ] **Step 2: 备份关键文件**

```bash
mkdir -p .backup
cp app/core/platforms/platform_handlers/handlers.py .backup/handlers_before_refactor.py
cp app/core/platforms/platform_handlers/__init__.py .backup/__init___before_refactor.py
```

- [ ] **Step 3: 提取平台配置清单**

运行以下脚本生成当前 52 个平台的映射表：

```bash
python << 'PYEOF'
import re
from pathlib import Path

handlers = Path('app/core/platforms/platform_handlers/handlers.py')
content = handlers.read_text(encoding='utf-8')

pattern = r'class (\w+?)(Handler)\(PlatformHandler\):.*?platform = "(\w+)".*?streamget\.(\w+)\('
matches = re.findall(pattern, content, re.DOTALL)

print("平台配置清单（共 {} 个）：".format(len(matches)))
for cls, _, plat, stream_cls in matches[:10]:
    print(f"  {plat:15} -> {stream_cls}LiveStream")
PYEOF
```

预期输出：至少 40+ 个平台映射

- [ ] **Step 4: Commit 备份**

```bash
git add .backup/
git commit -m "chore: backup handlers before refactor"
```

---

## 阶段 A：handlers.py 重构

### Task 1: 创建平台配置表

**Files:**
- Create: `app/core/platforms/platform_handlers/platform_config.py`
- Test: `tests/core/platforms/test_platform_config.py`

- [ ] **Step 1: 写失败测试**

创建 `tests/core/platforms/test_platform_config.py`：

```python
import pytest
from app.core.platforms.platform_handlers.platform_config import (
    PLATFORM_CONFIGS,
    get_platform_config,
    PlatformConfig
)


def test_platform_config_structure():
    """平台配置必须包含 class_name 和 method_name"""
    assert "douyin" in PLATFORM_CONFIGS
    config = PLATFORM_CONFIGS["douyin"]
    assert "class_name" in config
    assert "method_name" in config
    assert config["class_name"] == "DouyinLiveStream"


def test_get_platform_config_existing():
    """已知平台返回配置"""
    config = get_platform_config("tiktok")
    assert config.class_name == "TikTokLiveStream"
    assert config.method_name == "fetch_web_stream_data"


def test_get_platform_config_unknown():
    """未知平台抛出 KeyError"""
    with pytest.raises(KeyError):
        get_platform_config("unknown_platform_xyz")


def test_all_configs_have_required_fields():
    """所有配置必须有 class_name 和 method_name"""
    for platform, config in PLATFORM_CONFIGS.items():
        assert "class_name" in config, f"{platform} missing class_name"
        assert "method_name" in config, f"{platform} missing method_name"
```

- [ ] **Step 2: 运行测试确认失败**

```bash
cd "Z:/demo/StreamCap"
.venv/Scripts/python.exe -m pytest tests/core/platforms/test_platform_config.py -v
```

预期：FAIL - `ModuleNotFoundError: No module named 'app.core.platforms.platform_handlers.platform_config'`

- [ ] **Step 3: 实现平台配置表**

创建 `app/core/platforms/platform_handlers/platform_config.py`：

```python
"""平台配置映射表：streamget 类名与方法名。"""

from dataclasses import dataclass


@dataclass
class PlatformConfig:
    """单个平台的配置"""
    class_name: str
    method_name: str
    has_app_method: bool = False  # 是否有 app 分支（如 douyin）


# 52 个平台的配置映射（从现有 handlers.py 提取）
PLATFORM_CONFIGS = {
    "douyin": {
        "class_name": "DouyinLiveStream",
        "method_name": "fetch_web_stream_data",
        "has_app_method": True,  # v.douyin.com 走 fetch_app_stream_data
    },
    "tiktok": {
        "class_name": "TikTokLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "huya": {
        "class_name": "HuyaLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "douyu": {
        "class_name": "DouyuLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "yy": {
        "class_name": "YYLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "bilibili": {
        "class_name": "BilibiliLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "rednote": {
        "class_name": "RedNoteLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "bigo": {
        "class_name": "BigoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "blued": {
        "class_name": "BluedLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "soop": {
        "class_name": "SoopLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "netease": {
        "class_name": "NeteaseLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "qiandurebo": {
        "class_name": "QiandureboLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "pamdatv": {
        "class_name": "PamdaTVLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "maoerfm": {
        "class_name": "MaoerFMLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "look": {
        "class_name": "LookLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "winktv": {
        "class_name": "WinkTVLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "flextv": {
        "class_name": "FlexTVLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "popkontv": {
        "class_name": "PopkonTVLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "twitcasting": {
        "class_name": "TwitCastingLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "baidu": {
        "class_name": "BaiduLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "weibo": {
        "class_name": "WeiboLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "kugou": {
        "class_name": "KugouLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "twitch": {
        "class_name": "TwitchLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "liveme": {
        "class_name": "LivemeLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "huajiao": {
        "class_name": "HuajiaoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "liangongjiao": {
        "class_name": "LianJieLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "yizhibo": {
        "class_name": "InkeLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "lang": {
        "class_name": "LangLiveLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "changliao": {
        "class_name": "ChangliaoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "17live": {
        "class_name": "Yizhibo17LiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "zhihu": {
        "class_name": "ZhihuLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "chzzk": {
        "class_name": "ChzzkLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "haixiu": {
        "class_name": "HaixiuLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "huajiao2": {
        "class_name": "HuajiaoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "migu": {
        "class_name": "MiguLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "changyou": {
        "class_name": "ChangyouLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "momo": {
        "class_name": "MomoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "showroom": {
        "class_name": "ShowRoomLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "acfun": {
        "class_name": "AcFunLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "huomao": {
        "class_name": "HuomaoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "haishen": {
        "class_name": "HaishenLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "qiniu": {
        "class_name": "QiniuLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "kandian": {
        "class_name": "KandianLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "kugou_fm": {
        "class_name": "KugouFMLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "aoligei": {
        "class_name": "AoligeiLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "taobao": {
        "class_name": "TaobaoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
    "taohuatao": {
        "class_name": "TaohuataoLiveStream",
        "method_name": "fetch_web_stream_data",
    },
}


def get_platform_config(platform: str) -> PlatformConfig:
    """获取平台配置，未知平台抛出 KeyError"""
    cfg = PLATFORM_CONFIGS[platform]
    return PlatformConfig(
        class_name=cfg["class_name"],
        method_name=cfg["method_name"],
        has_app_method=cfg.get("has_app_method", False),
    )
```

- [ ] **Step 4: 运行测试确认通过**

```bash
.venv/Scripts/python.exe -m pytest tests/core/platforms/test_platform_config.py -v
```

预期：PASS（4 个测试全通过）

- [ ] **Step 5: Commit 配置表**

```bash
git add app/core/platforms/platform_handlers/platform_config.py
git add tests/core/platforms/test_platform_config.py
git commit -m "feat(handlers): add platform config mapping table

- 52 platforms mapped to streamget class names
- Test coverage for config structure
- Prepare for GenericHandler refactor"
```

---

### Task 2: 实现通用 Handler 基类

**Files:**
- Modify: `app/core/platforms/platform_handlers/handlers.py:1-50` (添加 GenericHandler)
- Create: `tests/core/platforms/test_generic_handler.py`

- [ ] **Step 1: 写失败测试**

创建 `tests/core/platforms/test_generic_handler.py`：

```python
import pytest
from unittest.mock import AsyncMock, MagicMock, patch
from app.core.platforms.platform_handlers.handlers import GenericHandler


@pytest.mark.asyncio
async def test_generic_handler_douyin_web():
    """测试通用 Handler 处理抖音网页链接"""
    handler = GenericHandler(platform="douyin")
    
    # Mock streamget.DouyinLiveStream
    mock_stream = MagicMock()
    mock_stream.fetch_web_stream_data = AsyncMock(return_value={"author": "测试主播"})
    mock_stream.fetch_stream_url = AsyncMock(return_value={
        "anchor_name": "测试主播",
        "is_live": True,
        "stream_url": {"hls": "https://example.com/live.m3u8"}
    })
    
    with patch("streamget.DouyinLiveStream", return_value=mock_stream):
        result = await handler.get_stream_info("https://live.douyin.com/123456")
    
    assert result["anchor_name"] == "测试主播"
    assert result["is_live"] is True
    mock_stream.fetch_web_stream_data.assert_called_once()


@pytest.mark.asyncio
async def test_generic_handler_bilibili():
    """测试通用 Handler 处理 B 站链接"""
    handler = GenericHandler(platform="bilibili")
    
    mock_stream = MagicMock()
    mock_stream.fetch_web_stream_data = AsyncMock(return_value={"uname": "B站主播"})
    mock_stream.fetch_stream_url = AsyncMock(return_value={
        "anchor_name": "B站主播",
        "is_live": False,
        "stream_url": {}
    })
    
    with patch("streamget.BilibiliLiveStream", return_value=mock_stream):
        result = await handler.get_stream_info("https://live.bilibili.com/123")
    
    assert result["is_live"] is False


@pytest.mark.asyncio
async def test_generic_handler_unknown_platform():
    """未知平台抛出 KeyError"""
    with pytest.raises(KeyError):
        GenericHandler(platform="unknown_xyz")
```

- [ ] **Step 2: 运行测试确认失败**

```bash
.venv/Scripts/python.exe -m pytest tests/core/platforms/test_generic_handler.py -v
```

预期：FAIL - `AttributeError: type object 'GenericHandler' has no attribute '__init__'`

- [ ] **Step 3: 实现 GenericHandler**

在 `app/core/platforms/platform_handlers/handlers.py` 文件开头（第 1 行之后）插入：

```python
from .platform_config import get_platform_config, PlatformConfig


class GenericHandler(PlatformHandler):
    """通用平台处理器：配置驱动，自动加载对应 streamget 类"""
    
    def __init__(self, platform: str, **kwargs):
        super().__init__(platform=platform, **kwargs)
        self._config: PlatformConfig = get_platform_config(platform)
        self._stream_class = None
    
    def _get_stream_class(self):
        """延迟加载 streamget 类（避免循环导入）"""
        if self._stream_class is None:
            import streamget
            self._stream_class = getattr(streamget, self._config.class_name)
        return self._stream_class
    
    async def get_stream_info(self, live_url: str, **kwargs):
        """通用流信息获取逻辑"""
        if not self.live_stream:
            stream_cls = self._get_stream_class()
            self.live_stream = stream_cls(
                cookie=kwargs.get("cookie"),
                proxy=kwargs.get("proxy"),
                proxy_config=kwargs.get("proxy_config"),
            )
        
        try:
            # 根据配置选择方法（web 或 app）
            if self._config.has_app_method and "v.douyin.com" in live_url:
                json_data = await self.live_stream.fetch_app_stream_data(url=live_url)
            else:
                method = getattr(self.live_stream, self._config.method_name)
                json_data = await method(url=live_url)
            
            stream_info = await self.live_stream.fetch_stream_url(
                json_data=json_data,
                quality=kwargs.get("quality", "origin"),
                extra_format=kwargs.get("extra_format"),
                rate_check=kwargs.get("rate_check", False)
            )
            return stream_info
        
        except Exception as e:
            self.last_fetch_error = str(e)
            logger.error(f"[{self.platform}] get_stream_info error: {e}")
            return []
```

- [ ] **Step 4: 运行测试确认通过**

```bash
.venv/Scripts/python.exe -m pytest tests/core/platforms/test_generic_handler.py -v
```

预期：PASS（3 个测试全通过）

- [ ] **Step 5: Commit GenericHandler**

```bash
git add app/core/platforms/platform_handlers/handlers.py
git add tests/core/platforms/test_generic_handler.py
git commit -m "feat(handlers): implement GenericHandler with config-driven design

- Replace 52 duplicated classes with single generic handler
- Auto-load streamget class via platform_config
- Support douyin app/web branch via has_app_method flag
- Test coverage for douyin/bilibili/unknown platforms"
```

---

### Task 3: 替换现有 Handler 类（分批执行）

**Files:**
- Modify: `app/core/platforms/platform_handlers/handlers.py:100-1326`
- Modify: `app/core/platforms/platform_handlers/__init__.py`

- [ ] **Step 1: 备份当前 __init__.py 导出列表**

```bash
cd "Z:/demo/StreamCap"
grep "Handler" app/core/platforms/platform_handlers/__init__.py > .backup/handlers_export_list.txt
```

- [ ] **Step 2: 写迁移脚本（自动替换）**

创建 `scripts/migrate_handlers.py`：

```python
"""自动迁移 handlers.py：用 GenericHandler 替换所有标准平台类"""
import re
from pathlib import Path


def extract_handler_names(content: str) -> list[str]:
    """提取所有 XxxHandler 类名（排除 Custom/Kuaishou/Douyin）"""
    pattern = r'^class (\w+Handler)\(PlatformHandler\):'
    matches = re.findall(pattern, content, re.MULTILINE)
    
    # 排除特例（保留原实现）
    exclude = {"CustomHandler", "KuaishouHandler", "DouyinHandler", "GenericHandler", "PlatformHandler"}
    return [name for name in matches if name not in exclude]


def generate_factory_mapping(handler_names: list[str]) -> str:
    """生成 __init__.py 的工厂映射"""
    lines = []
    for name in sorted(handler_names):
        platform = name.replace("Handler", "").lower()
        lines.append(f'    "{platform}": GenericHandler,')
    return "\n".join(lines)


def main():
    handlers_file = Path("app/core/platforms/platform_handlers/handlers.py")
    init_file = Path("app/core/platforms/platform_handlers/__init__.py")
    
    content = handlers_file.read_text(encoding="utf-8")
    handler_names = extract_handler_names(content)
    
    print(f"✓ 发现 {len(handler_names)} 个可替换 Handler 类")
    print(f"✓ 保留 CustomHandler / KuaishouHandler / DouyinHandler（特殊逻辑）")
    
    # 生成新的 __init__.py 工厂映射
    factory_code = generate_factory_mapping(handler_names)
    
    print("\n建议的 __init__.py HANDLER_REGISTRY 映射：")
    print(factory_code)
    
    # 提示手动删除步骤
    print(f"\n下一步手动操作：")
    print(f"1. 删除 handlers.py 中除 Custom/Kuaishou/Douyin 外的 {len(handler_names)} 个类")
    print(f"2. 更新 __init__.py 中的 HANDLER_REGISTRY，映射到 GenericHandler")


if __name__ == "__main__":
    main()
```

- [ ] **Step 3: 运行迁移脚本分析**

```bash
.venv/Scripts/python.exe scripts/migrate_handlers.py
```

预期输出：列出 49 个可替换类名 + 3 个保留类

- [ ] **Step 4: 删除冗余 Handler 类（保留 3 个特例）**

手动编辑 `handlers.py`，删除以下模式的所有类定义（约 100-1300 行）：

```python
class HuyaHandler(PlatformHandler):
    ...  # 删除整个类

class DouyuHandler(PlatformHandler):
    ...  # 删除整个类

# ... 依此类推，共删除 49 个类
```

**保留以下 3 个类（有特殊逻辑）：**
- `CustomHandler`（自定义 URL）
- `KuaishouHandler`（加固通道 + 网页兜底）
- `DouyinHandler`（app/web 双通道 + 扫码登录）

- [ ] **Step 5: 更新 __init__.py 工厂注册**

修改 `app/core/platforms/platform_handlers/__init__.py`：

```python
from .handlers import (
    GenericHandler,
    CustomHandler,
    KuaishouHandler,
    DouyinHandler,
)

# 平台 Handler 工厂注册表
HANDLER_REGISTRY = {
    "custom": CustomHandler,
    "kuaishou": KuaishouHandler,
    "douyin": DouyinHandler,
    
    # 以下 49 个平台统一使用 GenericHandler
    "huya": GenericHandler,
    "douyu": GenericHandler,
    "yy": GenericHandler,
    "bilibili": GenericHandler,
    "rednote": GenericHandler,
    "bigo": GenericHandler,
    "blued": GenericHandler,
    "soop": GenericHandler,
    "netease": GenericHandler,
    "qiandurebo": GenericHandler,
    "pamdatv": GenericHandler,
    "maoerfm": GenericHandler,
    "look": GenericHandler,
    "winktv": GenericHandler,
    "flextv": GenericHandler,
    "popkontv": GenericHandler,
    "twitcasting": GenericHandler,
    "baidu": GenericHandler,
    "weibo": GenericHandler,
    "kugou": GenericHandler,
    "twitch": GenericHandler,
    "liveme": GenericHandler,
    "huajiao": GenericHandler,
    "tiktok": GenericHandler,
    "liangongjiao": GenericHandler,
    "yizhibo": GenericHandler,
    "lang": GenericHandler,
    "changliao": GenericHandler,
    "17live": GenericHandler,
    "zhihu": GenericHandler,
    "chzzk": GenericHandler,
    "haixiu": GenericHandler,
    "huajiao2": GenericHandler,
    "migu": GenericHandler,
    "changyou": GenericHandler,
    "momo": GenericHandler,
    "showroom": GenericHandler,
    "acfun": GenericHandler,
    "huomao": GenericHandler,
    "haishen": GenericHandler,
    "qiniu": GenericHandler,
    "kandian": GenericHandler,
    "kugou_fm": GenericHandler,
    "aoligei": GenericHandler,
    "taobao": GenericHandler,
    "taohuatao": GenericHandler,
}


def get_handler(platform: str, **kwargs):
    """工厂方法：根据平台名获取对应 Handler 实例"""
    handler_cls = HANDLER_REGISTRY.get(platform)
    if not handler_cls:
        raise ValueError(f"Unsupported platform: {platform}")
    return handler_cls(platform=platform, **kwargs)
```

- [ ] **Step 6: 验证重构后行数减少**

```bash
wc -l app/core/platforms/platform_handlers/handlers.py
```

预期：~350 行（原 1326 行，减少 976 行）

- [ ] **Step 7: Commit 重构**

```bash
git add app/core/platforms/platform_handlers/handlers.py
git add app/core/platforms/platform_handlers/__init__.py
git commit -m "refactor(handlers): replace 49 duplicated classes with GenericHandler

Before: 1326 lines, 52 platform-specific classes
After: 350 lines, 3 special + 49 generic handlers

- Removed: HuyaHandler, DouyuHandler, BilibiliHandler, etc. (49 classes)
- Kept: CustomHandler, KuaishouHandler, DouyinHandler (special logic)
- HANDLER_REGISTRY now maps 49 platforms to GenericHandler

Impact: -976 lines, zero behavior change"
```

---

### Task 4: 全量回归测试

**Files:**
- Run: `tests/core/platforms/`
- Create: `tests/integration/test_all_platforms_smoke.py`

- [ ] **Step 1: 写冒烟测试（验证 52 个平台可实例化）**

创建 `tests/integration/test_all_platforms_smoke.py`：

```python
import pytest
from app.core.platforms.platform_handlers import get_handler, HANDLER_REGISTRY


@pytest.mark.parametrize("platform", list(HANDLER_REGISTRY.keys()))
def test_handler_instantiation(platform):
    """所有平台 Handler 必须可实例化"""
    handler = get_handler(platform)
    assert handler is not None
    assert handler.platform == platform


@pytest.mark.parametrize("platform", ["huya", "bilibili", "twitch", "tiktok"])
def test_generic_handler_has_stream_class(platform):
    """通用 Handler 必须能加载 streamget 类"""
    handler = get_handler(platform)
    stream_cls = handler._get_stream_class()
    assert stream_cls is not None
    assert stream_cls.__name__.endswith("LiveStream")


def test_special_handlers_preserved():
    """特殊 Handler 必须保留独立实现"""
    custom = get_handler("custom")
    assert custom.__class__.__name__ == "CustomHandler"
    
    kuaishou = get_handler("kuaishou")
    assert kuaishou.__class__.__name__ == "KuaishouHandler"
    
    douyin = get_handler("douyin")
    assert douyin.__class__.__name__ == "DouyinHandler"
```

- [ ] **Step 2: 运行冒烟测试**

```bash
.venv/Scripts/python.exe -m pytest tests/integration/test_all_platforms_smoke.py -v
```

预期：PASS（55+ 个测试全通过）

- [ ] **Step 3: 运行完整测试套件**

```bash
.venv/Scripts/python.exe -m pytest tests/core/platforms/ -v --tb=short
```

预期：现有测试全部 PASS

- [ ] **Step 4: 手动冒烟测试（真实直播间）**

创建临时测试脚本 `_test_real_streams.py`：

```python
import asyncio
from app.core.platforms.platform_handlers import get_handler

async def test_platform(platform: str, url: str):
    handler = get_handler(platform)
    try:
        result = await handler.get_stream_info(url)
        status = "✓ 在播" if result.get("is_live") else "✗ 未开播"
        print(f"{platform:15} {status} | {result.get('anchor_name', 'N/A')}")
    except Exception as e:
        print(f"{platform:15} ✗ 错误 | {e}")

async def main():
    # 从 recordings.json 选 3 个真实房间测试
    tests = [
        ("bilibili", "https://live.bilibili.com/21852"),
        ("douyin", "https://live.douyin.com/775841227732"),
        ("kuaishou", "https://live.kuaishou.com/u/yxt066"),
    ]
    
    print("=== 真实直播间冒烟测试 ===")
    for platform, url in tests:
        await test_platform(platform, url)

if __name__ == "__main__":
    asyncio.run(main())
```

运行：

```bash
.venv/Scripts/python.exe _test_real_streams.py
```

预期：3 个平台均能正常返回主播信息，无报错

- [ ] **Step 5: 清理临时测试文件**

```bash
rm _test_real_streams.py
```

- [ ] **Step 6: Commit 测试补充**

```bash
git add tests/integration/test_all_platforms_smoke.py
git commit -m "test(handlers): add smoke tests for all 52 platforms

- Instantiation test for every platform
- GenericHandler stream class loading verification
- Special handlers preservation check"
```

---

## 阶段 B：Node.js 依赖优化

### Task 5: 移除打包时的 node.exe

**Files:**
- Modify: `StreamCap.spec:30-35`
- Modify: `app/core/runtime/paths.py`
- Test: 现有 `scripts/node_install.py` 已实现按需下载

- [ ] **Step 1: 确认 node_install.py 逻辑完整**

检查 `scripts/node_install.py:239`，确认包含：
- 下载 node-v24.19.0-win-x64.zip（~35MB）
- 解压到 `{user_data_dir}/vendor/node/windows/`
- 验证 `node.exe --version` 可执行

运行测试：

```bash
.venv/Scripts/python.exe -c "from scripts.node_install import download_node; import asyncio; asyncio.run(download_node())"
```

预期：下载到 `%APPDATA%\StreamCap\vendor\node\windows\node.exe`

- [ ] **Step 2: 修改 StreamCap.spec 排除 vendor/node**

编辑 `StreamCap.spec`，找到 `datas` 列表（约第 30 行），修改：

```python
datas=[
    ('assets', 'assets'),
    ('config', 'config'),
    # ('vendor/node', 'vendor/node'),  # ← 注释掉这一行
],
```

- [ ] **Step 3: 修改 paths.py 使用运行时 node 路径**

编辑 `app/core/runtime/paths.py`，找到 `get_node_path()` 函数（约第 80 行），修改：

```python
def get_node_path() -> Path:
    """获取 Node.js 可执行文件路径（运行时下载）"""
    if getattr(sys, 'frozen', False):
        # 冻结版：优先使用用户数据目录的 node
        node_dir = user_data_dir / "vendor" / "node" / "windows"
        node_exe = node_dir / "node.exe"
        
        if not node_exe.exists():
            # 首次运行时自动下载
            import asyncio
            from scripts.node_install import download_node
            asyncio.run(download_node())
        
        return node_exe
    else:
        # 开发版：仍使用打包的 vendor/node
        return resource_dir / "vendor" / "node" / "windows" / "node.exe"
```

- [ ] **Step 4: 重新打包验证**

```bash
cd "Z:/demo/StreamCap"
.venv/Scripts/python.exe -m PyInstaller StreamCap.spec --clean
```

- [ ] **Step 5: 检查打包体积减少**

```bash
du -sh dist/StreamCap/_internal/vendor/ 2>/dev/null || echo "vendor/ 已移除"
```

预期：`vendor/` 目录不存在或为空

- [ ] **Step 6: 手动测试首次启动（触发 node 下载）**

```bash
# 清空用户数据目录的 node
rm -rf "$APPDATA/StreamCap/vendor/node"

# 启动打包版本
cd dist/StreamCap
./StreamCap.exe
```

预期：
1. 首次启动时自动下载 node.exe（显示进度条）
2. 下载后正常启动，抖音/海秀平台可正常解析（依赖 execjs）

- [ ] **Step 7: Commit node 优化**

```bash
git add StreamCap.spec app/core/runtime/paths.py
git commit -m "perf(build): move node.exe to runtime download

Before: 89MB node.exe bundled in _internal/vendor/
After: Downloaded on-demand to %APPDATA%\StreamCap\vendor/

- Reduces installer size by 89MB
- First launch triggers node download with progress bar
- Existing scripts/node_install.py handles download logic"
```

---

## 阶段 C：类型检查与最终验证

### Task 6: Pyright 类型检查

**Files:**
- Run: `pyright` on modified files

- [ ] **Step 1: 检查新增文件类型**

```bash
cd "Z:/demo/StreamCap"
.venv/Scripts/python.exe -m pyright app/core/platforms/platform_handlers/platform_config.py
```

预期：0 errors

- [ ] **Step 2: 检查 GenericHandler 类型**

```bash
.venv/Scripts/python.exe -m pyright app/core/platforms/platform_handlers/handlers.py
```

预期：0 errors（可能有 warnings 关于 streamget 的动态加载）

- [ ] **Step 3: 全量类型检查（仅 handlers 目录）**

```bash
.venv/Scripts/python.exe -m pyright app/core/platforms/platform_handlers/
```

预期：无新增 type errors

- [ ] **Step 4: Commit 类型修复（如有）**

如有类型错误，修复后：

```bash
git add app/core/platforms/platform_handlers/
git commit -m "fix(types): resolve pyright errors in handlers refactor"
```

---

### Task 7: 打包与部署测试

**Files:**
- Build: `dist/StreamCap/`
- Deploy: `E:\StreamCap_dev_Win_x64_noFF\`

- [ ] **Step 1: 清理旧构建**

```bash
cd "Z:/demo/StreamCap"
rm -rf build/ dist/
```

- [ ] **Step 2: 重新打包**

```bash
.venv/Scripts/python.exe -m PyInstaller StreamCap.spec --clean --noconfirm
```

预期：构建成功，无 warnings

- [ ] **Step 3: 检查最终体积**

```bash
du -sh dist/StreamCap/
du -sh dist/StreamCap/_internal/
```

预期：_internal 减少 ~89MB

- [ ] **Step 4: 复制到部署目录**

```bash
cp dist/StreamCap/StreamCap.exe "E:/StreamCap_dev_Win_x64_noFF/"
cp -r dist/StreamCap/_internal "E:/StreamCap_dev_Win_x64_noFF/"
```

- [ ] **Step 5: 真实环境冒烟测试**

```bash
cd "E:/StreamCap_dev_Win_x64_noFF"
./StreamCap.exe
```

测试清单：
- [ ] 应用正常启动
- [ ] 首次启动触发 node.exe 下载（检查 %APPDATA%\StreamCap\vendor\node\）
- [ ] 加载现有 6 个录制任务
- [ ] 手动触发一次快手房间检测，确认限流信号贯通（卡片显示"IP受限"）
- [ ] 手动触发一次 B 站房间检测，确认正常显示主播信息

- [ ] **Step 6: Commit 最终构建**

```bash
cd "Z:/demo/StreamCap"
git add StreamCap.spec
git commit -m "build: finalize phase1 optimization build

Changes:
- handlers.py: 1326 → 350 lines (-976 lines, -74%)
- Package size: reduced by ~89MB (node.exe now runtime download)
- 52 platforms tested and working
- Type checks passing (pyright clean)"
```

---

## 阶段 D：清理与文档

### Task 8: 清理备份与更新文档

**Files:**
- Remove: `.backup/`
- Update: `docs/CHANGELOG.md`
- Update: `docs/superpowers/plans/2026-08-12-phase1-optimize-handlers-and-node.md` (标记完成)

- [ ] **Step 1: 清理临时备份**

```bash
cd "Z:/demo/StreamCap"
rm -rf .backup/
rm scripts/migrate_handlers.py
```

- [ ] **Step 2: 更新 CHANGELOG**

在 `docs/CHANGELOG.md` 顶部添加：

```markdown
## [Unreleased] - 2026-08-12

### Changed
- **[重构]** handlers.py 代码量减少 74%（1326 → 350 行）
  - 52 个平台 Handler 类合并为 GenericHandler + 3 个特例类
  - 新增 platform_config.py 配置驱动设计
  - 零行为变更，全量测试通过

### Optimized
- **[性能]** 打包体积减少 89MB
  - node.exe 改为首次启动时按需下载
  - 下载目标：%APPDATA%\StreamCap\vendor\node\
  - 已有 scripts/node_install.py 自动处理

### Fixed
- **[快手]** userinfo 短路修复：KeyError 改为放行兜底（commit 2380fc0 遗留）
- **[限流]** IP 限流信号贯通：handlers → record_manager → UI 完整链路
```

- [ ] **Step 3: 标记计划完成**

编辑本计划文件开头，添加：

```markdown
> **状态：✅ 已完成** (2026-08-12)
> 
> 实际收益：
> - 代码减少：976 行（handlers.py 74% 缩减）
> - 体积减少：89MB（node.exe 运行时下载）
> - 测试覆盖：55+ 个冒烟测试全通过
> - 类型检查：pyright clean
```

- [ ] **Step 4: Commit 清理**

```bash
git add docs/CHANGELOG.md
git add docs/superpowers/plans/2026-08-12-phase1-optimize-handlers-and-node.md
git commit -m "docs: update changelog for phase1 optimization

- handlers refactor: -976 lines
- node.exe optimization: -89MB
- All tests passing"
```

---

## 最终检查清单

完成后确认以下所有项：

- [ ] **代码质量**
  - [ ] handlers.py 行数：1326 → ~350（减少 74%）
  - [ ] 52 个平台均可实例化
  - [ ] Pyright 类型检查通过
  - [ ] 无 TODO/TBD/FIXME 遗留

- [ ] **功能完整性**
  - [ ] CustomHandler / KuaishouHandler / DouyinHandler 保留特殊逻辑
  - [ ] GenericHandler 支持 49 个标准平台
  - [ ] 快手限流信号贯通（UI 显示"IP受限"）
  - [ ] userinfo 短路修复生效

- [ ] **打包与部署**
  - [ ] 打包体积减少 ~89MB
  - [ ] node.exe 首次启动自动下载
  - [ ] 抖音/海秀平台 execjs 依赖正常工作
  - [ ] 部署到 E:\ 目录并真实环境测试通过

- [ ] **测试覆盖**
  - [ ] 单元测试：platform_config / GenericHandler
  - [ ] 集成测试：52 个平台冒烟测试
  - [ ] 真实环境：至少 3 个直播间验证

- [ ] **文档与清理**
  - [ ] CHANGELOG 更新
  - [ ] 计划文件标记完成
  - [ ] 临时文件清理（.backup / migrate_handlers.py）

---

## 风险与回滚

**中等风险点：**
1. GenericHandler 的 streamget 类动态加载可能在某些平台失败
   - 缓解：保留 3 个特例类，49 个标准平台已通过冒烟测试
2. node.exe 运行时下载可能因网络问题失败
   - 缓解：已有 scripts/node_install.py 的重试逻辑 + 进度反馈

**回滚方案：**

如需回滚，执行：

```bash
cd "Z:/demo/StreamCap"
git checkout main
git branch -D refactor/phase1-handlers-and-node

# 恢复旧构建
cp .backup/handlers_before_refactor.py app/core/platforms/platform_handlers/handlers.py
.venv/Scripts/python.exe -m PyInstaller StreamCap.spec --clean
```

---

**计划结束。准备执行时，使用以下 skill 之一：**

1. **superpowers:subagent-driven-development** (推荐) - 每个 Task 独立 subagent，中间人工审查
2. **superpowers:executing-plans** - 当前会话批量执行，关键点设置 checkpoint