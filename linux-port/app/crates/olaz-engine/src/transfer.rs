//! Phone → PC message transfer: the official first-login offer and the sync progress, bridged to `window.__zaloTransfer`
//! (posts `{transfer:{…}}` and `{transferAvailable:{canStart, reason}}`). Port of `ZaloEngine+Transfer.swift`.

use serde_json::{Map, Value, json};

use crate::engine::{Change, ZaloEngine};
use crate::models::J;

#[derive(Debug, Clone, PartialEq)]
pub enum TransferPhase {
    Offer,
    WaitingForPhone,
    Transferring,
    Finished,
    Failed(String),
}

impl TransferPhase {
    pub fn adapter_name(&self) -> &'static str {
        match self {
            TransferPhase::Offer => "offer",
            TransferPhase::WaitingForPhone => "waitingForPhone",
            TransferPhase::Transferring => "transferring",
            TransferPhase::Finished => "finished",
            TransferPhase::Failed(_) => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageTransferState {
    pub phase: TransferPhase,
    pub progress: Option<f64>,
    pub synced_conversations: i64,
    pub total_conversations: Option<i64>,
    pub message: Option<String>,
    pub stage: Option<String>,
    pub stage_index: Option<i64>,
    pub stage_count: Option<i64>,
    pub stage_progress: Option<f64>,
    pub received_messages: Option<i64>,
    pub error_code: Option<i64>,
    pub error_name: Option<String>,
    pub can_retry: bool,
    pub official_title: Option<String>,
    pub official_description: Option<String>,
    pub official_suggestion: Option<String>,
}

impl MessageTransferState {
    pub fn waiting_detail(&self) -> &'static str {
        match self.stage.as_deref() {
            Some("confirmMobile") => "Yêu cầu đồng bộ đã được gửi. Hãy chấp nhận yêu cầu trong thông báo của Zalo trên điện thoại.",
            Some("waitForBackup") => "Điện thoại đang chuẩn bị dữ liệu tin nhắn. Hãy giữ Zalo mở trên điện thoại.",
            _ => "Mở ứng dụng Zalo trên điện thoại, sau đó chọn xác nhận đồng bộ tin nhắn sang máy tính.",
        }
    }
    pub fn stage_title(&self) -> &'static str {
        match self.stage.as_deref() {
            Some("conversationList") => "Đang tải danh sách hội thoại",
            Some("messages") => "Đang đồng bộ tin nhắn gần đây",
            Some("otherMessages") => "Đang đồng bộ các tin nhắn cũ hơn",
            Some("downloading") => "Đang tải dữ liệu từ điện thoại",
            Some("decrypting") => "Đang giải mã dữ liệu",
            Some("restoring") => "Đang khôi phục tin nhắn",
            _ => "Đang đồng bộ",
        }
    }
    pub fn transfer_detail(&self) -> String {
        if let Some(r) = self.received_messages.filter(|r| *r > 0) {
            return format!("Đã nhận {} tin nhắn", grouped(r));
        }
        if let Some(t) = self.total_conversations.filter(|t| *t > 0) {
            return format!("Đã đồng bộ {}/{t} hội thoại", self.synced_conversations);
        }
        if self.synced_conversations > 0 {
            return format!("Đã đồng bộ {} hội thoại", self.synced_conversations);
        }
        self.stage_title().into()
    }
}

fn grouped(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

pub fn transfer_blocked_text(reason: &str) -> &'static str {
    match reason {
        "INVALID_STATE" | "SYNC_RUNNING" => "Zalo đang tự đồng bộ với điện thoại. Bạn có thể đồng bộ lại khi xong.",
        "WORKER_NOT_READY" => "Zalo đang chuẩn bị dữ liệu, hãy chờ giây lát.",
        "DB_MIGRATING" => "Zalo đang sắp xếp lại dữ liệu trên máy, hãy chờ giây lát.",
        "MOBILE_NOT_SUPPORT" => "Zalo trên điện thoại chưa hỗ trợ đồng bộ này. Hãy cập nhật Zalo trên điện thoại.",
        "DISABLED" | "SYNC_SRC_DISABLED" => "Tài khoản này chưa được Zalo bật đồng bộ tin nhắn từ điện thoại.",
        "NO_PERMISSION" => "Thiết bị này chưa được phép đồng bộ tin nhắn.",
        _ => "Chưa thể đồng bộ lúc này, hãy thử lại sau.",
    }
}

