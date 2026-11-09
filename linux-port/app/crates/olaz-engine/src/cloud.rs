//! zCloud: plan, usage, the stored media / files by conversation, preview / save, delete, offload. Port of
//! `ZaloEngine+Cloud.swift`, bridged to `window.__zaloCloud` (posts `{zcloud:{…}}`). Writes follow a click.

use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::media::MediaResolution;
use crate::models::J;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloudItemKind {
    Photo,
    Video,
    File,
    Voice,
    Other,
}

impl CloudItemKind {
    pub const ALL: [CloudItemKind; 5] = [CloudItemKind::Photo, CloudItemKind::Video, CloudItemKind::File, CloudItemKind::Voice, CloudItemKind::Other];
    pub fn raw(self) -> &'static str {
        match self {
            CloudItemKind::Photo => "photo",
            CloudItemKind::Video => "video",
            CloudItemKind::File => "file",
            CloudItemKind::Voice => "voice",
            CloudItemKind::Other => "other",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "photo" => CloudItemKind::Photo,
            "video" => CloudItemKind::Video,
            "file" => CloudItemKind::File,
            "voice" => CloudItemKind::Voice,
            _ => CloudItemKind::Other,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            CloudItemKind::Photo => "Ảnh",
            CloudItemKind::Video => "Video",
            CloudItemKind::File => "File",
            CloudItemKind::Voice => "Tin nhắn thoại",
            CloudItemKind::Other => "Khác",
        }
    }
    pub fn symbol(self) -> &'static str {
        match self {
            CloudItemKind::Photo => "photo.fill",
            CloudItemKind::Video => "play.rectangle.fill",
            CloudItemKind::File => "doc.fill",
            CloudItemKind::Voice => "waveform",
            CloudItemKind::Other => "questionmark.square.fill",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CloudItem {
    pub id: String,
    pub kind: CloudItemKind,
    pub conversation_id: String,
    pub conversation_name: String,
    pub size: i64,
    pub date: Option<f64>,
    pub title: String,
    pub ext: String,
    /// Seconds.
    pub duration: f64,
    pub width: i64,
    pub height: i64,
}

impl CloudItem {
    pub fn from_json(j: &Value) -> Option<Self> {
        let key = j.s("key");
        if key.is_empty() {
            return None;
        }
        Some(CloudItem {
            id: key,
            kind: CloudItemKind::parse(&j.s("kind")),
            conversation_id: j.s("conv"),
            conversation_name: j.s("convName"),
            size: int(j.get_v("size")),
            date: date(j.get_v("date")),
            title: j.s("title"),
            ext: j.s("ext"),
            duration: j.get_v("duration").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0,
            width: int(j.get_v("width")),
            height: int(j.get_v("height")),
        })
    }
}

fn int(v: Option<&Value>) -> i64 {
    v.and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0)
}
fn date(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|ms| *ms > 0.0)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CloudPage {
    pub items: Vec<CloudItem>,
    pub has_more: bool,
    pub error: Option<String>,
}

pub const NOT_CONNECTED: &str = "Chưa kết nối được tới zCloud.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CloudBreakdown {
    pub photo: i64,
    pub video: i64,
    pub file: i64,
    pub voice: i64,
}

