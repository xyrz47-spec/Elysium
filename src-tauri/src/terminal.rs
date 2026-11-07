//! In-app PowerShell for the tools panel, on a Windows pseudo console (ConPTY via portable-pty).
//! Output goes to this PC's window only (plain Tauri events, not orchestrator::emit, which also feeds the phone relay).

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, State};

struct Term {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Default)]
pub struct Terminals {
    map: Mutex<HashMap<String, Term>>,
    /// Bumped on every open so events from a replaced shell can be ignored by the window.
    gen: AtomicU64,
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.clamp(2, 500), cols: cols.clamp(10, 1000), pixel_width: 0, pixel_height: 0 }
}

/// Bytes that end in the middle of a UTF-8 character stay in `carry` until the rest arrives.
fn take_text(carry: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(carry) {
        Ok(_) => carry.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => carry.len(),
    };
    let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
    carry.drain(..valid);
    text
}

/// Starts (or restarts) PowerShell for `id` in `cwd` and returns its generation number.
#[tauri::command]
pub fn term_open(app: AppHandle, st: State<'_, Terminals>, id: String, cols: u16, rows: u16, cwd: Option<String>) -> Result<u64, String> {
    if let Some(mut old) = st.map.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) {
        let _ = old.killer.kill();
    }
    let gen = st.gen.fetch_add(1, Ordering::Relaxed) + 1;
    let pair = native_pty_system().openpty(size(cols, rows)).map_err(|e| e.to_string())?;
    let mut cmd = CommandBuilder::new("powershell.exe");
    cmd.arg("-NoLogo");
    let dir = cwd.filter(|d| Path::new(d).is_dir()).or_else(|| std::env::var("USERPROFILE").ok());
    if let Some(d) = dir {
        cmd.cwd(d);
    }
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let killer = child.clone_killer();
    st.map.lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), Term { master: pair.master, writer, killer });

    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut carry = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    carry.extend_from_slice(&buf[..n]);
                    let data = take_text(&mut carry);
                    if !data.is_empty() {
                        let _ = app.emit("term:data", json!({ "id": id, "gen": gen, "data": data }));
                    }
                }
            }
        }
        let code = child.wait().ok().map(|s| s.exit_code());
        let _ = app.emit("term:exit", json!({ "id": id, "gen": gen, "code": code }));
    });
    Ok(gen)
}

#[tauri::command]
pub fn term_write(st: State<'_, Terminals>, id: String, data: String) -> Result<(), String> {
    let mut map = st.map.lock().unwrap_or_else(|p| p.into_inner());
    let t = map.get_mut(&id).ok_or("terminal not running")?;
    t.writer.write_all(data.as_bytes()).and_then(|_| t.writer.flush()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn term_resize(st: State<'_, Terminals>, id: String, cols: u16, rows: u16) -> Result<(), String> {
    let map = st.map.lock().unwrap_or_else(|p| p.into_inner());
    match map.get(&id) {
        Some(t) => t.master.resize(size(cols, rows)).map_err(|e| e.to_string()),
        None => Ok(()),
    }
}

#[tauri::command]
pub fn term_close(st: State<'_, Terminals>, id: String) {
    if let Some(mut t) = st.map.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) {
        let _ = t.killer.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_split_across_reads_is_kept() {
        let s = "가나".as_bytes();
        let mut carry = s[..4].to_vec(); // "가" + first byte of "나"
        assert_eq!(take_text(&mut carry), "가");
        carry.extend_from_slice(&s[4..]);
        assert_eq!(take_text(&mut carry), "나");
        assert!(carry.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn powershell_runs_on_a_pseudo_console() {
        let pair = native_pty_system().openpty(size(80, 24)).unwrap();
        let mut cmd = CommandBuilder::new("powershell.exe");
        cmd.args(["-NoLogo", "-NoProfile", "-Command", "Write-Output elysium-pty-ok"]);
        let mut child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                // ConPTY asks for the cursor position (DSR) and waits for the reply; xterm.js answers this in the app.
                if buf[..n].windows(4).any(|w| w == b"[6n") {
                    let _ = writer.write_all(b"[1;1R");
                    let _ = writer.flush();
                }
                out.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&out).contains("elysium-pty-ok") {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&out).into_owned());
        });
        let out = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("no output from powershell");
        let _ = child.kill();
        assert!(out.contains("elysium-pty-ok"), "unexpected output: {out:?}");
    }
}
