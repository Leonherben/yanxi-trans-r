use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

use super::{SelectionEvent, SelectionListener};

pub struct WindowsSelectionListener {
    _debounce_ms: u64,
    _min_length: usize,
    _max_length: usize,
    is_running: Arc<AtomicBool>,
}

impl WindowsSelectionListener {
    pub fn new(debounce_ms: u64, min_length: usize, max_length: usize) -> Self {
        Self {
            _debounce_ms: debounce_ms,
            _min_length: min_length,
            _max_length: max_length,
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SelectionListener for WindowsSelectionListener {
    fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    fn start(
        &self,
        _tx: UnboundedSender<SelectionEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.is_running.store(true, Ordering::SeqCst);
        // Windows 平台划词与剪贴板监听预留接入点
        Ok(())
    }
}
