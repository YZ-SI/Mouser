//! Named-pipe IPC server (JSON lines, `mouser_core::protocol`), one client at a time.
//!
//! * The pipe is created with a DACL granting only the current user's SID and SYSTEM
//!   (`D:P(A;;GA;;;<sid>)(A;;GA;;;SY)`), `FILE_FLAG_FIRST_PIPE_INSTANCE` (no squatting), at most
//!   one instance and `PIPE_REJECT_REMOTE_CLIENTS`.
//! * One thread does everything. The handle is overlapped so that one thread can wait for either
//!   "client sent bytes" or "something to push" (`Outbox::send` signals an event); there is no
//!   polling and no timer while idle. A synchronous handle would serialize a pending read against
//!   writes from other threads.
//! * Reads go through `read_line_bounded` (1 MiB cap); an over-long or malformed first message
//!   drops the connection. Writes to a stalled client time out after `WRITE_TIMEOUT_MS` and drop it.

use std::collections::VecDeque;
use std::io::{self, BufReader, Read};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use mouser_core::protocol::{
    AgentMessage, ClientMessage, PROTOCOL_VERSION, ProtocolError, decode_line, encode_line, read_line_bounded,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_CONNECTED,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

use mouser_win::wide;

use crate::log::log;

const WRITE_TIMEOUT_MS: u32 = 2000;
const OUT_CAP: usize = 256;
/// Byte bound of the outbox too (a stalled GUI must not pin megabytes of config broadcasts).
const OUT_BYTES: usize = 4 * 1024 * 1024;
/// A client that has not sent a valid Hello this soon after connecting is dropped.
static HELLO_MS: AtomicU32 = AtomicU32::new(5000);
const PIPE_BUF: u32 = 16 * 1024;

struct Ev(HANDLE);
// SAFETY: an event HANDLE is a kernel object usable from any thread.
unsafe impl Send for Ev {}
unsafe impl Sync for Ev {}
impl Drop for Ev {
    fn drop(&mut self) {
        // SAFETY: owned handle closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn new_event(manual: bool) -> io::Result<Ev> {
    // SAFETY: plain event creation.
    let h = unsafe { CreateEventW(null(), manual.into(), 0, null()) };
    if h.is_null() { Err(io::Error::last_os_error()) } else { Ok(Ev(h)) }
}

/// Outgoing message queue shared with the engine; `send` is a no-op while no client is connected.
pub struct Outbox {
    q: Mutex<(VecDeque<String>, usize)>,
    wake: Ev,
    client: AtomicBool,
}

impl Outbox {
    pub fn new() -> io::Result<Arc<Outbox>> {
        Ok(Arc::new(Outbox { q: Mutex::new((VecDeque::new(), 0)), wake: new_event(false)?, client: AtomicBool::new(false) }))
    }

    pub fn has_client(&self) -> bool {
        self.client.load(Ordering::Acquire)
    }

    /// Queue a message for the connected client (drops the oldest line beyond `OUT_CAP`).
    pub fn send(&self, m: &AgentMessage) {
        if !self.has_client() {
            return;
        }
        let Ok(line) = encode_line(m) else { return };
        let mut g = self.q.lock().unwrap_or_else(|e| e.into_inner());
        let (q, bytes) = &mut *g;
        while q.len() >= OUT_CAP || (*bytes + line.len() > OUT_BYTES && !q.is_empty()) {
            if let Some(old) = q.pop_front() {
                *bytes -= old.len();
            }
        }
        *bytes += line.len();
        q.push_back(line);
        drop(g);
        self.signal();
    }

    pub fn signal(&self) {
        // SAFETY: valid event handle.
        unsafe { SetEvent(self.wake.0) };
    }

    fn take(&self) -> Option<String> {
        let mut g = self.q.lock().unwrap_or_else(|e| e.into_inner());
        let l = g.0.pop_front()?;
        g.1 -= l.len();
        Some(l)
    }

    fn clear(&self) {
        let mut g = self.q.lock().unwrap_or_else(|e| e.into_inner());
        g.0.clear();
        g.1 = 0;
    }

    #[cfg(test)]
    fn queued(&self) -> (usize, usize) {
        let g = self.q.lock().unwrap_or_else(|e| e.into_inner());
        (g.0.len(), g.1)
    }
}

/// Handles one decoded client message; returns false to close the connection.
pub trait Backend: Send + Sync {
    fn handle(&self, msg: ClientMessage, out: &Outbox) -> bool;
    fn on_disconnect(&self);
    /// A client completed the Hello handshake (its replies are flushed right after `Welcome`).
    fn on_hello(&self, _out: &Outbox) {}
}

// ------------------------------------------------------------------ security

pub fn pipe_sddl(sid: &str) -> String {
    format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
}

struct Sd(PSECURITY_DESCRIPTOR);
// SAFETY: immutable LocalAlloc memory, freed once by its owner.
unsafe impl Send for Sd {}
impl Drop for Sd {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW (LocalAlloc).
        unsafe { LocalFree(self.0) };
    }
}

fn descriptor(sddl: &str) -> io::Result<Sd> {
    let mut p: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: NUL-terminated SDDL, valid out pointer.
    if unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(wide(sddl).as_ptr(), SDDL_REVISION_1, &mut p, null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Sd(p))
}

// ------------------------------------------------------------------ pipe I/O

struct Pipe(HANDLE);
// SAFETY: a pipe HANDLE may be used from one thread at a time.
unsafe impl Send for Pipe {}
impl Drop for Pipe {
    fn drop(&mut self) {
        // No DisconnectNamedPipe: it would discard bytes the client has not read yet (ShuttingDown).
        // SAFETY: owned handle.
        unsafe { CloseHandle(self.0) };
    }
}

fn create_pipe(name: &str, sd: &Sd) -> io::Result<Pipe> {
    let sa = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: 0 };
    // SAFETY: valid NUL-terminated name and attributes.
    let h = unsafe {
        CreateNamedPipeW(
            wide(name).as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUF,
            PIPE_BUF,
            0,
            &sa,
        )
    };
    if h == INVALID_HANDLE_VALUE { Err(io::Error::last_os_error()) } else { Ok(Pipe(h)) }
}

