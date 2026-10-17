use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

pub struct Shell {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    screen: Arc<Mutex<String>>,
    io_error: Arc<Mutex<Option<String>>>,
    // ConPTY 的所有权在 master 手里：master 一旦被释放就会 ClosePseudoConsole，
    // 子进程随之结束、读取线程立刻读到 EOF，界面就会一直停在“正在打开命令行…”。
    _master: Box<dyn MasterPty + Send>,
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
        let master: Box<dyn MasterPty + Send> = pair.master;
        let mut reader = master.try_clone_reader().map_err(|err| err.to_string())?;
        let writer = master.take_writer().map_err(|err| err.to_string())?;
        let screen = Arc::new(Mutex::new(String::new()));
        let screen_thread = screen.clone();
        let io_error = Arc::new(Mutex::new(None));
        let reader_error = io_error.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        crate::terminal_debug("pty_read EOF");
                        *reader_error.lock().unwrap() =
                            Some("终端输出流已关闭，shell 可能已经退出。".into());
                        break;
                    }
                    Err(err) => {
                        if err.kind() == std::io::ErrorKind::Interrupted {
                            continue;
                        }
                        crate::terminal_debug(format!("pty_read error={err}"));
                        *reader_error.lock().unwrap() = Some(format!("读取终端失败：{err}"));
                        break;
                    }
                    Ok(n) => {
                        crate::terminal_debug(format!(
                            "pty_read bytes={n} raw={:?}",
                            String::from_utf8_lossy(&buf[..n])
                        ));
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
            io_error,
            _master: master,
            _child: child,
        })
    }

    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        crate::terminal_debug(format!("shell_write {:?}", String::from_utf8_lossy(bytes)));
        let result: Result<(), String> = (|| {
            let mut writer = self
                .writer
                .lock()
                .map_err(|_| "终端写入锁不可用".to_string())?;
            writer
                .write_all(bytes)
                .and_then(|_| writer.flush())
                .map_err(|err| format!("写入终端失败：{err}"))
        })();
        if let Err(err) = &result {
            crate::terminal_debug(format!("shell_write error={err}"));
            *self.io_error.lock().unwrap() = Some(err.clone());
        }
        result
    }

    pub fn io_error(&self) -> Option<String> {
        self.io_error.lock().unwrap().clone()
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

#[cfg(all(test, windows))]
mod tests {
    use super::Shell;

    #[test]
    fn spawned_shell_keeps_receiving_output() {
        let shell = Shell::spawn(&std::env::temp_dir()).expect("spawn shell");
        let mut text = String::new();
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            text = shell.screen();
            if !text.is_empty() {
                break;
            }
        }
        assert!(
            !text.is_empty(),
            "pty master 必须随 Shell 一起存活，否则收不到任何输出"
        );
    }

    #[test]
    fn typing_reaches_the_shell() {
        let shell = Shell::spawn(&std::env::temp_dir()).expect("spawn shell");
        std::thread::sleep(std::time::Duration::from_millis(600));
        shell.write(b"echo hikari-probe\r").expect("write probe command");
        let mut text = String::new();
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            text = shell.screen();
            if text.matches("hikari-probe").count() >= 2 {
                break;
            }
        }
        println!("SCREEN={text:?}");
        assert!(
            text.matches("hikari-probe").count() >= 2,
            "写入 pty 的内容必须回显并被执行"
        );
        // 第二轮：模拟用户在终端里继续输入。
        std::thread::sleep(std::time::Duration::from_millis(2000));
        shell.write(b"abc\r").expect("write second command");
        let mut second = String::new();
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            second = shell.screen();
            if second.len() > text.len() + 4 {
                break;
            }
        }
        println!("SECOND={second:?}");
        assert!(second.len() > text.len(), "后续输入同样需要有回显");
        assert!(second.contains("abc"), "回显里应当能看到输入的字符");
    }
}
