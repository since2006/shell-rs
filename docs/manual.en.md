# ShellRS user manual

[Back to README](../README.md)

An SSH host manager built on GPUI Kit, with saved hosts and groups, login credentials that several hosts can share, local terminals, multiple SSH terminals, a WinSCP-style dual-pane SFTP browser and SSH port forwarding.

The three icons after “ShellRS” in the title bar switch what the left sidebar shows: the host list, the port forwarding list or the credential list. When the sidebar is hidden, clicking one of them shows it again.

Only one ShellRS runs at a time: opening it again while it’s running brings the existing window to the front, and the newly launched copy quits. Two copies would each load the hosts into memory and write them back to the same database, overwriting each other’s changes. (Copies pointed at different data folders with `SHELLRS_DATA_DIR` count as different and can run side by side.) To see two terminals or SFTP tabs side by side, drag a tab into a split.

On macOS, the close button at the top left of the window only hides ShellRS: open connections, SFTP transfers, port forwards and the external CLI keep running. Click the Dock icon to bring the window back with all its tabs. To actually quit, press ⌘Q or right-click the Dock icon and choose Quit. On Windows and Linux, closing the window quits.

## Hosts

Each item saved in the left sidebar is a host: an address, a port and a way to authenticate. To log in to the same machine as different users, save it as two hosts.

The host dialog’s “Authentication” has three options:

- **Password**: enter a username and password; the password is kept in the system keychain. Leave the password empty to be asked at each connection. Only the password is used: the SSH agent and private keys aren’t tried first.
- **Credential**: choose a saved credential. The username and the way to log in both come from the credential (see “Credentials” below), so the dialog no longer asks for a username or password. “Test connection” also uses the password saved in the credential. To log in with a private key, create a key credential.
- **No password**: enter only the username. ShellRS tries logging in without authentication, then the SSH agent, then the default private keys in `~/.ssh` (`id_ed25519`, `id_ecdsa`, `id_rsa`), the same as the `ssh` command does by default. If the server asks for a password, the connection fails with the reason instead of asking; if the server asks for a verification code after the key, you’re still asked for it.

Whichever you choose, ShellRS first asks the server whether it allows logging in without authentication, and logs straight in if it does. If “Test connection” runs with “Password” chosen but no password entered, it tries what “No password” would (no authentication, the SSH agent, the default private keys) and explains why if it can’t connect.

“Connection”, below “Group” in the host dialog, decides how to reach the host. Terminals, SFTP, port forwarding, the external CLI and “Test connection” all follow it:

- **Direct**: the default. This computer connects straight to the host’s address and port.
- **Jump hosts**: connects through other saved hosts, with as many hops as needed. A row of labels in the box shows the whole route (“This computer → aliyun-99 → zentao → This host”), and below it each jump host has a row: “×” removes it, and “Add jump host” searches by name or address. To change the order, remove jump hosts and add them again. ShellRS logs in to the first jump host, which connects to the second, and so on, until the last one connects to the host. Each jump host uses its own address, port, authentication (password, credential or no password) and known hosts entries; on the first connection you’re asked about each one’s host key, and the prompt says which jump host it is. A jump host’s own “Connection” has no effect here: for several hops, list them all here in order. Enter the host’s address as the jump hosts’ network sees it, such as an internal address.
- **Proxy**: connects through an HTTP proxy (`CONNECT`) or a SOCKS5 proxy, chosen in the drop-down. Enter the proxy’s address and port (such as `127.0.0.1:7890` on this computer); if the proxy needs authentication, also enter a username and password. The password is kept in the system keychain and shared by hosts that go through the same proxy with the same username. The proxy resolves the host’s address. If the proxy requires authentication but has no password, or rejects the username and password, the connection fails with the reason instead of asking.

When you change a host’s address or authentication, hosts that jump through it reconnect too (changing only the name doesn’t). When you delete a host that other hosts use as a jump host, the confirmation says how many hosts use it. Afterwards their jump host lists keep a “Deleted host” row, and connecting fails right away instead of quietly connecting directly or skipping a hop; edit those hosts to remove it or choose another. Open connections aren’t affected.

**Not implemented yet: combining jump hosts with a proxy, reordering jump hosts by dragging, HTTPS proxies, and showing the connection type in the host list.**

The last field in the dialog is “Notes”, which can span several lines (Enter starts a new line): what the machine is for, who looks after it, when it expires and so on. Notes are kept only in the local database and don’t affect connecting.

Hover over a host and `user@address:port` appears at the right of its row, with its notes below if it has any (only the first 6 lines of long ones). A host’s context menu has “Connect”, “Open SFTP”, “Edit host…”, “Duplicate”, “Copy IP address” (“Copy hostname” when the address is a domain name), “Copy ID” and “Delete”. “Copy ID” copies the host’s 16-character random identifier (like `Jwg5rHvXCxw89paM`), which other tools use to identify the host: it’s generated when the host is created and stays the same when you rename the host or change its address; a duplicated host gets a new ID, and deleted IDs are never handed out again. When no tabs are open, the center area shows the “Recent” list; double-click a host or press Enter to connect. Right-clicking a host there opens the same menu and selects the host.

“Find a host”, to the left of “New host…” above that list, opens a search box listing every saved host: recently connected ones first, the rest by name, each row showing its group and `user@address:port`. Type part of a name, address, username, notes or group to filter; with several words separated by spaces, all of them must match. Nothing is highlighted when it opens; once you type, the first match is highlighted and Enter connects to it. You can also move with ↑↓, or click a host to connect to it.

The search box above the host list (⌘K, or Ctrl+K on Windows and Linux) filters hosts by name, address and username, and finds groups by name: when a group’s name matches, the group is listed with all its subgroups and hosts. Results are expanded, and matching hosts appear together with every level of group above them. Enter connects to the first matching host (in list order; hosts listed only because their group matched don’t count). Esc clears the search.

Drag a group or host above or below another item of the same kind to reorder items at the same level; drag a host onto a group to move it into the group, and drag a group onto the middle of another group to make it a subgroup. Drag to an empty part of the list to move an item back to the top level. Groups always come before hosts at the same level, and the order and which groups are expanded are saved in the local database. Reordering by dragging is paused while you search.

## Quick connect

“Quick connect…” at the top right of the title bar connects to a host without saving it, for machines you log in to only once. As for a new host, the dialog asks for the address, port and authentication (Password, Credential or No password); there are no Group, Connection or Notes fields, and the name is optional (the address is used instead). You can “Test connection” first. “Connect” opens a terminal tab:

- **Fully featured**: “Connect in new tab”, “Open SFTP”, “Reconnect” and all the right sidebar tools work. The connection is direct.
- **Not saved**: it stays out of the host list, the start page’s Recent list and the database, and is gone once its terminal and SFTP tabs are all closed. Its tab menu has no “Edit host…”.
- **Password in memory only**: the password you enter doesn’t go into the keychain and is reused when reconnecting; if you left it empty and the server wants a password, you’re asked. With “Credential”, the password or private key saved in the credential is used.
- The external CLI’s `shellrs hosts list` lists it too (`"temporary": true` in the JSON, “(not saved)” in the table’s group column), and you can use its ID while its tabs are open.

Hosts you want to keep are created with “+” at the top of the host list, the context menu, or ⌘N (Ctrl+N on Windows and Linux).

## Port forwarding

Click the port forwarding icon in the title bar and the left sidebar switches to the list of forwards. Each forwarding rule goes through a saved host and opens an SSH connection of its own when it starts, independent of that host’s terminal or SFTP tabs: closing the tabs or choosing “Disconnect” on the host doesn’t affect it, and the host’s connection state doesn’t count it. While forwards are running, the number running appears next to the port forwarding icon in the title bar.

There are three kinds of forward. Think of each as “entrance → exit → destination”: a connection comes in at the entrance, goes through the SSH tunnel to the exit, and the exit connects to the destination:

| Type | Entrance (listens) | Exit | Use |
| --- | --- | --- | --- |
| Local (`ssh -L`) | This computer | Server | Reach a service on the server’s side from this computer, such as connecting a local client to a database that only the server can reach |
| Remote (`ssh -R`) | Server | This computer | Let the server’s side reach a service on this computer, such as showing someone a site you’re developing locally, or receiving webhooks |
| Dynamic (`ssh -D`) | This computer (SOCKS proxy) | Server | Use the server as a proxy, with the destination chosen by the app, such as browsing the web through the server |

