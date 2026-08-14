import json
import tempfile
import unittest
from datetime import timedelta
from pathlib import Path
from types import SimpleNamespace
from typing import Any, cast

from app.core.config.config_manager import ConfigManager
from app.core.recording.record_manager import GlobalRecordingState, RecordingManager
from app.core.recording.stream_manager import LiveStreamRecorder
from app.initialization.installation_manager import InstallationManager, InstallComponent
from app.models.recording.recording_model import Recording
from app.models.recording.recording_status_model import RecordingStatus
from app.ui.components.business.recording_dialog import RecordingDialog
from app.ui.components.common.save_progress_overlay import SaveProgressOverlay


def make_recording(
    rec_id: str = "recording",
    url: str = "https://live.douyin.com/1",
    quality: str = "HD",
) -> Recording:
    return Recording(
        rec_id=rec_id,
        url=url,
        streamer_name="streamer",
        record_format="TS",
        quality=quality,
        segment_record=False,
        segment_time="1800",
        monitor_status=True,
        scheduled_recording=False,
        scheduled_start_time="",
        monitor_hours="",
        recording_dir="",
        enabled_message_push=True,
        only_notify_no_record=False,
        flv_use_direct_download=False,
    )


class DummyServices:
    def __init__(self, fail_saves: bool = False) -> None:
        self.fail_saves = fail_saves
        self.saved: list[list[dict[str, Any]]] = []
        self.broadcasts: list[tuple[str, str]] = []
        self.settings_config = SimpleNamespace(
            user_config={
                "record_quality": "HD",
                "video_format": "TS",
                "segmented_recording_enabled": False,
                "video_segment_time": "1800",
                "flv_use_direct_download": False,
                "only_notify_no_record": False,
                "loop_time_seconds": 300,
            }
        )
        self.config_manager = self

    async def save_recordings_config(self, data: list[dict[str, Any]]) -> None:
        if self.fail_saves:
            raise OSError("simulated persistence failure")
        self.saved.append(data)

    def snapshot_bridges(self) -> list[Any]:
        return []

    def broadcast_card_update(self, recording: Recording) -> None:
        self.broadcasts.append(("card", recording.rec_id))

    def broadcast_pubsub(self, topic: str, recording: Recording) -> None:
        self.broadcasts.append((topic, recording.rec_id))


def make_manager(services: DummyServices) -> RecordingManager:
    manager = cast(RecordingManager, object.__new__(RecordingManager))
    manager.services = cast(Any, services)
    manager.settings = cast(Any, services.settings_config)
    manager.active_recorders = {}
    manager._ = {"OD": "Original", "UHD": "Ultra", "HD": "High"}
    return manager


