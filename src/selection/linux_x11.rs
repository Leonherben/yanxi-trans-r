use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use x11rb::connection::Connection;
use x11rb::protocol::xfixes;
use x11rb::protocol::xinput;
use x11rb::protocol::xproto::{
    self, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, KeyButMask, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::rust_connection::{DefaultStream, PollMode, Stream};
use x11rb::utils::RawFdContainer;

use super::{SelectionDedup, SelectionEvent, SelectionListener, sanitize_selection_text};

/// x11rb's default poll can wait indefinitely. This stream bounds handshake,
/// property replies, and flushing as well as the selection notification itself.
struct DeadlineStream {
    inner: DefaultStream,
    deadline: Instant,
}

impl DeadlineStream {
    fn check_deadline(&self) -> std::io::Result<()> {
        if Instant::now() >= self.deadline {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "X11 selection deadline",
            ))
        } else {
            Ok(())
        }
    }
}

impl Stream for DeadlineStream {
    fn poll(&self, _mode: PollMode) -> std::io::Result<()> {
        self.check_deadline()?;
        // Stream::poll explicitly permits spurious readiness. The read/write
        // calls remain nonblocking, so sleeping briefly avoids a busy loop.
        std::thread::sleep(Duration::from_millis(2));
        self.check_deadline()
    }

    fn read(&self, buf: &mut [u8], fds: &mut Vec<RawFdContainer>) -> std::io::Result<usize> {
        self.check_deadline()?;
        self.inner.read(buf, fds)
    }

    fn write(&self, buf: &[u8], fds: &mut Vec<RawFdContainer>) -> std::io::Result<usize> {
        self.check_deadline()?;
        self.inner.write(buf, fds)
    }
}

/// Linux's std UnixStream::connect has no timeout. Create a nonblocking socket
/// so even a full X server listen backlog cannot stall the hotkey thread.
fn connect_local_deadline(path: &str, deadline: Instant) -> Option<std::os::unix::net::UnixStream> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    #[repr(C)]
    struct UnixAddress {
        family: u16,
        path: [u8; 108],
    }
    unsafe extern "C" {
        #[link_name = "socket"]
        fn socket_raw(domain: i32, kind: i32, protocol: i32) -> i32;
        #[link_name = "connect"]
        fn connect_raw(fd: i32, address: *const UnixAddress, len: u32) -> i32;
    }
    let bytes = path.as_bytes();
    if bytes.len() >= 108 || bytes.contains(&0) {
        return None;
    }
    let mut address = UnixAddress {
        family: 1,
        path: [0; 108],
    }; // AF_UNIX
    address.path[..bytes.len()].copy_from_slice(bytes);
    // Linux SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC; no shared fd ownership.
    let fd = unsafe { socket_raw(1, 1 | 0x800 | 0x80000, 0) };
    if fd < 0 {
        return None;
    }
    // SAFETY: socket returned a newly owned valid fd, transferred exactly once.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    while Instant::now() < deadline {
        // SAFETY: address has Linux sockaddr_un layout and its length includes
        // the initialized, NUL-terminated path; fd stays alive through the call.
        let result =
            unsafe { connect_raw(owned.as_raw_fd(), &address, (2 + bytes.len() + 1) as u32) };
        if result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(106) {
            return Some(std::os::unix::net::UnixStream::from(owned)); // EISCONN
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(4 | 11 | 114 | 115) => std::thread::sleep(Duration::from_millis(2)),
            _ => return None,
        }
    }
    None
}

