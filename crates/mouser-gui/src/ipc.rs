//! Pipe client for the resident agent: blocking overlapped I/O on one handle (so the reader
//! thread and the UI thread's writes never serialise against each other), a reader thread
//! that forwards decoded messages, and "launch the agent and retry for 5 s" start-up.

use mouser_core::protocol::{self, AgentMessage, ClientMessage, PROTOCOL_VERSION};
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, ERROR_IO_PENDING, ERROR_MORE_DATA, GetLastError, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, ReadFile, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, DETACHED_PROCESS, CREATE_NEW_PROCESS_GROUP};

/// Default pipe of the agent of this session and user.
pub fn default_pipe_name() -> String {
    let sid = mouser_win::app_detect::current_user_sid().unwrap_or_default();
    protocol::pipe_name(crate::sys::session_id(), &sid)
}

/// How long the GUI keeps retrying while the agent starts.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Something that happened on the connection, delivered to the UI thread.
#[derive(Debug)]
pub enum Event {
    Message(AgentMessage),
    /// Connected and handshaken.
    Up,
    /// Could not connect, or the agent went away (reason for the error state).
    Down(String),
}

/// An overlapped byte-mode pipe handle; reads and writes may run concurrently.
struct Pipe(File);

impl Pipe {
    fn open(name: &str) -> io::Result<Self> {
        // SECURITY_IDENTIFICATION: a squatting server may identify us but cannot impersonate us.
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OVERLAPPED)
            .security_qos_flags(SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION)
            .open(name)
            .map(Pipe)
    }

    /// Run one overlapped request to completion.
    fn run(&self, op: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32) -> io::Result<usize> {
        let h = self.0.as_raw_handle() as HANDLE;
        // SAFETY: plain Win32 calls; `ov` and the event outlive the wait below.
        unsafe {
            let ev = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
            if ev.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = ev;
            let ok = op(h, &mut ov);
            let mut n = 0u32;
            let res = if ok != 0 {
                GetOverlappedResult(h, &ov, &mut n, 0)
            } else if GetLastError() == ERROR_IO_PENDING {
                GetOverlappedResult(h, &ov, &mut n, 1)
            } else {
                0
            };
            let err = GetLastError();
            CloseHandle(ev);
            if res != 0 || err == ERROR_MORE_DATA {
                Ok(n as usize)
            } else if err == ERROR_BROKEN_PIPE || err == ERROR_HANDLE_EOF {
                Ok(0)
            } else {
                Err(io::Error::from_raw_os_error(err as i32))
            }
        }
    }
}

struct Reader(Arc<Pipe>);

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let p = buf.as_mut_ptr();
        // SAFETY: `p`/`len` describe `buf`, which lives across the blocking call.
        self.0.run(|h, ov| unsafe { ReadFile(h, p, len, std::ptr::null_mut(), ov) })
    }
}

/// Outgoing queue depth; a full queue drops the message instead of blocking the UI thread.
const QUEUE: usize = 256;

/// Write half. `send` only enqueues; a writer thread owns the blocking `WriteFile`.
#[derive(Clone, Default)]
pub struct Sender(Arc<Mutex<Option<mpsc::SyncSender<Vec<u8>>>>>);

impl Sender {
    /// Queue one message; silently dropped when not connected (the UI shows the error state).
    pub fn send(&self, msg: &ClientMessage) {
        let Ok(line) = protocol::encode_line(msg) else { return };
        if let Some(tx) = self.0.lock().ok().and_then(|g| g.clone()) {
            let _ = tx.try_send(line.into_bytes());
        }
    }

    /// Start the writer thread for `pipe` and route `send` to it.
    fn attach(&self, pipe: Arc<Pipe>) {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(QUEUE);
        std::thread::spawn(move || {
            'outer: for line in rx {
                let mut data = line.as_slice();
                while !data.is_empty() {
                    let p = data.as_ptr();
                    let len = data.len() as u32;
                    // SAFETY: `p`/`len` describe `data`, alive during the call.
                    match pipe.run(|h, ov| unsafe { WriteFile(h, p, len, std::ptr::null_mut(), ov) }) {
                        Ok(n) if n > 0 => data = &data[n..],
                        _ => break 'outer,
                    }
                }
            }
        });
        if let Ok(mut g) = self.0.lock() {
            *g = Some(tx);
        }
    }

    fn detach(&self) {
        if let Ok(mut g) = self.0.lock() {
            *g = None;
        }
    }
}

/// `mouser-agent.exe` next to this executable.
pub fn agent_path() -> Option<std::path::PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("mouser-agent.exe"))
}

fn launch_agent() -> io::Result<()> {
    let path = agent_path().ok_or_else(|| io::Error::other("no exe dir"))?;
    Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags_detached()
        .spawn()
        .map(drop)
}

