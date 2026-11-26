use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RPC_E_CHANGED_MODE, S_FALSE, S_OK, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::DataExchange::{GetClipboardSequenceNumber, IsClipboardFormatAvailable};
use windows::Win32::System::Ole::{CF_BITMAP, CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationTextPattern, UIA_TextPatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetDoubleClickTime, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS,
    KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_LWIN, VK_MENU,
    VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetClassNameW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONUP, WM_QUIT,
    WM_RBUTTONUP, WindowFromPoint,
};

use super::{
    MAX_SELECTION_CHARS, SelectionDedup, SelectionEvent, SelectionListener, sanitize_selection_text,
};

#[derive(Clone, Copy)]
enum TriggerKind {
    Selection { seq: u64 },
    OutsideClick,
}

#[derive(Clone, Copy)]
struct TriggerEvent {
    pos: (i32, i32),
    kind: TriggerKind,
}

static HOOK_DOWN_X: AtomicI32 = AtomicI32::new(0);
static HOOK_DOWN_Y: AtomicI32 = AtomicI32::new(0);
static HOOK_LAST_UP_X: AtomicI32 = AtomicI32::new(0);
static HOOK_LAST_UP_Y: AtomicI32 = AtomicI32::new(0);
static HOOK_LAST_UP_MS: AtomicU32 = AtomicU32::new(0);
static HOOK_DBL_CLICK_MS: AtomicU32 = AtomicU32::new(500);
static HOOK_TRIGGER_SEQ: AtomicU32 = AtomicU32::new(0);
static HOOK_TRIGGER_TX: Mutex<Option<mpsc::SyncSender<TriggerEvent>>> = Mutex::new(None);

struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

fn is_own_process_window(hwnd: HWND) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid == std::process::id()
}