/// Blocking `Read` over an overlapped pipe that also flushes the outbox while it waits.
struct PipeIo<'a> {
    pipe: &'a Pipe,
    out: &'a Outbox,
    stop: &'a AtomicBool,
    read_ev: Ev,
    write_ev: Ev,
    /// Hello deadline; `None` once the client has said hello.
    deadline: Option<Instant>,
}

impl PipeIo<'_> {
    fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let mut off = 0;
        while off < data.len() {
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.hEvent = self.write_ev.0;
            let mut n = 0u32;
            // SAFETY: `data` and `ov` outlive the wait below.
            let ok = unsafe { WriteFile(self.pipe.0, data[off..].as_ptr(), (data.len() - off) as u32, null_mut(), &mut ov) };
            if ok == 0 {
                // SAFETY: trivial.
                let e = unsafe { GetLastError() };
                if e != ERROR_IO_PENDING {
                    return Err(io::Error::from_raw_os_error(e as i32));
                }
            }
            // SAFETY: valid event.
            if unsafe { WaitForSingleObject(self.write_ev.0, WRITE_TIMEOUT_MS) } == WAIT_TIMEOUT {
                // SAFETY: cancel then wait for the kernel to release `ov`/`data`.
                unsafe {
                    CancelIoEx(self.pipe.0, &ov);
                    GetOverlappedResult(self.pipe.0, &ov, &mut n, 1);
                }
                return Err(io::Error::new(io::ErrorKind::TimedOut, "client not reading"));
            }
            // SAFETY: completed operation.
            if unsafe { GetOverlappedResult(self.pipe.0, &ov, &mut n, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            off += n as usize;
        }
        Ok(())
    }

    fn flush_outbox(&self) -> io::Result<()> {
        while let Some(line) = self.out.take() {
            self.write_all(line.as_bytes())?;
        }
        Ok(())
    }
}

