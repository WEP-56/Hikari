use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

pub struct Shell {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    screen: Arc<Mutex<String>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Shell {
    pub fn spawn(cwd: &Path) -> Result<Self, String> {
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows: 24,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| err.to_string())?;
        let mut command = CommandBuilder::new("cmd.exe");
        command.arg("/k");
        command.arg("chcp 65001>nul");
        command.cwd(cwd);
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| err.to_string())?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| err.to_string())?;
        let writer = pair.master.take_writer().map_err(|err| err.to_string())?;
        let screen = Arc::new(Mutex::new(String::new()));
        let screen_thread = screen.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]);
                        let mut guard = screen_thread.lock().unwrap();
                        guard.push_str(&strip_ansi(&text));
                        if guard.len() > 24_000 {
                            let drop_to = guard.len() - 16_000;
                            guard.drain(..drop_to);
                        }
                    }
                }
            }
        });
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            screen,
            _child: child,
        })
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    pub fn screen(&self) -> String {
        self.screen.lock().unwrap().clone()
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self._child.kill();
    }
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\u{7}' {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if ch == '\r' {
            continue;
        }
        out.push(ch);
    }
    out
}
