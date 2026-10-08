---
name: shellrs
description: This skill should be used when the user asks to "connect to a server", "SSH into a machine", "run a command on a remote host", "check a server", "list servers" or "list SSH sessions", "upload/download or sync files to a server", "add, change or remove a saved host or credential", or mentions ShellRS or the `shellrs` CLI. Use `shellrs` instead of running `ssh`, `scp`, or `sftp` directly.
---

# ShellRS CLI

Use the `shellrs` CLI to manage the user's saved SSH hosts and credentials, run one-off commands, and transfer files through the user's running ShellRS desktop app. ShellRS holds the credentials and connects with the saved configuration.

Prefer `shellrs` over running `ssh`, `scp`, or `sftp` directly.
Run `shellrs --help` or `shellrs <command> --help` for details. Every command prints JSON when piped or given `--json`.

## Common Workflow

```bash
shellrs hosts list -q <keyword> --json
shellrs hosts show <host-id> --json
shellrs exec <host-id> "<command>"
shellrs upload <host-id> <local-path> <remote-path>
shellrs download <host-id> <remote-path> <local-path>
shellrs sync <host-id> <local-dir> <remote-dir> [--delete]
```

`<host-id>` is the 16-character `id` from `shellrs hosts list`, the same ID that ShellRS copies with Copy ID (复制 ID).

For a simple command, pass one complete remote shell command string as `<command>`.
Pass a command through stdin when it contains quotes, pipes, redirects, JSON, `$`, backticks, or nested shell code, and quote the heredoc delimiter so the local shell leaves it alone:

```bash
shellrs exec <host-id> --stdin <<'EOF'
<command>
EOF
```

## Hosts Opened From a Bastion Host

A host listed with `"temporary": true` may have been opened by a bastion host's link that allows one login. Then `exec`, `upload` and `download`, which log in again, fail with `missing_credential` or `connect_failed`, and the message says so. Run the command in the host's terminal open in ShellRS instead:

```bash
shellrs exec <host-id> --terminal "<command>"
shellrs exec <host-id> --terminal --stdin <<'EOF'
<command>
EOF
```

- It types into the user's own terminal tab, in sight of the user. It sends Ctrl-C first, which interrupts whatever runs there: use it only on a tab the user handed to you, not on one they are working in.
- The command runs where that terminal is now: its user, folder and machine, after any `su`, `sudo -i` or nested `ssh`. With `--json`, the result's `context` says where: `{"user", "host", "cwd", "shell", "interp"}`.
- stderr comes as stdout. One command at a time per terminal. Only bash, dash and busybox ash.
- Programs that ask for a password wait in the user's tab: use `sudo -n` and other non-interactive flags. Give your own tool a timeout; when the command is given up, it gets Ctrl-C.
- Files cannot be transferred this way; write a small text file with a heredoc in the command.

## File Transfer

```bash
shellrs upload <host-id> ./dist /tmp/dist
shellrs upload <host-id> ./app.tar.gz /tmp/release.tar.gz
shellrs download <host-id> /var/log/nginx ./logs/nginx
shellrs download <host-id> /tmp/app.tar.gz ./latest.tar.gz
shellrs sync <host-id> ./dist /opt/app --delete
```

`sync` copies what the local folder holds into the remote folder, making the remote folder when it is not there (its parent must exist). Files with the same size and modification time are skipped, so running it again copies only what changed. `--delete` first removes what is in the remote folder and not in the local one. It only goes from this machine to the host.

## Manage Hosts and Credentials

```bash
shellrs hosts list
shellrs hosts show <host-id>
shellrs hosts create < host.json
shellrs hosts update <host-id> < changes.json
shellrs hosts delete <host-id>
shellrs credentials list
shellrs credentials show <credential-id>
shellrs credentials create < credential.json
shellrs credentials update <credential-id> < changes.json
shellrs credentials delete <credential-id>
```

`create` and `update` read one JSON object from stdin; `update` changes only the fields given. `shellrs hosts create --help` and `shellrs credentials create --help` list the fields. What `show --json` prints can be edited and passed back to `update` as it is. For example:

```bash
shellrs hosts create <<'EOF'
{"name": "web-01", "host": "10.0.1.12", "group": "生产/web", "auth": "credential", "credential": "<credential-id>"}
EOF
```

