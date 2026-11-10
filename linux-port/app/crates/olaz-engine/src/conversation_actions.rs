//! Conversation actions: pin / unpin, "Khác" / "Ưu tiên", mark read / unread, hide / unhide with the PIN, delete.
//! Port of `ZaloEngine+ConversationActions.swift` and the pure `HiddenChatFlow`. Bridged to `window.__zaloConvActions`
//! (posts `{page:'main', convActions:{…}}`). Nothing fires on its own: each call follows a click. Ids and PINs are never logged.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use olaz_core::exec::spawn;
use serde_json::{Map, Value, json};

use crate::engine::{Change, ConversationSummary, ZaloEngine};
use crate::models::J;

/// Bit values of the adapter's `F` table.
pub mod flag {
    pub const PINNED: u32 = 1;
    pub const ARCHIVED: u32 = 2;
    pub const HIDDEN: u32 = 4;
    /// Not pinnable: no last message, a stranger, an OA that cannot be pinned, or hidden.
    pub const CANNOT_PIN: u32 = 8;
    pub const CANNOT_ARCHIVE: u32 = 16;
    pub const CANNOT_MUTE: u32 = 32;
    /// My Cloud.
    pub const CANNOT_DELETE: u32 = 64;
    /// Archived conversation while pinning there is off: neither pin nor unpin is offered.
    pub const NO_PIN_MENU: u32 = 128;
    pub const CANNOT_HIDE: u32 = 256;
    pub const UNREAD_MARK: u32 = 512;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaveType {
    Kick,
    Disband,
}

/// Sidebar row (`Thread`) or chat header (`ChatBox`, which has no archive, mark-read or delete).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPreset {
    Thread,
    ChatBox,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationActionsState {
    pub loaded: bool,
    /// The hidden-chat part has arrived; until then the list must not be shown.
    pub hidden_known: bool,
    pub flags: HashMap<String, u32>,
    pub left: HashMap<String, LeaveType>,
    pub hidden_enabled: bool,
    pub has_hidden_pin: bool,
    pub hidden_ids: HashSet<String>,
    pub mark_unread_enabled: bool,
    pub late_epoch: i64,
    pub late_seq: i64,
}

impl ConversationActionsState {
    pub fn flags_for(&self, id: &str) -> u32 {
        self.flags.get(id).copied().unwrap_or(0)
    }
    pub fn is_pinned(&self, id: &str) -> bool {
        self.flags_for(id) & flag::PINNED != 0
    }
    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden_ids.contains(id) || self.flags_for(id) & flag::HIDDEN != 0
    }
    pub fn has_unread_mark(&self, id: &str) -> bool {
        self.flags_for(id) & flag::UNREAD_MARK != 0
    }

