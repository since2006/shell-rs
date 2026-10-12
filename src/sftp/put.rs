//! Write one file on a host over SFTP, for a screenshot pasted into a
//! remote terminal. A connection of its own: the terminal's channel stays
//! the shell. Nobody is there to answer a question, so a host not trusted
//! yet or a password not saved fails at once.

use crate::connection::{ConnectionPromptKind, ConnectionPromptReply};
use crate::host::HostLogin;
use crate::i18n::t;
use gpui_kit::SharedString;

use super::{RemotePath, SftpCommand, SftpEvent, SharedSftpTransportProvider};

/// Put `bytes` at `path` on `login`'s host. Blocks, so it belongs on a
/// background thread.
pub fn put_bytes(
    provider: &SharedSftpTransportProvider,
    login: &HostLogin,
    path: &RemotePath,
    bytes: Vec<u8>,
) -> Result<(), SharedString> {
    let transport = provider.create(login);
    let (commands, command_receiver) = async_channel::unbounded();
    let (event_sender, events) = async_channel::unbounded();
    let worker = std::thread::Builder::new()
        .name("shellrs-paste-sftp".into())
        .spawn(move || transport.run(command_receiver, event_sender))
        .map_err(|error| {
            t!(
                "terminal.paste.image_upload_failed",
                error = error.to_string()
            )
        })?;

    let result = write_once(&commands, &events, path, bytes);
    let _ = commands.send_blocking(SftpCommand::Shutdown);
    let _ = worker.join();
    result
}

fn write_once(
    commands: &async_channel::Sender<SftpCommand>,
    events: &async_channel::Receiver<SftpEvent>,
    path: &RemotePath,
    bytes: Vec<u8>,
) -> Result<(), SharedString> {
    loop {
        match events.recv_blocking() {
            Ok(SftpEvent::Connected { .. }) => break,
            Ok(SftpEvent::Prompt(prompt)) => {
                refuse(commands, &prompt);
                return Err(prompt_error(&prompt));
            }
            Ok(SftpEvent::Disconnected(reason)) => {
                return Err(t!("terminal.paste.image_upload_failed", error = reason));
            }
            Ok(_) => {}
            Err(_) => {
                return Err(t!(
                    "terminal.paste.image_upload_failed",
                    error = t!("cli.transfer.sftp_ended")
                ));
            }
        }
    }
    let _ = commands.send_blocking(SftpCommand::WriteFile {
        request_id: 1,
        path: path.clone(),
        bytes,
        expected: None,
    });
    loop {
        match events.recv_blocking() {
            Ok(SftpEvent::FileWritten { result, .. }) => {
                return result.map(|_| ()).map_err(|error| {
                    t!(
                        "terminal.paste.image_upload_failed",
                        error = error.to_string()
                    )
                });
            }
            Ok(SftpEvent::Prompt(prompt)) => {
                refuse(commands, &prompt);
                return Err(prompt_error(&prompt));
            }
            Ok(SftpEvent::Disconnected(reason)) => {
                return Err(t!("terminal.paste.image_upload_failed", error = reason));
            }
            Ok(_) => {}
            Err(_) => {
                return Err(t!(
                    "terminal.paste.image_upload_failed",
                    error = t!("cli.transfer.sftp_ended")
                ));
            }
        }
    }
}

fn refuse(
    commands: &async_channel::Sender<SftpCommand>,
    prompt: &crate::connection::ConnectionPrompt,
) {
    let _ = commands.send_blocking(SftpCommand::PromptReply {
        request_id: prompt.request_id(),
        reply: ConnectionPromptReply::Cancel,
    });
}

fn prompt_error(prompt: &crate::connection::ConnectionPrompt) -> SharedString {
    match prompt.kind() {
        ConnectionPromptKind::UnknownHost(_) => t!("cli.transfer.host_key_unknown"),
        ConnectionPromptKind::HostKeyChanged(_) => t!("cli.transfer.host_key_changed"),
        ConnectionPromptKind::Authentication(_) => t!("cli.transfer.missing_credential"),
    }
}
