//! Group create: limits, the friend roster, selection rules, the avatar picture and the official create call. Port of
//! `ZaloEngine+GroupCreate.swift`, bridged to `window.__zaloGroupCreate`.

use std::collections::HashSet;
use std::time::Duration;

use serde_json::{Value, json};

use crate::contacts::{Friend, fold};
use crate::conversation_actions::ToastKind;
use crate::engine::{ConversationSummary, ZaloEngine};
use crate::models::J;

#[derive(Debug, Clone, PartialEq)]
pub struct GroupCreateLimits {
    pub max_members: usize,
    pub max_name_length: usize,
    pub name_pattern: String,
    pub blocked: HashSet<String>,
    pub official_accounts: HashSet<String>,
}

impl Default for GroupCreateLimits {
    fn default() -> Self {
        GroupCreateLimits { max_members: 100, max_name_length: 50, name_pattern: r"`|~|!|@|#|%|\*|\^|\$".into(), blocked: HashSet::new(), official_accounts: HashSet::new() }
    }
}

pub const BLOCKED_TEXT: &str = "Bỏ chặn để thêm người này";
pub const INVALID_NAME_TEXT: &str = "Tên nhóm chứa ký tự không hợp lệ. Vui lòng thử lại.";

impl GroupCreateLimits {
    pub fn from_json(v: Option<&Value>) -> Option<Self> {
        let root = v?;
        if root.s("status") != "ok" {
            return None;
        }
        let mut l = GroupCreateLimits::default();
        if let Some(m) = root.get_v("maxMembers").and_then(Value::as_u64).filter(|m| *m > 0) {
            l.max_members = m as usize;
        }
        if let Some(m) = root.get_v("maxNameLength").and_then(Value::as_u64).filter(|m| *m > 0) {
            l.max_name_length = m as usize;
        }
        if let Some(p) = root.opt_s("nameRegex").filter(|p| !p.is_empty()) {
            l.name_pattern = p;
        }
        let set = |k: &str| root.arr(k).iter().filter_map(|x| x.as_str().map(String::from)).collect::<HashSet<_>>();
        l.blocked = set("blocked");
        l.official_accounts = set("officialAccounts");
        Some(l)
    }
    pub fn is_valid_name(&self, name: &str) -> bool {
        if name.is_empty() {
            return true;
        }
        match regex::Regex::new(&self.name_pattern) {
            Ok(r) => !r.is_match(name),
            Err(_) => true,
        }
    }
    /// At most `max_name_length` UTF-16 units.
    pub fn clamped_name(&self, name: &str) -> String {
        let mut used = 0;
        let mut out = String::new();
        for c in name.chars() {
            if used + c.len_utf16() > self.max_name_length {
                break;
            }
            used += c.len_utf16();
            out.push(c);
        }
        out
    }
    pub fn over_limit_text(&self) -> String {
        format!("Bạn chỉ có thể tạo nhóm với {} người khác", self.max_members)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RosterPerson {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub section: String,
    pub sort_key: String,
    pub zalo_key: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupComposeRoster {
    pub recent: Vec<RosterPerson>,
    pub friends: Vec<RosterPerson>,
}

pub const RECENT_LIMIT: usize = 5;

fn person(f: &Friend) -> RosterPerson {
    RosterPerson {
        id: f.id.clone(),
        name: if f.name.is_empty() { "Người dùng Zalo".into() } else { f.name.clone() },
        avatar: f.avatar.clone(),
        section: f.section.clone(),
        sort_key: f.sort_key.clone(),
        zalo_key: fold(&f.zalo_name),
    }
}

impl GroupComposeRoster {
    pub fn new(friends: &[Friend], conversations: &[ConversationSummary], self_id: Option<&str>, official: &HashSet<String>) -> Self {
        let mut people: Vec<RosterPerson> = friends.iter().map(person).collect();
        people.sort_by(|a, b| (a.section == "#", &a.section, &a.sort_key).cmp(&(b.section == "#", &b.section, &b.sort_key)));
        let mut latest: Vec<&ConversationSummary> = conversations
            .iter()
            .filter(|c| !c.is_group && Some(c.id.as_str()) != self_id && !official.contains(&c.id) && people.iter().any(|p| p.id == c.id))
            .collect();
        latest.sort_by(|a, b| b.preview.as_ref().and_then(|p| p.date).unwrap_or(0.0).total_cmp(&a.preview.as_ref().and_then(|p| p.date).unwrap_or(0.0)));
        let recent = latest.iter().take(RECENT_LIMIT).filter_map(|c| people.iter().find(|p| p.id == c.id).cloned()).collect();
        GroupComposeRoster { recent, friends: people }
    }
    pub fn person(&self, id: &str) -> Option<&RosterPerson> {
        self.friends.iter().find(|p| p.id == id)
    }
    pub fn matching(&self, query: &str) -> Vec<RosterPerson> {
        let needle = fold(query.trim());
        if needle.is_empty() {
            return self.friends.clone();
        }
        self.friends.iter().filter(|p| p.sort_key.contains(&needle) || p.zalo_key.contains(&needle)).cloned().collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionChange {
    Added,
    Removed,
    OverLimit,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupComposeSelection {
    pub ids: Vec<String>,
}

impl GroupComposeSelection {
    pub fn can_confirm(&self) -> bool {
        self.ids.len() > 1
    }
    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|i| i == id)
    }
    pub fn toggle(&mut self, id: &str, limits: &GroupCreateLimits) -> SelectionChange {
        if let Some(i) = self.ids.iter().position(|x| x == id) {
            self.ids.remove(i);
            return SelectionChange::Removed;
        }
        if limits.blocked.contains(id) {
            return SelectionChange::Blocked;
        }
        if self.ids.len() >= limits.max_members {
            return SelectionChange::OverLimit;
        }
        self.ids.push(id.into());
        SelectionChange::Added
    }
    pub fn keep(&mut self, allowed: impl Fn(&str) -> bool) {
        self.ids.retain(|i| allowed(i));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarError {
    Unreadable,
    TooSmall,
    TooLight,
    TooHeavy,
}

impl AvatarError {
    pub fn text(self) -> &'static str {
        match self {
            AvatarError::Unreadable => "Không đọc được ảnh này. Hãy chọn một ảnh khác.",
            AvatarError::TooSmall => "Ảnh đại diện quá nhỏ. Kích thước tối thiểu là 240 x 240.",
            AvatarError::TooLight => "Ảnh đại diện quá nhỏ. Dung lượng tối thiểu là 7 KB.",
            AvatarError::TooHeavy => "Đã có lỗi xảy ra. Vui lòng sử dụng ảnh khác hoặc thử lại sau",
        }
    }
}

/// The group picture: a centered square, 240…1024 px, JPEG of at most 1 MB.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupAvatarImage {
    pub jpeg: Vec<u8>,
    pub side: u32,
}

impl GroupAvatarImage {
    pub const MIN_SIDE: u32 = 240;
    pub const MAX_SIDE: u32 = 1024;
    pub const MIN_FILE_BYTES: u64 = 7 * 1024;
    pub const MAX_FILE_BYTES: u64 = 30_000_000;
    pub const MAX_BYTES: usize = 1024 * 1024;

    pub fn from_file(path: &std::path::Path) -> Result<Self, AvatarError> {
        let bytes = std::fs::metadata(path).map_err(|_| AvatarError::Unreadable)?.len();
        if bytes < Self::MIN_FILE_BYTES {
            return Err(AvatarError::TooLight);
        }
        if bytes >= Self::MAX_FILE_BYTES {
            return Err(AvatarError::TooHeavy);
        }
        let data = std::fs::read(path).map_err(|_| AvatarError::Unreadable)?;
        let rgba = olaz_media::decode::decode_rgba(&data, u32::MAX).ok_or(AvatarError::Unreadable)?;
        Self::from_image(image::DynamicImage::ImageRgba8(rgba))
    }

    pub fn from_image(img: image::DynamicImage) -> Result<Self, AvatarError> {
        let (w, h) = (img.width(), img.height());
        let short = w.min(h);
        if short < Self::MIN_SIDE {
            return Err(AvatarError::TooSmall);
        }
        let square = img.crop_imm((w - short) / 2, (h - short) / 2, short, short);
        let side = short.min(Self::MAX_SIDE);
        let scaled = square.resize_exact(side, side, image::imageops::FilterType::Lanczos3);
        // Flatten onto white (JPEG has no alpha).
        let mut rgb = image::RgbImage::from_pixel(side, side, image::Rgb([255, 255, 255]));
        for (x, y, p) in scaled.to_rgba8().enumerate_pixels() {
            let a = p[3] as f32 / 255.0;
            let mix = |c: u8| (c as f32 * a + 255.0 * (1.0 - a)).round() as u8;
            rgb.put_pixel(x, y, image::Rgb([mix(p[0]), mix(p[1]), mix(p[2])]));
        }
        let mut quality = 90u8;
        loop {
            let mut out = Vec::new();
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            rgb.write_with_encoder(enc).map_err(|_| AvatarError::Unreadable)?;
            if out.len() <= Self::MAX_BYTES {
                return Ok(GroupAvatarImage { jpeg: out, side });
            }
            if quality <= 45 {
                return Err(AvatarError::Unreadable);
            }
            quality -= 15;
        }
    }

    pub fn base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&self.jpeg)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GroupCreateOutcome {
    Created { id: String, failed: Vec<String> },
    Exists { id: String, text: String },
    Invalid { text: String, dropped: Vec<String> },
    Failed { title: Option<String>, message: String, closes: bool },
    Pending(String),
}

pub const DEFAULT_FAILURE: &str = "Tạo nhóm không thành công.";
pub const NOT_READY: &str = "Ứng dụng chưa sẵn sàng, hãy thử lại sau giây lát.";
pub const NO_ANSWER: &str = "Chưa nhận được phản hồi. Hãy kiểm tra danh sách hội thoại trước khi thử lại.";

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

impl GroupCreateOutcome {
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(root) = v.filter(|v| v.get_v("status").and_then(Value::as_str).is_some()) else {
            return GroupCreateOutcome::Failed { title: None, message: DEFAULT_FAILURE.into(), closes: false };
        };
        let text = root.opt_s("text").filter(|t| !t.is_empty());
        let id = root.s("convId");
        match root.s("status").as_str() {
            "created" if !id.is_empty() => GroupCreateOutcome::Created { id, failed: strs(root.get_v("failed")) },
            "exists" if !id.is_empty() => GroupCreateOutcome::Exists { id, text: text.unwrap_or_else(|| "Nhóm bạn vừa tạo đã tồn tại".into()) },
            "invalid" => GroupCreateOutcome::Invalid { text: text.unwrap_or_else(|| DEFAULT_FAILURE.into()), dropped: strs(root.get_v("dropped")) },
            "pending" => GroupCreateOutcome::Pending(text.unwrap_or_else(|| NO_ANSWER.into())),
            "not-ready" => GroupCreateOutcome::Failed { title: None, message: text.unwrap_or_else(|| NOT_READY.into()), closes: false },
            "busy" => GroupCreateOutcome::Failed { title: None, message: text.unwrap_or_else(|| DEFAULT_FAILURE.into()), closes: false },
            _ => GroupCreateOutcome::Failed {
                title: root.opt_s("title").filter(|t| !t.is_empty()),
                message: text.unwrap_or_else(|| DEFAULT_FAILURE.into()),
                closes: root.b("close"),
            },
        }
    }
    pub fn log_name(&self) -> &'static str {
        match self {
            GroupCreateOutcome::Created { .. } => "created",
            GroupCreateOutcome::Exists { .. } => "exists",
            GroupCreateOutcome::Invalid { .. } => "invalid",
            GroupCreateOutcome::Failed { .. } => "failed",
            GroupCreateOutcome::Pending(_) => "pending",
        }
    }
}

pub fn not_added_text(names: &[String]) -> String {
    let shown = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    let rest = names.len().saturating_sub(3);
    format!("Chưa thêm được {} vào nhóm.", if rest > 0 { format!("{shown} và {rest} người khác") } else { shown })
}

impl ZaloEngine {
    async fn eval_group_create(&self, method: &str, args: Vec<Value>) -> Option<Value> {
        let json = Value::Array(args).to_string();
        self.evaluate(&format!("const c = window.__zaloGroupCreate; if (!c || !c.{method}) return {{status: 'not-ready'}}; return await c.{method}(...{json});")).await
    }

    pub async fn group_create_limits(&self) -> Option<GroupCreateLimits> {
        GroupCreateLimits::from_json(self.eval_group_create("info", vec![]).await.as_ref())
    }

    pub async fn group_create_limits_when_ready(&self, attempts: usize) -> Option<GroupCreateLimits> {
        for attempt in 0..attempts {
            if let Some(l) = self.group_create_limits().await {
                return Some(l);
            }
            if attempt + 1 < attempts {
                self.exec.timer(Duration::from_secs(1)).await;
            }
        }
        None
    }

    pub async fn create_group(&self, name: &str, members: &[String], avatar: Option<&GroupAvatarImage>) -> GroupCreateOutcome {
        let mut input = json!({"name": name, "members": members});
        if let Some(a) = avatar {
            input["avatar"] = json!({"data": a.base64(), "width": a.side, "height": a.side});
        }
        let outcome = match self.race(45, self.eval_group_create("create", vec![input])).await {
            Some(v) => GroupCreateOutcome::from_json(v.as_ref()),
            None => GroupCreateOutcome::Pending(NO_ANSWER.into()),
        };
        olaz_core::log::write(format!("group create → {}", outcome.log_name()));
        match &outcome {
            GroupCreateOutcome::Created { id, failed } => {
                self.open_created_group(id).await;
                if !failed.is_empty() {
                    let names: Vec<String> = failed
                        .iter()
                        .map(|f| self.state.borrow().contacts.friends.iter().find(|x| &x.id == f).map(|x| x.name.clone()).unwrap_or_else(|| "Người dùng Zalo".into()))
                        .collect();
                    self.show_conv_toast(ToastKind::Neutral, &not_added_text(&names));
                }
            }
            GroupCreateOutcome::Exists { id, text } => {
                self.open_created_group(id).await;
                self.show_conv_toast(ToastKind::Neutral, text);
            }
            GroupCreateOutcome::Pending(t) => self.show_conv_toast(ToastKind::Neutral, t),
            _ => {}
        }
        outcome
    }

    async fn open_created_group(&self, id: &str) {
        for _ in 0..50 {
            let known = {
                let s = self.state.borrow();
                s.conversations.iter().any(|c| c.id == id) || s.contacts.groups.iter().any(|g| g.id == id)
            };
            if known {
                break;
            }
            self.exec.timer(Duration::from_millis(100)).await;
        }
        self.open_contact_conversation(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn friend(id: &str, name: &str) -> Friend {
        crate::contacts::parse_friend(&json!({"id": id, "name": name})).unwrap()
    }

    #[test]
    fn limits_and_names() {
        let l = GroupCreateLimits::from_json(Some(&json!({"status": "ok", "maxMembers": 2, "maxNameLength": 3, "blocked": ["b"]}))).unwrap();
        assert!(GroupCreateLimits::from_json(Some(&json!({"status": "not-ready"}))).is_none());
        assert!(!l.is_valid_name("a@b") && l.is_valid_name("Nhóm 1"));
        assert_eq!(l.clamped_name("ab😀c"), "ab");
        let mut s = GroupComposeSelection::default();
        assert_eq!(s.toggle("b", &l), SelectionChange::Blocked);
        assert_eq!(s.toggle("1", &l), SelectionChange::Added);
        assert!(!s.can_confirm());
        s.toggle("2", &l);
        assert_eq!(s.toggle("3", &l), SelectionChange::OverLimit);
        assert_eq!(s.toggle("1", &l), SelectionChange::Removed);
        assert_eq!(not_added_text(&["A".into(), "B".into(), "C".into(), "D".into()]), "Chưa thêm được A, B, C và 1 người khác vào nhóm.");
    }

    #[test]
    fn roster_and_outcomes() {
        let friends = vec![friend("1", "Bình"), friend("2", "An"), friend("3", "9x")];
        let conv = |id: &str, d: f64| ConversationSummary {
            id: id.into(),
            is_group: false,
            name: String::new(),
            avatar: None,
            pinned: false,
            members: 0,
            unread: 0,
            preview: Some(crate::engine::Preview { text: String::new(), date: Some(d), from_me: false }),
        };
        let r = GroupComposeRoster::new(&friends, &[conv("1", 5.0), conv("2", 9.0), conv("x", 10.0)], None, &HashSet::new());
        assert_eq!(r.friends.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["2", "1", "3"]);
        assert_eq!(r.recent.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["2", "1"]);
        assert_eq!(r.matching("binh").len(), 1);
        assert!(matches!(GroupCreateOutcome::from_json(Some(&json!({"status": "created", "convId": "g1", "failed": ["9"]}))), GroupCreateOutcome::Created { ref failed, .. } if failed == &["9"]));
        assert!(matches!(GroupCreateOutcome::from_json(Some(&json!({"status": "created"}))), GroupCreateOutcome::Failed { .. }));
        assert_eq!(GroupCreateOutcome::from_json(None).log_name(), "failed");
    }

    #[test]
    fn avatar_rules() {
        let small = image::DynamicImage::new_rgb8(200, 400);
        assert_eq!(GroupAvatarImage::from_image(small), Err(AvatarError::TooSmall));
        let big = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(1600, 1200, |x, y| image::Rgb([(x % 255) as u8, (y % 255) as u8, 128])));
        let a = GroupAvatarImage::from_image(big).unwrap();
        assert_eq!(a.side, 1024);
        assert!(a.jpeg.len() <= GroupAvatarImage::MAX_BYTES && a.jpeg.starts_with(&[0xFF, 0xD8]));
    }
}
