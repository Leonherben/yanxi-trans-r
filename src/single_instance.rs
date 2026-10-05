use std::path::PathBuf;

pub struct SingleInstance {
    socket_path: PathBuf,
    _listener: Option<std::os::unix::net::UnixListener>,
}

pub enum InstanceCheck {
    Primary(SingleInstance),
    AlreadyRunning,
}

impl SingleInstance {
    pub fn check(app_name: &str) -> Result<InstanceCheck, Box<dyn std::error::Error + Send + Sync>> {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::data_local_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join("yanxi")
            });

        let _ = std::fs::create_dir_all(&runtime_dir);
        let socket_path = runtime_dir.join(format!("{app_name}.sock"));

        // 1. 尝试连接已存在的 Socket
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket_path) {
            use std::io::Write;
            let _ = stream.write_all(b"ACTIVATE\n");
            return Ok(InstanceCheck::AlreadyRunning);
        }

        // 2. 若连接失败，可能残留了旧文件的死锁，清理后绑定
        let _ = std::fs::remove_file(&socket_path);
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        listener.set_nonblocking(true)?;

        Ok(InstanceCheck::Primary(Self {
            socket_path,
            _listener: Some(listener),
        }))
    }

    pub fn listen_activations<F>(&mut self, on_activate: F)
    where
        F: Fn() + Send + 'static,
    {
        if let Some(listener) = self._listener.take() {
            std::thread::Builder::new()
                .name("yanxi-ipc-listener".into())
                .spawn(move || {
                    use std::io::Read;
                    // 设置阻塞模式用于工作线程
                    let _ = listener.set_nonblocking(false);
                    for stream in listener.incoming() {
                        if let Ok(mut s) = stream {
                            let mut buf = [0u8; 64];
                            if let Ok(n) = s.read(&mut buf) {
                                if &buf[..n] == b"ACTIVATE\n" || n > 0 {
                                    on_activate();
                                }
                            }
                        }
                    }
                })
                .expect("Failed to spawn IPC listener thread");
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