fn window_class_name(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let mut buf = [0u16; 128];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

fn is_desktop_or_taskbar(hwnd: HWND) -> bool {
    matches!(
        window_class_name(hwnd).as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
    )
}

fn is_console_window(hwnd: HWND) -> bool {
    matches!(
        window_class_name(hwnd).as_str(),
        "ConsoleWindowClass" | "CASCADIA_HOSTING_WINDOW_CLASS" | "mintty"
    )
}

fn read_uia_selection() -> Option<String> {
    // Use MTA for UIA calls on non-pumping worker threads so COM never stalls waiting on an STA loop.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let _guard = if hr == S_OK || hr == S_FALSE {
        ComGuard(true)
    } else if hr == RPC_E_CHANGED_MODE {
        ComGuard(false)
    } else {
        return None;
    };

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }.ok()?;
    let element = unsafe { automation.GetFocusedElement() }.ok()?;
    if unsafe { element.CurrentProcessId() }.ok() == Some(std::process::id() as i32) {
        return None;
    }

    let pattern: IUIAutomationTextPattern =
        unsafe { element.GetCurrentPatternAs(UIA_TextPatternId) }.ok()?;
    let ranges = unsafe { pattern.GetSelection() }.ok()?;
    let len = unsafe { ranges.Length() }.ok()?;
    if len <= 0 {
        return None;
    }

    let limit = (MAX_SELECTION_CHARS as i32) + 1;
    let mut parts = Vec::new();
    for idx in 0..len {
        if let Ok(range) = unsafe { ranges.GetElement(idx) } {
            if let Ok(bstr) = unsafe { range.GetText(limit) } {
                let text = bstr.to_string();
                if !text.trim().is_empty() {
                    parts.push(text);
                }
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn key_is_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

fn any_non_ctrl_modifier_down() -> bool {
    key_is_down(VK_MENU)
        || key_is_down(VK_SHIFT)
        || key_is_down(VK_LWIN)
        || key_is_down(VK_RWIN)
}

fn clipboard_has_non_text_payload() -> bool {
    let has_text = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() };
    if has_text {
        return false;
    }
    unsafe {
        IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok()
            || IsClipboardFormatAvailable(CF_BITMAP.0 as u32).is_ok()
            || IsClipboardFormatAvailable(CF_DIB.0 as u32).is_ok()
    }
}

fn send_ctrl_c() {
    let key_event = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        key_event(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        key_event(VK_C, KEYBD_EVENT_FLAGS(0)),
        key_event(VK_C, KEYEVENTF_KEYUP),
        key_event(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn read_via_clipboard_fallback(wait_for_modifiers: bool) -> Option<String> {
    let fg = unsafe { GetForegroundWindow() };
    if fg.0.is_null()
        || is_own_process_window(fg)
        || is_desktop_or_taskbar(fg)
        || is_console_window(fg)
    {
        return None;
    }

    // Do not clobber copied files or screenshots on the user's clipboard.
    if clipboard_has_non_text_payload() {
        return None;
    }

    if wait_for_modifiers {
        let deadline = Instant::now() + Duration::from_millis(200);
        while any_non_ctrl_modifier_down() || key_is_down(VK_CONTROL) {
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    } else if key_is_down(VK_CONTROL)
        || key_is_down(VK_MENU)
        || key_is_down(VK_LWIN)
        || key_is_down(VK_RWIN)
    {
        return None;
    }

    let mut clipboard = arboard::Clipboard::new().ok()?;
    let previous_text = clipboard.get_text().ok();
    let seq_before = unsafe { GetClipboardSequenceNumber() };

    send_ctrl_c();

    let deadline = Instant::now() + Duration::from_millis(180);
    let mut changed = false;
    while Instant::now() < deadline {
        if unsafe { GetClipboardSequenceNumber() } != seq_before {
            changed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    if !changed {
        return None;
    }

    let copied = clipboard.get_text().ok();
    match previous_text {
        Some(prev) => {
            let _ = clipboard.set_text(prev);
        }
        None => {
            let _ = clipboard.clear();
        }
    }
    copied
}

/// 读取当前选区文本：优先使用 UI Automation，必要时通过受保护的 Ctrl+C 回退。
pub fn get_current_selection() -> Option<String> {
    read_uia_selection().or_else(|| read_via_clipboard_fallback(true))
}

unsafe extern "system" fn mouse_hook_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode >= 0 && lparam.0 != 0 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if (info.flags & LLMHF_INJECTED) == 0 {
            let msg = wparam.0 as u32;
            if msg == WM_LBUTTONDOWN {
                HOOK_DOWN_X.store(info.pt.x, Ordering::Relaxed);
                HOOK_DOWN_Y.store(info.pt.y, Ordering::Relaxed);
            } else if msg == WM_LBUTTONUP {
                let up_x = info.pt.x;
                let up_y = info.pt.y;
                let down_x = HOOK_DOWN_X.load(Ordering::Relaxed);
                let down_y = HOOK_DOWN_Y.load(Ordering::Relaxed);
                let is_drag = (up_x - down_x).abs() >= 6 || (up_y - down_y).abs() >= 6;

                let last_up_x = HOOK_LAST_UP_X.swap(up_x, Ordering::Relaxed);
                let last_up_y = HOOK_LAST_UP_Y.swap(up_y, Ordering::Relaxed);
                let last_up_ms = HOOK_LAST_UP_MS.swap(info.time, Ordering::Relaxed);
                let dbl_ms = HOOK_DBL_CLICK_MS.load(Ordering::Relaxed);
                let is_double_click = !is_drag
                    && last_up_ms != 0
                    && info.time.saturating_sub(last_up_ms) <= dbl_ms
                    && (up_x - last_up_x).abs() <= 6
                    && (up_y - last_up_y).abs() <= 6;

                let shift_held = key_is_down(VK_SHIFT);
                let kind = if is_drag || is_double_click || shift_held {
                    let seq = HOOK_TRIGGER_SEQ.fetch_add(1, Ordering::Relaxed).wrapping_add(1) as u64;
                    Some(TriggerKind::Selection { seq })
                } else {
                    Some(TriggerKind::OutsideClick)
                };
                if let Some(kind) = kind {
                    if let Ok(guard) = HOOK_TRIGGER_TX.try_lock() {
                        if let Some(tx) = guard.as_ref() {
                            let _ = tx.try_send(TriggerEvent {
                                pos: (up_x, up_y),
                                kind,
                            });
                        }
                    }
                }
            } else if msg == WM_RBUTTONUP || msg == WM_MBUTTONUP {
                if let Ok(guard) = HOOK_TRIGGER_TX.try_lock() {
                    if let Some(tx) = guard.as_ref() {
                        let _ = tx.try_send(TriggerEvent {
                            pos: (info.pt.x, info.pt.y),
                            kind: TriggerKind::OutsideClick,
                        });
                    }
                }
            }
        }
    }
    unsafe { CallNextHookEx(None, ncode, wparam, lparam) }
}

pub struct WindowsSelectionListener {
    debounce_ms: u64,
    min_length: usize,
    max_length: usize,
    is_running: Arc<AtomicBool>,
    hook_thread_id: Arc<AtomicU32>,
}

impl WindowsSelectionListener {
    pub fn new(debounce_ms: u64, min_length: usize, max_length: usize) -> Self {
        Self {
            debounce_ms,
            min_length,
            max_length,
            is_running: Arc::new(AtomicBool::new(false)),
            hook_thread_id: Arc::new(AtomicU32::new(0)),
        }
    }
}

impl SelectionListener for WindowsSelectionListener {
    fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
        let tid = self.hook_thread_id.swap(0, Ordering::SeqCst);
        if tid != 0 {
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Ok(mut guard) = HOOK_TRIGGER_TX.lock() {
            *guard = None;
        }
    }

    fn start(
        &self,
        tx: UnboundedSender<SelectionEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let dbl_ms = unsafe { GetDoubleClickTime() };
        if dbl_ms > 0 {
            HOOK_DBL_CLICK_MS.store(dbl_ms, Ordering::Relaxed);
        }

        let (trigger_tx, trigger_rx) = mpsc::sync_channel::<TriggerEvent>(16);
        if let Ok(mut guard) = HOOK_TRIGGER_TX.lock() {
            *guard = Some(trigger_tx);
        }

        let is_running_worker = Arc::clone(&self.is_running);
        let debounce_ms = self.debounce_ms;
        let min_length = self.min_length;
        let max_length = self.max_length;

        std::thread::Builder::new()
            .name("yanxi-win-selection-worker".into())
            .spawn(move || {
                let mut dedup = SelectionDedup::default();
                while let Ok(mut latest) = trigger_rx.recv() {
                    if !is_running_worker.load(Ordering::SeqCst) {
                        break;
                    }
                    let seq = match latest.kind {
                        TriggerKind::OutsideClick => {
                            let _ = tx.send(SelectionEvent::OutsideClick { pos: latest.pos });
                            continue;
                        }
                        TriggerKind::Selection { seq } => seq,
                    };
                    let mut latest_seq = seq;
                    if debounce_ms > 0 {
                        std::thread::sleep(Duration::from_millis(debounce_ms));
                        while let Ok(newer) = trigger_rx.try_recv() {
                            match newer.kind {
                                TriggerKind::OutsideClick => {
                                    let _ = tx.send(SelectionEvent::OutsideClick { pos: newer.pos });
                                }
                                TriggerKind::Selection { seq: new_seq } => {
                                    latest = newer;
                                    latest_seq = new_seq;
                                }
                            }
                        }
                    }
                    if !is_running_worker.load(Ordering::SeqCst) {
                        break;
                    }
                    if HOOK_TRIGGER_SEQ.load(Ordering::Relaxed) as u64 != latest_seq {
                        continue;
                    }

                    let at_point = unsafe {
                        WindowFromPoint(POINT {
                            x: latest.pos.0,
                            y: latest.pos.1,
                        })
                    };
                    let fg = unsafe { GetForegroundWindow() };
                    if is_own_process_window(at_point)
                        || is_own_process_window(fg)
                        || is_desktop_or_taskbar(at_point)
                    {
                        continue;
                    }

                    let text = read_uia_selection()
                        .or_else(|| read_via_clipboard_fallback(false));
                    if let Some(raw) = text {
                        if let Some(clean) = sanitize_selection_text(&raw, min_length, max_length) {
                            if dedup.accept(&clean, Instant::now()) {
                                let _ = tx.send(SelectionEvent::Selected {
                                    text: clean,
                                    pos: latest.pos,
                                });
                            }
                        }
                    }
                }
            })?;

        let is_running_hook = Arc::clone(&self.is_running);
        let hook_tid = Arc::clone(&self.hook_thread_id);
        std::thread::Builder::new()
            .name("yanxi-win-mouse-hook".into())
            .spawn(move || {
                let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
                hook_tid.store(tid, Ordering::SeqCst);

                let hook = match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), None, 0) } {
                    Ok(h) => h,
                    Err(e) => {
                        eprintln!("[Yanxi Windows] 安装鼠标划词钩子失败: {e}");
                        is_running_hook.store(false, Ordering::SeqCst);
                        hook_tid.store(0, Ordering::SeqCst);
                        return;
                    }
                };

                let mut msg = MSG::default();
                while is_running_hook.load(Ordering::SeqCst) {
                    let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                    if ret.0 <= 0 {
                        break;
                    }
                }

                unsafe {
                    let _ = UnhookWindowsHookEx(hook);
                }
                hook_tid.store(0, Ordering::SeqCst);
                is_running_hook.store(false, Ordering::SeqCst);
            })
            .map_err(|e| {
                self.stop();
                e
            })?;

        Ok(())
    }
}