impl Read for PipeIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = self.read_ev.0;
        // SAFETY: `buf` and `ov` stay alive until this function returns, and it never returns while the read is pending.
        let ok = unsafe { ReadFile(self.pipe.0, buf.as_mut_ptr(), buf.len() as u32, null_mut(), &mut ov) };
        if ok == 0 {
            // SAFETY: trivial.
            let e = unsafe { GetLastError() };
            match e {
                ERROR_IO_PENDING => {}
                ERROR_BROKEN_PIPE | ERROR_NO_DATA => return Ok(0),
                _ => return Err(io::Error::from_raw_os_error(e as i32)),
            }
        }
        let handles = [self.read_ev.0, self.out.wake.0];
        loop {
            let ms = self.deadline.map_or(INFINITE, |d| d.saturating_duration_since(Instant::now()).as_millis().min(u128::from(u32::MAX - 1)) as u32);
            // SAFETY: two valid handles.
            let w = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, ms) };
            if w == WAIT_TIMEOUT {
                let mut n = 0u32;
                // SAFETY: cancel and wait so the kernel stops touching `buf`/`ov`.
                unsafe {
                    CancelIoEx(self.pipe.0, &ov);
                    GetOverlappedResult(self.pipe.0, &ov, &mut n, 1);
                }
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no hello"));
            }
            if w == WAIT_OBJECT_0 {
                let mut n = 0u32;
                // SAFETY: completed overlapped read.
                if unsafe { GetOverlappedResult(self.pipe.0, &ov, &mut n, 0) } == 0 {
                    // SAFETY: trivial.
                    let e = unsafe { GetLastError() };
                    return if e == ERROR_BROKEN_PIPE { Ok(0) } else { Err(io::Error::from_raw_os_error(e as i32)) };
                }
                return Ok(n as usize);
            }
            if w == WAIT_OBJECT_0 + 1 {
                let flushed = self.flush_outbox();
                if flushed.is_err() || self.stop.load(Ordering::SeqCst) {
                    let mut n = 0u32;
                    // SAFETY: cancel and wait so the kernel stops touching `buf`/`ov`.
                    unsafe {
                        CancelIoEx(self.pipe.0, &ov);
                        GetOverlappedResult(self.pipe.0, &ov, &mut n, 1);
                    }
                    return match flushed {
                        Err(e) => Err(e),
                        Ok(()) => Ok(0),
                    };
                }
                continue;
            }
            return Err(io::Error::last_os_error());
        }
    }
}

// ------------------------------------------------------------------ server

pub struct IpcServer {
    join: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    out: Arc<Outbox>,
}