trait Detached {
    fn creation_flags_detached(&mut self) -> &mut Self;
}
impl Detached for Command {
    fn creation_flags_detached(&mut self) -> &mut Self {
        std::os::windows::process::CommandExt::creation_flags(self, DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
    }
}

/// Connect (launching the agent once if it is not running), then run the reader loop on a
/// background thread. `deliver` is called from that thread; it must hop to the UI thread.
pub fn connect(pipe: String, launch: bool, sender: Sender, deliver: impl Fn(Event) + Send + 'static) {
    std::thread::spawn(move || {
        let start = Instant::now();
        let mut launched = false;
        let pipe_handle = loop {
            match Pipe::open(&pipe) {
                Ok(p) => break Arc::new(p),
                Err(e) => {
                    if !launched && launch && e.kind() == io::ErrorKind::NotFound {
                        launched = true;
                        if let Err(e) = launch_agent() {
                            deliver(Event::Down(format!("cannot start mouser-agent.exe: {e}")));
                            return;
                        }
                    }
                    if start.elapsed() >= CONNECT_TIMEOUT {
                        deliver(Event::Down(format!("{pipe}: {e}")));
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        sender.attach(pipe_handle.clone());
        sender.send(&ClientMessage::Hello { version: PROTOCOL_VERSION });
        sender.send(&ClientMessage::GetConfig);
        sender.send(&ClientMessage::GetStatus);
        deliver(Event::Up);
        let mut rd = BufReader::new(Reader(pipe_handle));
        let mut buf = Vec::new();
        let why = loop {
            match protocol::read_line_bounded(&mut rd, &mut buf) {
                Ok(true) => match std::str::from_utf8(&buf).ok().map(protocol::decode_line::<AgentMessage>) {
                    Some(Ok(AgentMessage::ShuttingDown)) => break "the agent is shutting down".to_string(),
                    Some(Ok(m)) => deliver(Event::Message(m)),
                    _ => {} // unknown message from a newer agent: ignore
                },
                Ok(false) => break "the agent closed the connection".to_string(),
                Err(e) => break e.to_string(),
            }
        };
        sender.detach();
        deliver(Event::Down(why));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::os::windows::io::FromRawHandle;
    use std::sync::mpsc;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT};

    struct SendHandle(HANDLE);
    // SAFETY: a pipe handle may be used from any thread.
    unsafe impl Send for SendHandle {}

    fn server(name: &str) -> std::thread::JoinHandle<Vec<ClientMessage>> {
        let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // Created before returning so the client can never race the pipe's existence.
        // SAFETY: standard pipe server setup; the handle is moved into a File below.
        let handle = unsafe {
            let h = CreateNamedPipeW(w.as_ptr(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 4096, 4096, 0, std::ptr::null());
            assert_ne!(h, INVALID_HANDLE_VALUE);
            SendHandle(h)
        };
        std::thread::spawn(move || {
            let handle = handle;
            // SAFETY: handle created above, owned only here.
            let f = unsafe {
                ConnectNamedPipe(handle.0, std::ptr::null_mut());
                File::from_raw_handle(handle.0 as _)
            };
            let mut rd = BufReader::new(&f);
            let mut got = Vec::new();
            let mut line = String::new();
            while got.len() < 4 && rd.read_line(&mut line).unwrap_or(0) > 0 {
                let m: ClientMessage = protocol::decode_line(&line).unwrap();
                if m == ClientMessage::GetConfig {
                    let cfg = AgentMessage::Config { config: serde_json::json!({"version": 11}) };
                    (&f).write_all(protocol::encode_line(&cfg).unwrap().as_bytes()).unwrap();
                }
                got.push(m);
                line.clear();
            }
            got
        })
    }

    #[test]
    fn handshake_read_and_concurrent_write_over_one_handle() {
        let name = format!(r"\\.\pipe\mouser-gui-test-{}", std::process::id());
        let srv = server(&name);
        let (tx, rx) = mpsc::channel();
        let sender = Sender::default();
        let tx = Mutex::new(tx);
        connect(name, false, sender.clone(), move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Up));
        // The reader thread is now blocked in ReadFile; a write must still go through.
        sender.send(&ClientMessage::SetDpi { dpi: 1600 });
        let mut saw_config = false;
        while let Ok(e) = rx.recv_timeout(Duration::from_secs(5)) {
            if matches!(e, Event::Message(AgentMessage::Config { .. })) {
                saw_config = true;
                break;
            }
        }
        assert!(saw_config);
        let got = srv.join().unwrap();
        assert_eq!(got[0], ClientMessage::Hello { version: PROTOCOL_VERSION });
        assert!(got.contains(&ClientMessage::SetDpi { dpi: 1600 }));
    }

    #[test]
    fn missing_agent_reports_down_after_the_retry_window() {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let t0 = Instant::now();
        connect(r"\\.\pipe\mouser-gui-test-absent".into(), false, Sender::default(), move |e| {
            let _ = tx.lock().unwrap().send(e);
        });
        let ev = rx.recv_timeout(Duration::from_secs(8)).unwrap();
        assert!(matches!(ev, Event::Down(_)));
        assert!(t0.elapsed() >= Duration::from_secs(4), "must retry for ~5 s, took {:?}", t0.elapsed());
    }
}