/// Direct PRIMARY conversion avoids arboard's unspecified blocking timeout.
/// No helper threads, processes, or CLIPBOARD requests are created.
pub fn get_current_selection() -> Option<String> {
    use x11rb::reexports::x11rb_protocol::{parse_display, xauth};
    let deadline = Instant::now() + Duration::from_millis(500);
    let display = parse_display::parse_display(None).ok()?;
    let screen_num = usize::from(display.screen);
    // Only Unix sockets and numeric TCP addresses: avoid unbounded DNS/connect.
    let mut connected = None;
    for addr in display.connect_instruction() {
        if Instant::now() >= deadline {
            return None;
        }
        let candidate = match addr {
            parse_display::ConnectAddress::Socket(path) => connect_local_deadline(&path, deadline)
                .and_then(|stream| DefaultStream::from_unix_stream(stream).ok()),
            parse_display::ConnectAddress::Hostname(host, port) => {
                let ip = if host == "localhost" {
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
                } else {
                    host.parse::<std::net::IpAddr>().ok()?
                };
                let socket = std::net::SocketAddr::new(ip, port);
                let timeout = deadline.saturating_duration_since(Instant::now());
                std::net::TcpStream::connect_timeout(&socket, timeout)
                    .ok()
                    .and_then(|stream| DefaultStream::from_tcp_stream(stream).ok())
            }
            _ => None,
        };
        if let Some(value) = candidate {
            connected = Some(value);
            break;
        }
    }
    let (inner, (family, address)) = connected?;
    let (auth_name, auth_data) = xauth::get_auth(family, &address, display.display)
        .ok()
        .flatten()
        .unwrap_or_default();
    let conn = RustConnection::connect_to_stream_with_auth_info(
        DeadlineStream { inner, deadline },
        screen_num,
        auth_name,
        auth_data,
    )
    .ok()?;
    let root = conn.setup().roots[screen_num].root;
    let window = conn.generate_id().ok()?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .ok()?
    .check()
    .ok()?;
    // Dropping this private connection destroys its auxiliary window even on timeout.
    let primary = u32::from(AtomEnum::PRIMARY);
    if conn.get_selection_owner(primary).ok()?.reply().ok()?.owner == 0 {
        return None;
    }
    let utf8 = conn
        .intern_atom(false, b"UTF8_STRING")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let property = conn
        .intern_atom(false, b"YANXI_CURRENT_SELECTION")
        .ok()?
        .reply()
        .ok()?
        .atom;
    for target in [utf8, u32::from(AtomEnum::STRING)] {
        conn.convert_selection(window, primary, target, property, x11rb::CURRENT_TIME)
            .ok()?;
        conn.flush().ok()?;
        while Instant::now() < deadline {
            match conn.poll_for_event().ok()? {
                Some(x11rb::protocol::Event::SelectionNotify(event))
                    if event.requestor == window
                        && event.selection == primary
                        && event.target == target =>
                {
                    if event.property == 0 {
                        break;
                    }
                    // Read enough to let the validator report >3000 characters, never truncate
                    // an oversized property into apparently valid input. INCR is unsupported.
                    let limit = (super::MAX_SELECTION_CHARS + 1) * 4;
                    let reply = conn
                        .get_property(
                            true,
                            window,
                            property,
                            AtomEnum::ANY,
                            0,
                            limit.div_ceil(4) as u32,
                        )
                        .ok()?
                        .reply()
                        .ok()?;
                    if reply.format != 8 || reply.type_ != target {
                        return None;
                    }
                    if reply.bytes_after > 0 {
                        // At least 12004 bytes => at least 3001 Unicode characters.
                        // Return the actual prefix, which validation will reject as too long.
                        return Some(String::from_utf8_lossy(&reply.value).into_owned());
                    }
                    let text = if target == utf8 {
                        String::from_utf8(reply.value).ok()?
                    } else {
                        reply.value.into_iter().map(char::from).collect()
                    };
                    return (!text.trim().is_empty()).then_some(text);
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }
    None
}

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

                if let Err(e) = xfixes::select_selection_input(
                    &conn,
                    dummy_window,
                    primary_atom,
                    selection_mask,
                ) {
                    eprintln!("[Yanxi X11] 注册 XFixes 选区监听失败: {e}");
                    is_running.store(false, Ordering::SeqCst);
                    return;
                }

                // 被动监听根窗口原始鼠标按键释放，用于未固定弹窗的外部点击自动收起（零指针抓取）
                let xinput_ready = xinput::xi_query_version(&conn, 2, 0)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .is_some()
                    && xinput::xi_select_events(
                        &conn,
                        root,
                        &[xinput::EventMask {
                            deviceid: 1, // XIAllMasterDevices
                            mask: vec![
                                xinput::XIEventMask::RAW_BUTTON_PRESS
                                    | xinput::XIEventMask::RAW_BUTTON_RELEASE,
                            ],
                        }],
                    )
                    .is_ok();
                if !xinput_ready {
                    eprintln!("[Yanxi X11] XInput2 被动鼠标监听不可用，将仅保留失焦自动收起");
                }

                let _ = conn.flush();
                println!("[Yanxi X11] 🎯 X11 选区监听线程启动完成");

                let mut pending_pos: Option<(i32, i32)> = None;
                let mut dedup = SelectionDedup::default();
                let mut last_button_press: Option<(u32, (i32, i32))> = None;

                while is_running.load(Ordering::SeqCst) {
                    let event = match conn.poll_for_event() {
                        Ok(Some(ev)) => ev,
                        Ok(None) => {
                            std::thread::sleep(Duration::from_millis(20));
                            continue;
                        }
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
                                    dedup.clear();
                                    pending_pos = None;
                                    let _ = tx.send(SelectionEvent::Cleared { pos });
                                } else {
                                    // 用户划词产生了新的选区
                                    if debounce_ms > 0 {
                                        let deadline = std::time::Instant::now()
                                            + Duration::from_millis(debounce_ms);
                                        while std::time::Instant::now() < deadline
                                            && is_running.load(Ordering::SeqCst)
                                        {
                                            std::thread::sleep(Duration::from_millis(10));
                                        }
                                    }

                                    // 智能消抖：检测鼠标左键是否仍在按住状态（即处于连续拖拽中）
                                    for _ in 0..10 {
                                        if !is_running.load(Ordering::SeqCst) {
                                            break;
                                        }
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
                            if notify.requestor == dummy_window
                                && notify.selection == primary_atom
                                && pending_pos.is_some()
                            {
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

                                let pos = pending_pos.take().unwrap_or((0, 0));

                                if let Some(clean) =
                                    sanitize_selection_text(&text, min_length, max_length)
                                {
                                    if dedup.accept(&clean, Instant::now()) {
                                        let _ =
                                            tx.send(SelectionEvent::Selected { text: clean, pos });
                                    }
                                }
                            }
                        }
                        x11rb::protocol::Event::XinputRawButtonPress(ev) => {
                            if matches!(ev.detail, 1..=3) {
                                if let Some(pos) = xproto::query_pointer(&conn, root)
                                    .ok()
                                    .and_then(|c| c.reply().ok())
                                    .map(|p| (p.root_x as i32, p.root_y as i32))
                                {
                                    last_button_press = Some((ev.detail, pos));
                                }
                            }
                        }
                        x11rb::protocol::Event::XinputRawButtonRelease(ev) => {
                            if matches!(ev.detail, 1..=3) {
                                if let Some(pos) = xproto::query_pointer(&conn, root)
                                    .ok()
                                    .and_then(|c| c.reply().ok())
                                    .map(|p| (p.root_x as i32, p.root_y as i32))
                                {
                                    let is_click = match last_button_press.take() {
                                        Some((btn, down)) if btn == ev.detail => {
                                            (pos.0 - down.0).abs() < 6
                                                && (pos.1 - down.1).abs() < 6
                                        }
                                        _ => true,
                                    };
                                    if is_click {
                                        let _ = tx.send(SelectionEvent::OutsideClick { pos });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }

                let _ = conn.destroy_window(dummy_window);
                let _ = conn.flush();
                is_running.store(false, Ordering::SeqCst);
            })
            .map_err(|e| {
                self.is_running.store(false, Ordering::SeqCst);
                e
            })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the synthetic PRIMARY owner from verify_desktop.py"]
    fn reads_synthetic_primary_fixture() {
        let started = Instant::now();
        assert_eq!(get_current_selection(), Some("Hello world".into()));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
