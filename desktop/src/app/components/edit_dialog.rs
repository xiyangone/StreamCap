use super::Dialog;
use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording},
    app::labels::{valid_url, FORMAT_OPTIONS, QUALITY_OPTIONS},
};
use leptos::prelude::*;
use serde_json::{Map, Value};

pub const KEEP: &str = "__keep";
pub const GLOBAL: &str = "__global";
#[derive(Clone, Copy)]
pub struct RecordingDraft {
    quality: RwSignal<String>,
    format: RwSignal<String>,
    segment: RwSignal<String>,
    time: RwSignal<String>,
    bitrate: RwSignal<String>,
    schedule: RwSignal<String>,
    starts: RwSignal<String>,
    hours: RwSignal<String>,
    push: RwSignal<String>,
    notify_only: RwSignal<String>,
    direct: RwSignal<String>,
}
impl RecordingDraft {
    pub fn new(batch: bool) -> Self {
        let initial = if batch { KEEP } else { GLOBAL };
        Self {
            quality: RwSignal::new(initial.into()),
            format: RwSignal::new(initial.into()),
            segment: RwSignal::new(initial.into()),
            time: RwSignal::new(initial.into()),
            bitrate: RwSignal::new(if batch { KEEP.into() } else { String::new() }),
            schedule: RwSignal::new(KEEP.into()),
            starts: RwSignal::new(KEEP.into()),
            hours: RwSignal::new(KEEP.into()),
            push: RwSignal::new(KEEP.into()),
            notify_only: RwSignal::new(KEEP.into()),
            direct: RwSignal::new(KEEP.into()),
        }
    }
    pub fn reset(self) {
        for field in [
            self.quality,
            self.format,
            self.segment,
            self.time,
            self.bitrate,
            self.schedule,
            self.starts,
            self.hours,
            self.push,
            self.notify_only,
            self.direct,
        ] {
            field.set(KEEP.into());
        }
    }
    pub fn load(self, rec: &Recording) {
        self.schedule.set(
            rec.scheduled_recording
                .map(|v| v.to_string())
                .unwrap_or(KEEP.into()),
        );
        self.starts
            .set(rec.scheduled_start_time.clone().unwrap_or(KEEP.into()));
        self.hours
            .set(rec.monitor_hours.clone().unwrap_or(KEEP.into()));
        self.push.set(
            rec.enabled_message_push
                .map(|v| v.to_string())
                .unwrap_or(KEEP.into()),
        );
        self.notify_only
            .set(if rec.inherits("only_notify_no_record") {
                GLOBAL.into()
            } else {
                rec.only_notify_no_record
                    .map(|v| v.to_string())
                    .unwrap_or(KEEP.into())
            });
        self.direct.set(if rec.inherits("flv_use_direct_download") {
            GLOBAL.into()
        } else {
            rec.flv_use_direct_download
                .map(|v| v.to_string())
                .unwrap_or(KEEP.into())
        });
        self.quality.set(if rec.inherits("quality") {
            GLOBAL.into()
        } else {
            rec.quality.clone().unwrap_or(GLOBAL.into())
        });
        self.format.set(if rec.inherits("record_format") {
            GLOBAL.into()
        } else {
            rec.record_format.clone().unwrap_or(GLOBAL.into())
        });
        self.segment.set(if rec.inherits("segment_record") {
            GLOBAL.into()
        } else {
            rec.segment_record
                .map(|v| v.to_string())
                .unwrap_or(GLOBAL.into())
        });
        self.time.set(if rec.inherits("segment_time") {
            GLOBAL.into()
        } else {
            rec.segment_time.clone().unwrap_or(GLOBAL.into())
        });
        self.bitrate
            .set(rec.video_bitrate.map(|v| v.to_string()).unwrap_or_default());
    }
    pub fn collect(self) -> Result<(Map<String, Value>, Vec<String>), String> {
        let mut changes = Map::new();
        let mut follow = Vec::new();
        for (value, api_key, field) in [
            (self.quality.get_untracked(), "quality", "quality"),
            (self.format.get_untracked(), "recordFormat", "record_format"),
            (self.time.get_untracked(), "segmentTime", "segment_time"),
        ] {
            if value == KEEP {
                continue;
            }
            if value == GLOBAL || value.trim().is_empty() {
                follow.push(field.into());
                continue;
            }
            if api_key == "segmentTime"
                && value
                    .parse::<u64>()
                    .ok()
                    .filter(|v| *v > 0 && *v <= 86400)
                    .is_none()
            {
                return Err(t("分段时长应为 1–86400 秒").into());
            }
            changes.insert(api_key.into(), Value::String(value));
        }
        let segment = self.segment.get_untracked();
        if segment == GLOBAL {
            follow.push("segment_record".into());
        } else if segment != KEEP {
            changes.insert("segmentRecord".into(), Value::Bool(segment == "true"));
        }
        let bitrate = self.bitrate.get_untracked();
        if bitrate != KEEP {
            let value = if bitrate.trim().is_empty() {
                Value::Null
            } else {
                let n = bitrate
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n > 0 && *n <= 1_000_000)
                    .ok_or(t("码率应为正整数，或留空复制源流"))?;
                Value::from(n)
            };
            changes.insert("videoBitrate".into(), value);
        }
        for (signal, key, field) in [
            (self.schedule, "scheduledRecording", ""),
            (self.push, "enabledMessagePush", ""),
            (
                self.notify_only,
                "onlyNotifyNoRecord",
                "only_notify_no_record",
            ),
            (
                self.direct,
                "flvUseDirectDownload",
                "flv_use_direct_download",
            ),
        ] {
            let value = signal.get_untracked();
            if value == GLOBAL && !field.is_empty() {
                follow.push(field.into());
            } else if value != KEEP {
                changes.insert(key.into(), Value::Bool(value == "true"));
            }
        }
        for (signal, key) in [
            (self.starts, "scheduledStartTime"),
            (self.hours, "monitorHours"),
        ] {
            let value = signal.get_untracked();
            if value != KEEP {
                changes.insert(key.into(), Value::String(value));
            }
        }
        Ok((changes, follow))
    }
}

