//! The external CLI: the `shellrs` command AI agents use to run commands
//! and move files on the user's saved hosts. The command holds no
//! credentials and opens no database: it hands each request to the running
//! app over a local socket, and the app connects with what it has saved.

mod backend;
mod client;
mod install;
#[cfg(any(windows, test))]
mod install_windows;
mod integration;
mod link;
mod manage;
#[cfg(windows)]
mod pipe_windows;
mod protocol;
mod server;
#[cfg(test)]
mod tests;

use std::{
    ffi::OsString,
    io::{self, IsTerminal as _, Read as _, Write},
    path::PathBuf,
};

use clap::{Parser, Subcommand};

use crate::i18n::t;

pub use backend::SshCliBackend;
pub use client::{Console, FAILURE_EXIT, PARTIAL_EXIT, activate_running_app};
pub use install::{
    AgentKind, BinaryStatus, IntegrationPaths, SKILL, SkillStatus, UserPath, binary_status,
    install_binary, install_skill, remove_binary, remove_skill, skill_status,
    update_outdated_binary, update_outdated_skills,
};
pub use integration::{CliIntegration, IntegrationStatus};
pub use link::{OpenLink, link_arguments};
pub use manage::CliChange;
pub(crate) use manage::{
    CredentialPlan, GroupPlan, SecretChange, credential_details, credential_secrets,
    find_credential, find_saved_host, host_details, host_info, host_secrets, plan_credential,
    plan_host, with_saved_credential_secrets, with_saved_passwords,
};
pub use protocol::{
    AuthChoice, CliError, CredentialDeleted, CredentialDetails, CredentialFields,
    CredentialKindChoice, ErrorCode, HostDeleted, HostDetails, HostFields, HostInfo, ProxyChoice,
    Reply, Request, RouteDetails, RouteFields, Secret, TransferCounters, TransferSummary,
};
pub use server::{ChangeReply, CliBackend, CliServer, CliTarget, CliUse};

// `--help` is English whatever the interface language, so it names the
// settings it points to in both.
const AFTER_HELP: &str = concat!(
    "Hosts are named by the 16-character ID that `shellrs hosts list` prints, ",
    "the one ShellRS copies with Copy ID (复制 ID).\n", // i18n: keep
    "ShellRS must be running, with Settings → External CLI → Enable external CLI ",
    "(设置 → 外部 CLI → 启用外部 CLI) turned on.\n", // i18n: keep
    "\n",
    "Exit codes: exec exits with the remote command's code; 1 means a transfer finished with failures;\n",
    "255 means shellrs could not do what was asked (the reason is printed as `shellrs: [code] message`).",
);

