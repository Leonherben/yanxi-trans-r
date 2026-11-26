#[cfg(unix)]
mod platform {
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
        pub fn check(
            app_name: &str,
        ) -> Result<InstanceCheck, Box<dyn std::error::Error + Send + Sync>> {
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
                        for mut s in listener.incoming().flatten() {
                            let mut buf = [0u8; 64];
                            if let Ok(n) = s.read(&mut buf) {
                                if &buf[..n] == b"ACTIVATE\n" || n > 0 {
                                    on_activate();
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
}

#[cfg(windows)]
mod platform {
    use sha2::{Digest, Sha256};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY,
        ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_SHARE_NONE, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
        ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
    };
    use windows::core::HSTRING;

    struct PipeHandle(HANDLE);

    // SAFETY: The wrapped Win32 named pipe handle is owned uniquely by SingleInstance
    // and moved to at most one listener thread.
    unsafe impl Send for PipeHandle {}

    impl Drop for PipeHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    pub struct SingleInstance {
        handle: Option<PipeHandle>,
    }

    pub enum InstanceCheck {
        Primary(SingleInstance),
        AlreadyRunning,
    }

    fn scoped_pipe_name(app_name: &str) -> String {
        let scope = std::env::var_os("XDG_RUNTIME_DIR")
            .or_else(|| dirs::data_local_dir().map(|p| p.into_os_string()))
            .or_else(|| std::env::var_os("USERNAME"))
            .unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(scope.to_string_lossy().as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        format!(r"\\.\pipe\yanxi-{app_name}-{}", &digest[..12])
    }

    fn try_activate_pipe(pipe_wide: &HSTRING) -> Result<bool, windows::core::Error> {
        let access = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0;
        let open = || unsafe {
            CreateFileW(
                pipe_wide,
                access,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };

        let handle = match open() {
            Ok(h) => h,
            Err(err) if err.code() == ERROR_PIPE_BUSY.to_hresult() => {
                unsafe {
                    let _ = WaitNamedPipeW(pipe_wide, 500);
                }
                match open() {
                    Ok(h) => h,
                    Err(retry_err) if retry_err.code() == ERROR_FILE_NOT_FOUND.to_hresult() => {
                        return Ok(false);
                    }
                    Err(retry_err) => return Err(retry_err),
                }
            }
            Err(err) if err.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(false),
            Err(err) => return Err(err),
        };

        let guard = PipeHandle(handle);
        let mut written = 0u32;
        unsafe {
            let _ = WriteFile(guard.0, Some(b"ACTIVATE\n"), Some(&mut written), None);
            let _ = FlushFileBuffers(guard.0);
        }
        Ok(true)
    }

    impl SingleInstance {
        pub fn check(
            app_name: &str,
        ) -> Result<InstanceCheck, Box<dyn std::error::Error + Send + Sync>> {
            let pipe_wide = HSTRING::from(scoped_pipe_name(app_name));

            if try_activate_pipe(&pipe_wide)? {
                return Ok(InstanceCheck::AlreadyRunning);
            }

            let handle = unsafe {
                CreateNamedPipeW(
                    &pipe_wide,
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1,
                    1024,
                    1024,
                    0,
                    None,
                )
            };

            if handle.is_invalid() {
                let err = unsafe { GetLastError() };
                if err == ERROR_ACCESS_DENIED && try_activate_pipe(&pipe_wide)? {
                    return Ok(InstanceCheck::AlreadyRunning);
                }
                return Err(format!("创建 Windows 命名管道失败: {err:?}").into());
            }

            Ok(InstanceCheck::Primary(Self {
                handle: Some(PipeHandle(handle)),
            }))
        }

        pub fn listen_activations<F>(&mut self, on_activate: F)
        where
            F: Fn() + Send + 'static,
        {
            if let Some(handle) = self.handle.take() {
                std::thread::Builder::new()
                    .name("yanxi-ipc-listener".into())
                    .spawn(move || {
                        loop {
                            let connected = unsafe { ConnectNamedPipe(handle.0, None) };
                            match connected {
                                Ok(()) => {}
                                Err(err) if err.code() == ERROR_PIPE_CONNECTED.to_hresult() => {}
                                Err(_) => break,
                            }

                            let mut buf = [0u8; 64];
                            let mut read = 0u32;
                            let ok = unsafe {
                                ReadFile(handle.0, Some(&mut buf), Some(&mut read), None).is_ok()
                            };
                            if ok && read > 0 {
                                on_activate();
                            }

                            unsafe {
                                let _ = DisconnectNamedPipe(handle.0);
                            }
                        }
                    })
                    .expect("Failed to spawn IPC listener thread");
            }
        }
    }
}

pub use platform::{InstanceCheck, SingleInstance};
