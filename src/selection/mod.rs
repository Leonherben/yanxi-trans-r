#[cfg(target_os = "linux")]
pub mod linux_x11;
#[cfg(target_os = "linux")]
pub use linux_x11::LinuxX11SelectionListener;
pub mod hotkey;
pub use hotkey::start_global_hotkey_listener;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::WindowsSelectionListener;

/// 选词事件实体
#[derive(Debug, Clone)]
pub enum SelectionEvent {
    Selected {
        text: String,
        pos: (i32, i32), // 鼠标当前屏幕绝对坐标 (x, y)
    },
    Cleared {
        pos: (i32, i32),
    },
}

/// 对捕获的选中文本进行边界清洗与合规性检查
pub fn sanitize_selection_text(text: &str, min_len: usize, max_len: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let char_count = trimmed.chars().count();
    if char_count < min_len || char_count > max_len {
        return None;
    }

    // 过滤全是不可见符号或标点符号的非法选区
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }

    Some(trimmed.to_string())
}

/// 手动翻译选区时允许的最大 Unicode 字符数（不是 UTF-8 字节数）。
pub const MAX_SELECTION_CHARS: usize = 3000;

pub fn validate_selection_text(text: &str) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("请先选中需要翻译的文字".into());
    }
    if trimmed.chars().count() > MAX_SELECTION_CHARS {
        return Err(format!(
            "选中文字过长，请控制在 {MAX_SELECTION_CHARS} 个字符以内"
        ));
    }
    if !trimmed.chars().any(char::is_alphanumeric) {
        return Err("选区中只有标点或符号，请选择包含文字的内容".into());
    }
    Ok(trimmed.to_owned())
}

/// 只读取当前 PRIMARY 选区；没有选区时不读取历史剪贴板。
pub fn get_current_selection() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        linux_x11::get_current_selection()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_filter_counts_characters() {
        assert_eq!(sanitize_selection_text(" 中文 ", 2, 2), Some("中文".into()));
        assert!(sanitize_selection_text("中", 2, 3).is_none());
        assert!(sanitize_selection_text("中文语", 1, 2).is_none());
        assert!(sanitize_selection_text("。！？", 1, 10).is_none());
    }

    #[test]
    fn manual_selection_validation_boundaries() {
        let at_limit = "中".repeat(MAX_SELECTION_CHARS);
        assert_eq!(validate_selection_text(&at_limit), Ok(at_limit.clone()));
        assert!(
            validate_selection_text(&(at_limit + "文"))
                .unwrap_err()
                .contains("3000")
        );
        assert!(validate_selection_text(" \n\t").is_err());
        assert!(validate_selection_text("……？！").is_err());
        assert_eq!(
            validate_selection_text(" 你好，world！ "),
            Ok("你好，world！".into())
        );
    }
}

/// 选词监听器通用 Trait
pub trait SelectionListener: Send + Sync {
    /// 启动监听，并在有选词事件发生时向 Channel 发送数据
    fn start(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<SelectionEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

    /// 停止监听并释放系统资源
    fn stop(&self);

    /// 查询当前是否处于监听运行中
    fn is_running(&self) -> bool;
}

/// 创建当前平台最优的选词监听器实现
pub fn create_selection_listener(
    debounce_ms: u64,
    min_length: usize,
    max_length: usize,
) -> Box<dyn SelectionListener> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxX11SelectionListener::new(
            debounce_ms,
            min_length,
            max_length,
        ))
    }
    #[cfg(windows)]
    {
        Box::new(WindowsSelectionListener::new(
            debounce_ms,
            min_length,
            max_length,
        ))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        compile_error!("当前操作系统平台暂未支持划词监听器");
    }
}