pub fn transfer_error_text(code: Option<i64>, name: Option<&str>) -> String {
    match name.unwrap_or("") {
        "NO_NETWORK" | "ERR_CONNECTION_TIMED_OUT" | "REQUEST_SOCKET_CLOSED" => "Không có kết nối mạng. Hãy kiểm tra mạng rồi thử lại.".into(),
        "MOBILE_LOW_STORAGE" => "Bộ nhớ điện thoại không đủ để đồng bộ.".into(),
        "PC_LOW_STORAGE" => "Bộ nhớ trên máy tính không đủ để đồng bộ.".into(),
        "MOBILE_BUSY" | "MOBILE_BUSY_SYNCING" | "MOBILE_BUSY_MANUAL_BACKUP" => "Điện thoại đang bận. Hãy thử lại sau ít phút.".into(),
        "MOBILE_LOGGED_OUT" => "Zalo trên điện thoại đã đăng xuất.".into(),
        "USER_REJECT_BACKUP" | "MOBILE_USER_CANCELS_SYNC" => "Yêu cầu đồng bộ đã bị từ chối trên điện thoại.".into(),
        "USER_DONT_CONFIRM" | "SYNC_REQUEST_TIMEOUT_NO_NEED_CONFIRM" | "MOBILE_DONT_AUTO_CONFIRM" | "REQUEST_TIMEOUT" => "Điện thoại chưa xác nhận yêu cầu đồng bộ.".into(),
        "MOBILE_IDLE_TIMEOUT" | "WAKE_UP_TIMEOUT" => "Zalo trên điện thoại đã ngừng phản hồi. Hãy mở Zalo trên điện thoại rồi thử lại.".into(),
        "BATCH_TIMEOUT" | "DOWNLOAD_TIMEOUT" => "Quá trình đồng bộ bị gián đoạn do quá thời gian chờ.".into(),
        "CLIENT_NOT_SUPPORT" => "Phiên bản Zalo trên điện thoại chưa hỗ trợ đồng bộ này.".into(),
        _ => match code {
            Some(c) => format!("Đồng bộ không thành công (mã lỗi {c})."),
            None => "Đồng bộ không thành công.".into(),
        },
    }
}

fn int(v: Option<&Value>) -> Option<i64> {
    v.and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
}
fn unit(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).map(|f| f.clamp(0.0, 1.0))
}

pub fn parse_transfer(j: &Value) -> Option<MessageTransferState> {
    let error_name = j.opt_s("errorName");
    let error_code = int(j.get_v("errorCode"));
    let phase = match j.opt_s("phase")?.as_str() {
        "offer" => TransferPhase::Offer,
        "waitingForPhone" => TransferPhase::WaitingForPhone,
        "transferring" => TransferPhase::Transferring,
        "finished" => TransferPhase::Finished,
        "failed" => TransferPhase::Failed(transfer_error_text(error_code, error_name.as_deref())),
        _ => return None,
    };
    let text = |k: &str| j.opt_s(k).filter(|s| !s.is_empty());
    Some(MessageTransferState {
        phase,
        progress: unit(j.get_v("progress")),
        synced_conversations: int(j.get_v("syncedConversations")).unwrap_or(0),
        total_conversations: int(j.get_v("totalConversations")),
        message: text("message"),
        stage: text("stage"),
        stage_index: int(j.get_v("stageIndex")),
        stage_count: int(j.get_v("stageCount")),
        stage_progress: unit(j.get_v("stageProgress")),
        received_messages: int(j.get_v("receivedMessages")),
        error_code,
        error_name,
        can_retry: j.b("canRetry"),
        official_title: text("officialTitle"),
        official_description: text("officialDescription"),
        official_suggestion: text("officialSuggestion"),
    })
}

