//! A `ProxyCommand` transport: an external command whose stdin and stdout
//! carry the SSH bytes, the way OpenSSH runs it. The process is spawned
//! without a window on Windows and lives exactly as long as the connection
//! that runs over it: dropping the stream kills it.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    process::{Child, ChildStdin, ChildStdout, Command},
};

/// Expand OpenSSH's tokens in a `ProxyCommand`: `%h` the host, `%p` the
/// port, `%r` the remote user, `%n` the host as written (the same as `%h`
/// here), `%d` the local home, and `%%` a literal percent. Any other `%x`
/// is passed through untouched.
pub(super) fn expand(command: &str, host: &str, port: u16, user: &str) -> String {
    let home = dirs::home_dir()
        .map(|home| home.display().to_string())
        .unwrap_or_else(|| "~".to_owned());
    let mut out = String::with_capacity(command.len());
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('h') => {
                chars.next();
                out.push_str(host);
            }
            Some('p') => {
                chars.next();
                out.push_str(&port.to_string());
            }
            Some('r') => {
                chars.next();
                out.push_str(user);
            }
            Some('n') => {
                chars.next();
                out.push_str(host);
            }
            Some('d') => {
                chars.next();
                out.push_str(&home);
            }
            Some('%') => {
                chars.next();
                out.push('%');
            }
            _ => out.push('%'),
        }
    }
    out
}

/// A stream to the host carried by a `ProxyCommand` process: what is
/// written goes to the process's stdin, what the process writes comes out
/// of its stdout. Dropping it ends the process (its whole group on Unix,
/// so a `sh -c` wrapper's children cannot outlive the connection).
pub(super) struct ProxyCommandStream {
    stdin: ChildStdin,
    stdout: ChildStdout,
    child: Child,
}

/// Spawn `command` with its stdio wired as the stream. On Unix the command
/// runs through `sh -c`, as OpenSSH runs it, so shell redirections and
/// variables work; on Windows it runs through `cmd /C` without a console.
pub(super) fn spawn(command: &str) -> io::Result<ProxyCommandStream> {
    let mut command_builder = Command::new(shell());
    #[cfg(unix)]
    {
        command_builder.arg("-c").arg(command);
        // The command becomes its own process group, so the group can be
        // signalled as one when the connection ends.
        command_builder.process_group(0);
    }
    #[cfg(windows)]
    {
        command_builder.arg("/C").arg(command);
        // CREATE_NO_WINDOW: a ProxyCommand has no console of its own.
        command_builder.creation_flags(0x0800_0000);
    }
    let mut child = command_builder
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("proxy command has no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("proxy command has no stdout"))?;
    Ok(ProxyCommandStream {
        stdin,
        stdout,
        child,
    })
}

#[cfg(unix)]
fn shell() -> &'static str {
    "sh"
}

#[cfg(windows)]
fn shell() -> &'static str {
    "cmd"
}

impl AsyncRead for ProxyCommandStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ProxyCommandStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stdin).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_shutdown(cx)
    }
}

impl Drop for ProxyCommandStream {
    fn drop(&mut self) {
        // `kill_on_drop` only signals the direct child (`sh`); on Unix the
        // actual relay is its grandchild, so the whole group is signalled
        // and the direct child reaped so it does not linger as a zombie.
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // The group's PGID is the direct child's PID (`process_group(0)`).
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = self.child.try_wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_expand_as_openssh_does() {
        assert_eq!(
            expand("ncat %h %p", "db.internal", 2222, "root"),
            "ncat db.internal 2222"
        );
        assert_eq!(
            expand("%r@%h:%p", "10.0.0.5", 22, "deploy"),
            "deploy@10.0.0.5:22"
        );
        assert_eq!(expand("%%h", "h", 22, "u"), "%h");
        assert_eq!(expand("x %q y", "h", 22, "u"), "x %q y");
    }

    #[tokio::test]
    async fn a_proxy_command_streams_both_ways() {
        // `cat` echoes whatever it reads: an end-to-end relay through a real
        // child process.
        let mut stream = spawn("cat").unwrap();
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        stream.write_all(b"ping").await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}
