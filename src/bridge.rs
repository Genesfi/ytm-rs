//! FocusGrow & DynamicNotification / Dynamic Island WebSocket Bridge (`ws://127.0.0.1:8765`).
//!
//! Lightweight RFC 6455 WebSocket server using `std::net`, `sha1`, and `base64`, matching
//! NabyTune's bridge protocol so FocusGrow and DynamicNotification connect to ytm-rs
//! directly — displaying track metadata, controlling playback, and restoring the window
//! straight out of the system tray.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

/// Incoming control commands sent by FocusGrow or DynamicNotification.
#[derive(Clone, Debug, PartialEq)]
pub enum BridgeEvent {
    PlayPause,
    Next,
    Previous,
    ToggleWindow,
    Seek(Duration),
}

/// Main window handle on Windows, used to reliably restore & bring ytm-rs to the
/// foreground from the system tray or minimized state when FocusGrow / Dynamic Island
/// requests `toggleMusicWindow`.
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

#[cfg(windows)]
pub fn set_hwnd(hwnd: *mut std::ffi::c_void) {
    MAIN_HWND.store(hwnd as isize, Ordering::Relaxed);
}

#[cfg(windows)]
pub fn is_foreground_visible() -> bool {
    let raw = MAIN_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return false;
    }
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsIconic, IsWindowVisible};
    let hwnd = HWND(raw as *mut _);
    // SAFETY: plain Win32 queries on the app's own main window handle.
    unsafe { GetForegroundWindow() == hwnd && IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() }
}

#[cfg(not(windows))]
pub fn is_foreground_visible() -> bool {
    false
}

/// Force-restore and bring the main window to the foreground on Windows (bypassing
/// foreground lock when triggered from another process like FocusGrow / Dynamic Island).
#[cfg(windows)]
pub fn force_show_window() {
    let raw = MAIN_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };
    let hwnd = HWND(raw as *mut _);
    // SAFETY: standard Win32 foreground activation on our own main window handle.
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = BringWindowToTop(hwnd);

        let fg = GetForegroundWindow();
        let cur_thread = GetCurrentThreadId();
        let fg_thread = GetWindowThreadProcessId(fg, None);

        if fg_thread != 0 && fg_thread != cur_thread {
            let _ = AttachThreadInput(cur_thread, fg_thread, true);
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(cur_thread, fg_thread, false);
        } else {
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(not(windows))]
pub fn force_show_window() {}

fn exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "ytm-rs.exe".to_owned())
}

pub struct Bridge {
    events: Receiver<BridgeEvent>,
    latest: Arc<Mutex<Option<Value>>>,
    clients: Arc<Mutex<Vec<TcpStream>>>,
    stop: Arc<AtomicBool>,
    exe: String,
}

impl Bridge {
    /// Start listening on `ws://127.0.0.1:8765`. Returns `None` if the port is already
    /// in use (e.g. NabyTune is running); callers can retry later.
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let listener = TcpListener::bind("127.0.0.1:8765").ok()?;
        let _ = listener.set_nonblocking(true);

        let (sender, events) = channel();
        let latest = Arc::new(Mutex::new(None));
        let clients: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let l_clone = Arc::clone(&latest);
        let c_clone = Arc::clone(&clients);
        let s_clone = Arc::clone(&stop);

        thread::Builder::new()
            .name("ytmpx-bridge".into())
            .spawn(move || {
                run_server(listener, sender, l_clone, c_clone, s_clone, wake);
            })
            .ok()?;