#[component]
pub fn RecordingFields(draft: RecordingDraft, #[prop(optional)] batch: bool) -> impl IntoView {
    view! {
        <div class="form-grid">
            <label class="field"><span>{t("清晰度")}</span><select class="input" prop:value=move || draft.quality.get() on:change=move |e| draft.quality.set(event_target_value(&e))>
                {batch.then(|| view! { <option value=KEEP>{t("保持原值")}</option> })}<option value=GLOBAL>{t("跟随全局")}</option>
                {QUALITY_OPTIONS.iter().map(|(code,label)| view! { <option value=*code>{*label}</option> }).collect_view()}
            </select></label>
            <label class="field"><span>{t("录制格式")}</span><select class="input" prop:value=move || draft.format.get() on:change=move |e| draft.format.set(event_target_value(&e))>
                {batch.then(|| view! { <option value=KEEP>{t("保持原值")}</option> })}<option value=GLOBAL>{t("跟随全局")}</option>
                {FORMAT_OPTIONS.iter().map(|format| view! { <option value=*format>{*format}</option> }).collect_view()}
            </select></label>
            <label class="field"><span>{t("分段录制")}</span><select class="input" prop:value=move || draft.segment.get() on:change=move |e| draft.segment.set(event_target_value(&e))>
                {batch.then(|| view! { <option value=KEEP>{t("保持原值")}</option> })}<option value=GLOBAL>{t("跟随全局")}</option><option value="true">{t("开启")}</option><option value="false">{t("关闭")}</option>
            </select></label>
            <div class="field-group"><label class="field"><span>{t("分段时长")}<small>{t("秒")}</small></span><input class="input" inputmode="numeric" placeholder=move || if draft.time.get() == KEEP { t("保持原值") } else { t("跟随全局") }
                prop:value=move || { let v = draft.time.get(); if v.starts_with("__") { String::new() } else { v } } on:input=move |e| draft.time.set(event_target_value(&e)) /></label>
                <div class="field-tools"><button class="text-button" type="button" on:click=move |_| draft.time.set(GLOBAL.into())>{t("跟随全局")}</button>{batch.then(|| view! { <button class="text-button" type="button" on:click=move |_| draft.time.set(KEEP.into())>{t("保持原值")}</button> })}</div>
            </div>
            <div class="field-group full-width"><label class="field"><span>{t("自定义码率")}<small>"kbps"</small></span><input class="input" inputmode="numeric" placeholder=move || if draft.bitrate.get() == KEEP { t("保持原值") } else { t("复制源流，不重新编码") }
                prop:value=move || { let v = draft.bitrate.get(); if v == KEEP { String::new() } else { v } } on:input=move |e| draft.bitrate.set(event_target_value(&e)) /></label>
                <div class="field-tools"><button class="text-button" type="button" on:click=move |_| draft.bitrate.set(String::new())>{t("复制源流")}</button>{batch.then(|| view! { <button class="text-button" type="button" on:click=move |_| draft.bitrate.set(KEEP.into())>{t("保持原值")}</button> })}</div>
            </div>

            <div class="full-width form-divider" />
            {[(draft.schedule,t("定时录制"),false),(draft.push,t("任务通知"),false),(draft.notify_only,t("仅通知不录制"),true),(draft.direct,t("FLV 直下"),true)].into_iter().map(|(signal,label,inherited)|view!{<label class="field"><span>{label}</span><select class="input" prop:value=move||signal.get() on:change=move|e|signal.set(event_target_value(&e))><option value=KEEP>{t("保持原值")}</option>{inherited.then(||view!{<option value=GLOBAL>{t("跟随全局")}</option>})}<option value="true">{t("开启")}</option><option value="false">{t("关闭")}</option></select></label>}).collect_view()}
            {[(draft.starts,t("定时开始时间"),t("例如 08:00,23:00")),(draft.hours,t("对应监控小时数"),t("例如 2,3；跨午夜也有效"))].into_iter().map(|(signal,label,hint)|view!{<label class="field"><span>{label}</span><input class="input" placeholder=hint prop:value=move||{let value=signal.get();if value==KEEP{String::new()}else{value}} on:input=move|e|signal.set(event_target_value(&e))/><small>{hint}</small>{batch.then(||view!{<button type="button" class="text-button" on:click=move |_|signal.set(KEEP.into())>{t("保持原值")}</button>})}</label>}).collect_view()}
        </div>
    }
}

