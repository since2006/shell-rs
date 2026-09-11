use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result};
use async_channel::Sender;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use super::{
    TerminalSize, TerminalTransport, TerminalTransportCommand, TerminalTransportEvent,
    TerminalTransportFactory, send_event,
};

#[derive(Default)]
pub struct LocalPtyTransportFactory;

impl TerminalTransportFactory for LocalPtyTransportFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(LocalPtyTransport { command: None })
    }
}

struct LocalPtyTransport {
    command: Option<Vec<OsString>>,
}

impl TerminalTransport for LocalPtyTransport {
    fn run(
        self: Box<Self>,
        initial_size: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(to_pty_size(initial_size))
            .context("无法创建本地 PTY")?;

        let mut command = self
            .command
            .map(CommandBuilder::from_argv)
            .unwrap_or_else(CommandBuilder::new_default_prog);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "shellr");

        let mut child = pair
            .slave
            .spawn_command(command)
            .context("无法启动系统默认 shell")?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("无法读取本地 PTY")?;
        let mut writer = pair.master.take_writer().context("无法写入本地 PTY")?;

        let output_events = events.clone();
        let reader_thread = thread::Builder::new()
            .name("shellr-pty-reader".into())
            .spawn(move || {
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            if !send_event(
                                &output_events,
                                TerminalTransportEvent::Output(buffer[..count].to_vec()),
                            ) {
                                break;
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        // Unix PTYs commonly report EIO when the slave closes.
                        Err(error) if error.raw_os_error() == Some(5) => break,
                        Err(error) => {
                            send_event(
                                &output_events,
                                TerminalTransportEvent::Failed(format!("读取 PTY 失败：{error}")),
                            );
                            break;
                        }
                    }
                }
            })
            .context("无法启动 PTY 读取线程")?;

        send_event(&events, TerminalTransportEvent::Started);
        let mut shutting_down = false;
        let exit = loop {
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(TerminalTransportCommand::Write(bytes)) if !shutting_down => {
                    if let Err(error) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                        send_event(
                            &events,
                            TerminalTransportEvent::Failed(format!("写入 PTY 失败：{error}")),
                        );
                        let _ = child.kill();
                        shutting_down = true;
                    }
                }
                Ok(TerminalTransportCommand::Resize(size)) if !shutting_down => {
                    if let Err(error) = pair.master.resize(to_pty_size(size)) {
                        send_event(
                            &events,
                            TerminalTransportEvent::Failed(format!("调整 PTY 尺寸失败：{error}")),
                        );
                    }
                }
                Ok(TerminalTransportCommand::Shutdown)
                | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if !shutting_down {
                        shutting_down = true;
                        let _ = child.kill();
                    }
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            if let Some(status) = child.try_wait().context("无法读取 shell 退出状态")? {
                break status;
            }
        };

        drop(writer);
        drop(pair.master);
        let _ = reader_thread.join();
        if !shutting_down {
            send_event(
                &events,
                TerminalTransportEvent::Exited {
                    code: exit.exit_code(),
                    signal: exit.signal().map(str::to_owned),
                },
            );
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Instant;

    use super::*;

    fn shell_transport(script: &str) -> Box<dyn TerminalTransport> {
        Box::new(LocalPtyTransport {
            command: Some(vec!["/bin/sh".into(), "-c".into(), script.into()]),
        })
    }

    fn drain_until_exit(
        events: &async_channel::Receiver<TerminalTransportEvent>,
        commands: &mpsc::Sender<TerminalTransportCommand>,
    ) -> (String, Option<u32>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut code = None;
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(TerminalTransportEvent::Output(bytes)) => output.extend(bytes),
                Ok(TerminalTransportEvent::Exited {
                    code: exit_code, ..
                }) => {
                    code = Some(exit_code);
                    break;
                }
                Ok(TerminalTransportEvent::Started | TerminalTransportEvent::Failed(_)) => {}
                Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
                Err(async_channel::TryRecvError::Closed) => break,
            }
        }
        if code.is_none() {
            let _ = commands.send(TerminalTransportCommand::Shutdown);
        }
        (String::from_utf8_lossy(&output).into_owned(), code)
    }

    #[test]
    fn runs_resizes_writes_and_reaps_a_deterministic_shell() {
        let (commands, command_receiver) = mpsc::channel();
        let (events, event_receiver) = async_channel::unbounded();
        let transport = shell_transport(
            "printf 'ready\\n'; read value; printf 'size:'; stty size; printf 'got:%s\\n' \"$value\"; exit 7",
        );
        let worker = thread::spawn(move || {
            transport.run(TerminalSize::new(80, 24, 8, 16), command_receiver, events)
        });

        commands
            .send(TerminalTransportCommand::Resize(TerminalSize::new(
                100, 31, 9, 18,
            )))
            .unwrap();
        commands
            .send(TerminalTransportCommand::Write(b"hello\n".to_vec()))
            .unwrap();

        let (output, code) = drain_until_exit(&event_receiver, &commands);
        assert_eq!(code, Some(7));
        assert!(output.contains("ready"));
        assert!(output.contains("size:31 100"));
        assert!(output.contains("got:hello"));
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn shutdown_terminates_and_reaps_the_child() {
        let (commands, command_receiver) = mpsc::channel();
        let (events, _event_receiver) = async_channel::unbounded();
        let transport = shell_transport("printf 'ready\\n'; sleep 30");
        let worker =
            thread::spawn(move || transport.run(TerminalSize::DEFAULT, command_receiver, events));

        commands.send(TerminalTransportCommand::Shutdown).unwrap();
        worker.join().unwrap().unwrap();
    }
}

fn to_pty_size(size: TerminalSize) -> PtySize {
    let rows = size.rows().min(u16::MAX as usize) as u16;
    let cols = size.columns().min(u16::MAX as usize) as u16;
    PtySize {
        rows,
        cols,
        pixel_width: size.cell_width().saturating_mul(cols),
        pixel_height: size.cell_height().saturating_mul(rows),
    }
}
