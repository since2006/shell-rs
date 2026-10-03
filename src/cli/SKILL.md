---
name: shellrs
description: This skill should be used when the user asks to "connect to a server", "SSH into a machine", "run a command on a remote host", "check a server", "list servers" or "list SSH sessions", "upload/download files to a server", or mentions ShellRS or the `shellrs` CLI. Use `shellrs` instead of running `ssh`, `scp`, or `sftp` directly.
---

# ShellRS CLI

Use the `shellrs` CLI to list the user's saved SSH hosts, run one-off commands, and transfer files through the user's running ShellRS desktop app. ShellRS holds the credentials and connects with the saved configuration.

Prefer `shellrs` over running `ssh`, `scp`, or `sftp` directly.
Run `shellrs --help` or `shellrs <command> --help` for details. `list` prints JSON when piped or given `--json`.

## Common Workflow

```bash
shellrs list --query <keyword> --json
shellrs exec <host-id> "<command>"
shellrs upload <host-id> <local-path> <remote-path>
shellrs download <host-id> <remote-path> <local-path>
```

`<host-id>` is the 16-character `id` from `shellrs list` (the same ID that ShellRS copies with 复制 ID).

For a simple command, pass one complete remote shell command string as `<command>`.
Pass a command through stdin when it contains quotes, pipes, redirects, JSON, `$`, backticks, or nested shell code, and quote the heredoc delimiter so the local shell leaves it alone:

```bash
shellrs exec <host-id> --stdin <<'EOF'
<command>
EOF
```

## File Transfer

```bash
shellrs upload <host-id> ./dist /tmp/dist
shellrs upload <host-id> ./app.tar.gz /tmp/release.tar.gz
shellrs download <host-id> /var/log/nginx ./logs/nginx
shellrs download <host-id> /tmp/app.tar.gz ./latest.tar.gz
```

## Windows

The same commands work in PowerShell, cmd, and Git Bash. Local paths may be Windows paths (`.\dist`, `C:\Users\me\app.tar.gz`); remote paths are still POSIX paths.

In PowerShell, pass multi-line commands and commands containing quotes through stdin with a here-string instead of a heredoc. Windows PowerShell 5.1 breaks double quotes inside arguments, so do not rely on them there:

```powershell
@'
<command>
'@ | shellrs exec <host-id> --stdin
```

Before running `shellrs` in PowerShell, make it read and write UTF-8, or non-ASCII text (such as the Chinese names of saved hosts and remote output) turns into `?` or mojibake:

```powershell
$OutputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::new()
```

The exit code is in `$LASTEXITCODE`.

## Rules

- Do not ask the user for SSH passwords, private keys, or passphrases. ShellRS holds those credentials.
- `shellrs exec` takes a host id and one complete remote shell command string, opens a temporary SSH connection, runs the command, and closes the connection. Its stdout and stderr are the remote command's, and its exit code is the remote command's exit code.
- The remote command gets no stdin, so interactive programs (editors, pagers, password prompts) will not work. Use non-interactive flags such as `--yes`, `-y`, or `--no-pager`.
- Long-running tasks should use tmux, nohup, or systemd on the remote host.
- Hosts with `"temporary": true` in `shellrs list` were opened in ShellRS from a bastion host's link rather than saved. Their id stays the same while their tab is open in ShellRS, so keep using it for every command; there is no need to list before each one. Once the tab is closed the id is gone (`host_not_found`), and opening the host from the bastion again gives it a new id: list again only then. Every command logs in to them again, which a bastion host may refuse if it gave out a one-time login; report that to the user instead of retrying.
- `shellrs upload` takes a host id, a local file or folder path, and a remote destination path. `shellrs download` takes a host id, a remote file or folder path, and a local destination path. Folders are copied recursively, and existing files are overwritten.
- Destination paths follow scp: if the destination exists and is a directory, the source keeps its name inside that directory; otherwise the destination path is the final file or folder path, and its parent directory must exist.

## Errors

Errors are printed to stderr with a code in brackets, such as `[not_enabled]`. Exit code 255 means `shellrs` could not run the command at all.

- `not_running`: ShellRS is not open. Ask the user to open ShellRS.
- `not_enabled`: ask the user to turn on 启用外部 CLI in ShellRS under 设置 → 外部 CLI.
- `host_not_found`: run `shellrs list` again and use an `id` from its output.
- `host_key_unknown`, `missing_credential`: ShellRS has not connected to this host yet, or has no saved password. Ask the user to connect to the host once in ShellRS and save the password.
- `host_key_changed`: the server's host key changed. Tell the user; do not try to work around it.
- `connect_failed` saying there is no permission to connect to ShellRS: the agent's sandbox blocks local inter-process communication. Ask the user to allow it, or run the command outside the sandbox.