#[component]
pub fn EditRecordingDialog(target: RwSignal<Option<Recording>>) -> impl IntoView {
    let state = gateway::app_state();
    let draft = RecordingDraft::new(false);
    let name = RwSignal::new(String::new());
    let url = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    Effect::new(move |_| {
        if let Some(rec) = target.get() {
            draft.load(&rec);
            name.set(rec.streamer_name);
            url.set(rec.url);
            error.set(None);
        }
    });
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let Some(rec) = target.get_untracked() else {
            return;
        };
        if !valid_url(url.get_untracked().trim()) {
            error.set(Some(t("请输入完整的 HTTP / HTTPS 地址").into()));
            return;
        }
        let (mut changes, follow) = match draft.collect() {
            Ok(data) => data,
            Err(message) => {
                error.set(Some(message));
                return;
            }
        };
        changes.insert(
            "streamerName".into(),
            Value::String(name.get_untracked().trim().into()),
        );
        changes.insert(
            "url".into(),
            Value::String(url.get_untracked().trim().into()),
        );
        busy.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::update_recording(&rec.rec_id, changes, follow).await {
                Ok(()) => {
                    let _ = target.try_set(None);
                    state.notify(t("任务设置已保存"));
                    if let Err(message) = gateway::refresh_recordings(state).await {
                        state.fail(message);
                    }
                }
                Err(message) => {
                    let _ = error.try_set(Some(message));
                }
            }
            let _ = busy.try_set(false);
        });
    };
    view! {
        <Dialog open=Signal::derive(move || target.get().is_some()) title=t("编辑直播间") busy=busy on_close=Callback::new(move |_| target.set(None))>
            <p class="dialog-description">{t("为这个直播间单独设置录制偏好，也可保持跟随全局。")}</p>
            <form on:submit=submit><fieldset disabled=move || busy.get()>
                <label class="field"><span>{t("主播名称")}</span><input class="input" autofocus prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e)) /></label>
                <label class="field"><span>{t("直播间地址")}</span><input class="input" prop:value=move || url.get() on:input=move |e| url.set(event_target_value(&e)) /></label>
                <div class="form-divider" />
                <RecordingFields draft=draft />
            </fieldset>
            <Show when=move || error.get().is_some()><p class="field-error" role="alert">{move || error.get().unwrap_or_default()}</p></Show>
            <footer class="modal-actions"><button class="button secondary" type="button" disabled=move || busy.get() on:click=move |_| target.set(None)>{t("取消")}</button><button class="button primary" type="submit" disabled=move || busy.get()>{move || if busy.get() { t("保存中…") } else { t("保存修改") }}</button></footer>
            </form>
        </Dialog>
    }
}