    /// Applies a `convActions` payload (the early `{hidden}` shape or the full snapshot).
    pub fn apply(&mut self, json: &Value) {
        if let Some(hidden) = json.get_v("hidden").filter(|h| h.is_object()) {
            self.hidden_known = true;
            self.hidden_enabled = hidden.get_v("enabled").and_then(Value::as_bool).unwrap_or(false);
            self.has_hidden_pin = hidden.get_v("hasPin").and_then(Value::as_bool).unwrap_or(false);
            self.hidden_ids = hidden.arr("ids").iter().filter_map(|v| v.as_str().map(String::from)).collect();
        }
        let Some(raw) = json.get_v("flags").and_then(Value::as_object) else { return };
        self.loaded = true;
        self.flags = raw.iter().filter_map(|(k, v)| v.as_u64().map(|b| (k.clone(), b as u32))).filter(|(_, b)| *b != 0).collect();
        self.left = json
            .get_v("left")
            .and_then(Value::as_object)
            .map(|o| o.iter().map(|(k, v)| (k.clone(), if v.as_str() == Some("disband") { LeaveType::Disband } else { LeaveType::Kick })).collect())
            .unwrap_or_default();
        self.mark_unread_enabled = json.get_v("markUnread").and_then(Value::as_bool).unwrap_or(false);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanPin {
    Pin,
    Unpin,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanArchive {
    MoveToOther,
    MoveToFocus,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanRead {
    MarkRead,
    MarkUnread,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMute {
    Mute,
    Unmute,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanHide {
    Hide,
    Unhide,
}

/// What the official menu offers for one conversation, in official order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationMenuPlan {
    pub pin: Option<PlanPin>,
    pub archive: Option<PlanArchive>,
    pub read: Option<PlanRead>,
    pub mute: Option<PlanMute>,
    pub hide: Option<PlanHide>,
    pub can_delete: bool,
    /// "Phân loại" (official `showLabel`).
    pub shows_label: bool,
    /// "Rời nhóm": a group the user is still a member of.
    pub can_leave: bool,
}

impl ConversationMenuPlan {
    pub fn is_empty(&self) -> bool {
        self.pin.is_none() && self.archive.is_none() && self.read.is_none() && self.mute.is_none() && self.hide.is_none() && !self.can_delete && !self.shows_label && !self.can_leave
    }

    pub fn make(id: &str, preset: MenuPreset, state: &ConversationActionsState, unread: i64, muted: bool) -> Self {
        let f = state.flags_for(id);
        let has = |b: u32| f & b != 0;
        let mut plan = ConversationMenuPlan::default();
        let is_self = has(flag::CANNOT_DELETE);
        let thread = preset == MenuPreset::Thread;
        // A group I was removed from / that was disbanded: only delete.
        if state.left.contains_key(id) {
            plan.can_delete = thread && !has(flag::CANNOT_DELETE);
            return plan;
        }
        if !has(flag::NO_PIN_MENU) {
            if has(flag::PINNED) {
                plan.pin = Some(PlanPin::Unpin);
            } else if !has(flag::CANNOT_PIN) {
                plan.pin = Some(PlanPin::Pin);
            }
        }
        if thread && !has(flag::CANNOT_ARCHIVE) {
            plan.archive = Some(if has(flag::ARCHIVED) { PlanArchive::MoveToFocus } else { PlanArchive::MoveToOther });
        }
        plan.shows_label = true;
        if thread && !has(flag::HIDDEN) && !is_self {
            if unread > 0 || has(flag::UNREAD_MARK) {
                plan.read = Some(PlanRead::MarkRead);
            } else if state.mark_unread_enabled {
                plan.read = Some(PlanRead::MarkUnread);
            }
        }
        if muted {
            plan.mute = Some(PlanMute::Unmute);
        } else if !has(flag::CANNOT_MUTE) {
            plan.mute = Some(PlanMute::Mute);
        }
        if state.hidden_enabled {
            if has(flag::HIDDEN) {
                plan.hide = Some(PlanHide::Unhide);
            } else if !has(flag::CANNOT_HIDE) {
                plan.hide = Some(PlanHide::Hide);
            }
        }
        plan.can_delete = thread && !has(flag::CANNOT_DELETE);
        plan.can_leave = is_group_id(id);
        plan
    }
}

/// Result of an adapter action; `text` is the official toast text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationActionResult {
    pub ok: bool,
    pub text: String,
    /// Pin refused because the tab is full: the board opens instead.
    pub full: bool,
    pub bad_pin: bool,
    /// A PIN is already set (the caller's view was stale): nothing was changed.
    pub pin_exists: bool,
    /// Shown as a plain message, not a success toast.
    pub neutral: bool,
}

pub const NO_ANSWER: &str = "Chưa nhận được phản hồi. Hãy kiểm tra lại trước khi thử lại.";
pub const STILL_RUNNING: &str = "Thao tác vẫn đang được xử lý. Hãy kiểm tra lại danh sách hội thoại trước khi thử lại.";

impl ConversationActionResult {
    pub fn failed(text: &str) -> Self {
        ConversationActionResult { ok: false, text: text.into(), ..Default::default() }
    }
    pub fn from_json(v: Option<&Value>) -> Self {
        let Some(root) = v.filter(|v| v.is_object()) else { return Self::failed(NO_ANSWER) };
        let Some(ok) = root.get_v("ok").and_then(Value::as_bool) else { return Self::failed(NO_ANSWER) };
        let b = |k: &str| root.get_v(k).and_then(Value::as_bool).unwrap_or(false);
        ConversationActionResult {
            ok,
            text: root.s("text").chars().take(300).collect(),
            full: b("full"),
            bad_pin: b("badPin"),
            pin_exists: b("pinExists"),
            neutral: b("neutral"),
        }
    }
}

/// The full-board dialog (`POP_PINNED_CONVERSATION`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinBoard {
    pub tab: i64,
    pub limit: i64,
    pub total: i64,
    /// How many pins must go before a new one fits.
    pub need: i64,
    /// Pinned conversation ids, newest first.
    pub items: Vec<String>,
}

impl PinBoard {
    pub fn from_json(v: Option<&Value>) -> Option<Self> {
        let root = v?;
        let limit = root.get_v("limit").and_then(Value::as_i64).filter(|l| *l > 0)?;
        Some(PinBoard {
            tab: root.get_v("tab").and_then(Value::as_i64).unwrap_or(0),
            limit,
            total: root.get_v("total").and_then(Value::as_i64).unwrap_or(0),
            need: root.get_v("need").and_then(Value::as_i64).unwrap_or(0),
            items: root.arr("items").iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        })
    }
    /// The confirm button is enabled once the chosen removals free a slot.
    pub fn frees_slot(&self, removing: usize) -> bool {
        (self.items.len() as i64 - removing as i64) < self.limit
    }
}

/// Group conversation ids carry the official `GROUPID_PREFIX`.
pub fn is_group_id(id: &str) -> bool {
    id.starts_with('g')
}

/// A dialog / sheet a conversation action asks the shell to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationPrompt {
    Delete(String),
    PinBoard(String),
    Hide(String),
    Unhide(String),
    /// Open a hidden conversation.
    Unlock(String),
    /// "Quản lý thẻ phân loại".
    LabelManager,
    /// "Rời nhóm" confirm; `owner` = the user created the group (ownership passes to another member).
    LeaveGroup { id: String, owner: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Neutral,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvToast {
    pub seq: u64,
    pub kind: ToastKind,
    pub text: String,
}

/// Client-side state: toast, removals, the single hidden conversation unlocked with the PIN.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationActionUI {
    pub toast: Option<ConvToast>,
    /// Conversations that left the list (deleted / hidden): the shell clears the selection when it was one of them.
    pub removal: Option<(u64, HashSet<String>)>,
    pub unlocked_id: Option<String>,
    pub pin_search_ids: Vec<String>,
    seq: u64,
}

impl ConversationActionUI {
    pub fn show(&mut self, kind: ToastKind, text: &str) {
        self.seq += 1;
        self.toast = Some(ConvToast { seq: self.seq, kind, text: text.into() });
    }
    pub fn remove(&mut self, ids: HashSet<String>) {
        if ids.is_empty() {
            return;
        }
        self.seq += 1;
        if self.unlocked_id.as_ref().is_some_and(|u| ids.contains(u)) {
            self.unlocked_id = None;
        }
        self.removal = Some((self.seq, ids));
    }
    pub fn is_unlocked(&self, id: &str) -> bool {
        self.unlocked_id.as_deref() == Some(id)
    }
    pub fn relock(&mut self) {
        self.unlocked_id = None;
        self.pin_search_ids.clear();
    }
    pub fn reset(&mut self) {
        self.relock();
        self.toast = None;
        self.removal = None;
    }
}

/// Timing rules of the hidden-chat lock.
pub mod hidden_guard {
    pub const STALL_LIMIT_S: f64 = 30.0;
    pub const RELOCK_AFTER_INACTIVITY_S: f64 = 60.0;
    pub const STALLED_MESSAGE: &str = "Không đọc được trạng thái trò chuyện ẩn — đang thử lại";
    pub fn is_stalled(waiting_since: Option<f64>, now: f64) -> bool {
        waiting_since.is_some_and(|w| now - w >= STALL_LIMIT_S)
    }
    /// A short focus loss must not relock.
    pub fn should_relock(inactive_for: f64) -> bool {
        inactive_for >= RELOCK_AFTER_INACTIVITY_S
    }
}

pub const DELETE_LINE1: &str = "Toàn bộ nội dung trò chuyện sẽ bị xóa vĩnh viễn.";
pub const DELETE_LINE2: &str = "Bạn có chắc chắn muốn xóa?";
pub const DELETE_CONFIRM: &str = "Xóa";
pub const DELETE_CANCEL: &str = "Không";
/// Official STR_GROUP_LEAVE_CONFIRM / STR_LEAVEGROUP_MENU / STR_CHECK_NET.
pub const LEAVE_TITLE: &str = "Rời nhóm?";
pub const LEAVE_MESSAGE: &str = "Rời nhóm sẽ đồng thời xóa toàn bộ tin nhắn của nhóm đó. Bạn có muốn tiếp tục?";
pub const LEAVE_OWNER_MESSAGE: &str = "Rời nhóm sẽ xóa toàn bộ tin nhắn. Quyền trưởng nhóm sẽ được trao ngẫu nhiên cho thành viên khác";
pub const LEAVE_CONFIRM: &str = "Rời nhóm";

// MARK: Hidden chat PIN flow (pure)

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowKind {
    Hide(String),
    /// `requires_pin` false = opened with the PIN already (a plain confirmation).
    Unhide(String, bool),
    Open(String),
    SetPin,
    ChangePin,
    ResetPin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Introduce,
    CreatePin,
    ConfirmPin,
    EnterPin,
    UnhideConfirm,
    OldPin,
    NewPin,
    RepeatPin,
    ResetConfirm,
    HideSuccess,
    SetPinSuccess,
    ChangeSuccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    None,
    Hide(String),
    HideWithNewPin(String),
    Unhide(Option<String>),
    VerifyToOpen(String),
    VerifyOld(String),
    SetPin(String, bool),
}

pub const WRONG_PIN: &str = "Mã PIN không đúng. Vui lòng thử lại.";
pub const PIN_ALREADY_SET: &str = "Đã có mã PIN cho trò chuyện ẩn. Hãy dùng “Đổi mã PIN” nếu muốn đổi.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenChatFlow {
    pub kind: FlowKind,
    pub screen: Screen,
    pub error: Option<String>,
    pending: Option<String>,
}

impl HiddenChatFlow {
    pub fn new(kind: FlowKind, has_pin: bool) -> Self {
        let screen = match &kind {
            FlowKind::Hide(_) => {
                if has_pin {
                    Screen::EnterPin
                } else {
                    Screen::Introduce
                }
            }
            FlowKind::Unhide(_, requires) => {
                if *requires {
                    Screen::EnterPin
                } else {
                    Screen::UnhideConfirm
                }
            }
            FlowKind::Open(_) => Screen::EnterPin,
            FlowKind::SetPin => Screen::CreatePin,
            FlowKind::ChangePin => Screen::OldPin,
            FlowKind::ResetPin => Screen::ResetConfirm,
        };
        HiddenChatFlow { kind, screen, error: None, pending: None }
    }

    pub fn conversation_id(&self) -> Option<&str> {
        match &self.kind {
            FlowKind::Hide(id) | FlowKind::Unhide(id, _) | FlowKind::Open(id) => Some(id),
            _ => None,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.screen {
            Screen::Introduce => "Ẩn trò chuyện",
            Screen::CreatePin => "Đặt mã PIN",
            Screen::ConfirmPin => "Xác nhận lại mã PIN",
            Screen::EnterPin => match self.kind {
                FlowKind::Hide(_) => "Nhập mã PIN để ẩn trò chuyện",
                FlowKind::Open(_) => "Nhập mã PIN để mở trò chuyện",
                _ => "Bỏ ẩn trò chuyện",
            },
            Screen::UnhideConfirm => "Bỏ ẩn trò chuyện",
            Screen::OldPin | Screen::NewPin | Screen::RepeatPin => "Đổi mã PIN",
            Screen::ResetConfirm => "Đặt lại mã PIN",
            Screen::HideSuccess => "Ẩn trò chuyện thành công",
            Screen::SetPinSuccess => "Cài đặt mã PIN thành công",
            Screen::ChangeSuccess => "Đổi mã PIN thành công",
        }
    }

    pub fn takes_pin(&self) -> bool {
        matches!(self.screen, Screen::CreatePin | Screen::ConfirmPin | Screen::EnterPin | Screen::OldPin | Screen::NewPin | Screen::RepeatPin)
    }

    pub fn start_setting_pin(&mut self) {
        self.error = None;
        self.screen = Screen::CreatePin;
    }

    pub fn submit(&mut self, pin: &str) -> Step {
        match self.screen {
            Screen::CreatePin => {
                self.pending = Some(pin.into());
                self.error = None;
                self.screen = Screen::ConfirmPin;
                Step::None
            }
            Screen::ConfirmPin => {
                if self.pending.as_deref() != Some(pin) {
                    self.error = Some(WRONG_PIN.into());
                    return Step::None;
                }
                self.error = None;
                if self.kind == FlowKind::SetPin { Step::SetPin(pin.into(), false) } else { Step::HideWithNewPin(pin.into()) }
            }
            Screen::EnterPin => match self.kind {
                FlowKind::Hide(_) => Step::Hide(pin.into()),
                FlowKind::Unhide(..) => Step::Unhide(Some(pin.into())),
                FlowKind::Open(_) => Step::VerifyToOpen(pin.into()),
                _ => Step::None,
            },
            Screen::OldPin => Step::VerifyOld(pin.into()),
            Screen::NewPin => {
                self.pending = Some(pin.into());
                self.error = None;
                self.screen = Screen::RepeatPin;
                Step::None
            }
            Screen::RepeatPin => {
                if self.pending.as_deref() != Some(pin) {
                    self.error = Some(WRONG_PIN.into());
                    return Step::None;
                }
                self.error = None;
                Step::SetPin(pin.into(), true)
            }
            _ => Step::None,
        }
    }

    pub fn old_pin_verified(&mut self) {
        self.error = None;
        self.screen = Screen::NewPin;
    }

    pub fn back(&mut self) {
        self.pending = None;
        self.error = None;
        match self.screen {
            Screen::RepeatPin => self.screen = Screen::NewPin,
            Screen::ConfirmPin => self.screen = Screen::CreatePin,
            _ => {}
        }
    }

    pub fn fail(&mut self, text: &str) {
        self.error = Some(text.into());
    }

    /// The page refused to set a PIN because one exists: ask for it instead, never overwrite it.
    pub fn pin_already_exists(&mut self) {
        self.pending = None;
        if matches!(self.kind, FlowKind::Hide(_)) {
            self.error = None;
            self.screen = Screen::EnterPin;
        } else {
            self.error = Some(PIN_ALREADY_SET.into());
        }
    }

    /// The work finished: the success screen of the kind (false = nothing more to show, close).
    pub fn succeeded(&mut self) -> bool {
        self.pending = None;
        self.error = None;
        match (&self.kind, self.screen) {
            (FlowKind::Hide(_), Screen::ConfirmPin) => {
                self.screen = Screen::HideSuccess;
                true
            }
            (FlowKind::SetPin, _) => {
                self.screen = Screen::SetPinSuccess;
                true
            }
            (FlowKind::ChangePin, _) => {
                self.screen = Screen::ChangeSuccess;
                true
            }
            _ => false,
        }
    }
}

// MARK: Engine

impl ZaloEngine {
    async fn conv_action_raw(&self, method: &str, args: Vec<Value>) -> Option<Value> {
        let json = Value::Array(args).to_string();
        let script = format!("const c = window.__zaloConvActions; if (!c || !c.{method}) return null; return await c.{method}(...{json});");
        self.evaluate(&script).await
    }

    async fn conv_action(&self, method: &str, args: Vec<Value>, secs: u64) -> ConversationActionResult {
        let timer = self.exec.timer(Duration::from_secs(secs));
        let call = self.conv_action_raw(method, args);
        futures::pin_mut!(call);
        let answer = match futures::future::select(call, timer).await {
            futures::future::Either::Left((v, _)) => v,
            futures::future::Either::Right(_) => return ConversationActionResult::failed(STILL_RUNNING),
        };
        let result = ConversationActionResult::from_json(answer.as_ref());
        if !result.ok {
            olaz_core::log::write(format!("conversation action {method} → failed"));
        }
        result
    }

    fn conv_ui(&self, f: impl FnOnce(&mut ConversationActionUI)) {
        f(&mut self.state.borrow_mut().conversation_ui);
        self.changed(Change::Conversations);
    }

    /// Shows the official toast text (empty text: nothing to say).
    pub fn report_conv(&self, r: &ConversationActionResult) {
        if r.text.is_empty() || r.full || r.pin_exists {
            return;
        }
        let kind = if !r.ok {
            ToastKind::Failure
        } else if r.neutral {
            ToastKind::Neutral
        } else {
            ToastKind::Success
        };
        self.show_conv_toast(kind, &r.text);
    }

    /// Shows a conversation toast for 3.2 s.
    pub fn show_conv_toast(&self, kind: ToastKind, text: &str) {
        self.conv_ui(|ui| ui.show(kind, text));
        let seq = self.state.borrow().conversation_ui.toast.as_ref().map(|t| t.seq);
        let weak = self.weak();
        let exec = self.exec.clone();
        spawn(&self.exec, async move {
            exec.timer(Duration::from_millis(3200)).await;
            let Some(me) = weak.upgrade() else { return };
            if me.state.borrow().conversation_ui.toast.as_ref().map(|t| t.seq) == seq {
                me.conv_ui(|ui| ui.toast = None);
            }
        });
    }

    fn run_conv(&self, method: &'static str, args: Vec<Value>, secs: u64, after: impl FnOnce(&ZaloEngine, &ConversationActionResult) + 'static) {
        let Some(me) = self.weak().upgrade() else { return };
        spawn(&self.exec, async move {
            let r = me.conv_action(method, args, secs).await;
            after(&me, &r);
        });
    }

    pub fn set_conversation_prompt(&self, p: Option<ConversationPrompt>) {
        self.state.borrow_mut().conversation_prompt = p;
        self.changed(Change::Notice);
    }

    /// "Ghim hội thoại". A full tab opens the board instead.
    pub fn pin_conversation(&self, id: &str) {
        let id2 = id.to_string();
        self.run_conv("pin", vec![json!(id)], 40, move |e, r| {
            if r.full {
                e.set_conversation_prompt(Some(ConversationPrompt::PinBoard(id2)));
            } else {
                e.report_conv(r);
            }
        });
    }

    pub fn unpin_conversation(&self, id: &str) {
        self.run_conv("unpin", vec![json!(id)], 40, |e, r| e.report_conv(r));
    }

    pub async fn pin_board(&self, id: &str) -> Option<PinBoard> {
        PinBoard::from_json(self.conv_action_raw("pinBoard", vec![json!(id)]).await.as_ref())
    }

    /// Board confirm: unpin the chosen ones, then pin `id`.
    pub async fn pin_replacing(&self, unpin: Vec<String>, id: &str) -> ConversationActionResult {
        let r = self.conv_action("pinReplacing", vec![json!(unpin), json!(id)], 40).await;
        self.report_conv(&r);
        r
    }

    pub fn move_conversation(&self, id: &str, to_other: bool) {
        self.run_conv(if to_other { "archive" } else { "unarchive" }, vec![json!(id)], 45, |e, r| e.report_conv(r));
    }

    pub fn request_delete_conversation(&self, id: &str) {
        self.set_conversation_prompt(Some(ConversationPrompt::Delete(id.into())));
    }

    pub fn confirm_delete_conversation(&self, id: &str) {
        let id2 = id.to_string();
        self.run_conv("deleteConversation", vec![json!(id)], 60, move |e, r| {
            if r.ok {
                e.conv_ui(|ui| ui.remove(HashSet::from([id2])));
            }
            e.report_conv(r);
        });
    }

    /// Asks whether the user owns the group (for the confirm's wording), then shows the confirm.
    pub fn request_leave_group(&self, id: &str) {
        let Some(me) = self.weak().upgrade() else { return };
        let id = id.to_string();
        spawn(&self.exec, async move {
            let script = format!("const g = window.__zaloGroupLeave; return g ? g.isOwner(...[{}]) : false;", json!(id));
            let timer = me.exec.timer(Duration::from_secs(3));
            let call = me.evaluate(&script);
            futures::pin_mut!(call);
            let owner = match futures::future::select(call, timer).await {
                futures::future::Either::Left((v, _)) => v.and_then(|v| v.as_bool()).unwrap_or(false),
                futures::future::Either::Right(_) => false,
            };
            me.set_conversation_prompt(Some(ConversationPrompt::LeaveGroup { id, owner }));
        });
    }

    /// Leaves the group through the official fetcher (`window.__zaloGroupLeave`, Linux adapter); the conversation
    /// leaves the list on success.
    pub fn confirm_leave_group(&self, id: &str) {
        let Some(me) = self.weak().upgrade() else { return };
        let id = id.to_string();
        spawn(&self.exec, async move {
            let script = format!("const g = window.__zaloGroupLeave; if (!g) return null; return await g.leave(...[{}]);", json!(id));
            let timer = me.exec.timer(Duration::from_secs(45));
            let call = me.evaluate(&script);
            futures::pin_mut!(call);
            let r = match futures::future::select(call, timer).await {
                futures::future::Either::Left((v, _)) => ConversationActionResult::from_json(v.as_ref()),
                futures::future::Either::Right(_) => ConversationActionResult::failed(STILL_RUNNING),
            };
            if r.ok {
                me.conv_ui(|ui| ui.remove(HashSet::from([id.clone()])));
            } else {
                olaz_core::log::write("conversation action leaveGroup → failed");
            }
            me.report_conv(&r);
        });
    }

    pub fn mark_conversation_read(&self, id: &str) {
        self.run_conv("markRead", vec![json!(id)], 40, |e, r| e.report_conv(r));
    }

    pub fn mark_conversation_unread(&self, id: &str) {
        self.run_conv("markUnread", vec![json!(id)], 40, |e, r| e.report_conv(r));
    }

    // Hidden chats (the PIN goes straight into the official calls).

    pub async fn check_hidden_chat_pin(&self, pin: &str) -> bool {
        ConversationActionResult::from_json(self.conv_action_raw("checkPin", vec![json!(pin)]).await.as_ref()).ok
    }

    pub async fn hide_conversation(&self, id: &str, pin: &str, new_pin: bool) -> ConversationActionResult {
        let r = self.conv_action(if new_pin { "hideWithNewPin" } else { "hide" }, vec![json!(id), json!(pin)], 40).await;
        if r.ok {
            self.conv_ui(|ui| ui.remove(HashSet::from([id.to_string()])));
        }
        r
    }

    /// `pin` None = the conversation was opened with the PIN already.
    pub async fn unhide_conversation(&self, id: &str, pin: Option<&str>) -> ConversationActionResult {
        self.conv_action("unhide", vec![json!(id), pin.map(|p| json!(p)).unwrap_or(Value::Null)], 40).await
    }

    pub async fn set_hidden_chat_pin(&self, pin: &str, changed: bool) -> ConversationActionResult {
        self.conv_action("setPin", vec![json!(pin), json!(changed)], 40).await
    }

    /// Removes the PIN and every hidden chat: they all leave the view and nothing stays unlocked.
    pub async fn reset_hidden_chat_pin(&self) -> ConversationActionResult {
        let formerly = self.state.borrow().conversation_actions.hidden_ids.clone();
        let r = self.conv_action("resetPin", vec![], 60).await;
        if r.ok {
            self.conv_ui(|ui| {
                ui.remove(formerly);
                ui.relock();
            });
        }
        r
    }

    /// Sidebar search: when `query` is the PIN, the hidden conversations; None otherwise. Nothing is unlocked by a search.
    pub async fn hidden_conversations_matching(&self, query: &str) -> Option<Vec<String>> {
        let enabled = self.state.borrow().conversation_actions.hidden_enabled;
        if !enabled || query.chars().count() != 4 || !query.chars().all(|c| c.is_ascii_digit()) {
            self.set_pin_search(vec![]);
            return None;
        }
        let ids: Option<Vec<String>> = self
            .conv_action_raw("hiddenSearch", vec![json!(query)])
            .await
            .and_then(|r| r.get_v("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()));
        self.set_pin_search(ids.clone().unwrap_or_default());
        ids
    }

    fn set_pin_search(&self, ids: Vec<String>) {
        if self.state.borrow().conversation_ui.pin_search_ids != ids {
            self.conv_ui(|ui| ui.pin_search_ids = ids);
        }
    }

    /// A hidden conversation is closed unless it was opened with the PIN (and only that one).
    pub fn is_conversation_locked(&self, id: &str) -> bool {
        let s = self.state.borrow();
        s.conversation_actions.is_hidden(id) && !s.conversation_ui.is_unlocked(id)
    }

    /// Opened from the PIN search results: unlocks that one conversation.
    pub fn unlock_from_pin_search(&self, id: &str) -> bool {
        if !self.state.borrow().conversation_ui.pin_search_ids.iter().any(|i| i == id) {
            return false;
        }
        self.unlock_hidden_conversation(id);
        true
    }

    pub fn unlock_hidden_conversation(&self, id: &str) {
        let id = id.to_string();
        self.conv_ui(|ui| ui.unlocked_id = Some(id));
    }

    pub fn relock_hidden(&self) {
        self.conv_ui(|ui| ui.relock());
    }

    /// Conversations that may be counted / listed: none while the hidden state is unknown.
    pub fn visible_conversations(&self) -> Vec<ConversationSummary> {
        let s = self.state.borrow();
        if !s.conversation_actions.hidden_known {
            return vec![];
        }
        s.conversations.iter().filter(|c| !s.conversation_actions.is_hidden(&c.id)).cloned().collect()
    }

    pub fn conversation_menu_plan(&self, id: &str, preset: MenuPreset) -> ConversationMenuPlan {
        let unread = if preset == MenuPreset::Thread {
            self.state.borrow().conversations.iter().find(|c| c.id == id).map(|c| c.unread).unwrap_or(0)
        } else {
            0
        };
        let muted = self.is_conversation_muted(id);
        ConversationMenuPlan::make(id, preset, &self.state.borrow().conversation_actions, unread, muted)
    }

    /// Logout: nothing of the previous account's menus, prompts or unlocks survives.
    pub fn reset_conversation_actions(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.conversation_actions = ConversationActionsState::default();
            s.conversation_prompt = None;
            s.conversation_ui.reset();
        }
        self.republish_conversation_actions();
    }

    /// Asks the page to post its current state again (it only posts on change).
    pub fn republish_conversation_actions(&self) {
        self.call_chat("republish", vec![], "__zaloConvActions");
    }

    pub(crate) fn handle_conversation_actions(&self, state: &Map<String, Value>) {
        let Some(json) = state.get("convActions").filter(|j| j.is_object()) else { return };
        let mut next = self.state.borrow().conversation_actions.clone();
        next.apply(json);
        let epoch = json.get_v("epoch").and_then(Value::as_i64).unwrap_or(next.late_epoch);
        if epoch != next.late_epoch {
            next.late_epoch = epoch;
            next.late_seq = 0;
        }
        if let Some(late) = json.get_v("late").filter(|l| l.is_object()) {
            if let Some(seq) = late.get_v("seq").and_then(Value::as_i64).filter(|s| *s > next.late_seq) {
                next.late_seq = seq;
                let text = late.s("text");
                let ok = late.get_v("ok").and_then(Value::as_bool).unwrap_or(false);
                let neutral = late.get_v("neutral").and_then(Value::as_bool) == Some(true);
                if !text.is_empty() {
                    self.show_conv_toast(if !ok { ToastKind::Failure } else if neutral { ToastKind::Neutral } else { ToastKind::Success }, &text);
                }
                let removed = late.s("removedId");
                if !removed.is_empty() {
                    self.conv_ui(|ui| ui.remove(HashSet::from([removed])));
                }
            }
        }
        if next != self.state.borrow().conversation_actions {
            self.state.borrow_mut().conversation_actions = next;
            self.changed(Change::Conversations);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_engine(t: &std::rc::Rc<olaz_core::exec::TestExecutor>, dir: &std::path::Path, answers: Value) -> std::rc::Rc<ZaloEngine> {
        let e = ZaloEngine::new(
            t.exec(),
            olaz_core::paths::XdgPaths::rooted(dir),
            crate::Services {
                media: None,
                secrets: std::sync::Arc::new(olaz_main::secrets::MemoryStore::default()),
                open_url: std::rc::Rc::new(|_| {}),
                badge: None,
                webhost_binary: "/nonexistent".into(),
                debug: false,
            },
        );
        *e.fake_renderer.borrow_mut() = Some(std::rc::Rc::new(crate::fixtures::FakeRenderer::new(answers.as_object().unwrap())));
        e
    }

    #[test]
    fn leave_group_confirms_then_leaves() {
        let tmp = tempfile::tempdir().unwrap();
        let t = olaz_core::exec::TestExecutor::new();
        let e = fake_engine(&t, tmp.path(), json!({
            "__zaloGroupLeave.isOwner": true,
            "__zaloGroupLeave.leave": {"$seq": [{"ok": true, "text": "Bạn đã rời nhóm"}, {"ok": false, "text": "Vui lòng kiểm tra lại kết nối mạng của bạn."}]}
        }));
        e.request_leave_group("g42");
        t.run_until_stalled();
        assert_eq!(e.state.borrow().conversation_prompt, Some(ConversationPrompt::LeaveGroup { id: "g42".into(), owner: true }));
        e.set_conversation_prompt(None);
        e.confirm_leave_group("g42");
        t.run_until_stalled();
        {
            let s = e.state.borrow();
            assert!(s.conversation_ui.removal.as_ref().is_some_and(|(_, ids)| ids.contains("g42")));
            let toast = s.conversation_ui.toast.clone().unwrap();
            assert_eq!((toast.kind, toast.text.as_str()), (ToastKind::Success, "Bạn đã rời nhóm"));
        }
        e.confirm_leave_group("g43");
        t.run_until_stalled();
        let s = e.state.borrow();
        assert!(!s.conversation_ui.removal.as_ref().unwrap().1.contains("g43"), "a failed leave keeps the conversation");
        assert_eq!(s.conversation_ui.toast.as_ref().unwrap().kind, ToastKind::Failure);
    }

    #[test]
    fn leave_group_is_offered_for_groups_still_joined() {
        let s = state(json!({}));
        assert!(ConversationMenuPlan::make("g1", MenuPreset::Thread, &s, 0, false).can_leave);
        assert!(ConversationMenuPlan::make("g1", MenuPreset::ChatBox, &s, 0, false).can_leave);
        assert!(!ConversationMenuPlan::make("123", MenuPreset::Thread, &s, 0, false).can_leave);
        assert!(!ConversationMenuPlan::make("g9", MenuPreset::Thread, &s, 0, false).can_leave, "already left / disbanded");
    }

    fn state(flags: Value) -> ConversationActionsState {
        let mut s = ConversationActionsState::default();
        s.apply(&json!({"hidden": {"enabled": true, "hasPin": false, "ids": ["h"]}, "flags": flags, "left": {"g9": "disband"}, "markUnread": true}));
        s
    }

    #[test]
    fn applies_snapshots() {
        let mut s = ConversationActionsState::default();
        s.apply(&json!({"hidden": {"enabled": true, "ids": ["a"]}}));
        assert!(s.hidden_known && !s.loaded && s.is_hidden("a"));
        s.apply(&json!({"flags": {"b": 1, "c": 0}, "left": {"g": "x"}}));
        assert!(s.loaded && s.is_pinned("b") && !s.flags.contains_key("c"));
        assert_eq!(s.left["g"], LeaveType::Kick);
        assert!(s.is_hidden("a"), "the hidden part stays when only flags arrive");
    }

    #[test]
    fn plans_follow_the_official_menu() {
        let s = state(json!({"p": flag::PINNED, "self": flag::CANNOT_DELETE | flag::CANNOT_HIDE, "h": flag::HIDDEN | flag::CANNOT_PIN, "a": flag::ARCHIVED | flag::NO_PIN_MENU}));
        let plain = ConversationMenuPlan::make("x", MenuPreset::Thread, &s, 0, false);
        assert_eq!(plain.pin, Some(PlanPin::Pin));
        assert_eq!(plain.archive, Some(PlanArchive::MoveToOther));
        assert_eq!(plain.read, Some(PlanRead::MarkUnread));
        assert_eq!(plain.mute, Some(PlanMute::Mute));
        assert_eq!(plain.hide, Some(PlanHide::Hide));
        assert!(plain.can_delete && plain.shows_label);
        let unread = ConversationMenuPlan::make("p", MenuPreset::Thread, &s, 3, true);
        assert_eq!((unread.pin, unread.read, unread.mute), (Some(PlanPin::Unpin), Some(PlanRead::MarkRead), Some(PlanMute::Unmute)));
        let me = ConversationMenuPlan::make("self", MenuPreset::Thread, &s, 2, false);
        assert!(me.read.is_none() && me.hide.is_none() && !me.can_delete);
        let hidden = ConversationMenuPlan::make("h", MenuPreset::Thread, &s, 1, false);
        assert!(hidden.pin.is_none() && hidden.read.is_none());
        assert_eq!(hidden.hide, Some(PlanHide::Unhide));
        let archived = ConversationMenuPlan::make("a", MenuPreset::Thread, &s, 0, false);
        assert!(archived.pin.is_none());
        assert_eq!(archived.archive, Some(PlanArchive::MoveToFocus));
        let header = ConversationMenuPlan::make("x", MenuPreset::ChatBox, &s, 5, false);
        assert!(header.archive.is_none() && header.read.is_none() && !header.can_delete && header.pin.is_some());
        let left = ConversationMenuPlan::make("g9", MenuPreset::Thread, &s, 0, false);
        assert_eq!(left, ConversationMenuPlan { can_delete: true, ..Default::default() });
    }

    #[test]
    fn results_and_board() {
        assert_eq!(ConversationActionResult::from_json(None).text, NO_ANSWER);
        assert_eq!(ConversationActionResult::from_json(Some(&json!({"text": "x"}))).text, NO_ANSWER);
        let r = ConversationActionResult::from_json(Some(&json!({"ok": false, "text": "Sai", "badPin": true})));
        assert!(!r.ok && r.bad_pin && r.text == "Sai");
        assert!(PinBoard::from_json(Some(&json!({"limit": 0}))).is_none());
        let b = PinBoard::from_json(Some(&json!({"limit": 2, "need": 1, "items": ["a", "b"]}))).unwrap();
        assert!(!b.frees_slot(0) && b.frees_slot(1));
    }

    #[test]
    fn hidden_flow() {
        let mut f = HiddenChatFlow::new(FlowKind::Hide("c".into()), false);
        assert_eq!(f.screen, Screen::Introduce);
        f.start_setting_pin();
        assert_eq!(f.submit("1234"), Step::None);
        assert_eq!(f.screen, Screen::ConfirmPin);
        assert_eq!(f.submit("1111"), Step::None);
        assert_eq!(f.error.as_deref(), Some(WRONG_PIN));
        assert_eq!(f.submit("1234"), Step::HideWithNewPin("1234".into()));
        assert!(f.succeeded() && f.screen == Screen::HideSuccess);

        let mut f = HiddenChatFlow::new(FlowKind::Hide("c".into()), false);
        f.start_setting_pin();
        f.submit("1234");
        f.submit("1234");
        f.pin_already_exists();
        assert_eq!(f.screen, Screen::EnterPin);
        assert_eq!(f.submit("9999"), Step::Hide("9999".into()));

        let mut c = HiddenChatFlow::new(FlowKind::ChangePin, true);
        assert_eq!(c.submit("0000"), Step::VerifyOld("0000".into()));
        c.old_pin_verified();
        c.submit("1111");
        assert_eq!(c.screen, Screen::RepeatPin);
        c.back();
        assert_eq!(c.screen, Screen::NewPin);
        c.submit("2222");
        assert_eq!(c.submit("2222"), Step::SetPin("2222".into(), true));
        assert!(c.succeeded() && c.title() == "Đổi mã PIN thành công");

        let u = HiddenChatFlow::new(FlowKind::Unhide("c".into(), false), true);
        assert_eq!(u.screen, Screen::UnhideConfirm);
        let mut o = HiddenChatFlow::new(FlowKind::Open("c".into()), true);
        assert_eq!(o.title(), "Nhập mã PIN để mở trò chuyện");
        assert_eq!(o.submit("1234"), Step::VerifyToOpen("1234".into()));
        let mut s = HiddenChatFlow::new(FlowKind::SetPin, false);
        s.submit("1");
        s.submit("1");
        s.pin_already_exists();
        assert_eq!(s.error.as_deref(), Some(PIN_ALREADY_SET));
        assert!(hidden_guard::should_relock(60.0) && !hidden_guard::should_relock(5.0));
        assert!(hidden_guard::is_stalled(Some(0.0), 31.0) && !hidden_guard::is_stalled(None, 99.0));
    }

    #[test]
    fn ui_removal_relocks_the_unlocked_one() {
        let mut ui = ConversationActionUI::default();
        ui.unlocked_id = Some("a".into());
        ui.remove(HashSet::new());
        assert!(ui.removal.is_none());
        ui.remove(HashSet::from(["a".to_string()]));
        assert!(ui.unlocked_id.is_none() && ui.removal.is_some());
    }
}