impl IpcServer {
    pub fn start(name: String, sddl: String, out: Arc<Outbox>, backend: Arc<dyn Backend>) -> io::Result<IpcServer> {
        let sd = descriptor(&sddl)?;
        // Fail early (and visibly) if the pipe cannot be created at all.
        let first = create_pipe(&name, &sd)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, o2) = (stop.clone(), out.clone());
        let join = std::thread::Builder::new()
            .name("MouserIpc".into())
            .stack_size(256 * 1024)
            .spawn(move || server_loop(name, sd, first, s2, o2, backend))?;
        Ok(IpcServer { join: Some(join), stop, out })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.out.signal();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn server_loop(name: String, sd: Sd, first: Pipe, stop: Arc<AtomicBool>, out: Arc<Outbox>, backend: Arc<dyn Backend>) {
    let mut next = Some(first);
    let mut warned = false;
    let Ok(conn_ev) = new_event(true) else { return };
    while !stop.load(Ordering::SeqCst) {
        let pipe = match next.take() {
            Some(p) => p,
            None => match create_pipe(&name, &sd) {
                Ok(p) => {
                    warned = false;
                    p
                }
                Err(e) => {
                    if !warned {
                        log(&format!("[Ipc] cannot create pipe {name}: {e}"));
                        warned = true;
                    }
                    // Retry only when woken (stop) or after a pause; this path is an error state, not idle.
                    // SAFETY: valid event.
                    unsafe { WaitForSingleObject(out.wake.0, 5000) };
                    continue;
                }
            },
        };
        if !wait_for_client(&pipe, &conn_ev, &out, &stop) {
            continue;
        }
        serve(&pipe, &out, &stop, backend.as_ref());
    }
}

/// Overlapped `ConnectNamedPipe`; false when stopping or on error.
fn wait_for_client(pipe: &Pipe, ev: &Ev, out: &Outbox, stop: &AtomicBool) -> bool {
    let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
    ov.hEvent = ev.0;
    // SAFETY: `ov` outlives the wait.
    unsafe { windows_sys::Win32::System::Threading::ResetEvent(ev.0) };
    // SAFETY: valid pipe and overlapped.
    if unsafe { ConnectNamedPipe(pipe.0, &mut ov) } != 0 {
        return true;
    }
    // SAFETY: trivial.
    match unsafe { GetLastError() } {
        ERROR_PIPE_CONNECTED => true,
        ERROR_IO_PENDING => loop {
            let handles = [ev.0, out.wake.0];
            // SAFETY: valid handles.
            let w = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
            if w == WAIT_OBJECT_0 {
                return true;
            }
            if stop.load(Ordering::SeqCst) {
                let mut n = 0;
                // SAFETY: cancel and drain before `ov` goes away.
                unsafe {
                    CancelIoEx(pipe.0, &ov);
                    GetOverlappedResult(pipe.0, &ov, &mut n, 1);
                }
                return false;
            }
            // wake signalled by a push with nobody connected: ignore
            out.clear();
        },
        ERROR_NO_DATA => false, // client connected and left before we looked: next instance
        e => {
            log(&format!("[Ipc] ConnectNamedPipe failed: {e}"));
            false
        }
    }
}

fn serve(pipe: &Pipe, out: &Outbox, stop: &AtomicBool, backend: &dyn Backend) {
    let (Ok(read_ev), Ok(write_ev)) = (new_event(true), new_event(true)) else { return };
    out.clear();
    out.client.store(true, Ordering::Release);
    log("[Ipc] client connected");
    let mut rd = BufReader::with_capacity(4096, PipeIo { pipe, out, stop, read_ev, write_ev, deadline: Some(Instant::now() + Duration::from_millis(u64::from(HELLO_MS.load(Ordering::Relaxed)))) });
    let mut buf = Vec::with_capacity(512);
    let mut hello = false;
    loop {
        match read_line_bounded(&mut rd, &mut buf) {
            Ok(true) => {}
            Ok(false) => break,
            Err(ProtocolError::Io(e)) if e.kind() == io::ErrorKind::Interrupted => break,
            Err(ProtocolError::Io(e)) if e.kind() == io::ErrorKind::TimedOut => {
                log("[Ipc] dropping client: no valid hello in time");
                break;
            }
            Err(e) => {
                log(&format!("[Ipc] dropping client: {e}"));
                out.send(&AgentMessage::Error { message: e.to_string() });
                let _ = rd.get_ref().flush_outbox();
                break;
            }
        }
        let msg = match std::str::from_utf8(&buf).map_err(|e| e.to_string()).and_then(|s| decode_line::<ClientMessage>(s).map_err(|e| e.to_string())) {
            Ok(m) => m,
            Err(e) => {
                out.send(&AgentMessage::Error { message: e });
                if rd.get_ref().flush_outbox().is_err() {
                    break;
                }
                continue;
            }
        };
        if !hello {
            match msg {
                ClientMessage::Hello { version } if version == PROTOCOL_VERSION => {
                    hello = true;
                    rd.get_mut().deadline = None;
                    out.send(&AgentMessage::Welcome { version: PROTOCOL_VERSION });
                    backend.on_hello(out);
                    if rd.get_ref().flush_outbox().is_err() {
                        break;
                    }
                    continue;
                }
                _ => {
                    out.send(&AgentMessage::Error { message: format!("expected hello with version {PROTOCOL_VERSION}") });
                    let _ = rd.get_ref().flush_outbox();
                    break;
                }
            }
        }
        let keep = backend.handle(msg, out);
        if rd.get_ref().flush_outbox().is_err() || !keep || stop.load(Ordering::SeqCst) {
            break;
        }
    }
    out.client.store(false, Ordering::Release);
    out.clear();
    backend.on_disconnect();
    log("[Ipc] client disconnected");
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouser_win::app_detect::current_user_sid;
    use std::fs::OpenOptions;
    use std::io::{BufRead, Write};

    struct Echo(AtomicBool);
    impl Backend for Echo {
        fn handle(&self, msg: ClientMessage, out: &Outbox) -> bool {
            match msg {
                ClientMessage::GetConfig => out.send(&AgentMessage::Config { config: serde_json::json!({"ok": true}) }),
                ClientMessage::Shutdown => {
                    out.send(&AgentMessage::ShuttingDown);
                    return false;
                }
                _ => out.send(&AgentMessage::Error { message: "unhandled".into() }),
            }
            true
        }
        fn on_disconnect(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn sddl_grants_only_user_and_system() {
        let s = pipe_sddl("S-1-5-21-1-2-3-1001");
        assert_eq!(s, "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)");
        assert!(descriptor(&s).is_ok());
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    // Real pipe, real DACL: Hello/Welcome, a request/reply, push from another thread, Shutdown closes.
    #[test]
    fn pipe_round_trip_with_dacl_and_push() {
        let name = format!(r"\\.\pipe\mouser-rs-agent-test-{}", std::process::id());
        let out = Outbox::new().unwrap();
        let disc = Arc::new(Echo(AtomicBool::new(false)));
        let backend: Arc<dyn Backend> = disc.clone();
        let sddl = pipe_sddl(&current_user_sid().unwrap());
        let mut srv = IpcServer::start(name.clone(), sddl, out.clone(), backend).unwrap();
        let pipe = OpenOptions::new().read(true).write(true).open(&name).unwrap();
        let mut w = pipe.try_clone().unwrap();
        let mut r = BufReader::new(pipe);
        let mut line = String::new();
        let rpc = |w: &mut std::fs::File, r: &mut BufReader<std::fs::File>, m: &ClientMessage| {
            w.write_all(encode_line(m).unwrap().as_bytes()).unwrap();
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            decode_line::<AgentMessage>(&l).unwrap()
        };
        assert_eq!(rpc(&mut w, &mut r, &ClientMessage::Hello { version: PROTOCOL_VERSION }), AgentMessage::Welcome { version: PROTOCOL_VERSION });
        assert!(matches!(rpc(&mut w, &mut r, &ClientMessage::GetConfig), AgentMessage::Config { .. }));
        // push from another thread while the server thread is parked in its read
        let o2 = out.clone();
        std::thread::spawn(move || o2.send(&AgentMessage::DebugLog { line: "pushed".into() })).join().unwrap();
        r.read_line(&mut line).unwrap();
        assert!(matches!(decode_line::<AgentMessage>(&line).unwrap(), AgentMessage::DebugLog { line } if line == "pushed"));
        // garbage gets an Error, connection stays up
        w.write_all(b"not json\n").unwrap();
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(matches!(decode_line::<AgentMessage>(&line).unwrap(), AgentMessage::Error { .. }));
        assert_eq!(rpc(&mut w, &mut r, &ClientMessage::Shutdown), AgentMessage::ShuttingDown);
        line.clear();
        assert!(matches!(r.read_line(&mut line), Ok(0) | Err(_)), "server closed after Shutdown");
        srv.stop();
        assert!(disc.0.load(Ordering::SeqCst), "on_disconnect ran");
    }

    #[test]
    fn silent_client_is_dropped_after_hello_timeout() {
        HELLO_MS.store(300, Ordering::Relaxed);
        let name = format!(r"\\.\pipe\mouser-rs-agent-test4-{}", std::process::id());
        let out = Outbox::new().unwrap();
        let backend: Arc<dyn Backend> = Arc::new(Echo(AtomicBool::new(false)));
        let mut srv = IpcServer::start(name.clone(), pipe_sddl(&current_user_sid().unwrap()), out, backend).unwrap();
        let pipe = OpenOptions::new().read(true).write(true).open(&name).unwrap();
        let mut r = BufReader::new(pipe);
        let t = Instant::now();
        let mut l = String::new();
        assert!(matches!(r.read_line(&mut l), Ok(0) | Err(_)), "server closes the silent client");
        assert!(t.elapsed() < Duration::from_secs(4));
        srv.stop();
        HELLO_MS.store(5000, Ordering::Relaxed);
    }

    #[test]
    fn outbox_is_bounded_in_bytes_and_count() {
        let o = Outbox::new().unwrap();
        o.client.store(true, Ordering::Release);
        let big = AgentMessage::DebugLog { line: "x".repeat(900_000) };
        for _ in 0..20 {
            o.send(&big);
        }
        let (n, bytes) = o.queued();
        assert!(bytes <= OUT_BYTES + 1_000_000 && n <= 5, "{n} lines {bytes} bytes");
        for _ in 0..1000 {
            o.send(&AgentMessage::ShuttingDown);
        }
        assert!(o.queued().0 <= OUT_CAP);
        o.clear();
        assert_eq!(o.queued(), (0, 0));
    }

    #[test]
    fn first_message_must_be_hello() {
        let name = format!(r"\\.\pipe\mouser-rs-agent-test2-{}", std::process::id());
        let out = Outbox::new().unwrap();
        let backend: Arc<dyn Backend> = Arc::new(Echo(AtomicBool::new(false)));
        let mut srv = IpcServer::start(name.clone(), pipe_sddl(&current_user_sid().unwrap()), out, backend).unwrap();
        let pipe = OpenOptions::new().read(true).write(true).open(&name).unwrap();
        let mut w = pipe.try_clone().unwrap();
        let mut r = BufReader::new(pipe);
        w.write_all(encode_line(&ClientMessage::GetConfig).unwrap().as_bytes()).unwrap();
        let mut l = String::new();
        r.read_line(&mut l).unwrap();
        assert!(matches!(decode_line::<AgentMessage>(&l).unwrap(), AgentMessage::Error { .. }));
        l.clear();
        assert!(matches!(r.read_line(&mut l), Ok(0) | Err(_)));
        srv.stop();
    }

    #[test]
    fn second_instance_of_same_pipe_name_is_refused() {
        let name = format!(r"\\.\pipe\mouser-rs-agent-test3-{}", std::process::id());
        let sd = descriptor(&pipe_sddl(&current_user_sid().unwrap())).unwrap();
        let _a = create_pipe(&name, &sd).unwrap();
        assert!(create_pipe(&name, &sd).is_err(), "FIRST_PIPE_INSTANCE blocks squatting");
    }

    // Every pipe/event handle is RAII: connect/Hello/disconnect cycles must not grow the handle count.
    #[test]
    fn client_connect_cycles_do_not_leak_handles() {
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
        let count = || {
            let mut n = 0u32;
            // SAFETY: valid pseudo handle and out pointer.
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut n) };
            n
        };
        let name = format!(r"\\.\pipe\mouser-rs-agent-test5-{}", std::process::id());
        let backend: Arc<dyn Backend> = Arc::new(Echo(AtomicBool::new(false)));
        let mut srv = IpcServer::start(name.clone(), pipe_sddl(&current_user_sid().unwrap()), Outbox::new().unwrap(), backend).unwrap();
        let cycle = || {
            // Between clients the server re-creates its single instance: not found (2) or busy (231) means "not yet".
            let mut p = loop {
                match OpenOptions::new().read(true).write(true).open(&name) {
                    Err(e) if matches!(e.raw_os_error(), Some(2 | 231)) => std::thread::sleep(Duration::from_millis(2)),
                    r => break r.unwrap(),
                }
            };
            p.write_all(encode_line(&ClientMessage::Hello { version: PROTOCOL_VERSION }).unwrap().as_bytes()).unwrap();
            let mut l = String::new();
            BufReader::new(p.try_clone().unwrap()).read_line(&mut l).unwrap();
        };
        for _ in 0..5 {
            cycle();
        }
        let before = count();
        for _ in 0..300 {
            cycle();
        }
        // Slack absorbs handles opened by tests running in parallel threads; a per-cycle leak adds >= 300.
        let after = count();
        srv.stop();
        assert!(after <= before + 64, "handles grew from {before} to {after}");
    }
}