- `group` is a path such as `生产/web`; groups that are not there are made.
- A host logs in with `"auth": "password"` (its own password), `"credential"` (a saved credential, by its ID; the host takes the credential's user) or `"no_password"` (the SSH agent and the default keys).
- `route` is `{"type": "direct"}`, `{"type": "jump", "hosts": ["<host-id>", ...]}` or `{"type": "proxy", "kind": "http", "host": "...", "port": 3128}`.
- A credential is `"kind": "password"`, `"key"` (with `key_path`, a key file on this machine, or `private_key`, the key's text for ShellRS to keep) or `"agent"`.
- `hosts delete` refuses a host whose tabs are open in ShellRS, with `host_in_use`; `--force` closes them.

## Windows

The same commands work in PowerShell, cmd, and Git Bash. Local paths may be Windows paths (`.\dist`, `C:\Users\me\app.tar.gz`); remote paths are still POSIX paths. In Git Bash, use the heredoc form above.

In PowerShell, prefer `exec --json`: build the request as an object, pipe it in, and read the result as JSON. Nothing in the command needs quoting (Windows PowerShell 5.1 breaks double quotes inside arguments), and the result is plain ASCII, so the console's code page cannot garble remote output:

```powershell
@{ host = '<host-id>'; command = 'grep -c "error" /var/log/app.log' } | ConvertTo-Json -Compress | shellrs exec --json | ConvertFrom-Json
```

`exec --json` reads `{"host": "<host-id>", "command": "<command>"}` from stdin and, once the command ends, prints `{"exit_code": N, "stdout": "...", "stderr": "..."}`, or `{"error": {"code": "...", "message": "..."}}` when it could not run it. The exit code is the same as without `--json`, in `$LASTEXITCODE`. Windows PowerShell 5.1 pipes text to programs as ASCII: write non-ASCII characters in the command as `\uXXXX` escapes.

`--json` prints nothing until the command ends. To see the output as it comes (a long build, a large log), pass the command through stdin with a here-string instead, after making PowerShell read and write UTF-8:

```powershell
$OutputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::new()
@'
<command>
'@ | shellrs exec <host-id> --stdin
```

For `--terminal`, add `terminal = $true`:

```powershell
@{ host = '<host-id>'; command = 'df -h'; terminal = $true } | ConvertTo-Json -Compress | shellrs exec --json | ConvertFrom-Json
```

The `@'` must end its line and the `'@` must start one. Set the same encodings before the other commands, or non-ASCII text (such as the Chinese names of saved hosts) turns into `?` or mojibake.

To create or update a host or credential from Windows PowerShell 5.1, pipe the JSON in the same way, writing non-ASCII characters in it as `\uXXXX` escapes (`"group": "\u751f\u4ea7"` for 生产):

```powershell
@{ name = 'web-01'; host = '10.0.1.12'; user = 'deploy' } | ConvertTo-Json -Compress | shellrs hosts create
```

## Rules

- Do not ask the user for SSH passwords, private keys, or passphrases. ShellRS holds those credentials.
- Only when the user gives you a password, key or passphrase to save in ShellRS, put it in the JSON on stdin (`password`, `private_key`, `passphrase`), never in a command argument. ShellRS keeps it in the system keychain and never prints it back: `show` only says whether one is saved.
- Create, change or delete hosts and credentials only when the user asks you to. Ask before `hosts delete --force`: it closes the host's open tabs. Deleting a credential makes its hosts log in on their own.
- `shellrs exec` takes a host id and one complete remote shell command string, opens a temporary SSH connection, runs the command, and closes the connection. Its stdout and stderr are the remote command's, and its exit code is the remote command's exit code.
- The remote command gets no stdin, so interactive programs (editors, pagers, password prompts) will not work. Use non-interactive flags such as `--yes`, `-y`, or `--no-pager`.
- Long-running tasks should use tmux, nohup, or systemd on the remote host.
- `shellrs upload` takes a host id, a local file or folder path, and a remote destination path. `shellrs download` takes a host id, a remote file or folder path, and a local destination path. Folders are copied recursively, and existing files are overwritten.
- `shellrs sync` takes a host id, a local folder and a remote folder, and copies the local folder's contents into the remote one; `--delete` removes remote files absent locally.
- Destination paths follow scp: if the destination exists and is a directory, the source keeps its name inside that directory; otherwise the destination path is the final file or folder path, and its parent directory must exist.

## Errors

Errors are printed to stderr with a code in brackets, such as `[not_enabled]`. Exit code 255 means `shellrs` could not run the command at all.

- `not_running`: ShellRS is not open. Ask the user to open ShellRS.
- `not_enabled`: ask the user to turn on Enable external CLI (启用外部 CLI) in ShellRS under Settings → External CLI (设置 → 外部 CLI).
- `host_not_found`: run `shellrs hosts list` again and use an `id` from its output. `credential_not_found`: the same with `shellrs credentials list`.
- `bad_request` from `create` or `update`: the message says which field is wrong, in the words the ShellRS forms use.
- `host_in_use`: the host's tabs are open in ShellRS. Ask the user whether to close them (`--force`).
- `save_failed`: the change was made, but not all of it could be written to disk or the keychain. Tell the user the message.
- `version_mismatch`: the `shellrs` command and the running ShellRS are different versions. Ask the user to restart or update ShellRS.
- `host_key_unknown`, `missing_credential`: ShellRS has not connected to this host yet, or has no saved password. Ask the user to connect to the host once in ShellRS and save the password. For a host opened from a bastion host, use `--terminal` (see above).
- `host_key_changed`: the server's host key changed. Tell the user; do not try to work around it.
- `no_terminal` (`--terminal`): the host has no connected terminal in ShellRS. Ask the user to open or reconnect it.
- `terminal_busy` (`--terminal`): a full-screen program has the terminal, another `--terminal` command still runs there, the shell did not answer, or it did not start the command (a bastion host's command filter may hold it back). Tell the user the message.
- `unsupported_shell` (`--terminal`): the terminal runs a shell other than bash, dash or busybox ash. Tell the user.
- `too_long` (`--terminal`): split the command into shorter ones.
- `connect_failed` saying there is no permission to connect to ShellRS: the agent's sandbox blocks local inter-process communication. Ask the user to allow it, or run the command outside the sandbox.