impl CloudBreakdown {
    fn from_json(v: Option<&Value>) -> Self {
        let d = v.cloned().unwrap_or(Value::Null);
        CloudBreakdown { photo: int(d.get_v("photo")), video: int(d.get_v("video")), file: int(d.get_v("file")), voice: int(d.get_v("voice")) }
    }
    pub fn total(&self) -> i64 {
        self.photo + self.video + self.file + self.voice
    }
    pub fn bytes(&self, k: CloudItemKind) -> i64 {
        match k {
            CloudItemKind::Photo => self.photo,
            CloudItemKind::Video => self.video,
            CloudItemKind::File => self.file,
            CloudItemKind::Voice => self.voice,
            CloudItemKind::Other => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlanKind {
    Paid,
    #[default]
    Free,
    Grace,
    AfterGrace,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CloudPlan {
    pub kind: PlanKind,
    pub name: String,
    pub quota: i64,
    pub usage: i64,
    pub family: i64,
    pub cloud_media: i64,
    pub message_backup: i64,
    pub my_cloud: i64,
    pub expire_at: Option<f64>,
    pub auto_renew: Option<bool>,
    pub months: i64,
    pub has_key: bool,
    pub backup_media: bool,
    pub backup_my_cloud: bool,
    pub backup_messages: bool,
    pub file_limit: i64,
}

impl CloudPlan {
    pub fn remaining(&self) -> i64 {
        (self.quota - self.usage - self.family).max(0)
    }
    pub fn is_paid(&self) -> bool {
        matches!(self.kind, PlanKind::Paid | PlanKind::Grace)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CloudConversation {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub is_group: bool,
    pub total: i64,
    pub last_activity: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CloudSettings {
    pub offload_enabled: bool,
    pub offload_available: bool,
    pub last_offload_at: Option<f64>,
    pub last_offload_size: i64,
    pub ttl_days: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CloudState {
    pub plan: Option<CloudPlan>,
    pub media: CloudBreakdown,
    pub my_cloud_types: CloudBreakdown,
    pub conversations: Vec<CloudConversation>,
    pub settings: CloudSettings,
    pub changed: i64,
    pub loaded: bool,
    pub hidden_lookup: bool,
    pub self_id: Option<String>,
}

impl Default for CloudState {
    fn default() -> Self {
        CloudState {
            plan: None,
            media: Default::default(),
            my_cloud_types: Default::default(),
            conversations: vec![],
            settings: Default::default(),
            changed: 0,
            loaded: false,
            hidden_lookup: true,
            self_id: None,
        }
    }
}

impl CloudState {
    pub fn apply(&mut self, j: &Value) {
        if let Some(id) = j.opt_s("selfId").filter(|i| !i.is_empty()) {
            self.self_id = Some(id);
        }
        if let Some(p) = j.get_v("plan").filter(|p| p.is_object()) {
            let opt = p.get_v("optIn").cloned().unwrap_or(Value::Null);
            self.plan = Some(CloudPlan {
                kind: match p.s("kind").as_str() {
                    "paid" => PlanKind::Paid,
                    "grace" => PlanKind::Grace,
                    "afterGrace" => PlanKind::AfterGrace,
                    _ => PlanKind::Free,
                },
                name: p.opt_s("name").unwrap_or_else(|| "zCloud".into()),
                quota: int(p.get_v("quota")),
                usage: int(p.get_v("usage")),
                family: int(p.get_v("family")),
                cloud_media: int(p.get_v("cloudMedia")),
                message_backup: int(p.get_v("messageBackup")),
                my_cloud: int(p.get_v("myCloud")),
                expire_at: date(p.get_v("expireAt")),
                auto_renew: p.get_v("autoRenew").and_then(Value::as_bool),
                months: int(p.get_v("months")),
                has_key: p.b("hasKey"),
                backup_media: opt.b("media"),
                backup_my_cloud: opt.b("myCloud"),
                backup_messages: opt.b("backup"),
                file_limit: int(p.get_v("fileLimit")),
            });
        }
        if let Some(t) = j.get_v("types").filter(|t| t.is_object()) {
            self.media = CloudBreakdown::from_json(t.get_v("media"));
            self.my_cloud_types = CloudBreakdown::from_json(t.get_v("myCloud"));
        }
        if let Some(rows) = j.get_v("conversations").and_then(Value::as_array) {
            self.conversations = rows
                .iter()
                .filter_map(|r| {
                    let id = r.s("id");
                    (!id.is_empty()).then(|| CloudConversation {
                        id,
                        name: r.s("name"),
                        avatar: r.opt_s("avatar").filter(|a| !a.is_empty()),
                        is_group: r.b("isGroup"),
                        total: int(r.get_v("total")),
                        last_activity: date(r.get_v("lastTs")),
                    })
                })
                .collect();
        }
        if let Some(s) = j.get_v("settings").filter(|s| s.is_object()) {
            self.settings = CloudSettings {
                offload_enabled: s.b("offloadEnabled"),
                offload_available: s.b("offloadAvailable"),
                last_offload_at: date(s.get_v("lastOffloadAt")),
                last_offload_size: int(s.get_v("lastOffloadSize")),
                ttl_days: int(s.get_v("ttlDays")),
            };
        }
        if let Some(c) = j.get_v("changed").and_then(Value::as_i64) {
            self.changed = c;
        }
        if j.get_v("loaded").and_then(Value::as_bool) == Some(true) {
            self.loaded = true;
        }
        if let Some(l) = j.get_v("hiddenLookup").and_then(Value::as_bool) {
            self.hidden_lookup = l;
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CloudFile {
    pub path: String,
    pub kind: CloudItemKind,
    pub title: String,
    pub ext: String,
    pub size: i64,
    pub date: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CloudFetch {
    Ready(CloudFile),
    Expired,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CloudActionResult {
    pub ok: bool,
    pub deleted: Vec<String>,
    pub failed: Vec<String>,
    pub error: Option<String>,
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudSort {
    Date,
    Size,
}

impl ZaloEngine {
    async fn call_cloud(&self, method: &str, args: Vec<Value>) -> Option<Value> {
        let json = Value::Array(args).to_string();
        self.race(90, self.evaluate(&format!("const c = window.__zaloCloud; if (!c || !c.{method}) return null; return await c.{method}(...{json});")))
            .await
            .flatten()
    }

    pub fn refresh_cloud(&self) {
        self.call_chat("refresh", vec![], "__zaloCloud");
    }

    pub async fn cloud_list(&self, conv: Option<&str>, kind: Option<CloudItemKind>, sort: CloudSort, ascending: bool, offset: usize, limit: usize) -> CloudPage {
        let opts = json!({
            "convId": conv.unwrap_or(""), "kind": kind.map(|k| k.raw()).unwrap_or(""),
            "sort": if sort == CloudSort::Size { "size" } else { "date" }, "dir": if ascending { "asc" } else { "desc" },
            "offset": offset, "limit": limit,
        });
        let Some(j) = self.call_cloud("list", vec![opts]).await.filter(|j| j.is_object()) else {
            return CloudPage { items: vec![], has_more: false, error: Some(NOT_CONNECTED.into()) };
        };
        CloudPage { items: j.arr("items").iter().filter_map(CloudItem::from_json).collect(), has_more: j.b("hasMore"), error: j.opt_s("error") }
    }

    pub async fn cloud_thumbnail(&self, key: &str) -> MediaResolution {
        MediaResolution::from_json(self.call_cloud("thumb", vec![json!(key)]).await.as_ref())
    }

    pub async fn cloud_fetch(&self, key: &str) -> CloudFetch {
        let Some(j) = self.call_cloud("fetch", vec![json!(key)]).await.filter(|j| j.is_object()) else { return CloudFetch::Failed(NOT_CONNECTED.into()) };
        match j.s("state").as_str() {
            "ok" => {
                let path = j.s("path");
                if !path.starts_with('/') {
                    return CloudFetch::Failed("Không tìm thấy tệp đã tải.".into());
                }
                CloudFetch::Ready(CloudFile { path, kind: CloudItemKind::parse(&j.s("kind")), title: j.s("title"), ext: j.s("ext"), size: int(j.get_v("size")), date: date(j.get_v("date")) })
            }
            "expired" => CloudFetch::Expired,
            _ => CloudFetch::Failed("Không tải được nội dung này lúc này. Vui lòng thử lại sau.".into()),
        }
    }

    pub async fn cloud_delete(&self, keys: Vec<String>) -> CloudActionResult {
        let Some(j) = self.call_cloud("deleteItems", vec![json!(keys)]).await.filter(|j| j.is_object()) else {
            return CloudActionResult { ok: false, deleted: vec![], failed: keys, error: Some(NOT_CONNECTED.into()) };
        };
        CloudActionResult { ok: j.b("ok"), deleted: strings(j.get_v("deleted")), failed: strings(j.get_v("failed")), error: j.opt_s("error").filter(|e| !e.is_empty()) }
    }

    pub async fn cloud_set_offload(&self, enabled: bool) -> CloudActionResult {
        let Some(j) = self.call_cloud("setOffload", vec![json!(enabled)]).await.filter(|j| j.is_object()) else {
            return CloudActionResult { ok: false, error: Some(NOT_CONNECTED.into()), ..Default::default() };
        };
        CloudActionResult { ok: j.b("ok"), error: j.opt_s("error"), ..Default::default() }
    }

    pub fn reset_cloud(&self) {
        self.state.borrow_mut().cloud = CloudState::default();
    }

    pub(crate) fn handle_cloud(&self, state: &Map<String, Value>) {
        let Some(j) = state.get("zcloud").filter(|z| z.is_object()) else { return };
        let mut next = self.state.borrow().cloud.clone();
        next.apply(j);
        if next != self.state.borrow().cloud {
            self.state.borrow_mut().cloud = next;
            self.changed(Change::Other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_state() {
        let mut s = CloudState::default();
        s.apply(&json!({"selfId": "1", "plan": {"kind": "paid", "quota": 100, "usage": 30, "family": 80, "optIn": {"media": true}},
            "types": {"media": {"photo": 5, "video": 7}}, "conversations": [{"id": "c", "total": 9, "lastTs": 0}, {"id": ""}],
            "settings": {"offloadEnabled": true, "ttlDays": 30}, "changed": 3, "loaded": true}));
        let p = s.plan.clone().unwrap();
        assert!(p.is_paid() && p.backup_media && p.remaining() == 0 && p.name == "zCloud");
        assert_eq!((s.media.total(), s.media.bytes(CloudItemKind::Video)), (12, 7));
        assert_eq!(s.conversations.len(), 1);
        assert!(s.conversations[0].last_activity.is_none());
        assert!(s.loaded && s.settings.offload_enabled && s.changed == 3 && s.hidden_lookup);
        s.apply(&json!({"hiddenLookup": false}));
        assert!(!s.hidden_lookup && s.plan.is_some(), "partial snapshots keep the rest");
        let item = CloudItem::from_json(&json!({"key": "k", "kind": "voice", "duration": 2500, "size": 10})).unwrap();
        assert_eq!((item.kind, item.duration), (CloudItemKind::Voice, 2.5));
        assert!(CloudItem::from_json(&json!({"key": ""})).is_none());
    }
}