        log::info!("bridge: FocusGrow / DynamicNotification server listening on ws://127.0.0.1:8765");
        Some(Self { events, latest, clients, stop, exe: exe_name() })
    }

    pub fn events(&self) -> Vec<BridgeEvent> {
        self.events.try_iter().collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn broadcast_track(
        &self,
        title: &str,
        artist: &str,
        album: Option<&str>,
        cover_url: Option<&str>,
        duration: Duration,
        elapsed: Duration,
        video_id: &str,
        playing: bool,
    ) {
        let val = json!({
            "event": "track",
            "source": "ytm-rs",
            "exeName": self.exe,
            "isPlaying": playing,
            "metadata": {
                "title": title,
                "author": if artist.is_empty() { "YouTube Music" } else { artist },
                "artist": artist,
                "url": format!("https://music.youtube.com/watch?v={video_id}"),
                "image": cover_url.unwrap_or_default(),
                "coverUrl": cover_url.unwrap_or_default(),
                "totalDuration": duration.as_millis() as u64,
                "elapsed": elapsed.as_secs_f64(),
                "playlist": album.unwrap_or_default()
            }
        });
        let payload = val.to_string();
        *self.latest.lock().unwrap() = Some(val);
        broadcast(&self.clients, &payload);
    }

    pub fn broadcast_state(&self, playing: bool, elapsed: Duration) {
        if let Some(latest) = self.latest.lock().unwrap().as_mut() {
            latest["isPlaying"] = Value::Bool(playing);
            latest["metadata"]["elapsed"] = json!(elapsed.as_secs_f64());
        }
        let payload = json!({
            "event": if playing { "resume" } else { "pause" },
            "isPlaying": playing,
            "source": "ytm-rs",
            "exeName": self.exe
        })
        .to_string();
        broadcast(&self.clients, &payload);
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run_server(
    listener: TcpListener,
    event_tx: Sender<BridgeEvent>,
    latest: Arc<Mutex<Option<Value>>>,
    clients: Arc<Mutex<Vec<TcpStream>>>,
    stop: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));

                if do_handshake(&mut stream).is_some() {
                    if let Some(payload) = latest.lock().unwrap().as_ref().map(Value::to_string) {
                        let _ = send_frame(&mut stream, &payload);
                    }
                    let _ = stream.set_nonblocking(true);
                    clients.lock().unwrap().push(stream);
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(40));
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(100));
            }
        }

        poll_clients(&clients, &event_tx, &latest, &wake);
    }
}

fn websocket_accept_key(key: &str) -> String {
    let concat = format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let hash = Sha1::digest(concat.as_bytes());
    STANDARD.encode(hash)
}

fn do_handshake(stream: &mut TcpStream) -> Option<String> {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf).ok()?;
    let req = std::str::from_utf8(&buf[..n]).ok()?;

    let mut key = None;
    for line in req.lines() {
        if let Some((header, val)) = line.split_once(':')
            && header.trim().eq_ignore_ascii_case("Sec-WebSocket-Key")
        {
            key = Some(val.trim().to_string());
            break;
        }
    }
    let key = key?;
    let accept = websocket_accept_key(&key);

    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(resp.as_bytes()).ok()?;
    stream.flush().ok()?;
    Some(key)
}

fn send_frame(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    let payload = text.as_bytes();
    let len = payload.len();
    let mut header = Vec::with_capacity(10);
    header.push(0x81); // FIN + Text opcode

    if len < 126 {
        header.push(len as u8);
    } else if len <= 0xFFFF {
        header.push(126);
        header.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        header.push(127);
        header.extend_from_slice(&(len as u64).to_be_bytes());
    }

    stream.write_all(&header)?;
    stream.write_all(payload)?;
    stream.flush()
}

fn poll_clients(
    clients: &Arc<Mutex<Vec<TcpStream>>>,
    event_tx: &Sender<BridgeEvent>,
    latest: &Arc<Mutex<Option<Value>>>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) {
    let mut guard = clients.lock().unwrap();
    guard.retain_mut(|stream| {
        loop {
            match read_client_frame(stream) {
                Ok(Some(msg)) => {
                    handle_incoming_message(&msg, event_tx, stream, latest, wake);
                }
                Ok(None) => break,
                Err(_) => return false,
            }
        }
        true
    });
}

