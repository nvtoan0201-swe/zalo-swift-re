//! Guided tour steps. Linux: "⌘K" → "Ctrl+K", "⇧Return" → "Shift+Enter", "⌥⌘I" → "Ctrl+Alt+I".

pub struct TourStep {
    pub id: &'static str,
    pub title: &'static str,
    pub message: &'static str,
    pub symbol: &'static str,
}

pub const STEPS: [TourStep; 5] = [
    TourStep {
        id: "mailboxes",
        title: "Hộp thư ở thanh bên",
        message: "Tin nhắn, Khác, Chưa đọc và Người lạ là các hộp thư. Cloud của tôi và Danh bạ cũng nằm ở đây. Lọc theo thẻ phân loại bằng nút thẻ cạnh ô tìm kiếm.",
        symbol: "tray.2",
    },
    TourStep {
        id: "search",
        title: "Tìm mọi thứ với Ctrl+K",
        message: "Gõ tên một người hoặc nhóm để mở ngay. Nhấn Ctrl+K ở bất kỳ đâu để mở bảng tìm nhanh.",
        symbol: "magnifyingglass",
    },
    TourStep {
        id: "message",
        title: "Rê chuột lên tin nhắn",
        message: "Rê chuột lên tin nhắn để thả cảm xúc hoặc trả lời. Chuột phải để chuyển tiếp, ghim, thu hồi và hơn thế nữa.",
        symbol: "face.smiling",
    },
    TourStep {
        id: "composer",
        title: "Nhắn tin, sticker và bình chọn",
        message: "Nhấn + để chọn sticker, emoji hoặc tạo bình chọn trong nhóm. Enter để gửi, Shift+Enter để xuống dòng, gõ @ để nhắc tên thành viên.",
        symbol: "plus.bubble",
    },
    TourStep {
        id: "toolbar",
        title: "Gọi điện và xem thông tin",
        message: "Gọi thoại, gọi video và mở bảng thông tin (Ctrl+Alt+I) với thành viên, ảnh, tệp và liên kết trong cuộc trò chuyện.",
        symbol: "phone",
    },
];