- **Creating and editing**: “+” at the top of the list opens the dialog. A row of three cards at the top chooses the type; the diagram in the middle fills in the addresses as you type and marks which machine the entrance is on; and a sentence below explains what the rule does (such as “Connecting to 127.0.0.1:8080 on this computer is the same as connecting to db.internal:3306 from the SSH server.”). The “SSH server” in the diagram and the sentence is the host chosen in “Via host”; the diagram calls it by its role, not by the host’s name. A dot moves slowly along the diagram in the direction of the connection: through the SSH tunnel first, then on to the destination, pausing and starting over; switching type sends it off again from the new entrance. The name is optional; without one, the list shows a summary of the forward (`8080 → db.internal:3306`, `9000 ← localhost:3000`, `SOCKS 1080`).
- **Starting and stopping**: the button at the end of each row, double-click the row, or select it and press Enter; the context menu also has “Edit port forward…” and “Delete”. “Create” only saves the rule and doesn’t start it. Rules with “Start when ShellRS opens” checked start along with the app.
- **Status**: the end of the row shows “Connecting”, “Running” (with the current number of connections), “Connection lost, reconnecting”, or “Stopped:” with the reason. When a single connection can’t be forwarded (for example, the server can’t reach the destination), only that row says so, and the forward keeps running.
- **Login**: uses the host’s settings, the password saved in the keychain and ShellRS’s own known hosts file. A host you’re connecting to for the first time, or a password that has to be entered, brings up the same prompt as in a terminal, saying which forward is connecting; cancelling the prompt stops the forward.
- **Reconnecting after a drop**: when the connection drops, ShellRS reconnects automatically up to three times, after 1, 3 and 10 seconds. While it reconnects, the local listening port stays open, and new incoming connections are closed straight away. Automatic reconnects never prompt: if a password needs entering again or a host key needs confirming, the forward stops with the reason, and you can start it by hand. It also stops after three failed attempts, with a notification.
- **After changes**: editing a running rule’s type, host, listening address or destination, or the address, authentication or connection of the host it goes through (including the username, type or private key file of the credential that host uses, and its jump hosts), restarts the forward with the new settings; changing only the name or auto start doesn’t.
- **Deleting a host**: deleting a host (or a group that contains it) also deletes the forwarding rules that go through it, stopping running ones first; the confirmation says how many.
- **Dynamic forwarding** supports SOCKS5 (no authentication, `CONNECT`, IPv4, domain names, IPv6) and SOCKS4 / 4a. Domain names are resolved by the server.
- **The listening address** defaults to `127.0.0.1`. With another address, local and dynamic forwards can be used by other devices on the same network; for a remote forward, other machines can connect only if the server’s `sshd_config` sets `GatewayPorts clientspecified` or `yes`. The default is `no`, in which case the forward still runs without an error but listens only on the server’s own `127.0.0.1` (the same as a hand-written `ssh -R`). The dialog points this out when you enter a non-loopback address. Ports below 1024 need administrator rights (root, on the server). Port 0 (letting the system pick a port) isn’t supported.

**Not implemented yet: managing forwards through the external CLI, and ordering rules. The system’s “Reduce motion” setting doesn’t turn off the diagram’s animation yet.**

## Credentials

Click the credentials (key) icon in the title bar and the left sidebar switches to the list of credentials. A credential is a set of login details that several hosts can share: a name, a username and a way to log in:

| Type | What it keeps | When logging in |
| --- | --- | --- |
| Password | Username and password | Uses the password saved in the credential; asks if none is saved or it’s rejected |
| Key | Username, private key and passphrase | Uses only this private key; the passphrase is saved per key file and shared by credentials that use the same file |
| SSH Agent | Username | Uses only the keys in the SSH agent: on macOS / Linux the agent that `SSH_AUTH_SOCK` points to, on Windows the OpenSSH Authentication Agent service |

- **Creating and editing**: “+” at the top of the list, “New credential…” in the empty list, or the context menu on empty space opens the dialog; to edit a credential, double-click it, select it and press Enter, or right-click it and choose “Edit credential…”. Passwords and private key passphrases go into the system keychain; the database holds only the name, username, type and private key path.
- **Where the key comes from**: a key credential’s “Private key” has three sources.
  - **Local file**: choose a private key file; only its path is saved. If the file moves or is deleted, choose it again.
  - **Paste**: paste a private key in OpenSSH, PEM or PuTTY format. If you paste a public key or something unrecognizable, ShellRS says why. The passphrase field appears only when the private key is encrypted with one.
  - **Generate**: choose Ed25519 (the default) or RSA 4096, and the key is generated as soon as you choose. The dialog shows the public key, with a button to copy it. The passphrase can be left empty; if you enter one, it encrypts the private key and goes into the keychain. A generated key has the credential’s name as its comment.
  - ShellRS writes pasted and generated private keys to files in `keys/` in the data folder (with random file names, readable only by your account), and the credential refers to that file; when you edit the credential, it shows as “Local file” with that path. Pasting or generating again overwrites the file directly, and connected hosts don’t reconnect because of it; switching to another file or another type, or deleting the credential, deletes the file together with its passphrase, and the dialog warns you first. ShellRS never changes or deletes your own private key files.
  - The empty list and the context menu on empty space also have “Generate key…”, which opens the same dialog with “Key · Generate” already chosen. A key credential’s context menu has “Copy public key”, for adding the public key to `~/.ssh/authorized_keys` on a server.
- **Hosts follow the credential**: changing a credential’s username changes it for every host that uses the credential. Changing the username, type or private key file reconnects the connected hosts that use it (changing only the name doesn’t); the dialog says how many hosts use it.
- **Deleting**: right-click and choose “Delete”. If hosts use the credential, the confirmation says how many and how they’ll log in afterwards: hosts using a password credential switch to “Password” and ask when connecting; hosts using a key or SSH Agent credential switch to “No password”. Usernames stay the same; passwords and private keys aren’t copied over. Open connections aren’t affected, and the credential’s password is deleted from the keychain along with it.
- **An SSH Agent credential** says why right away when it can’t reach the agent, the agent has no keys, or the server accepts none of them; it never falls back to asking for a password. Hosts set to “No password” try the same agent too, skip it silently if it can’t be reached, and go on to the default private keys (on Windows as well).

**Not implemented yet: creating a credential right from the host dialog, ordering credentials, a custom SSH agent socket, installing the public key on a host automatically, and exporting the private keys ShellRS keeps.**

## Settings

The “Settings” button at the bottom of the sidebar, or ⌘, (Ctrl+, on Windows and Linux), opens a “Settings” tab in the center area; if it’s already open, it switches to that tab instead of opening another. The settings page has two columns: on the left the categories (Appearance, Terminal, Highlighting, Keyboard shortcuts, External CLI, General, About), with a search box at the top and a width you can drag; on the right the settings of the chosen category. Changes take effect at once and are saved in `settings.json` in the data folder.