fn read_client_frame(stream: &mut TcpStream) -> std::io::Result<Option<String>> {
    let mut hdr = [0u8; 2];
    match stream.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(e) => return Err(e),
    }

    let opcode = hdr[0] & 0x0F;
    if opcode == 0x8 {
        return Err(std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "closed"));
    }

    let is_masked = (hdr[1] & 0x80) != 0;
    let mut payload_len = (hdr[1] & 0x7F) as usize;

    if payload_len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext)?;
        payload_len = u16::from_be_bytes(ext) as usize;
    } else if payload_len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext)?;
        payload_len = u64::from_be_bytes(ext) as usize;
    }

    let mask = if is_masked {
        let mut m = [0u8; 4];
        stream.read_exact(&mut m)?;
        Some(m)
    } else {
        None
    };

    const MAX_FRAME_BYTES: usize = 64 * 1024;
    if payload_len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"));
    }

    let mut payload = vec![0u8; payload_len];
    stream.read_exact(&mut payload)?;

    if let Some(m) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= m[i % 4];
        }
    }

    if opcode == 0x9 {
        let pong = [0x8A, 0x00];
        let _ = stream.write_all(&pong);
        let _ = stream.flush();
        return Ok(None);
    }

    String::from_utf8(payload)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn parse_command(msg: &str) -> Option<ParsedCmd> {
    let text = msg.trim();
    if text.is_empty() {
        return None;
    }
    let cmd_str = if let Ok(val) = serde_json::from_str::<Value>(text) {
        let cmd = val["command"].as_str()?;
        if (cmd == "seek" || cmd == "seekTo")
            && let Some(secs) = val["value"].as_f64()
        {
            return Some(ParsedCmd::Event(BridgeEvent::Seek(Duration::from_secs_f64(secs.max(0.0)))));
        }
        cmd.to_string()
    } else {
        text.to_string()
    };

    match cmd_str.as_str() {
        "playPause" | "toggle" => Some(ParsedCmd::Event(BridgeEvent::PlayPause)),
        "next" => Some(ParsedCmd::Event(BridgeEvent::Next)),
        "prev" | "previous" => Some(ParsedCmd::Event(BridgeEvent::Previous)),
        "toggleMusicWindow" | "showWindow" | "openWindow" => Some(ParsedCmd::Event(BridgeEvent::ToggleWindow)),
        "getTrack" | "getMetadata" | "status" => Some(ParsedCmd::RequestLatest),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
enum ParsedCmd {
    Event(BridgeEvent),
    RequestLatest,
}

fn handle_incoming_message(
    msg: &str,
    event_tx: &Sender<BridgeEvent>,
    stream: &mut TcpStream,
    latest: &Arc<Mutex<Option<Value>>>,
    wake: &Arc<dyn Fn() + Send + Sync>,
) {
    match parse_command(msg) {
        Some(ParsedCmd::Event(ev)) => {
            let _ = event_tx.send(ev);
            wake();
        }
        Some(ParsedCmd::RequestLatest) => {
            if let Some(payload) = latest.lock().unwrap().as_ref().map(Value::to_string) {
                let _ = send_frame(stream, &payload);
            }
        }
        None => {}
    }
}

fn broadcast(clients: &Arc<Mutex<Vec<TcpStream>>>, payload: &str) {
    let mut guard = clients.lock().unwrap();
    guard.retain_mut(|stream| send_frame(stream, payload).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6455_accept_key_matches_spec() {
        // Example from RFC 6455 Section 1.3.
        assert_eq!(websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn parses_focusgrow_and_island_commands() {
        assert_eq!(parse_command(r#"{"command":"playPause"}"#), Some(ParsedCmd::Event(BridgeEvent::PlayPause)));
        assert_eq!(parse_command(r#"{"command":"next"}"#), Some(ParsedCmd::Event(BridgeEvent::Next)));
        assert_eq!(parse_command(r#"{"command":"previous"}"#), Some(ParsedCmd::Event(BridgeEvent::Previous)));
        assert_eq!(parse_command(r#"{"command":"toggleMusicWindow"}"#), Some(ParsedCmd::Event(BridgeEvent::ToggleWindow)));
        assert_eq!(parse_command(r#"{"command":"seek","value":42.5}"#), Some(ParsedCmd::Event(BridgeEvent::Seek(Duration::from_secs_f64(42.5)))));
        assert_eq!(parse_command(r#"{"command":"getTrack"}"#), Some(ParsedCmd::RequestLatest));
        assert_eq!(parse_command("toggleMusicWindow"), Some(ParsedCmd::Event(BridgeEvent::ToggleWindow)));
    }
}