class RecordingManagerRegressionTests(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        GlobalRecordingState.recordings = []

    def tearDown(self) -> None:
        GlobalRecordingState.recordings = []

    def test_offline_monitored_item_is_editable_but_active_items_are_not(self) -> None:
        manager = make_manager(DummyServices())
        offline = make_recording("offline")
        offline.status_info = RecordingStatus.MONITORING
        assert manager.can_edit_recording(offline)

        for attr in ("is_checking", "is_live", "is_recording", "stopping_in_progress"):
            recording = make_recording(attr)
            setattr(recording, attr, True)
            assert not manager.can_edit_recording(recording), attr

        preparing = make_recording("preparing")
        preparing.status_info = RecordingStatus.PREPARING_RECORDING
        assert not manager.can_edit_recording(preparing)

        active = make_recording("active")
        manager.active_recorders[active.rec_id] = cast(Any, object())
        assert not manager.can_edit_recording(active)

    async def test_batch_edit_skips_live_items_and_keeps_specified_values(self) -> None:
        services = DummyServices()
        manager = make_manager(services)
        offline = make_recording("offline")
        live = make_recording("live")
        live.is_live = True
        GlobalRecordingState.recordings = [offline, live]

        applied, skipped = await manager.batch_edit_recordings({"quality": "UHD"}, set())

        assert (applied, skipped) == (1, 1)
        assert offline.quality == "UHD"
        assert "quality" not in offline.inherited_fields
        assert live.quality == "HD"

    async def test_failed_batch_and_single_saves_restore_previous_values(self) -> None:
        manager = make_manager(DummyServices(fail_saves=True))
        recording = make_recording("rollback")
        GlobalRecordingState.recordings = [recording]

        try:
            await manager.batch_edit_recordings({"quality": "UHD"}, set())
        except OSError:
            pass
        else:
            raise AssertionError("batch save failure was swallowed")
        assert recording.quality == "HD"

        try:
            await manager.update_recording_card(
                recording,
                {"url": "https://www.huya.com/123", "quality": "UHD"},
            )
        except OSError:
            pass
        else:
            raise AssertionError("single save failure was swallowed")
        assert recording.url == "https://live.douyin.com/1"
        assert recording.quality == "HD"

    async def test_editing_url_updates_platform_metadata(self) -> None:
        manager = make_manager(DummyServices())
        recording = make_recording("platform")
        GlobalRecordingState.recordings = [recording]

        await manager.update_recording_card(recording, {"url": "https://www.huya.com/123"})

        assert (recording.platform, recording.platform_key) == ("\u864e\u7259\u76f4\u64ad", "huya")

    def test_null_inherited_values_round_trip_as_null(self) -> None:
        manager = make_manager(DummyServices())
        recording = Recording.from_dict(
            {
                "rec_id": "inherited",
                "url": "https://example.com/live.m3u8",
                "streamer_name": "streamer",
                "record_format": None,
                "quality": None,
                "segment_record": None,
                "segment_time": None,
                "monitor_status": True,
                "scheduled_recording": False,
                "scheduled_start_time": "",
                "monitor_hours": "",
                "recording_dir": "",
                "enabled_message_push": True,
                "only_notify_no_record": None,
                "flv_use_direct_download": None,
            }
        )

        manager.apply_global_defaults(recording)
        stored = recording.to_dict()

        assert recording.quality == "HD"
        assert recording.record_format == "TS"
        assert stored["quality"] is None
        assert stored["record_format"] is None


class SupportingRegressionTests(unittest.IsolatedAsyncioTestCase):
    async def test_config_save_errors_are_not_swallowed(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            missing_path = Path(temp_dir) / "missing" / "config.json"
            try:
                await ConfigManager._save_config(str(missing_path), {}, "saved", "failed")
            except OSError:
                pass
            else:
                raise AssertionError("config save failure was swallowed")

    def test_batch_parser_does_not_leak_values_between_lines(self) -> None:
        assert RecordingDialog._parse_batch_line("2,https://a.example/live,A") == (
            "HD",
            "https://a.example/live",
            "A",
        )
        assert RecordingDialog._parse_batch_line("https://b.example/live") == (
            "OD",
            "https://b.example/live",
            "",
        )
        assert RecordingDialog._parse_batch_line("3,https://c.example/live") == (
            "SD",
            "https://c.example/live",
            "",
        )

    def test_custom_script_command_preserves_paths_with_spaces(self) -> None:
        command = LiveStreamRecorder._split_script_command('python "/tmp/My Scripts/hook.py"')
        assert command == ["python", "/tmp/My Scripts/hook.py"]

    def test_last_duration_round_trip(self) -> None:
        recording = make_recording("duration")
        recording.last_duration = timedelta(seconds=123.5)

        restored = Recording.from_dict(recording.to_dict())

        assert restored.last_duration.total_seconds() == 123.5

    async def test_flet_overlays_and_install_dialog_construct_without_a_page_session(self) -> None:
        language = json.loads(Path("locales/zh_CN.json").read_text(encoding="utf-8"))
        language_manager = SimpleNamespace(language=language, add_observer=lambda _observer: None)
        overlay = SaveProgressOverlay(SimpleNamespace(language_manager=language_manager))
        overlay._initialize_components()
        assert overlay.cancel_button.content
        assert overlay.overlay.controls

        class FakeWindow:
            height = 800

        class FakePage:
            web = False
            height = 800
            window = FakeWindow()

            def __init__(self) -> None:
                self.overlay: list[Any] = []
                self.update_count = 0

            def update(self) -> None:
                self.update_count += 1

        page = FakePage()
        app = SimpleNamespace(page=page, language_manager=language_manager)
        installation = InstallationManager(app)

        async def check_component() -> bool:
            return False

        async def install_component(_callback: Any) -> bool:
            return True

        component: InstallComponent = {
            "name": "Test",
            "check_func": check_component,
            "install_func": install_component,
        }
        installation.components_to_install = [component]

        await installation.show_install_dialog()
        await installation.update_component_progress("Test", 1.0, "Complete")

        assert installation.install_dialog.open
        assert len(installation.install_dialog.actions) == 2
        assert all(getattr(action, "content", None) for action in installation.install_dialog.actions)


if __name__ == "__main__":
    unittest.main()