impl ZaloEngine {
    pub fn respond_to_transfer(&self, accept: bool) {
        let phase = self.state.borrow().transfer.as_ref().map(|t| t.phase.adapter_name());
        self.call_chat("respond", vec![json!(accept), json!(phase)], "__zaloTransfer");
    }

    pub fn dismiss_transfer(&self) {
        let phase = self.state.borrow_mut().transfer.take().map(|t| t.phase.adapter_name());
        self.call_chat("dismiss", vec![json!(phase)], "__zaloTransfer");
        self.changed(Change::Notice);
    }

    pub fn start_transfer(&self) {
        self.call_chat("start", vec![], "__zaloTransfer");
    }

    pub(crate) fn handle_transfer_availability(&self, state: &Map<String, Value>) {
        let j = state.get("transferAvailable").cloned().unwrap_or(Value::Null);
        let next = (j.b("canStart"), j.opt_s("reason"));
        let mut s = self.state.borrow_mut();
        if (s.can_start_transfer, s.transfer_blocked_reason.clone()) != next {
            s.can_start_transfer = next.0;
            s.transfer_blocked_reason = next.1;
            drop(s);
            self.changed(Change::Notice);
        }
    }

    pub(crate) fn handle_transfer(&self, state: &Map<String, Value>) {
        let next = state.get("transfer").and_then(parse_transfer);
        if state.get("transfer").and_then(|t| t.get_v("phase")).is_some() && next.is_none() {
            olaz_core::log::write("transfer: unknown phase");
            return;
        }
        if self.state.borrow().transfer != next {
            let finished = next.as_ref().is_some_and(|t| t.phase == TransferPhase::Finished);
            self.state.borrow_mut().transfer = next;
            self.changed(Change::Notice);
            if finished {
                // "Hoàn tất" closes by itself after 4 s.
                let (weak, exec) = (self.weak(), self.exec.clone());
                olaz_core::exec::spawn(&self.exec, async move {
                    exec.timer(std::time::Duration::from_secs(4)).await;
                    if let Some(me) = weak.upgrade() {
                        if me.state.borrow().transfer.as_ref().is_some_and(|t| t.phase == TransferPhase::Finished) {
                            me.dismiss_transfer();
                        }
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phases_and_texts() {
        let t = parse_transfer(&json!({"phase": "transferring", "progress": 1.5, "syncedConversations": 3, "totalConversations": 9, "stage": "messages", "stageIndex": 2, "stageCount": 4})).unwrap();
        assert_eq!((t.progress, t.transfer_detail(), t.stage_title()), (Some(1.0), "Đã đồng bộ 3/9 hội thoại".to_string(), "Đang đồng bộ tin nhắn gần đây"));
        let r = parse_transfer(&json!({"phase": "transferring", "receivedMessages": 12345})).unwrap();
        assert_eq!(r.transfer_detail(), "Đã nhận 12.345 tin nhắn");
        let f = parse_transfer(&json!({"phase": "failed", "errorName": "MOBILE_LOGGED_OUT", "canRetry": true})).unwrap();
        assert_eq!(f.phase, TransferPhase::Failed("Zalo trên điện thoại đã đăng xuất.".into()));
        assert_eq!(transfer_error_text(Some(42), None), "Đồng bộ không thành công (mã lỗi 42).");
        assert!(parse_transfer(&json!({"phase": "weird"})).is_none());
        assert_eq!(transfer_blocked_text("DB_MIGRATING"), "Zalo đang sắp xếp lại dữ liệu trên máy, hãy chờ giây lát.");
        let w = parse_transfer(&json!({"phase": "waitingForPhone", "stage": "confirmMobile"})).unwrap();
        assert!(w.waiting_detail().starts_with("Yêu cầu đồng bộ đã được gửi"));
    }
}
