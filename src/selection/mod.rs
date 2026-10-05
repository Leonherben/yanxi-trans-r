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

    if trimmed.len() < min_len || trimmed.len() > max_len {
        return None;
    }

    // 过滤全是不可见符号或标点符号的非法选区
    if !trimmed.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }

    Some(trimmed.to_string())
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
        Box::new(LinuxX11SelectionListener::new(debounce_ms, min_length, max_length))
    }
    #[cfg(windows)]
    {
        Box::new(WindowsSelectionListener::new(debounce_ms, min_length, max_length))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        compile_error!("当前操作系统平台暂未支持划词监听器");
    }
}
