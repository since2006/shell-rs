# Changelog

[简体中文](CHANGELOG.zh.md)

One section per version, titled `## [version]`, optionally followed by a date. Each section is written in English here and in Chinese in [CHANGELOG.zh.md](CHANGELOG.zh.md), in the same change. The release workflow (`.github/workflows/release.yml`) takes the section with the tag’s version, as it is, for the GitHub release. The website’s changelog shows this file in English and Japanese and CHANGELOG.zh.md in Chinese (ShellRS’s update dialog opens it with What’s new). Written in Markdown, for users.

## [0.1.19]

### Added

- SSH hosts can fill in one-time codes (TOTP). In the host dialog’s Advanced, turn on Fill in one-time codes and enter the OTP secret: when the server asks for a code during keyboard-interactive login, ShellRS sends the current 6-digit code. With Prompt to match left empty, it recognizes the usual prompts, such as Verification code, OTP and MFA. Terminals, SFTP, port forwarding, the external CLI, Test connection and jump hosts all fill it in. The OTP secret is stored only in the system keychain and syncs with cloud sync.
- Terminal background images: in Appearance, set one for light and one for dark, each with its own on switch and Transparency, and they follow the appearance. The image covers only the terminal; with split panes, each pane gets the whole image. ([#43](https://github.com/since2006/shell-rs/issues/43), [#113](https://github.com/since2006/shell-rs/issues/113))
- A Show host details button right of the search box in the host list: each host shows user@address:port under its name, and its notes on another line if it has any. Click it again for names only. ShellRS remembers the setting. ([#116](https://github.com/since2006/shell-rs/issues/116))

### Improved

- A host that uses a credential can have its own username, so one private key logs in to servers with different usernames. Leave it empty to use the credential’s. ShellRS 0.1.18 and earlier still log these hosts in with the credential’s username. ([#112](https://github.com/since2006/shell-rs/issues/112))
- Highlighting comes with 11 preset rules, from the scheme a user shared in [#113](https://github.com/since2006/shell-rs/issues/113): errors, successes, warnings, IP and MAC addresses, sizes with units, dates and times, numbers and more, in colors that read well on the default light and dark backgrounds. If you hadn’t changed the three old examples, they’re replaced with the new presets; rules you changed stay.
- The left and right sidebars can be dragged narrower (to 180 and 220), and the widths you set stay after restarting. Settings › General › Window › Sidebar widths restores the defaults.

### Fixed

- On macOS, holding down a key in a terminal showed the accent picker and typed the character once; it now repeats. ([#114](https://github.com/since2006/shell-rs/issues/114))
- With cloud sync on a ZSpace NAS’s WebDAV, setting up sync in an empty folder said another device had already turned on sync.

## [0.1.18]

### Added

- A menu bar on macOS, as in other Mac apps: ⌘H hides ShellRS, ⌥⌘H hides other apps and ⌘M minimizes the window. About ShellRS shows the version and a link to the website. ([#88](https://github.com/since2006/shell-rs/issues/88))
- The compose pane remembers the commands you send. Press ↑ and ↓ to go back through them as in a terminal, up to the last 500.
- Snippets are in the right sidebar next to local terminals too. Click one to type it into the local terminal.

### Improved

- Snippet categories nest like host groups, and you can drag snippets and categories to reorder them. The list shows only names; hover over a snippet to see its command. Order and nesting sync with cloud sync; ShellRS 0.1.17 and earlier show every category at the top level, but don’t change the order or nesting elsewhere.
- On Windows and Linux, Close tab now defaults to Ctrl+Shift+W instead of Ctrl+W, as in Windows Terminal, so it works in a terminal too. Ctrl+W still goes to the shell. If you changed this shortcut, yours stays.
- The Follow switch in the right sidebar’s Files is remembered per host: Files opened for that host later, also after restarting ShellRS, start the way you last set it.
- The transfer queue scrolls to a new batch when it’s added.

## [0.1.17]

### Shared core for mobile

This release moves the desktop app's SSH connections, SFTP transfers, host and credential management, database persistence and cloud sync into Rust libraries independent of the interface. The desktop app now uses these shared cores, and Swift and Kotlin bindings let future iOS and Android clients use the same authentication, resumable transfers and end-to-end encrypted sync. The shared cores and mobile bindings are MIT licensed; the desktop app remains GPL-3.0-only.

### Added

- App lock: require a password at startup, lock automatically after inactivity, and choose whether the external CLI remains available while locked. The lock settings and password verifier are kept in the system keychain.
- On Windows, “Minimize to tray on close” under Settings → General → Window keeps connections, transfers and port forwards running after the window is closed. Restore the window or quit from the tray. Off by default. ([#83](https://github.com/since2006/shell-rs/issues/83))
- Choose the terminal cursor's shape and whether it blinks under Settings → Terminal → Interaction.
- “Duplicate session” in a terminal tab's menu carries over its title and, for SSH terminals that report it, its working directory.

### Improved

- The left sidebar remembers whether it was shown or hidden across launches.
- Open saved hosts directly from the title bar's Quick connect button. The sidebar toggle follows the panel's edge when resized, and the panel stays at least as wide as its default.

### Fixed

- On macOS, waking from sleep or switching displays could crash ShellRS while the screen was temporarily unavailable.
- On Windows without the Visual C++ Redistributable, such as Windows 11 LTSC, ShellRS didn’t start: “VCRUNTIME140.dll was not found”.

## [0.1.17-beta.1]

### Fixed

- On Windows without the Visual C++ Redistributable, such as Windows 11 LTSC, ShellRS didn’t start: “VCRUNTIME140.dll was not found”.

## [0.1.16]

### Added

- Cloud sync: sync your data between computers, end-to-end encrypted (E2EE): it’s encrypted before it leaves this computer, so remote storage holds only ciphertext, and only devices with the password can read it.
- When the tabs don’t fit, scroll the tab bar sideways with the mouse wheel.

### Fixed

- In a terminal, Home and End didn’t move to the start and end of the line when zsh on the server was set up with oh-my-zsh and the like.

## [0.1.16-beta.1]

### Added

- Cloud sync: sync your data between computers, end-to-end encrypted (E2EE): it’s encrypted before it leaves this computer, so remote storage holds only ciphertext, and only devices with the password can read it.
- When the tabs don’t fit, scroll the tab bar sideways with the mouse wheel.

## [0.1.15]

### Added

- Compose pane: write and edit commands below the terminal, several lines if you like, before sending them. Open it with ⌘⇧E (Ctrl+Shift+E on Windows and Linux) or from the terminal’s context menu. Command broadcast’s command bar is now the compose pane.
- Middle-click a tab to close it, as × does.

### Improved

- On Windows the interface font size defaults to 14, the same size as the system’s own text.
- Importing hosts no longer lists hosts whose username is git, or the addresses of GitHub, GitLab and other code hosting services, which are for pushing and pulling code, not for logging in.
- With only one tab left, it still looks like a selected tab, as it does among others.

### Fixed

- On Windows, the dialog of “Upload…” showed only folders, so no file could be chosen.
- In the Linux AppImage, commands such as cargo and rustc failed with “unknown proxy name” in a local terminal running zsh.
- When the computer was busy, files dropped on a remote terminal were sometimes asked for in a dialog instead of going up at once.
- In a terminal, typing with an input method after moving the cursor back into text already typed put the composition on top of the text after it, so neither could be read.
- The borders around the buttons at either end of the tab bar didn’t line up with the tab bar’s bottom line.
- The bottoms of letters such as g and p were cut off in tab names.

## [0.1.15-beta.2]

### Improved

- With only one tab left, it still looks like a selected tab, as it does among others.

### Fixed

- In a terminal, typing with an input method after moving the cursor back into text already typed put the composition on top of the text after it, so neither could be read.

## [0.1.15-beta.1]

### Added

- Compose pane: write and edit commands below the terminal, several lines if you like, before sending them. Open it with ⌘⇧E (Ctrl+Shift+E on Windows and Linux) or from the terminal’s context menu. Command broadcast’s command bar is now the compose pane.

### Improved

- On Windows the interface font size defaults to 14, the same size as the system’s own text.
- Importing hosts no longer lists hosts whose username is git, or the addresses of GitHub, GitLab and other code hosting services, which are for pushing and pulling code, not for logging in.

### Fixed

- On Windows, the dialog of “Upload…” showed only folders, so no file could be chosen.
- The borders around the buttons at either end of the tab bar didn’t line up with the tab bar’s bottom line.
- The bottoms of letters such as g and p were cut off in tab names.

## [0.1.14]

### Added

- Projects in the right sidebar beside local terminals: keep local terminals in groups like hosts, and after reopening ShellRS carry on in the folders they were in.
- Terminals in Projects keep their last 1,000 lines of output when closed or when ShellRS quits, and show them when opened again; you can turn this off in Settings.
- Shift+Enter takes a new line in programs that support it, such as Claude Code, in local and remote terminals.
- Maximize window ⌃⌘↑ and Restore window ⌃⌘↓ shortcuts (macOS).
- A system-wide Show or hide ShellRS shortcut that works from any app; it has no keys until you set them in Settings › Keyboard shortcuts.

### Improved

- The right sidebar remembers for each host whether it’s open and which tool it shows: open Files on a host, and it opens by itself whenever you connect to that host.
- In a terminal on Windows and Linux, Ctrl with a letter (such as tmux’s Ctrl+B) goes to the shell instead of triggering a ShellRS shortcut.
- The mouse pointer is an I-beam over a terminal’s text.
- A local terminal tab with a long title shows just the folder’s name.
- A long tab name ends in “…” instead of making the tab very wide.
- When importing hosts, hovering over a name, address or other value cut short shows all of it.

### Fixed

- Box drawing such as the lines between tmux panes came apart into dashes in terminals.
- On small screens, such as a laptop at 150% scaling, the window ran off the screen the first time it opened.
- On Linux, GNOME and other desktops showed a generic app icon in the dock instead of ShellRS’s.
- When importing hosts, a long authentication name covered the Status column.

## [0.1.14-beta.4]

### Added

- Projects in the right sidebar beside local terminals: keep local terminals in groups like hosts, and after reopening ShellRS carry on in the folders they were in.
- Terminals in Projects keep their last 1,000 lines of output when closed or when ShellRS quits, and show them when opened again; you can turn this off in Settings.

## [0.1.14-beta.3]

### Improved

- The right sidebar remembers for each host whether it’s open and which tool it shows: open Files on a host, and it opens by itself whenever you connect to that host.
- The mouse pointer is an I-beam over a terminal’s text.

## [0.1.14-beta.2]

### Added

- Maximize window ⌃⌘↑ and Restore window ⌃⌘↓ shortcuts (macOS).
- A system-wide Show or hide ShellRS shortcut that works from any app; it has no keys until you set them in Settings › Keyboard shortcuts.

### Improved

- In a terminal on Windows and Linux, Ctrl with a letter (such as tmux’s Ctrl+B) goes to the shell instead of triggering a ShellRS shortcut.

## [0.1.14-beta.1]

### Added

- Shift+Enter takes a new line in programs that support it, such as Claude Code, in local and remote terminals.

### Improved

- A local terminal tab with a long title shows just the folder’s name.
- A long tab name ends in “…” instead of making the tab very wide.

## [0.1.13]

### Added

- Import hosts from ~/.ssh/config and ~/.ssh/known_hosts: open it from the host list’s “…” menu or the context menu of its empty space, preview the hosts, tick the ones to import, and change each one’s name, group, username and authentication.
- Broadcast input: click the megaphone at the left of the tab bar and check a few terminals; they’re shown side by side, and the command bar above them sends a command to all of them. Turn on Sync keys to have typing in one go to the others too.
- Windows portable version: unzip and run, with all its data in the data folder next to the program; it updates itself too.

### Improved

- After a remote terminal disconnects, Space closes the tab; Enter still reconnects.
- Less processor use in virtual machines without a graphics card (where the processor draws the interface): fewer frames while a terminal scrolls, a cursor that doesn’t blink, and no animations.
- In the Chinese interface, What’s new opens the changelog in Chinese.

### Fixed

- Terminal text looked blurry at display scaling such as 150%.
- A terminal shown in a split popped up a reminder when its bell rang.

## [0.1.13-beta.3]

### Added

- Broadcast input: click the megaphone at the left of the tab bar and check a few terminals; they’re shown side by side, and the command bar above them sends a command to all of them. Turn on Sync keys to have typing in one go to the others too.

### Fixed

- A terminal shown in a split no longer pops up a reminder when its bell rings.

## [0.1.13-beta.2]

### Improved

- Less processor use in virtual machines without a graphics card (where the processor draws the interface): fewer frames while a terminal scrolls, a cursor that doesn’t blink, and no animations.

## [0.1.13-beta.1]

### Added

- Windows portable version: unzip and run, with all its data in the data folder next to the program; it updates itself too.

### Improved

- In the Chinese interface, What’s new opens the changelog in Chinese.

## [0.1.12]

### Added

- ZMODEM in the terminal: run rz or sz on the remote end to upload or download files, or drop files on a remote terminal to upload them.
- Telnet hosts, with automatic login, jump hosts and proxies.
- Serial hosts: connect to a serial port of this computer, with Test connection and Send break.
- Kubernetes in the right sidebar: browse cluster resources and container logs, edit YAML, restart and delete. Off by default; turn it on in Settings › Terminal › Sidebar tools.
- tmux in the right sidebar: list, create, attach, rename and end tmux sessions.
- GPU/NPU in the right sidebar: utilization, memory, temperature, power and compute processes of NVIDIA and Huawei Ascend cards. Off by default; turn it on in Settings › Terminal › Sidebar tools.
- Settings › Terminal › Sidebar tools: choose which tools the right sidebar shows.
- Interface font size in Settings › Appearance; outside the terminal, ⌘= / ⌘- change it.
- Select all, Copy IP and Copy ID in the terminal’s context menu.

### Improved

- A host’s protocol can be changed when editing it.
- Editing or deleting a host no longer affects tabs already open; new tabs use the new settings.
- Host icons are always in color; a tab that is disconnected or connecting shows its icon and name in gray instead of a status dot.
- The status bar shows the current tab’s connection.
- SFTP transfers several files at once, so many small files upload and download faster; set how many in Settings › SFTP.
- System names, CPU models and the like that don’t fit in Monitor wrap instead of being cut off.

### Fixed

- A bastion host’s ssh:// link with a / in the username connected to the wrong address.
- When a jump host couldn’t reach the target host, the error read as if the jump host itself had timed out.
- Dragging a selection past the top or bottom of the terminal didn’t scroll, so text off screen couldn’t be selected.
- After running programs such as ollama, 11;rgb:… appeared after the prompt.
- The Linux version didn’t open on older systems such as Ubuntu 20.04; glibc 2.31 or later is now enough.

## [0.1.11-beta.2]

### Added

- Telnet hosts, with automatic login, jump hosts and proxies.
- Serial hosts: connect to a serial port of this computer, with Test connection and Send break.
- Kubernetes in the right sidebar: browse cluster resources and container logs, edit YAML, restart and delete. Off by default; turn it on in Settings › Terminal › Sidebar tools.
- tmux in the right sidebar: list, create, attach, rename and end tmux sessions.

### Improved

- A host’s protocol can be changed when editing it.
- Editing or deleting a host no longer affects tabs already open; new tabs use the new settings.
- Host icons are always in color; a tab that is disconnected or connecting shows its icon and name in gray instead of a status dot.
- The status bar shows the current tab’s connection.

## [0.1.11-beta.1]

### Added

- GPU/NPU in the right sidebar: utilization, memory, temperature, power and compute processes of NVIDIA and Huawei Ascend cards, laid out for 8-card machines. Off by default; turn it on in Settings › Terminal › Sidebar tools.
- Settings › Terminal › Sidebar tools: choose which tools the right sidebar shows.
- Interface font size in Settings › Appearance; outside the terminal, ⌘= / ⌘- change it.
- Select all, Copy IP and Copy ID in the terminal’s context menu.

### Improved

- SFTP transfers several files at once, so many small files upload and download faster; set how many in Settings › SFTP.
- System names, CPU models and the like that don’t fit in Monitor wrap instead of being cut off.

### Fixed

- A bastion host’s ssh:// link with a / in the username connected to the wrong address.
- Dragging a selection past the top or bottom of the terminal didn’t scroll, so text off screen couldn’t be selected.
- After running programs such as ollama, 11;rgb:… appeared after the prompt.
- The Linux version didn’t open on older systems such as Ubuntu 20.04; glibc 2.31 or later is now enough.

## [0.1.10]

### Added

- Files in the right sidebar: browse, upload, download and edit remote files beside the terminal, over the terminal’s own connection, without logging in again.
- Files follows the terminal’s current folder; bash and zsh can be set up for it in one click.
- Find files on the remote side of SFTP.
- Color icons for SFTP file lists (Settings › SFTP › Icon style).
- Tabs show their connection: a yellow dot while connecting, a red one when disconnected. Press Enter in a disconnected remote terminal to reconnect.

### Improved

- When a server’s host key changes, you can check the fingerprint and choose Update and connect, without editing known_hosts by hand.
- A host’s name can be left empty; its address is used instead.

### Fixed

- On Windows, some bastion hosts set to use ShellRS as their Xshell or WinSCP client said “Please use the official client”.
- Tabs connected to Windows hosts didn’t show the Windows icon.

## [0.1.10-beta.2]

### Improved

- When a server’s host key changes, you can check the fingerprint and choose Update and connect, without editing known_hosts by hand.

### Fixed

- On Windows, some bastion hosts set to use ShellRS as their Xshell or WinSCP client said “Please use the official client”.

## [0.1.10-beta.1]

### Improved

- When a server’s host key changes, you can check the fingerprint and choose Update and connect, without editing known_hosts by hand.

### Fixed

- On Windows, some bastion hosts set to use ShellRS as their Xshell client said “Please use the official client”.

## [0.1.9]

### Added

- Local terminal in Settings › Terminal: choose the shell local terminals run, with its arguments; the list offers the common shells installed on this computer.
- Custom shortcuts can use Shift with Insert, Delete and similar keys, such as Shift+Insert to paste.

### Improved

- On Windows, local terminals run PowerShell by default, and their tabs show only the program’s name.
- The Windows installer is in Chinese on Chinese systems.
- The app icon is the same size as other apps’ icons on macOS.

## [0.1.9-beta.1]

### Added

- Local terminal in Settings › Terminal: choose the shell local terminals run, with its arguments; the list offers the common shells installed on this computer.
- Custom shortcuts can use Shift with Insert, Delete and similar keys, such as Shift+Insert to paste.

### Improved

- On Windows, local terminals run PowerShell by default.
- The Windows installer is in Chinese on Chinese systems.
- The app icon is the same size as other apps’ icons on macOS.

## [0.1.8]

### Added

- Interaction in Settings › Terminal: copy on select, and make right-click paste, show a menu or do nothing.

### Improved

- macOS packages come separately for Apple silicon and Intel, so downloads are smaller.

### Fixed

- On ESXi and similar hosts, Test connection said keyboard-interactive authentication was needed, and saved passwords didn’t work.
- Restart to update said a file couldn’t be found when updates were checked for soon after starting.

## [0.1.8-beta.1]

### Added

- Interaction in Settings › Terminal: copy on select, and make right-click paste, show a menu or do nothing.

### Improved

- macOS packages come separately for Apple silicon and Intel, so downloads are smaller.

### Fixed

- On ESXi and similar hosts, Test connection said keyboard-interactive authentication was needed, and saved passwords didn’t work.

## [0.1.7]

### Added

- On Windows, the SFTP local folder list shows every drive, so you can switch to another one.

### Improved

- Click a column title in SFTP file lists to sort by it.

### Fixed

- Uploads through some bastion hosts failed, and saving in the editor dropped the SFTP connection.
- Columns widened by dragging in SFTP went back to their old width after changing folders.

## [0.1.7-beta.2]

### Added

- On Windows, the SFTP local folder list shows every drive, so you can switch to another one.

### Improved

- Click a column title in SFTP file lists to sort by it.

### Fixed

- Columns widened by dragging in SFTP went back to their old width after changing folders.

## [0.1.7-beta.1]

### Fixed

- Uploads through some bastion hosts failed, and saving in the editor dropped the SFTP connection.

## [0.1.6]

### Added

- `shellrs exec --terminal` in the external CLI: runs a command in a terminal already open in ShellRS, for bastion hosts that allow logging in only once.
- A scrollbar on the right of the terminal, to drag through earlier output.

## [0.1.6-beta.1]

### Added

- `shellrs exec --terminal` in the external CLI: runs a command in a terminal already open in ShellRS, for bastion hosts that allow logging in only once.

## [0.1.5]

### Added

- An English interface: it follows the system language by default, or choose one in Settings › Appearance › Language; changes apply at once.
- An English user manual; the README shows the English version by default.
- Anonymous usage statistics, which can be turned off in Settings › About › Privacy.

### Improved

- The default dark appearance is a softer charcoal gray.
- Hovered and selected rows in lists are easier to tell apart.
- With split panes, the right sidebar follows the remote terminal you click into, and its title shows the host’s name.
- Docker tabs that don’t fit scroll sideways instead of being cut off.

### Fixed

- The right sidebar closes once a tab dragged into another group covers the terminal.

## [0.1.4]

### Added

- Terminal themes: 10 color schemes each for the light and dark appearance, and the interface can take the theme’s colors too.
- Keyword highlighting in the terminal: color the text that matches your rules, and get a notification when one matches if you like.
- Keyboard shortcuts in Settings: view, rebind, disable or restore shortcuts, with new shortcuts to switch tabs.
- In the terminal, ⌘= / ⌘- / ⌘0 (Ctrl on other platforms) change the terminal font size; outside it, they zoom the interface.
- Settings › General: remember the window’s size and position.
- Settings › About: a link to the website.
- The external CLI can list, create, change and delete hosts and credentials (`shellrs hosts`, `shellrs credentials`), and sync folders with `shellrs sync`.

### Improved

- Searching the host list also finds group names.
- Settings stays on its category when you switch to another tab and back.
- The terminal font size ranges from 10 to 28.
- The external CLI lists hosts with `shellrs hosts list`.

### Fixed

- The terminal no longer freezes when a remote program stops halfway through a frame.

## [0.1.3]

### Added

- A built-in editor for SFTP: double-click a text file to edit it in an editor tab, and ⌘S saves it straight back to the server, with syntax highlighting for common languages.
- SFTP preview: double-click an image for a quick look, and zoom in, zoom out or see it at actual size; right-click Markdown and SVG files and choose Preview.
- Show hidden files in SFTP, separately for the local and remote sides.
- Copy path in the SFTP context menu.
- Web addresses in the terminal show as links; ⌘-click (Ctrl-click on other platforms) opens them in the browser.
- Mouse support in the terminal: click and scroll in vim, htop and tmux.
- What remote programs (such as tmux and vim) copy goes straight to this computer’s clipboard.
- Terminal notifications: when a program asks for a notification or rings the bell while ShellRS isn’t in front, you get a system notification.
- `shellrs exec --json` in the external CLI: no more quoting trouble or garbled text in Windows PowerShell.

### Improved

- The context menu of SFTP folders no longer has a needless Open; double-click to go in.

## [0.1.2]

### Added

- Quick connect: Quick connect… at the top right of the title bar connects to a host without saving it, keeping the password in memory only (to add a host, use + in the host list or ⌘N).
- Find a host on the start page: search the saved hosts, then press Enter or click to connect.

### Improved

- Test connection no longer needs a password; without one, it tries the way No password does.

## [0.1.1]

### Improved

- A shorter skill description for AI agents.

## [0.1.0]

The first public release of ShellRS.

### Added

- Hosts and groups: saved, with groups nested to any depth, search, and dragging to arrange them.
- Credentials: passwords, keys (a file on this computer, pasted or newly generated) and the SSH agent; several hosts can share one credential.
- Connections: direct, through jump hosts, or through an HTTP / SOCKS5 proxy.
- Terminals: local terminals and multiple SSH terminals, with find, clear and latency.
- The right sidebar: Monitor, Network, Processes, Services, Docker, History and Snippets for the current remote terminal.
- SFTP: a WinSCP-style dual-pane file browser, with resumable uploads and downloads and a transfer queue.
- Port forwarding: local, remote and dynamic, reconnecting after a drop, and able to start with ShellRS.
- Opening from bastion hosts: open a terminal or SFTP with `ssh://` and `sftp://` links, as with Xshell and WinSCP, without saving the connection.
- The external CLI: the `shellrs` command lets AI agents run commands and transfer files with the saved hosts.
- Online updates: new versions are checked for and downloaded in the background, announced at the top right of the title bar, and installed on restart or quit.

## [0.1.0-beta.9]

### Added

- Bastion hosts can open ShellRS’s SFTP with an `sftp://` link, as they open WinSCP.
- When ShellRS is opened from a bastion host, the host sidebar on the left collapses and the terminal or SFTP fills the window.

## [0.1.0-beta.8]

### Fixed

- Connections opened from bastion hosts closed right after logging in.

## [0.1.0-beta.7]

### Fixed

- Some bastion hosts couldn’t be reached: SSH servers such as Apache SSHD that have no curve25519 and offer only 2048-bit DH groups used to drop the connection during the handshake. Key exchange now matches OpenSSH’s defaults, with ECDH (NIST curves), and group exchange accepts 2048-bit groups: any server OpenSSH reaches with its defaults, ShellRS reaches too.
- A failed connection says why, such as connection refused, timed out, or no algorithm in common with the server (listing the server’s), instead of only “Couldn’t establish an SSH connection. Check the address, port and host key”.
- Docker: an image with several tags shows one row per tag; details and deletion apply to that row’s tag, and deleting removes only that tag. An image with a repository name but no tag shows as “repository:<none>” instead of being taken for “repository:latest”. Images with a registry address (such as `registry.example.com/app:1.0`) can now be deleted and inspected too.

## [0.1.0-beta.6]

### Added

- Opening from bastion hosts: bastion hosts such as JumpServer can call ShellRS as they call Xshell (`ShellRS ssh://user@address:port`, or `-url ssh://… -newtab name`) to open a terminal tab to the host. It’s a temporary connection: it isn’t saved to the host list and goes away when its tabs close, and a password in the link stays in memory only. When ShellRS is already running, the link opens there instead of in another copy. The program to enter in the bastion host is `shellrs.exe` in the installation folder on Windows (by default `%LOCALAPPDATA%\Programs\ShellRS\shellrs.exe`), and `/Applications/ShellRS.app/Contents/MacOS/shellrs` on macOS.
- External CLI: `shellrs list` also lists temporary connections with tabs open (marked as temporary), so agents can run commands and transfer files with them.
- External CLI: Agent Skills can be installed to WorkBuddy.

### Improved

- Once a new version has downloaded, the update button at the top right of the title bar turns green to stand out. The update dialog no longer lists the release notes: What’s new opens the changelog on the website, next to Restart to update.
- Docker: compose projects with several containers start collapsed; a project with one container, running, starts expanded.

## [0.1.0-beta.5]

### Added

- A right sidebar for terminals: with an SSH terminal in front, a column of tool buttons runs down the right edge of the window. Click one to open it on the right; ⌘⌥B (Ctrl+Alt+B on other platforms) shows or hides it. The tools run over the terminal’s own SSH connection, without logging in again:
  - **Snippets**: save common commands by category, and type one into the terminal or run it with a click.
  - **History**: the host’s bash history, searchable; type a command into the terminal or run it with a click.
  - **Docker**: containers grouped by compose project, to start, stop, restart and view logs; details and deletion for volumes, images and networks.
  - **Services**: systemd services’ status and logs, to start, stop, restart and enable at boot.
  - **Processes**: the memory and CPU use of processes, with details and ending a process.
  - **Network**: TCP and UDP connections and listening ports, with the processes they belong to.
  - **Monitor**: live CPU, memory, network and disk.

  Services, Processes, Network and Monitor support Linux hosts only.
- Copy IP address in the host context menu (Copy hostname when the address is a domain name).

### Improved

- Hints in the port forwarding and credential lists show on the right of the row, as in the host list.
- The SFTP selection rectangle has the same color as selected files.
- The app icon has more margin.

### Note

- The first start upgrades the local database (adding snippets), backing it up first as `shellrs.db.v13.bak`. After the upgrade, 0.1.0-beta.4 and earlier can no longer open the database.

## [0.1.0-beta.4]

### Fixed

- Windows: paths in the SFTP local panel no longer show as `\\?\C:\Users` but as the usual `C:\Users`, and so does Source in the transfer queue. Local bookmarks added earlier with `\\?\` can be deleted and added again; uploads stopped halfway before the update can’t be resumed and have to start over.

## [0.1.0-beta.3]

### Improved

- macOS: the close button at the top left of the window now hides ShellRS in the background, with open connections, SFTP transfers and port forwarding running as before; click the Dock icon to get the window back with all its tabs. To quit, press ⌘Q or right-click the Dock icon and choose Quit. Before, after the window was closed, ShellRS stayed in the Dock but its window couldn’t be opened.
- The start page no longer repeats a hint when there are no recent connections.

## [0.1.0-beta.2]

The first test release of ShellRS, on the Beta channel only, to try out installation and online updates. Later test releases arrive by online update.

### Added

- Hosts and groups: saved, with groups nested to any depth, search, and dragging to arrange them.
- Credentials: passwords, keys (a file on this computer, pasted or newly generated) and the SSH agent; several hosts can share one credential.
- Connections: direct, through jump hosts, or through an HTTP / SOCKS5 proxy.
- Terminals: local terminals and multiple SSH terminals, with find, clear and latency.
- SFTP: a WinSCP-style dual-pane file browser, with resumable uploads and downloads and a transfer queue.
- Port forwarding: local, remote and dynamic, reconnecting after a drop, and able to start with ShellRS.
- The external CLI: the `shellrs` command lets AI agents run commands and transfer files with the saved hosts.
- Online updates: new versions are checked for and downloaded in the background, announced at the top right of the title bar, and installed on restart or quit.
