use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use x11rb::connection::Connection;
use x11rb::protocol::xfixes;
use x11rb::protocol::xproto::{
    self, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, KeyButMask, WindowClass,
};
use x11rb::rust_connection::RustConnection;

use super::{sanitize_selection_text, SelectionEvent, SelectionListener};

pub struct LinuxX11SelectionListener {
    debounce_ms: u64,
    min_length: usize,
    max_length: usize,
    is_running: Arc<AtomicBool>,
}

impl LinuxX11SelectionListener {
    pub fn new(debounce_ms: u64, min_length: usize, max_length: usize) -> Self {
        Self {
            debounce_ms,
            min_length,
            max_length,
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl SelectionListener for LinuxX11SelectionListener {
    fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }

    fn start(
        &self,
        tx: UnboundedSender<SelectionEvent>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let is_running = Arc::clone(&self.is_running);
        let debounce_ms = self.debounce_ms;
        let min_length = self.min_length;
        let max_length = self.max_length;

        std::thread::Builder::new()
            .name("yanxi-x11-selection".into())
            .spawn(move || {
                let (conn, screen_num) = match RustConnection::connect(None) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[Yanxi X11] 无法连接至 X11 显示服务: {e}");
                        is_running.store(false, Ordering::SeqCst);
                        return;
                    }
                };

                let screen = &conn.setup().roots[screen_num];
                let root = screen.root;

                // 协商 XFixes 扩展 (5.0)
                if let Err(e) = xfixes::query_version(&conn, 5, 0).map_err(|e| e.to_string()) {
                    eprintln!("[Yanxi X11] 无法查询 XFixes 扩展: {e}");
                    is_running.store(false, Ordering::SeqCst);
                    return;
                }

                // 创建未映射的隐藏接收窗口
                let dummy_window = match conn.generate_id() {
                    Ok(id) => id,
                    Err(e) => {
                        eprintln!("[Yanxi X11] 生成窗口 ID 失败: {e}");
                        is_running.store(false, Ordering::SeqCst);
                        return;
                    }
                };

                let mut aux = CreateWindowAux::new();
                aux.event_mask = Some(EventMask::PROPERTY_CHANGE);

                if let Err(e) = conn.create_window(
                    x11rb::COPY_DEPTH_FROM_PARENT,
                    dummy_window,
                    root,
                    0,
                    0,
                    1,
                    1,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &aux,
                ) {
                    eprintln!("[Yanxi X11] 创建辅助窗口失败: {e}");
                    is_running.store(false, Ordering::SeqCst);
                    return;
                }

                let primary_atom: u32 = AtomEnum::PRIMARY.into();
                let utf8_atom = conn
                    .intern_atom(false, b"UTF8_STRING")
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| r.atom)
                    .unwrap_or(AtomEnum::STRING.into());
                let yanxi_prop = conn
                    .intern_atom(false, b"YANXI_SELECTION")
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| r.atom)
                    .unwrap_or(utf8_atom);

                // 通过 XFixes 监听 PRIMARY 选区变动
                let selection_mask = xfixes::SelectionEventMask::SET_SELECTION_OWNER
                    | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
                    | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE;

                if let Err(e) =
                    xfixes::select_selection_input(&conn, dummy_window, primary_atom, selection_mask)
                {
                    eprintln!("[Yanxi X11] 注册 XFixes 选区监听失败: {e}");
                    is_running.store(false, Ordering::SeqCst);
                    return;
                }

                let _ = conn.flush();
                println!("[Yanxi X11] 🎯 X11 选区监听线程启动完成");

                let mut pending_pos: Option<(i32, i32)> = None;
                let mut last_text = String::new();

                while is_running.load(Ordering::SeqCst) {
                    let event = match conn.wait_for_event() {
                        Ok(ev) => ev,
                        Err(e) => {
                            eprintln!("[Yanxi X11] X11 事件循环异常中断: {e}");
                            break;
                        }
                    };

                    match event {
                        x11rb::protocol::Event::XfixesSelectionNotify(notify) => {
                            if notify.selection == primary_atom {
                                if notify.owner == 0 {
                                    // 选区被释放清空 (例如点击了空白区域取消选区)
                                    let pos = xproto::query_pointer(&conn, root)
                                        .ok()
                                        .and_then(|c| c.reply().ok())
                                        .map(|p| (p.root_x as i32, p.root_y as i32))
                                        .unwrap_or((0, 0));
                                    last_text.clear();
                                    let _ = tx.send(SelectionEvent::Cleared { pos });
                                } else {
                                    // 用户划词产生了新的选区
                                    if debounce_ms > 0 {
                                        std::thread::sleep(Duration::from_millis(debounce_ms));
                                    }

                                    // 智能消抖：检测鼠标左键是否仍在按住状态（即处于连续拖拽中）
                                    for _ in 0..10 {
                                        if let Some(reply) = xproto::query_pointer(&conn, root)
                                            .ok()
                                            .and_then(|c| c.reply().ok())
                                        {
                                            if !reply.mask.contains(KeyButMask::BUTTON1) {
                                                break;
                                            }
                                        }
                                        std::thread::sleep(Duration::from_millis(30));
                                    }

                                    // 获取当前鼠标绝对坐标
                                    let pos = xproto::query_pointer(&conn, root)
                                        .ok()
                                        .and_then(|c| c.reply().ok())
                                        .map(|p| (p.root_x as i32, p.root_y as i32))
                                        .unwrap_or((0, 0));

                                    pending_pos = Some(pos);

                                    // 请求转换选区数据至 UTF8_STRING
                                    let _ = conn.convert_selection(
                                        dummy_window,
                                        primary_atom,
                                        utf8_atom,
                                        yanxi_prop,
                                        x11rb::CURRENT_TIME,
                                    );
                                    let _ = conn.flush();
                                }
                            }
                        }
                        x11rb::protocol::Event::SelectionNotify(notify) => {
                            if notify.requestor == dummy_window && notify.selection == primary_atom {
                                let mut text = String::new();

                                if notify.property != 0 {
                                    if let Some(reply) = conn
                                        .get_property(
                                            true, // 读取后自动清理 property
                                            dummy_window,
                                            notify.property,
                                            AtomEnum::ANY,
                                            0,
                                            1024 * 1024 / 4,
                                        )
                                        .ok()
                                        .and_then(|c| c.reply().ok())
                                    {
                                        text = String::from_utf8_lossy(&reply.value).to_string();
                                    }
                                }

                                // 备选方案：若部分窗口未按 ICCCM 规范响应属性，自动降级至系统级 xsel 捕获
                                if text.trim().is_empty() {
                                    if let Ok(out) = std::process::Command::new("xsel")
                                        .arg("-o")
                                        .output()
                                    {
                                        if out.status.success() {
                                            text = String::from_utf8_lossy(&out.stdout).to_string();
                                        }
                                    }
                                }

                                let pos = pending_pos.take().unwrap_or((0, 0));

                                if let Some(clean) =
                                    sanitize_selection_text(&text, min_length, max_length)
                                {
                                    if clean != last_text {
                                        last_text = clean.clone();
                                        let _ = tx.send(SelectionEvent::Selected {
                                            text: clean,
                                            pos,
                                        });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                is_running.store(false, Ordering::SeqCst);
            })?;

        Ok(())
    }
}