const HOST_FIELDS: &str = "\
Reads one JSON object from stdin. Every field may be left out; `update` changes only the fields given.
  name        Required to create.
  host        The address (IP or host name). Required to create. Also read as `address`.
  port        Default 22.
  user        Default root. A host using a credential logs in as the credential's user.
  group       A group path such as \"Production/Databases\", made when it is not there; null for the top level.
  auth        \"password\", \"credential\" or \"no_password\".
  credential  A credential ID from `shellrs credentials list`, for auth \"credential\" (implied).
  password    The host's own password, saved to the keychain and never printed; null deletes it.
              Changing host, port or user without it keeps the saved one.
  route       {\"type\": \"direct\"}
              {\"type\": \"jump\", \"hosts\": [ID, ...]}   (saved hosts, in order)
              {\"type\": \"proxy\", \"kind\": \"http\" | \"socks5\", \"host\": ..., \"port\": ...,
               \"user\": ..., \"password\": ...}
  notes       Free text.
What `shellrs hosts show <ID> --json` prints can be edited and passed back as it is.";

const CREDENTIAL_FIELDS: &str = "\
Reads one JSON object from stdin. Every field may be left out; `update` changes only the fields given.
  name         Required to create.
  kind         \"password\" (the default), \"key\" (implied by a key) or \"agent\".
  user         Default root.
  password     A password credential's, saved to the keychain and never printed; null deletes it.
  key_path     A private key file on this machine (relative to the current directory, or ~/...).
  private_key  The private key's text instead, for ShellRS to keep in a file of its own.
  passphrase   The key's passphrase, saved to the keychain; null deletes it.
What `shellrs credentials show <ID> --json` prints can be edited and passed back as it is.";

/// Run commands, move files and manage the SSH hosts saved in ShellRS.
///
/// ShellRS holds the passwords and keys and makes the connections; this
/// command never reads them. One given to it to save goes to the keychain
/// and is never printed back.
#[derive(Debug, Parser)]
#[command(name = "shellrs", version, after_help = AFTER_HELP)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show, create, change or delete saved hosts.
    Hosts {
        #[command(subcommand)]
        command: HostsCommand,
    },
    /// Show, create, change or delete saved credentials.
    Credentials {
        #[command(subcommand)]
        command: CredentialsCommand,
    },
    /// Run one command on a saved host and print its output.
    ///
    /// Opens a temporary connection, runs the command in the login shell,
    /// and closes it. The command gets no stdin. The exit code is the
    /// remote command's.
    Exec {
        /// Host ID, from `shellrs hosts list`.
        #[arg(required_unless_present = "json")]
        id: Option<String>,
        /// One complete remote shell command, quoted as one argument.
        #[arg(required_unless_present_any = ["stdin", "json"])]
        command: Option<String>,
        /// Read the command from stdin instead, for commands with quotes,
        /// pipes, `$` or several lines.
        #[arg(long, conflicts_with = "command")]
        stdin: bool,
        /// Read {"host": ID, "command": COMMAND} from stdin and print
        /// {"exit_code", "stdout", "stderr"} once the command ends, errors as
        /// {"error": {"code", "message"}}. The output is ASCII, everything
        /// else escaped: no shell quoting, and no console code page can
        /// garble it.
        #[arg(long, conflicts_with_all = ["id", "command", "stdin"])]
        json: bool,
    },
    /// Copy a local file or folder to a saved host.
    ///
    /// Like scp: when the destination is an existing directory the source
    /// goes inside it under its own name; otherwise the destination is the
    /// copy's path. Folders are copied recursively; existing files are
    /// overwritten.
    Upload {
        /// Host ID, from `shellrs hosts list`.
        id: String,
        /// Local file or folder.
        local: PathBuf,
        /// Remote destination. `~` is the login directory.
        remote: String,
        /// Print the summary as JSON. Implied when stdout is not a terminal.
        #[arg(long)]
        json: bool,
    },
    /// Copy a file or folder from a saved host to this machine.
    ///
    /// Destination rules are scp's, as for `upload`.
    Download {
        /// Host ID, from `shellrs hosts list`.
        id: String,
        /// Remote file or folder. `~` is the login directory.
        remote: String,
        /// Local destination.
        local: PathBuf,
        /// Print the summary as JSON. Implied when stdout is not a terminal.
        #[arg(long)]
        json: bool,
    },
    /// Copy what a local folder holds into a folder on a saved host,
    /// skipping files that have not changed.
    ///
    /// The remote folder is made when it is not there (its parent must
    /// be). A file of the same size and modification time is left alone;
    /// any other is copied over. Only this way: nothing comes back.
    Sync {
        /// Host ID, from `shellrs hosts list`.
        id: String,
        /// Local folder, whose contents are copied.
        local: PathBuf,
        /// Remote folder. `~` is the login directory.
        remote: String,
        /// First delete what is in the remote folder and not in the local
        /// one.
        #[arg(long)]
        delete: bool,
        /// Print the summary as JSON. Implied when stdout is not a terminal.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HostsCommand {
    /// List the saved hosts, and the ones connected to without saving
    /// while their tabs are open.
    List {
        /// Only hosts whose name, host, user, group or ID matches.
        #[arg(short, long)]
        query: Option<String>,
        /// Print JSON. Implied when stdout is not a terminal.
        #[arg(long)]
        json: bool,
    },
    /// Show one host in full. Passwords are never shown, only whether one
    /// is saved.
    Show {
        /// Host ID, from `shellrs hosts list`.
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Save a new host, described by JSON on stdin.
    #[command(after_help = HOST_FIELDS)]
    Create {
        #[arg(long)]
        json: bool,
    },
    /// Change a saved host: the fields of the JSON on stdin.
    #[command(after_help = HOST_FIELDS)]
    Update {
        /// Host ID, from `shellrs hosts list`.
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a saved host, with its port forwarding rules.
    Delete {
        /// Host ID, from `shellrs hosts list`.
        id: String,
        /// Delete it even with its tabs open in ShellRS, closing them.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CredentialsCommand {
    /// List the credentials.
    List {
        /// Only credentials whose name, user, kind or ID matches.
        #[arg(short, long)]
        query: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one credential. Passwords and keys are never shown, only
    /// whether they are saved.
    Show {
        /// Credential ID, from `shellrs credentials list`.
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Save a new credential, described by JSON on stdin.
    #[command(after_help = CREDENTIAL_FIELDS)]
    Create {
        #[arg(long)]
        json: bool,
    },
    /// Change a credential: the fields of the JSON on stdin.
    #[command(after_help = CREDENTIAL_FIELDS)]
    Update {
        /// Credential ID, from `shellrs credentials list`.
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a credential. The hosts using it log in on their own from
    /// then on.
    Delete {
        /// Credential ID, from `shellrs credentials list`.
        id: String,
        #[arg(long)]
        json: bool,
    },
}

/// The command line entry: `args` without the program name. Returns the
/// process exit code.
pub fn main(args: Vec<OsString>) -> i32 {
    // In the language the app's window is in, as what the app sends back.
    if let Ok(path) = crate::app::settings_path() {
        let (settings, _) = crate::settings::SettingsStore::load(path);
        crate::i18n::set_locale(settings.settings().language.resolved());
    }
    let cli = match Cli::try_parse_from(std::iter::once(OsString::from("shellrs")).chain(args)) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return error.exit_code();
        }
    };
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let json = !stdout.is_terminal();
    let stderr_is_terminal = stderr.is_terminal();
    let mut stdout = ConsoleText::new(stdout, !json);
    let mut stderr = ConsoleText::new(stderr, stderr_is_terminal);
    let mut console = Console {
        stdout: &mut stdout,
        stderr: &mut stderr,
        json,
        exec_json: false,
        stderr_is_terminal,
    };
    let code = match request(cli.command, &mut console) {
        Ok(request) => client::run(&crate::app::cli_socket_path(), request, &mut console),
        Err(code) => code,
    };
    let _ = stdout.finish();
    let _ = stderr.finish();
    code
}

/// The request a command line asks for, or the exit code of why it
/// cannot be sent.
fn request(command: Command, console: &mut Console) -> Result<Request, i32> {
    Ok(match command {
        Command::Exec {
            id,
            command,
            stdin,
            json,
        } => {
            // From here on, errors are JSON too.
            console.exec_json = json;
            let (host, command) = if json {
                exec_request(&read_stdin(console)?)
                    .map_err(|message| console.error(ErrorCode::BadRequest, &message))?
            } else if stdin {
                (id.unwrap_or_default(), read_stdin(console)?)
            } else {
                (id.unwrap_or_default(), command.unwrap_or_default())
            };
            let command = normalize_command(&command);
            if command.trim().is_empty() {
                return Err(console.error(ErrorCode::BadRequest, &t!("cli.exec.empty")));
            }
            Request::Exec { host, command }
        }
        Command::Upload {
            id,
            local,
            remote,
            json,
        } => {
            console.json |= json;
            Request::Upload {
                host: id,
                source: absolute(local, console)?,
                destination: remote,
            }
        }
        Command::Download {
            id,
            remote,
            local,
            json,
        } => {
            console.json |= json;
            Request::Download {
                host: id,
                source: remote,
                destination: absolute(local, console)?,
            }
        }
        Command::Sync {
            id,
            local,
            remote,
            delete,
            json,
        } => {
            console.json |= json;
            Request::Sync {
                host: id,
                source: absolute(local, console)?,
                destination: remote,
                delete,
            }
        }
        Command::Hosts { command } => hosts_request(command, console)?,
        Command::Credentials { command } => credentials_request(command, console)?,
    })
}

fn hosts_request(command: HostsCommand, console: &mut Console) -> Result<Request, i32> {
    Ok(match command {
        HostsCommand::List { query, json } => {
            console.json |= json;
            Request::List { query }
        }
        HostsCommand::Show { id, json } => {
            console.json |= json;
            Request::ShowHost { host: id }
        }
        HostsCommand::Create { json } => {
            console.json |= json;
            Request::CreateHost {
                fields: host_fields(console, "shellrs hosts create < host.json")?,
            }
        }
        HostsCommand::Update { id, json } => {
            console.json |= json;
            let example = format!("shellrs hosts update {id} < changes.json");
            Request::UpdateHost {
                fields: host_fields(console, &example)?,
                host: id,
            }
        }
        HostsCommand::Delete { id, force, json } => {
            console.json |= json;
            Request::DeleteHost { host: id, force }
        }
    })
}

fn credentials_request(command: CredentialsCommand, console: &mut Console) -> Result<Request, i32> {
    Ok(match command {
        CredentialsCommand::List { query, json } => {
            console.json |= json;
            Request::ListCredentials { query }
        }
        CredentialsCommand::Show { id, json } => {
            console.json |= json;
            Request::ShowCredential { credential: id }
        }
        CredentialsCommand::Create { json } => {
            console.json |= json;
            Request::CreateCredential {
                fields: credential_fields(console, "shellrs credentials create < credential.json")?,
            }
        }
        CredentialsCommand::Update { id, json } => {
            console.json |= json;
            let example = format!("shellrs credentials update {id} < changes.json");
            Request::UpdateCredential {
                fields: credential_fields(console, &example)?,
                credential: id,
            }
        }
        CredentialsCommand::Delete { id, json } => {
            console.json |= json;
            Request::DeleteCredential { credential: id }
        }
    })
}

/// The JSON object on stdin. Not from a terminal: nothing would say what
/// to type, or that it waits for it.
fn read_json(console: &mut Console, example: &str) -> Result<serde_json::Value, i32> {
    if io::stdin().is_terminal() {
        return Err(console.error(
            ErrorCode::BadRequest,
            &t!("cli.json.from_terminal", example = example),
        ));
    }
    let text = read_stdin(console)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    serde_json::from_str(text).map_err(|error| {
        console.error(
            ErrorCode::BadRequest,
            &t!("cli.json.not_json", error = error),
        )
    })
}

fn host_fields(console: &mut Console, example: &str) -> Result<HostFields, i32> {
    let value = read_json(console, example)?;
    manage::host_fields(value).map_err(|error| {
        console.error(
            ErrorCode::BadRequest,
            &t!("cli.json.invalid", error = error),
        )
    })
}

/// A key file named relative to here, or to the home directory, is sent
/// as the absolute path the app can find.
fn credential_fields(console: &mut Console, example: &str) -> Result<CredentialFields, i32> {
    let value = read_json(console, example)?;
    let mut fields = manage::credential_fields(value).map_err(|error| {
        console.error(
            ErrorCode::BadRequest,
            &t!("cli.json.invalid", error = error),
        )
    })?;
    if let Some(path) = fields.key_path.take() {
        let path = match path.strip_prefix("~") {
            Ok(rest) => dirs::home_dir().map(|home| home.join(rest)).unwrap_or(path),
            Err(_) => path,
        };
        fields.key_path = Some(absolute(path, console)?);
    }
    Ok(fields)
}

fn read_stdin(console: &mut Console) -> Result<String, i32> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|error| {
            console.error(
                ErrorCode::BadRequest,
                &t!("cli.stdin.unreadable", error = error),
            )
        })?;
    Ok(text)
}

/// What `exec --json` reads.
#[derive(serde::Deserialize)]
struct ExecJson {
    host: String,
    command: String,
}

/// The host and command of an `exec --json` request.
fn exec_request(text: &str) -> Result<(String, String), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let request: ExecJson = serde_json::from_str(text).map_err(|error| {
        t!(
            "cli.exec.bad_json",
            form = r#"{"host": ..., "command": ...}"#,
            error = error
        )
        .to_string()
    })?;
    Ok((request.host, request.command))
}

/// A command as the remote shell should see it. PowerShell ends every line
/// it pipes to a program with CRLF, and a POSIX shell would take the CR as
/// part of the command; a BOM would be the start of the first word.
fn normalize_command(command: &str) -> String {
    command
        .strip_prefix('\u{feff}')
        .unwrap_or(command)
        .replace("\r\n", "\n")
}

/// The app runs in another directory, so local paths travel absolute.
fn absolute(path: PathBuf, console: &mut Console) -> Result<PathBuf, i32> {
    std::path::absolute(&path).map_err(|error| {
        console.error(
            ErrorCode::BadRequest,
            &t!(
                "cli.path.unresolvable",
                path = path.display(),
                error = error
            ),
        )
    })
}

/// Standard output or error, made safe for a Windows console: remote output
/// may not be UTF-8 (a GBK server, a binary file), and the Windows console
/// refuses bytes that are not. Invalid bytes become U+FFFD there; a
/// character split across two writes is held until its second half comes.
/// Anywhere else, bytes pass through untouched.
struct ConsoleText<W: Write> {
    inner: W,
    lossy: bool,
    /// The start of a character whose rest has not been written yet.
    pending: Vec<u8>,
}

impl<W: Write> ConsoleText<W> {
    fn new(inner: W, is_terminal: bool) -> Self {
        Self {
            inner,
            lossy: cfg!(windows) && is_terminal,
            pending: Vec::new(),
        }
    }

    /// Write out a character cut off at the very end, and flush.
    fn finish(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            self.pending.clear();
            self.inner.write_all("\u{fffd}".as_bytes())?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Write for ConsoleText<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.lossy {
            return self.inner.write(bytes);
        }
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        let mut rest: &[u8] = &self.pending;
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    rest = &[];
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    text.push_str(&String::from_utf8_lossy(valid));
                    match error.error_len() {
                        Some(invalid) => {
                            text.push('\u{fffd}');
                            rest = &after[invalid..];
                        }
                        // The rest of the character is still to come.
                        None => break,
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        self.inner.write_all(text.as_bytes())?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The arguments the process was started with, apart from the one macOS
/// adds when launching an app; `None` without any. The command's, unless
/// [`link_arguments`] finds a link to open in them.
pub fn command_line_arguments() -> Option<Vec<OsString>> {
    let args: Vec<OsString> = std::env::args_os()
        .skip(1)
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn_"))
        .collect();
    (!args.is_empty()).then_some(args)
}