- **Appearance → Interface → Language**: System, 简体中文 or English. The default is System: Simplified Chinese when the system language is Chinese, English for any other language. Changes apply at once, without a restart: the text in the window, open tabs and search box hints all switch to the new language; prompts already written into terminals and notifications already shown stay as they were. The external CLI’s output uses this language too.
- **Appearance → Interface → Appearance**: Dark, Light or System, System by default; System switches along with the system’s light and dark mode. The theme button in the title bar also changes this setting.
- **Appearance → Theme**: the left column has light themes and the right column dark themes; choose one in each. The light appearance uses the one chosen on the left, the dark appearance the one on the right; with System they switch along with the system, and the same happens when you switch with the title bar’s theme button. By default only terminals take the chosen theme’s colors, and the interface stays a neutral gray (the dark one is an easy-on-the-eyes charcoal, not pure black). Turn on “Match interface to theme” at the top and the interface takes the theme’s colors too: the sidebar, tab bar, dialogs and so on use the theme’s background and text colors, and states such as success, warning and error use the theme’s green, yellow and red. Each card shows a sample in that theme’s colors, and the chosen one has a blue check mark at its top right. The defaults are ShellRS Light and ShellRS Dark, which match the interface. There are 10 built-in light themes and 10 dark ones: light ShellRS Light, Solarized Light, Tokyo Day, Catppuccin Latte, Flexoki Light, One Half Light, Gruvbox Light, Kanagawa Lotus, Rosé Pine Dawn, Everforest Light; dark ShellRS Dark, Solarized Dark, Tokyo Night, Catppuccin Mocha, Flexoki Dark, One Half Dark, Gruvbox Dark, Kanagawa Wave, Dracula, Nord. The light and dark versions of the same theme sit on the same row of the two columns. Changes apply at once to every open terminal, and the font and highlighting previews use the current theme too; when vim or another program asks the terminal for its background color, it gets the current theme’s. **Not implemented yet: custom themes, importing color schemes, and per-host themes.**
- **Terminal → Font**: Font family (lists only the monospaced fonts installed on this computer; the default is the system’s monospaced font, Menlo on macOS), Font size (10–28 pixels, default 13) and Line height (1.0–2.0 times the font size, default 1.54, the same as the earlier 20-pixel line height). Changes apply at once to every open terminal; in a terminal, ⌘= / ⌘- (Ctrl on Windows and Linux) also change the font size step by step, and ⌘0 goes back to the default. The preview below lays out letters, digits, easily confused characters, symbols, Chinese, tab stops and a command line the way the terminal does. If the font in the settings file isn’t installed on this computer, the default font is used.
- **Highlighting**: the main switch, the preview and the rules table, which color text in terminals by rule and alert you on matches; see “Terminal”.
- **Keyboard shortcuts**: the shortcuts of the main features, in four groups (General, Tabs, Terminal, Editor); each row is a feature and its keys. Click the keys to get “Press a key combination”, then press a new combination to use it; Esc or clicking elsewhere cancels. The two buttons at the end of the row are Disable (the row then shows “None”; click again to restore the default) and Restore default; the “Reset All” button at the top of the page restores them all. Changes take effect at once, and the keys shown in tooltips and context menus follow.
  - A combination needs ⌘, ⌥ or ⌃ (Ctrl or Alt on Windows and Linux), or must be one of F1–F12; for “Switch to tab 1…9”, press any digit with a modifier, and all nine change together.
  - A combination already taken by another shortcut in the same place, or by text fields and other built-in features, is refused, and you’re told what uses it; to use it anyway, change or disable that one first. Terminal and editor shortcuts can share keys with general shortcuts, and in the terminal or editor they take precedence (in a terminal, for example, ⌘K clears the screen).
  - Defaults (macOS / Windows and Linux): New host ⌘N / Ctrl+N, New group ⌘⇧N / Ctrl+Shift+N, Open settings ⌘, / Ctrl+,, Show or hide sidebar ⌘B / Ctrl+B, Show or hide right sidebar ⌘⌥B / Ctrl+Alt+B, Focus search ⌘K / Ctrl+K, Zoom in, Zoom out and Actual size ⌘= ⌘- ⌘0 / Ctrl+= Ctrl+- Ctrl+0, Quit ⌘Q / Ctrl+Q; New local terminal ⌘T / Ctrl+T, Close tab ⌘W / Ctrl+W, Next tab ⌘⇧] / Ctrl+Tab, Previous tab ⌘⇧[ / Ctrl+Shift+Tab, Switch to tab 1…9 ⌘1…⌘9 / Alt+1…9 (9 is the last tab); in the terminal, Copy, Paste, Find, Find next, Find previous and Clear screen ⌘C ⌘V ⌘F ⌘G ⌘⇧G ⌘K / Ctrl+Shift+C, Ctrl+Shift+V, Ctrl+Shift+F, F3, Shift+F3, Ctrl+Shift+K, and Bigger text, Smaller text and Default text size ⌘= ⌘- ⌘0 / Ctrl+= Ctrl+- Ctrl+0; in the editor, Save ⌘S / Ctrl+S. Switching tabs stays within the group of tabs the current tab is in, wrapping around at either end. In a terminal, ⌘= / ⌘- / ⌘0 change the terminal font size (1 pixel at a time, within the same 10–28 range as “Terminal › Font size”; saved in the settings and kept for the next launch); outside a terminal they zoom the whole interface (five steps, back to the default after a restart).
  - Shortcuts in the SFTP file list and the image preview, and fixed keys such as Enter, the arrow keys, Tab and Esc, aren’t changed here.
- **External CLI**: see the next section.
- **General → Window**: “Remember window size” restores the last window size and whether it was maximized, and “Remember window position” puts the window back on the screen and in the place it last was. Both are on by default and take effect at the next launch. If that screen is gone, or the window would be mostly off screen, it opens in the middle of the screen; a window larger than the screen shrinks to fit. Full screen isn’t remembered: the window comes back at its size from before full screen. The window’s size and position are recorded in `window.json` in the data folder as it moves.
- **About**: the “Updates” group, with the current version and checking for updates, the update channel and automatic updates (see “Download and updates”); “Send anonymous usage statistics” in the “Privacy” group, on by default (see “Privacy”); and “Website” in the “ShellRS” group, which opens shellrs.com in your browser.

## External CLI

The `shellrs` command lets AI agents such as Codex, Claude Code and OpenCode run remote commands and transfer and sync files on the hosts saved in ShellRS, and also view, create, change and delete hosts and credentials. The command itself reads no passwords or private keys, and no database: it hands the request to the running ShellRS, which connects with the saved settings and the keychain. Requests travel over local interprocess communication: the socket `cli.sock` in the data folder on macOS and Linux, and the named pipe `\\.\pipe\shellrs-cli-<hash of the data folder>` on Windows.

In Settings → External CLI:

- **Enable external CLI**: off by default. While it’s off, ShellRS still listens but refuses every request, and the command tells you to turn it on here. Once it’s on, any program running as the current user on this computer can use the saved hosts this way, and can also change hosts and credentials (there’s no separate switch). Only the current user can read and write the socket, and ShellRS also checks the identity of the user connecting. On Windows, the pipe likewise admits only the current user and only local connections, and before sending a request the command checks that the pipe belongs to the current user (or an administrator), so nobody else can grab a pipe with the same name.
- **Command**: puts `shellrs` on the PATH.
  - macOS: links it to `/usr/local/bin/shellrs`; if that folder isn’t writable, the system asks for authorization.
  - Linux: links it to `~/.local/bin/shellrs`.
  - On macOS and Linux, “Remove” deletes only a link that points to shellrs; other files with the same name are left alone.
  - Windows: a GUI app has no console, so the command is a separate console program, `shellrs-cli.exe` (next to `shellrs.exe`). “Install” copies it to `%LOCALAPPDATA%\ShellRS\bin\shellrs.exe` and adds that folder to the current user’s PATH (no administrator needed); terminals and agents find it once they’re reopened. After ShellRS updates, it refreshes this copy when it starts; when it can’t, “Update” appears. The copy can be updated even while it’s in use. “Remove” deletes the copy and takes the folder off the PATH.
- **Agent Skills**: writes the skill that teaches agents to use `shellrs` to the standard folder (`~/.agents/skills`) or to the skills folder of Codex, Claude Code, OpenCode or WorkBuddy; when the content differs from the current version, “Update” appears. “Copy skills” copies the whole skill to the clipboard.

Commands (`shellrs --help` has the full details):

```sh
shellrs hosts list [-q <keyword>] [--json]     # list hosts; JSON when the output isn't a terminal
shellrs hosts show <ID> [--json]               # one host's full settings
shellrs exec <ID> "<command>"                  # or --stdin to read the command from stdin
shellrs exec --json                            # read {"host", "command"} from stdin, print JSON
shellrs upload <ID> <local-path> <remote-path>
shellrs download <ID> <remote-path> <local-path>
shellrs sync <ID> <local-folder> <remote-folder> [--delete]
shellrs hosts create | update <ID> | delete <ID> [--force]
shellrs credentials list | show <ID> | create | update <ID> | delete <ID>
```

- `<ID>` is the 16-character host ID: the `id` in `shellrs hosts list`, which is also what “Copy ID” on a host’s context menu copies. The list also includes quick connections and external connections whose tabs are open (`"temporary": true`); see “Quick connect” and “Opening from a bastion host (external connections)”.
- `exec` opens a new connection each time and disconnects when it’s done. The remote command gets no stdin, and the exit code is the remote command’s; if a signal killed it, the exit code is 128 plus the signal number.
- Uploads and downloads use the SFTP tab’s transfer engine (recursive, `.filepart`, reconnecting after a drop), and destination paths follow scp’s rules: if the destination is an existing folder, the item goes into it under its own name; otherwise the destination is the path of the copy itself, and its parent folder must exist. Existing files are overwritten; new files keep the source’s executable permission. `~` means the login folder.
- `sync` goes only from this computer to the host: it syncs the contents of the local folder into the remote folder. If the remote folder doesn’t exist, it’s created (its parent must exist); if it’s a file, that’s an error; and it can’t be the root folder `/`. Files whose size and modification time haven’t changed, and links whose target hasn’t changed, are skipped and counted as “unchanged” in the summary, so syncing again sends only what changed; other files are overwritten, and new files keep their executable permission. `--delete` first deletes items in the remote folder that aren’t in the local one, or are of a different type (without following links), but never the `.filepart` and `.shellrs-….backup` files that transfers in progress leave behind, and it doesn’t clean up folders that can’t be read locally; items it can’t delete count as failures.
- `hosts` and `credentials` manage the saved hosts and credentials. `create` and `update` read a JSON object from stdin (`update` changes only the fields given); the fields are in `shellrs hosts create --help` and `shellrs credentials create --help`. What `show --json` prints can be edited and passed straight back to `update`. The checks are the same as in the dialogs, and errors use the dialogs’ wording:
  - Hosts: `name`, `host` (the address), `port` (default 22), `user` (default root), `group` (a group path such as `Production/Databases`, created level by level if it doesn’t exist; an error if more than one group has that path), `auth` (`password` / `credential` / `no_password`), `credential` (a credential ID), `password`, `route` (direct, jump hosts listed by ID, or an HTTP / SOCKS5 proxy) and `notes`. Jump hosts must be saved hosts, and not the host itself. If you change the address, port or user without giving a password, the saved password moves along, as in the dialog.
  - Credentials: `name`, `kind` (`password` / `key` / `agent`), `user`, `password`, `key_path` (a private key file on this computer) or `private_key` (the full text of a private key, like “Paste” in the dialog, kept in `keys/` in the data folder), and `passphrase`. A credential’s ID is a 16-character random string, shown by `credentials list`.
  - Passwords, passphrases and private keys are write-only: they go into the system keychain or `keys/`, no command ever prints them, and `show` only says whether one is saved; for a private key ShellRS keeps, it doesn’t even show the path.
  - Quick connections and external connections can be shown with `show`, but not changed or deleted.
  - `hosts delete` doesn’t ask for confirmation and deletes the host’s port forwarding rules along with it; if the host has tabs open in ShellRS, it refuses (`host_in_use`), and only with `--force` does it close the tabs and delete the host. Hosts that jump through it are left with a “Deleted host” hop. `credentials delete` doesn’t ask either; hosts that use the credential switch to logging in on their own (as in the app).
  - ShellRS makes changes one at a time: a change that hasn’t had its turn within 10 seconds is withdrawn and not made; once it has started, the command waits for it to finish.
- On failure, the command prints `shellrs: [code] message` to stderr and exits with 255. The error codes are `not_running` (ShellRS isn’t running), `not_enabled` (the external CLI isn’t enabled), `host_not_found`, `credential_not_found`, `host_key_unknown` (you haven’t connected to this host in ShellRS yet), `host_key_changed`, `missing_credential` (no saved password or passphrase), `connect_failed`, `transfer_failed`, `bad_request`, `host_in_use`, `save_failed` (the change was made but couldn’t all be written to the database or keychain) and `version_mismatch` (the command and the running ShellRS are different versions, or ShellRS is too old to know the command). The CLI never prompts: an unknown host or a missing password fails right away, so connect once in ShellRS first. When a transfer finishes but some items failed, the exit code is 1.
- `exec --json` reads `{"host": "<ID>", "command": "<command>"}` from stdin and, once the command ends, prints `{"exit_code", "stdout", "stderr"}`, or `{"error": {"code", "message"}}` on an error, with the same exit codes as without `--json`. The output is ASCII only: Chinese and other characters are escaped as `\uXXXX`, and output that isn’t UTF-8 becomes U+FFFD. The command doesn’t go through the local shell’s quoting, and the console code page can’t garble the output, which suits Windows PowerShell and agents that need to read stdout and stderr separately.
- macOS, Linux and Windows are supported. In Windows PowerShell, prefer `exec --json` for running commands (`@{ host = '<ID>'; command = '…' } | ConvertTo-Json -Compress | shellrs exec --json`). To watch the output while the command runs (long builds, lots of logs), pass the command as a here-string through `--stdin` instead (`@'…'@ | shellrs exec <ID> --stdin`), and, as for `list`, `upload` and `download`, first run `$OutputEncoding = [Console]::OutputEncoding = [Text.UTF8Encoding]::new()`, or Chinese and other non-ASCII text turns into question marks or garbage. Any BOM at the start of the command text is removed and CRLF becomes LF, so the remote shell sees no stray carriage returns.

## Opening from a bastion host (external connections)

The local client of a bastion host such as JumpServer can launch ShellRS to open a terminal the way it launches Xshell, or to open SFTP the way it launches WinSCP: set ShellRS as the SSH client or the SFTP client, with the arguments written as for Xshell or WinSCP. When ShellRS gets an `ssh://` link, it opens a terminal tab and connects; when it gets an `sftp://` link, it opens an SFTP tab:

```sh
ShellRS ssh://user[:password]@address[:port]
ShellRS -url ssh://user[:password]@address[:port] -newtab tab-name
ShellRS sftp://user[:password]@address[:port]
ShellRS /sessionname=tab-name sftp://user[:password]@address[:port]
```

- ShellRS understands Xshell’s `-url` (the link), `-newtab` (the tab name; the address if it isn’t given) and `-newwin` (ignored: ShellRS has only one window), and WinSCP’s `/sessionname=tab-name` (which can also be written `-sessionname=`), in any letter case; other options such as `/newinstance` and `/ini=nul` are ignored. Only `ssh://` and `sftp://` are supported, and the port defaults to 22. Special characters in the username and password can be escaped by URL rules (`@` as `%40`); a username that itself contains `@` also works, since only what follows the last `@` is the address. The path in an `sftp://` link (which WinSCP uses to choose the folder to open) isn’t read yet; the remote side opens the home folder as usual.
- **The tab fills the window**: when an external connection opens, the host list on the left collapses automatically (as with ⌘B); press ⌘B (Ctrl+B on Windows and Linux) or double-click the tab to show it again.
- **External connection, not saved**: it stays out of the host list, the start page’s Recent list and the database, and is gone once its terminal and SFTP tabs are all closed. Its tab menu has no “Edit host…”; everything else (“Connect in new tab”, “Open SFTP”, “Reconnect”) works as for a saved host.
- **One channel only**: many bastion hosts end the whole session when a second channel opens on the same connection. An SFTP tab opens only one anyway; a terminal doesn’t detect the host’s system (its tab shows the first letter of the name), and the right sidebar has only Snippets: History, Docker, Services, Processes, Network and Monitor all run commands over another channel, so they aren’t offered here.
- The password in the link stays only in memory, never in the keychain, and is reused when reconnecting; if the link has no password and the server wants one, you’re asked. The first time you connect to a bastion host, you trust its host key, which is recorded in ShellRS’s own `known_hosts` and not asked about again.
- When ShellRS is already running, the link goes to the running ShellRS, its window comes to the front, and no second ShellRS starts. If the link has a problem (not `ssh://` or `sftp://`, no username or address, a wrong port), ShellRS shows a notification saying why.
- The program for the bastion host to launch:
  - Windows: `shellrs.exe` in the installation folder (by default `%LOCALAPPDATA%\Programs\ShellRS\shellrs.exe`). Don’t use the `shellrs.exe` on the PATH: that’s the external CLI’s copy.
  - macOS: `/Applications/ShellRS.app/Contents/MacOS/shellrs`. Don’t use `open -a ShellRS --args …`: when ShellRS is already running, the system drops the arguments.
  - Linux: the AppImage itself.
- The external CLI’s `shellrs hosts list` lists external connections too (`"temporary": true` in the JSON, “(not saved)” in the table’s group column). While the tabs are open, an agent can use the connection’s ID to run commands and transfer files, and the ID stays the same all that time. Once the tabs close, the ID stops working; the next time the bastion host launches ShellRS (even for the same asset), it’s a new external connection with a new ID. As with saved hosts, each command logs in again with the username and password from the link; if the bastion host passed a one-time token, that login is refused.
- A password in command-line arguments is visible in this computer’s process list; that comes with how the bastion host passes arguments.

## Tabs

Remote terminal tabs and SFTP tabs use the host’s system badge as their icon (the same one as in the host list). A remote terminal tab’s context menu has “Rename tab…” (for this run only; leave the name empty to go back to the host’s name; handy for telling several connections to the same host apart), “Connect in new tab”, “Open SFTP”, “Copy IP address” / “Copy hostname”, “Reconnect”, “Edit host…”, and “Close”, “Close to the left”, “Close to the right”, “Close others” and “Close all”. An SFTP tab’s menu has “Rename tab…” (leave the name empty to go back to “host name · SFTP”), “Open SFTP”, “Copy IP address” / “Copy hostname”, “Reconnect” and the close commands, and the right of its tab bar has the same “SFTP” and “Reconnect” buttons as a remote terminal’s; a local terminal tab’s menu has “Restart” and the close commands. The “…” button at the right of the tab bar has the same menu as the current tab’s context menu. Double-click any tab’s title to hide or show the left sidebar (like ⌘B and the title bar button). While a remote terminal is running, or an SFTP tab is connected, the right of the tab bar shows the round-trip latency of that SSH connection, updated every 5 seconds: green below 100 ms, yellow at 100–200 ms, red above 200 ms or on a timeout. It’s measured during SFTP transfers too, and the reading rises when a transfer takes up the bandwidth. When closing tabs in bulk, an SFTP tab that’s uploading still asks first.

## Terminal

The terminal’s context menu has “Copy”, “Paste”, “Find…” and “Clear screen”; a remote terminal’s also has “Open SFTP”, “Reconnect” and “Disconnect”. “Disconnect” disconnects only this tab: the tab stays and can reconnect at any time, and other tabs and SFTP for the same host aren’t affected.

- **Find** (⌘F, or Ctrl+Shift+F on Windows and Linux): opens the find bar at the top right of the terminal and searches the screen and the scrollback for the literal text. An all-lowercase query ignores case; one with capital letters matches case. All matches are highlighted, the current one darker, and the find bar shows “current/total”. Enter or ⌘G (F3) goes to the next match (newer output), Shift+Enter or ⌘⇧G (Shift+F3) to the previous one (older output), wrapping around at the ends. Esc closes it. If text is selected when it opens, that text becomes the query.
- **Clear screen** (⌘K, or Ctrl+Shift+K on Windows and Linux; when the terminal doesn’t have focus, ⌘K still focuses the sidebar’s search box): clears the screen and the scrollback, leaving only the prompt line the cursor is on, including anything typed but not yet entered. Clearing happens only locally; nothing is sent to the server. It isn’t available while a full-screen program such as vim or top is running.
- **Links**: `http://` and `https://` addresses in the output, and links that programs mark explicitly (such as `ls --hyperlink` and gcc’s error messages), are shown in blue and underlined. Hold ⌘ (Ctrl on Windows and Linux) and click to open one in the system browser; a plain click still selects text. Rest the pointer on a link for a moment to see its real address: for a link a program marks, the text shown can differ from the address. A trailing period, comma or unmatched closing parenthesis isn’t part of the address, and a long address wrapped over two lines is still one link. Other schemes (such as `file://`) aren’t treated as links.
- **Mouse**: when programs such as vim (`:set mouse=a`), htop, tmux (`set -g mouse on`) and mc ask for the mouse, clicks, drags and the scroll wheel all go to the program. Dragging with Shift held still selects text, and scrolling with Shift held still scrolls through the scrollback; right-clicking always opens ShellRS’s menu.
- **Notifications**: programs can ask for a notification (OSC 9, or OSC 777’s `notify;title;body`), and a terminal bell alerts you too. When ShellRS isn’t in front, you get a system notification (macOS asks for permission the first time), and clicking it takes you back to that terminal; when ShellRS is in front but the terminal is in another tab, a notice appears at the top right of the window, and clicking it switches there. Notifications that programs ask for appear even when the terminal is right in front of you; the bell alerts you only when you can’t see that terminal (Tab completion and the like ring it too). The notification title is the tab name, so you can tell which host it came from. Each terminal alerts at most once every 2 seconds for program notifications and once every 10 seconds for the bell. Settings › Terminal › Notifications can turn each one off. To be told when a command finishes, append `; printf '\e]9;Done\a'` or `; tput bel` to it; command-line tools such as Claude Code alert you the same way when they need you.
- **Highlighting**: the rules in Settings › Highlighting show matching text in the rule’s color in every terminal, scrollback included. Only the display changes: the server’s output and copied text stay the same; matching keeps working after the window is resized and long lines wrap, and a word wrapped over two lines is colored in both parts. The feature is off by default and works only once you turn on “Highlight keywords” at the top of the page; turning it off disables all the rules at once. “Preview” shows sample output in the current terminal’s font and colors, and rule changes show up at once in the preview and in open terminals. The rules table has one rule per row, edited right in the table: “On”, “Regular expression” (case-sensitive; start it with `(?i)` to ignore case), “Note”, a color (click the swatch for a color picker, or type `#rrggbb` next to it) and “Notify”; the trash can at the end of the row deletes the rule. A mistake in a regular expression is pointed out below the field, and the rule doesn’t apply until it’s fixed. Rules apply from top to bottom, and where they overlap, the earlier rule wins; drag the handle at the start of a row to reorder. Three sample rules come to start with: `ERROR` in red, `WARN` in amber and IPv4 addresses in blue, all on and not notifying, so turning on the main switch shows the effect right away; you can change or delete them. A rule with “Notify” checked alerts you when you can’t see the terminal, the same way as the bell: a system notification when ShellRS isn’t in front, and a notice at the top right of the window when the terminal is in another tab. The title is “tab name: note” (the regular expression when the note is empty), and the body is the matching line. Only new output is checked, each line once it ends; each terminal alerts at most once every 10 seconds, and the same line redrawn over and over at most once every 5 minutes. Nothing is highlighted or alerted in full-screen programs such as vim, less, top and tmux. **Not implemented yet: per-host rules, text styles such as bold, and reordering with the keyboard.**
- **Remote copy**: what remote programs copy with OSC 52 (such as tmux with `set -g set-clipboard on`, or vim’s OSC 52 plugins) goes straight to this computer’s clipboard. Remote programs can’t read this computer’s clipboard.

## Right sidebar

When the current tab is a remote terminal, a column of tool buttons runs down the right edge of the window: Snippets, History, Docker, Services, Processes, Network and Monitor. Click one to open that tool on the right, and click it again to collapse it; ⌘⌥B (Ctrl+Alt+B on Windows and Linux) shows or hides the right sidebar. Drag the right sidebar’s left edge to widen it; it can’t get narrower than it was when it opened. The right sidebar appears only with remote terminals: switching to SFTP, a local terminal or Settings hides it together with its buttons, and going back to any remote terminal brings it back as it was, now for the host of the current terminal. With tabs dragged into a split, the right sidebar stays: it works on the remote terminal you switched to or clicked into last, stays put when you click into SFTP or a local terminal in the other half, and hides only when no remote terminal is on screen. Its title names the host it works on (such as “Monitor · web-01”), and the status bar follows the half you clicked into too. Hosts known not to run Linux (macOS, Windows, BSD) don’t show the Services, Processes, Network and Monitor icons; History and Docker are there for every host except Windows ones (on a Mac, Docker works too if Docker Desktop is installed).

**Snippets** keeps the commands you use often, shared by every host. Click the plus at the top to create a snippet: a name, a category and a command, which can span several lines. Snippets are grouped by category, with the ones without a category last, under “Uncategorized”; click a category heading to collapse or expand it, and right-click it to create a snippet in that category, rename the category or delete it (its snippets are deleted with it, and the confirmation says how many). The folder button at the top creates a category. You can search by name or command; with several words, snippets that contain all of them are listed.

Clicking a snippet runs its command in the terminal (snippets are set to “Run on click” by default). When the pointer is on a snippet, an icon at its right shows what a click will do: a lightning bolt means a click runs it, ▶ means a click only types it in. Uncheck “Run on click” in the snippet dialog, and clicking the snippet only puts the command on the terminal’s input line (replacing anything already typed), for you to edit and press Enter. Clicking the lightning bolt or ▶ at the right always runs it right away; the context menu also has “Insert into terminal”, “Copy command”, “Edit…” and “Delete…”. Nothing is typed in while a full-screen program such as vim or less is running in the terminal. On Windows hosts, what’s already typed isn’t replaced; the command is typed after it.

**History** lists the remote host’s bash history (`~/.bash_history`), most recent first, each command once, with how many times it ran below it; when bash recorded times (`HISTTIMEFORMAT` is set), it also says when it last ran, and multi-line commands are recognized. You can search; with several words, commands that contain all of them are listed, ignoring case. Click a command to put it on the terminal’s input line (replacing anything already typed, without running it), for you to edit and press Enter; the “Run” button at the right runs it right away, and the context menu can also copy it. Nothing is typed in while a full-screen program such as vim or less is running in the terminal.

bash writes a session’s commands to the history file only when the shell exits, so commands you’ve just run in the current terminal show up only after it exits. The list is read when you open History or switch terminals, and after that only when you click the refresh button; when the history file is long, only its last 512 KB is read.

**Docker** shows the Docker and Compose versions at the top, and below them four tabs with counts: “Containers”, “Volumes”, “Images” and “Networks”. On “Containers”, the containers of the same compose project are grouped under a project card (project name, running / total, project folder); click the project’s row to collapse or expand it. A project with a single container that’s running starts expanded, and a project with several containers starts collapsed; containers that belong to no project are under “Standalone containers”. Each project and container has “Stop” (or “Start”) and “Restart” at the right, and a project’s buttons act on all its containers. Click a container or its “…” to open its details: the “Details” tab has the name, ID, image, creation time, entrypoint, command, ports, mounts, environment and labels, and the “Logs” tab the container’s last 200 lines of output; at the bottom you can start, stop and restart it, and delete it once it’s stopped. The “Volumes”, “Images” and “Networks” tabs list each item’s name and basic information and mark whether a container uses it; those no container uses can be deleted with the delete button at the right (Docker’s own bridge, host and none networks can’t be deleted). Click a card to open its details: for a volume, the driver, mountpoint, creation time, the containers using it, options and labels; for an image, the full ID, tags, digests, creation time, size, platform, number of layers, the containers using it, configuration (entrypoint, command, working directory, user, exposed ports), environment and labels; for a network, the driver, scope, IPv6, subnets and gateways, the containers attached with their addresses, options and labels. Items can be deleted from the bottom of the details too.

Stopping, restarting and deleting ask first; starting happens right away. The result appears as a notification, and the list is read again. When you’re not logged in as root and not in the docker group, passwordless sudo is used. The list is read when you open Docker or switch terminals, and after that only when you click the refresh button. If Docker isn’t installed or isn’t running on the host, it says so.

**Services** lists the systemd services on the remote host (not templates) in two groups: “Custom services” (unit files in `/etc/systemd/system`, the ones an administrator added) and “System services”. The top shows the systemd version and the overall state of the machine (“Degraded” when a service has failed); there are four tabs with counts, “All”, “Active”, “Inactive” and “Failed”, and you can search by service name or description. Each service has a card: a status dot, the name, description, active state and whether it starts at boot. A running service has “Stop” and “Restart” at the right, one that isn’t running has “Start”, and “…” or clicking the card opens its details. The details’ “Status” tab has the load state, active state, at boot, main PID, memory, tasks, restarts, exit status, start and stop times, and unit file; the “Journal” tab has the service’s last 200 lines in the journal. At the bottom of the details and in the context menu you can likewise start, stop and restart it, and enable or disable it at boot.

Stopping and restarting ask first (with a special warning when you stop the SSH service); starting and the at-boot setting happen right away. The result appears as a notification, and the list is read again. These actions need root: when you’re not logged in as root, they run through passwordless sudo, and if sudo needs a password, you’re told that root is needed. The list is read when you open Services or switch terminals, and after that only when you click the refresh button. For hosts that don’t use systemd, it says they’re not supported yet.

**Processes** lists the processes on a remote Linux host (without kernel threads), each on a card: a status dot (green running, gray sleeping, yellow waiting for I/O or stopped, red zombie; hover over it to see the state), the process name, PID, user, start time (in this computer’s time zone), memory use with its share of physical memory, and CPU usage. CPU usage is computed the way `top` does it, per core: two full cores is 200%. It takes two readings to compute, so right after the tool opens it shows “—”, and the figure appears 2 seconds later. Processes are sorted by memory, most first, by default; click “CPU” to sort by CPU instead, and click the same button again to reverse the order. You can search by process name, PID or user, and the number at the right is how many are listed. The list refreshes every 15 seconds, reading only while Processes is showing; click the refresh button at the top right to read it at once.

Click a process to open its details: PID, parent, user, state (with the `ps` state letters in parentheses, such as `Ssl`), start time, elapsed time, TTY, priority, nice, CPU usage, CPU time, resident memory, virtual memory, number of threads, of direct children and of all descendants, and below them the full command line (read when the details open; “Copy command” copies it). At the bottom are “Copy PID”, “End process” and “Kill”.

Right-click a process for “View details”, “End process…” (sends SIGTERM, so the process can clean up before it exits) or “Kill process…” (sends SIGKILL, ending it immediately); ending it from the details works the same way, and both ask first. The result appears as a notification, and the list is read again 1 second later. When you’re not logged in as root, you can end only your own processes. With more than 2500 processes, only the first 2500 are listed, and the top of the list says so.

**Network** lists every TCP and UDP connection and listening port on a remote Linux host, each on a card: protocol, state, local and remote addresses, the process using it with its PID, and its user. The top right of each card says whether it’s a “Listening port”, “Inbound” (connected to a port this host listens on) or “Outbound”. The top of the panel shows the total, the number of listening ports and the number established; you can search by address, port, state, PID, process name or user, and filter by protocol and state. Listening sockets come first, then established ones, with TIME_WAIT and other closing ones last; when several processes listen on the same port together (such as nginx’s workers), they’re merged into one card.

The list is read once when you open Network or switch terminals and doesn’t change by itself after that; click the refresh button at the top right to read it again. The data is read over this terminal’s existing SSH connection, with no second login, using the host’s `ss` command; on hosts without `ss` (such as BusyBox) it’s read from `/proc/net` instead, and then processes don’t show; when you’re not logged in as root, you see only your own processes. In both cases the top of the list says so. With more than 3000 connections, only the first 3000 are listed (listening ports are always among them), and the total stays accurate.

**Monitor** shows, for a remote Linux host:

- **System**: host name, architecture, distribution and uptime (such as “32 days 23:40:26”).
- **CPU**: model, number of cores and average usage; with several cores, a small bar for each core below, and hovering over a bar shows that core’s number and usage. Only one row shows by default; when there are too many cores for one row, click “N cores” at the top right to show them all.
- **Memory**: physical memory usage and the amount used; a second row when there’s swap.
- **Network**: upload and download rates of the main interface (the one with the default route); with several interfaces, click “N interfaces” at the top right to show them all (loopback interfaces and interfaces that have never sent or received data aren’t listed).
- **Disks**: the usage of each local mount point (without in-memory file systems such as tmpfs, or mounts under system folders).

The data is read over this terminal’s existing SSH connection, with no second login and nothing to install; it refreshes every 2 seconds (disks every 30 seconds), reading only while Monitor is showing. Each refresh reads only a few system files, and starts no processes on the server besides the shell that runs it. Usage of 70% or more shows in yellow, 90% or more in red. When the terminal isn’t connected, it says so; when a host whose system wasn’t known yet turns out not to be Linux, it says it isn’t supported yet.

## SFTP

Open a host’s “SFTP” tab to browse local and remote folders. As with terminals, each “Open SFTP” opens a new tab, so one host can have several, each with its own connection, each showing its own folders. It works like WinSCP’s Commander interface. SFTP uses a separate SSH connection that asks only for the SFTP subsystem, so the server doesn’t need to provide a shell; closing the terminal doesn’t stop transfers.

### Toolbars

Each pane has two rows of toolbar, with the path label below them. When a pane is too narrow, buttons hide whole, from right to left, rather than wrapping (their commands are still in the context menus and shortcuts). A new SFTP tab splits evenly between the local and remote sides; wherever you drag the dividers between the sides and the queue, they stay there until the tab closes, even when you switch to another tab and back:

- First row (navigation):
  - The folder list: lists every level from the root to the current folder, plus Home, Desktop, Documents and Downloads on the local side; choose one to go there.
  - Bookmarks: the button opens the “Open folder” dialog (see below); the drop-down arrow next to it lists this host’s bookmarks for this side, which you click to go there, and can also add or remove the current path. Bookmarks are saved in the local database and deleted along with the host.
  - Parent folder, Root folder, Home folder, Refresh, Back, Forward.
  - Show / hide hidden files: files and folders whose names start with `.`. An open eye means they’re shown, a crossed-out eye that they’re hidden. Local and remote each have their own switch, independent of each other; each side’s choice applies to every SFTP tab and is saved in the settings; both are hidden by default. While they’re hidden, the bottom of the pane says “N hidden”; while they’re shown, hidden files’ text is lighter.
- Second row (files):
  - “Download…” on the remote side and “Upload…” on the local side, whose drop-down also has “Choose files to upload…”.
  - Delete, Rename…, Properties…, New (Folder… / File…).

### Path label

Works like WinSCP’s path label:

- It shows the full path of the current folder, such as `/home/tester/`.
- Every level can be clicked: hovering highlights the path from the root to that level, and clicking goes there.
- When the path doesn’t fit, the root and the last few levels stay, and the middle folds into `…`; clicking `…` goes to the last folded level.
- Clicking the current folder, or double-clicking the empty space to the right of the path, opens the “Open folder” dialog.
- Context menu: Go to, Refresh, Add path to bookmarks, Copy path, Open folder or bookmark….

### Open folder or bookmark

Works like WinSCP’s Open directory dialog. Open it from the path label, the bookmarks button, ⌘O (Ctrl+O on Windows and Linux; also ⌘⇧G on macOS) or the path label’s context menu:

- “Folder” is filled in with the current folder, already selected, so typing replaces it. `~` means the home folder, and relative paths start from the current folder. The local side also has “Browse…”.
- Below are this host’s bookmarks for this side. A bookmark that matches the folder is selected; clicking a bookmark puts it in the folder field, and double-clicking opens it directly.
- “Add” adds the folder to the bookmarks, “Remove” (or Delete in the list) removes the selected bookmark and selects the one next to it, and “Up” and “Down” change the order.
- Bookmark changes are saved at once and aren’t undone by “Cancel”, the same as in WinSCP.
- Enter or “Open” goes to the folder; Esc or “Cancel” closes the dialog.

The status bar below the list normally shows the number of items and how many are selected; while connecting, or when reading a folder takes longer than 0.3 seconds, it shows “Connecting to SFTP…” or “Reading folder…”. When a folder can’t be read or the connection drops, the reason appears in red at the far bottom left of the window (where “Connected to” and the host’s name normally are). A dropped connection is noticed even while idle, without waiting for a refresh, as soon as the same host’s terminal would notice. To reconnect, use the “Reconnect” button in the tab bar or the tab menu. While disconnected, any action in the remote list (refreshing, opening a folder, deleting, uploading, downloading and so on) brings up a “Lost the SFTP connection to …” dialog that explains why and can “Reconnect” right away. None of this makes the list move.

The columns are the same as in WinSCP: Name, Size, Type and Modified on the local side; Name, Size, Modified, Permissions and Owner on the remote side.

- Sizes are shown in KB by default (`4,008,960 KB`, rounded up; only empty files are 0 KB), as in WinSCP. Right-click the “Size” column header to switch to “Bytes” (`4,105,175,040 B`), “Kilobytes (KB)” or “Short (B, KB, MB, GB)”, which uses B, KB, MB or GB depending on the size, such as `3.8 GB`; the choice applies to every SFTP tab and is saved in the settings. Sorting by size always uses the exact number of bytes.
- Times look like `2026/4/22 12:44:53`.
- Permissions look like `rwxr-xr-x`.
- The owner comes from the server’s `ls -l` line.
- A symbolic link to a folder shows as a folder with an arrow, and you can double-click it to go in.

### Selection and context menus

- As in WinSCP with “Full row select” turned off, the “Name” cell is the item: on hover only the name cell turns gray; clicks, double-clicks and right-clicks all act on the name cell; and a selected item’s whole name cell is highlighted. The row’s other columns and the area below the list count as empty space.
- Mouse: click a name cell to select it (deselecting everything else), ⌘-click (Ctrl-click on Windows and Linux) to add or remove items, Shift-click to select a range, double-click to open. Clicking empty space deselects everything (unless ⌘ or Shift is held).
- Box selection: press and drag in the empty space to the right of the names, in the size, type, time and other columns, or below the list to draw a selection box; the rows inside it are selected. Start dragging with ⌘ or Shift held to add to the current selection. Dragging to the top or bottom edge of the list scrolls it. Dragging a file name is what drags files to the other side for transfer: pressing on an unselected file name selects only it (deselecting everything else), and only it is dragged; pressing on a selected file name drags everything selected. While dragging, “Upload N items” or “Download N items” appears only over the other side’s list; over your own list or anywhere else it can’t be dropped, the pointer shows that it isn’t allowed.
- Keyboard: ↑↓, Home, End, PageUp and PageDown move, with Shift to extend the selection; Space or Insert toggles the current row; ⌘A selects all; Tab switches to the other side.
- Right-clicking an unselected name cell selects it first; right-clicking empty space clears the selection and opens the empty-space menu.
  - The menu on file names: Edit (for files; images, SVG and Markdown also have Preview, and images other than SVG have only Preview), Upload… / Download…, Copy path (the full paths of the selected items, one per line), Delete, Rename…, Properties…. Double-clicking a folder goes into it, so the menu has no “Open”.
  - The empty-space menu: Go to (Parent folder / Root folder / Home folder / Back / Forward), Refresh, Show hidden files (checked means this side shows them), Add path to bookmarks, New.

### Shortcuts

| Command | Shortcut |
|---|---|
| Upload / Download (depending on the pane) | F5 |
| Rename | F2 |
| New folder | F7 |
| Delete | F8, Delete; also ⌘⌫ on macOS |
| Properties | F9; also ⌘I on macOS |
| Edit file | F4 |
| Refresh | ⌘R / Ctrl+R |
| Parent folder | Backspace; also ⌘↑ on macOS |
| Root folder | ⌘\ / Ctrl+\ |
| Home folder | ⌘⇧H on macOS, Ctrl+H on Windows and Linux |
| Back / Forward | ⌥← / ⌥→; also ⌘[ / ⌘] on macOS |
| Add path to bookmarks | ⌘D / Ctrl+D |
| Open folder / Edit file | Enter; also ⌘↓ on macOS |
| Open folder or bookmark | ⌘O / Ctrl+O; also ⌘⇧G on macOS |
| Show / hide hidden files (depending on the pane) | ⌘⇧. on macOS, Ctrl+Alt+H on Windows and Linux |

### File operations

- Deleting asks first, saying what will be deleted and what happens:
  - Local items move to the Trash (the Recycle Bin on Windows).
  - Remote items are deleted permanently, a folder together with everything in it.
  - Deleting a symbolic link deletes only the link itself.
- Renaming and creating:
  - The name is checked against the current list: it can’t be empty, can’t be `.` or `..`, and can’t be the same as an existing item’s.
  - Renaming never overwrites an existing item.
- Properties:
  - Shows the location, size and modification time, plus the owner and group for remote items.
  - Permissions are edited with a 3×3 grid of checkboxes and an octal number, kept in sync.
  - With several items selected, permission bits that differ between them stay as they are unless you change them.
  - When folders are included, the change can be applied recursively; a recursive change checks “Add execute permission to folders (X)” by default, so the folders can still be opened.
  - Symbolic links are left out of permission changes.

### Editing files

Text files can be edited right in ShellRS, remote and local alike (like WinSCP’s internal editor):

- Opening: double-click a file, select it and press Enter or F4, or right-click it and choose “Edit”. A file made with “New › File…” opens right away too. Files open in an editor tab in the center area, which can be dragged into a split like terminals and SFTP; each file has only one tab, and opening it again switches to that tab.
- The editor has line numbers, find and replace (⌘F / Ctrl+F; replace ⌘⇧F / Ctrl+H), and coloring by file name for Shell, YAML, TOML, JSON, Python, JavaScript, HTML, CSS, Markdown, SQL, PHP, Go, Lua, Makefile and Diff; other files are plain text. In Makefiles and files indented with tabs, the Tab key types a tab character.
- Saving: ⌘S / Ctrl+S or “Save” above the tab. A remote file is written straight back to the original file (in place) over the connection of the SFTP tab that opened it, so its owner, permissions and the links pointing to it stay the same. Before saving, ShellRS checks the file’s size and modification time: if someone else changed or deleted it after you opened it, you’re asked whether to “Overwrite” first, rather than having it overwritten silently. If the connection drops partway through writing, the file on the server may be only partly written; the editor still has your text, so reconnect and save again.
- “Reload” reads the file again from the server or the disk, asking first if there are unsaved changes.
- A dot on the tab means unsaved changes. Closing such a tab asks whether to “Save”, “Discard changes” or “Cancel”; closing several tabs at once, closing an SFTP tab (which also closes the remote files it opened) or quitting ShellRS with unsaved files asks once first. On macOS, quitting from the Dock and logging out or shutting down don’t ask.
- The bottom left of the window shows the SFTP connection the file goes through (“Local file” for local files), and the bottom right the cursor position, the encoding and the line endings (such as “UTF-8 · LF”).
- Limits: only UTF-8 text files of 5 MB or less open (with or without a BOM; the BOM and CRLF line endings are kept on save). Larger files, binary files and files in other encodings such as GBK are refused with the reason; remote ones can be downloaded with “Download…” instead.

### Preview

Images (PNG, JPG, GIF, WebP, SVG, BMP, ICO, TIFF) and Markdown can be previewed, remote and local alike:

- Opening: double-click an image. Markdown and SVG are text, so double-clicking them opens the editor; to preview them, choose “Preview” in the context menu.
- The preview is a dialog that covers most of the window; Esc, the close button at the top right or the “Close” button closes it. Markdown is shown formatted: you can scroll, select and copy, and click links to open them in the browser.
- Images start at “Fit to window”: scaled down in proportion until they fit (small images aren’t enlarged) and centered, with the size in pixels, the file size and the current scale above. The “Zoom in” and “Zoom out” buttons (or ⌘= / ⌘-, Ctrl on Windows and Linux) step through 10%–800%, and scrolling the wheel or trackpad with ⌘ (Ctrl) held zooms smoothly; “Actual size” (⌘0) shows one image pixel per point, and “Fit to window” (⌘9) goes back to the size that fits; double-click the image to switch between the two. When the image is larger than the preview area, you can scroll in every direction, and zooming keeps the point in the middle of the view in place.
- At the bottom, “Download…” downloads a remote file to this computer (opening the download confirmation), and for Markdown, “Edit” opens the editor (or switches to it if it’s already open).
- The preview shows the file on the server or the disk; changes not yet saved in the editor don’t appear in it.
- Limits: images up to 20 MB, Markdown up to 5 MB and in UTF-8; larger files are refused with the reason, and remote ones can be downloaded with “Download…” instead.

### Uploading and downloading

- Ways to start:
  - Upload: “Upload…”, F5 in the local list, “Choose files to upload…”, dragging local items by their file names to the remote list, or dragging files from Finder to the remote list.
  - Download: “Download…”, F5 in the remote list, or dragging remote items by their file names to the local list.
  - Dropping on a folder’s row puts the items in that folder; dropping anywhere else puts them in the current folder.
- Every way shows a confirmation where you can change the destination folder; for downloads, “Browse…” chooses a folder on this computer. The confirmation says how many items of which kinds (such as “Upload 2 files to …”), shows the source folder once, and lists each item on its own line, with the full path on hover.
- Once confirmed, the source, endpoints and destination are fixed. Each SFTP tab runs one batch at a time (uploads and downloads share it), handling the batch’s items one after another; while a transfer runs, you can start more uploads or downloads, and each new batch waits in Transfers and starts automatically when the one before it ends. Other SFTP tabs, hosts and terminals stay usable.
- Recursive transfers include empty folders. Symbolic links are recreated as links, not followed. Transfers are binary by default, and regular files keep their modification time.
- New items get the destination side’s default permissions, and overwriting keeps the destination’s existing basic permissions. Owners, ACLs and extended attributes aren’t copied.
- Folders with the same name are merged. When a file or link conflicts, you’re asked whether to overwrite, skip or cancel, and can apply the answer to the rest of the batch’s conflicts; a folder conflicting with something that isn’t a folder is never deleted automatically.
- On an error, you can retry, skip or cancel, and the summary at the end lists what succeeded, what was skipped and what failed.

### Transfers

Works like WinSCP’s queue panel: it appears below the two panes when there are transfers, its height can be dragged, and it collapses once all finished batches are cleared.

- One row per batch: Direction (upload / download), Source (the item’s path for a single item, the folder they’re in for several), Destination, Transferred (in the same units as the “Size” column), Time (time left), Speed (over the last few seconds) and Progress.
- A batch in progress shows an overall progress bar, with a second line below for the file being transferred and its own progress bar, like the two progress bars in WinSCP’s progress dialog.
- The Progress column shows: Waiting, Scanning, a percentage, Waiting for an answer, a note about reconnecting, Stopped, Done, N items failed, Removed.
- Click the arrow at the start of a row to expand the batch’s results item by item (Done, Skipped, or Failed with the reason), newest first.
- Toolbar: Resume (a stopped batch), Stop (a batch in progress), Remove from queue (the selected waiting or finished batches, or a stopped batch), Discard partial transfer, Clear finished.
- A stopped batch stays at the front of the queue; the batches after it start only once you resume it, discard its partial transfer or remove it from the queue. Removing it from the queue doesn’t delete its resume record, so transferring the same files again still asks whether to resume.
- The queue lasts only for this run and is cleared when the SFTP tab closes; closing the tab with unfinished batches asks first.

## Resuming and replacing

Files are first written to `<destination file name>.filepart`. Write requests go through a bounded pipeline, and the real file is published only after every write has been acknowledged and the file has closed successfully; when the server supports it, `fsync@openssh.com` is called first. Servers that support `posix-rename@openssh.com` replace the file atomically; on other servers, the old file is first moved to `.shellrs-<UUID>.backup` and then the new file is published, a compatibility path that isn’t guaranteed to be atomic. If the reply to a publish is lost, ShellRS recovers using the managed temporary path, the stage, the type and the size; when it can’t tell safely, it keeps the recovery files and reports it.

Resume records are kept in `upload-resume/` in the app’s data folder, as versioned JSON written atomically; they never contain passwords, passphrases or private keys, and they don’t change the database schema. A record ties together the endpoint, host fingerprint, source, destination and safe-replace stage. Resuming works as in WinSCP: ShellRS reads the current size of the remote `.filepart`, skips that much of the local file and carries on; it’s up to you to make sure the source is still the same version. Uploads never read the remote file back to verify it.

Downloads likewise write first to `<destination file name>.filepart` on this computer; when done, ShellRS sets the remote modification time, runs fsync, and renames it to the real file within the same file system (an atomic replace). Concurrent read requests may come back out of order or short, but the temporary file is written strictly in order, because resuming trusts only its length. Download resume records are kept in `download-resume/` and tie together the endpoint, host fingerprint, remote source with its size and modification time, and local destination; if the remote file changes, the download can’t resume and can only start over or be skipped.

On network failures, each batch reconnects automatically up to three times, after 1, 3 and 10 seconds. After that, resume the batch with “Resume” in Transfers; resuming by hand resets the count. Authentication failures, a changed host key and a deliberate cancel need you to deal with them. Reconnecting uses the original connection settings, so editing the host doesn’t change the destination of a transfer in progress.

Cancelling, closing the tab, disconnecting or quitting keeps the resume records; closing a tab with a transfer in progress asks first. After a restart, transfers don’t continue by themselves; choosing the same source and destination again asks whether to resume. While a batch is stopped, “Discard partial transfer” cleans up its managed temporary files; if there’s a recovery backup that can’t be judged safely, the files are kept and the reason is given. Items published successfully have their records and backups cleaned up automatically.

When a file name can’t be represented exactly, ShellRS reports an error. The remote string decoding of the current `russh-sftp 3.0.0` replaces invalid UTF-8, so the browser refuses remote names that contain the replacement character `U+FFFD`, to avoid acting on the wrong path. Symbolic links use OpenSSH’s SFTP v3 argument order.

**Not implemented yet: folder sync in SFTP tabs (the external CLI’s `shellrs sync` can sync), a foreground transfer progress dialog, reordering the queue, filtering, finding files, a folder tree, and keeping the Dock layout.**

## Download and updates

Packages are on [GitHub Releases](https://github.com/since2006/shell-rs/releases):

| System | First install | Automatic updates |
|---|---|---|
| macOS (one package for Apple Silicon and Intel) | DMG; drag ShellRS into Applications | Yes |
| Windows x64 | Setup program, which installs for the current user into `%LOCALAPPDATA%\Programs\ShellRS`, with no administrator rights needed | Yes |
| Linux x64 | AppImage | Yes |

ShellRS checks for a new version 30 seconds after it starts, then every 6 hours. When there is one, it downloads and verifies it in the background, and then a green update button appears at the top right of the title bar. Click it: “What’s new” opens the changelog on the website, and “Restart to update” switches to the new version and reopens. Restarting closes open connections and stops transfers (keeping their progress for resuming) and port forwards; the dialog lists exactly which.

“Updates” in “Settings › About” has just three rows:

- **Current version**: below it, the current state (up to date, downloading, downloaded and so on); at the right, the version number and a button, normally “Check for updates”, which becomes “Restart to update…” once an update is downloaded. The build number and platform are in the version number’s tooltip.
- **Update channel**: Stable is for everyday use; Beta gets test versions early, and stable releases too when they come out. After you switch, ShellRS checks the new channel right away (with automatic updates off, it waits until you check by hand).
- **Automatic updates**: when this is off, ShellRS goes online only when you check by hand, and when it finds a new version, you decide whether to download it.

- **Verification**: the update manifest is signed with ShellRS’s release key, and the package is checked against the size and SHA-256 in the manifest; if any step doesn’t match, nothing is installed. On macOS, the new version’s developer signature is checked as well.
- **Privacy**: checking for updates sends only ShellRS’s version number, the operating system and the CPU architecture, nothing that could identify this computer or you. For anonymous usage statistics, see “Privacy”.
- **When automatic updates aren’t possible**: when ShellRS runs straight from the DMG or outside the Applications folder, you can’t write to where it’s installed, on Linux it isn’t run as the AppImage, or it’s a development build you compiled yourself. The About page then says why, and offers “Open download page” when a new version is out.
- **Going back to an older version**: before the database is upgraded to a new schema, it’s backed up as `shellrs.db.v<old version>.bak`. An older version refuses to open a database with a newer schema, so as not to mess up data a newer version saved; to go back, rename the backup to `shellrs.db`.

## Privacy

ShellRS never sends hosts, credentials, session content, commands, file names or paths, or any identifier that could identify this computer or you. Only these two things go online (besides connecting to your hosts):

- **Checking for updates**: sends only ShellRS’s version number, the operating system and the CPU architecture; see “Download and updates”.
- **Anonymous usage statistics**: sent to [Aptabase](https://aptabase.com) to learn how many installs there are, how many devices use ShellRS each day, how the versions are spread, and which features are used most. “Send anonymous usage statistics” in “Settings › About › Privacy” is on by default; turn it off and nothing more is sent, and the counts collected on this computer are cleared too.

Every statistics event carries ShellRS’s version number and the operating system’s name and version (such as macOS 15.1, Windows 10.0.22631, Ubuntu 24.04). There are only four events:

| Event | When it’s sent |
|---|---|
| `app_installed` | Once, on the first launch. The data folder records that it was reported; a different data folder, or deleting it, reports it again |
| `app_started` | Every launch |
| `app_active` | The first time each day (UTC) the window comes to the front; not again on the day ShellRS started. On macOS, a closed window with ShellRS left in the background doesn’t count |
| `usage` | How many times each feature was used: once 4 hours have passed since the first use, summed up into one event at the next launch, activation or use |

The first three carry these settings and counts, each with a fixed set of possible values: CPU architecture, appearance, interface language, light and dark themes (names of built-in themes), whether the interface matches the theme, whether highlighting, the external CLI and automatic updates are on, the update channel, whether any shortcut was changed, and how many hosts, credentials, port forwards and snippets are saved (only as a band: 0, 1-5, 6-20, 21-100, 100+).

`usage` has only counts, no content. It counts: SSH terminals connected (also counted separately for jump hosts, proxies, credentials, no password, quick connections and external connections), SFTP connected (also counted for quick connections and external connections), local terminals, port forwards started (local, remote, dynamic), upload and download batches, the editor opening local and remote files, previews of images and Markdown, opening each of the seven right sidebar tools, snippets and history commands sent to the terminal, terminal find, notifications (program, bell, keyword), the external CLI’s `exec`, `upload`, `download`, `sync`, `hosts` and `credentials` commands, and turning highlighting and the external CLI on and off.

- Counts are first kept in `analytics.json` in the data folder, so quitting doesn’t lose them, and they’re sent later. Being offline or on an intranet doesn’t get in the way: when sending fails, ShellRS waits 1 minute, then doubles the wait each time up to 6 hours, and retries only while you’re using ShellRS, sending no requests while it’s idle.
- Aptabase infers the country and region from the request’s IP address but doesn’t store the IP. It tells devices apart by a hash of the IP and User-Agent with a random salt that changes every day, so it can’t follow the same device across days.
- Development builds you compile yourself don’t send statistics, and neither does the external CLI (the `shellrs` command) itself.
