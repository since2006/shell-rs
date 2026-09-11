use std::sync::mpsc;

use anyhow::Result;
use async_channel::Sender;

use crate::session::Session;

use super::{
    ShellEffect, TerminalSize, TerminalTransport, TerminalTransportCommand, TerminalTransportEvent,
    TerminalTransportFactory, banner, prompt, reply, send_event,
};

#[derive(Clone)]
pub struct MockSshTransportFactory {
    session: Session,
}

impl MockSshTransportFactory {
    pub fn new(session: &Session) -> Self {
        Self {
            session: session.clone(),
        }
    }
}

impl TerminalTransportFactory for MockSshTransportFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(MockSshTransport {
            session: self.session.clone(),
        })
    }
}

struct MockSshTransport {
    session: Session,
}

impl TerminalTransport for MockSshTransport {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        send_event(&events, TerminalTransportEvent::Started);

        let mut opening = banner(&self.session)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\r\n");
        opening.push_str("\r\n");
        opening.push_str(&prompt(&self.session));
        send_event(
            &events,
            TerminalTransportEvent::Output(opening.into_bytes()),
        );

        let mut input = Vec::new();
        let mut escape_state = 0_u8;
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::Write(bytes) => {
                    for byte in bytes {
                        if escape_state != 0 {
                            escape_state = match (escape_state, byte) {
                                (1, b'[' | b'O') => 2,
                                (2, 0x40..=0x7e) => 0,
                                (2, _) => 2,
                                _ => 0,
                            };
                            continue;
                        }
                        match byte {
                            b'\r' | b'\n' => {
                                send_event(
                                    &events,
                                    TerminalTransportEvent::Output(b"\r\n".to_vec()),
                                );
                                let command = String::from_utf8_lossy(&input).into_owned();
                                input.clear();
                                let response = reply(&self.session, &command);
                                if matches!(response.effect, Some(ShellEffect::Clear)) {
                                    send_event(
                                        &events,
                                        TerminalTransportEvent::Output(b"\x1b[2J\x1b[H".to_vec()),
                                    );
                                } else {
                                    for line in response.lines {
                                        let mut output = line.to_string();
                                        output.push_str("\r\n");
                                        send_event(
                                            &events,
                                            TerminalTransportEvent::Output(output.into_bytes()),
                                        );
                                    }
                                }
                                if matches!(response.effect, Some(ShellEffect::Exit)) {
                                    send_event(
                                        &events,
                                        TerminalTransportEvent::Exited {
                                            code: 0,
                                            signal: None,
                                        },
                                    );
                                    return Ok(());
                                }
                                send_event(
                                    &events,
                                    TerminalTransportEvent::Output(
                                        prompt(&self.session).into_bytes(),
                                    ),
                                );
                            }
                            0x03 => {
                                input.clear();
                                let output = format!("^C\r\n{}", prompt(&self.session));
                                send_event(
                                    &events,
                                    TerminalTransportEvent::Output(output.into_bytes()),
                                );
                            }
                            0x7f | 0x08 => {
                                if input.pop().is_some() {
                                    while input
                                        .last()
                                        .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
                                    {
                                        input.pop();
                                    }
                                    send_event(
                                        &events,
                                        TerminalTransportEvent::Output(b"\x08 \x08".to_vec()),
                                    );
                                }
                            }
                            // The mock has no command-line navigation; consume
                            // complete CSI/SS3 sequences without echoing them.
                            0x1b => escape_state = 1,
                            byte => {
                                input.push(byte);
                                send_event(&events, TerminalTransportEvent::Output(vec![byte]));
                            }
                        }
                    }
                }
                TerminalTransportCommand::Resize(_) => {}
                TerminalTransportCommand::Shutdown => break,
            }
        }
        Ok(())
    }
}
