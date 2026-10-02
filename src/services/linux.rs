//! Reading and running a Linux host's systemd services: the commands, and
//! the parsers for what they print.

use std::collections::HashMap;

use super::model::{Service, ServiceCommand, ServiceStatus, ServiceTable};

/// What the list asks `systemctl show` for each service.
const LIST_PROPERTIES: [&str; 6] = [
    "Id",
    "Description",
    "ActiveState",
    "UnitFileState",
    "FragmentPath",
    "LoadState",
];

/// What a service's details ask for.
const STATUS_PROPERTIES: [&str; 14] = [
    "Id",
    "Description",
    "LoadState",
    "ActiveState",
    "SubState",
    "UnitFileState",
    "FragmentPath",
    "MainPID",
    "MemoryCurrent",
    "TasksCurrent",
    "NRestarts",
    "ExecMainStatus",
    "ActiveEnterTimestamp",
    "InactiveEnterTimestamp",
];

/// `-p Id -p Description …`: one property a flag, which systemd before 230
/// needs (later ones take them comma-separated too).
fn properties(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("-p {name}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What 系统服务 runs on the host for the list: systemd's version and how
/// the machine is doing, then every service's state in one `systemctl
/// show`: those with a unit file (but templates, which run only as
/// instances) and those loaded without one (instances, and some LSB
/// scripts). Each part under an `@@` line of its own.
///
/// One line, in single quotes for `sh -c`, so that whatever the login shell
/// is (bash, zsh, fish, csh) it hands the script to `sh` untouched: the
/// script has no single quote, no `!` for csh to expand, and no two
/// backslashes in a row for fish to make one. The `case` patterns open
/// with `(`: inside `$(…)`, bash 3.2 takes a pattern's bare `)` for the
/// end of the substitution.
pub fn command() -> String {
    format!(
        "sh -c 'export LC_ALL=C PATH=$PATH:/usr/sbin:/sbin; \
         if test -d /run/systemd/system; then \
         echo @@version; systemctl --version 2>/dev/null; \
         echo @@state; systemctl is-system-running 2>/dev/null; \
         names=$({{ systemctl list-unit-files --type=service --no-legend --no-pager 2>/dev/null; \
         systemctl list-units --type=service --all --no-legend --no-pager --plain 2>/dev/null; }} \
         | while read -r name rest; do case $name in (*@.service) ;; (*.service) printf \"%s\\n\" \"$name\";; esac; done); \
         echo @@units; systemctl show {properties} -- $names 2>/dev/null; \
         else echo @@unsupported; uname -s; fi'",
        properties = properties(&LIST_PROPERTIES),
    )
}

/// Whether `name` is a service's name and nothing else, so that it can go
/// into a command: systemd allows letters, digits and `:_.@-`, and `\` for
/// its escapes (「\x2d」).
pub fn valid_name(name: &str) -> bool {
    name.ends_with(".service")
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ":_.@-\\".contains(character))
        && !name.contains("\\\\")
}

/// `script` as root: as it is when the login is root, else through `sudo`
/// without a password, which fails at once where one is needed.
fn as_root(script: &str) -> String {
    format!("if test \"$(id -u)\" = 0; then {script}; else sudo -n {script}; fi")
}

/// What runs `command` on the service `name`, saying how it went in an
/// `@@status` line with `systemctl`'s complaint before it. `None` for a
/// name that is not one.
pub fn control_command(name: &str, command: ServiceCommand) -> Option<String> {
    valid_name(name).then(|| {
        let script = format!("systemctl {} -- \"{name}\"", command.verb());
        format!("sh -c '{} 2>&1; echo @@status $?'", as_root(&script))
    })
}

/// How a command went, from what `control_command` printed: why not, when
/// it did not.
pub fn controlled(output: &str) -> Result<(), String> {
    let (said, status) = output
        .rsplit_once("@@status")
        .ok_or_else(|| "主机没有回答".to_string())?;
    if status.trim() == "0" {
        return Ok(());
    }
    let said = said.trim();
    Err(
        if said.contains("password is required")
            || said.contains("Interactive authentication required")
            || said.contains("Access denied")
            || said.contains("not in the sudoers")
        {
            "需要 root 权限：以 root 登录，或给这个用户配置免密码的 sudo".into()
        } else if said.contains("command not found") && said.contains("sudo") {
            "需要 root 权限：这台主机没有 sudo，请以 root 登录".into()
        } else if said.is_empty() {
            format!("systemctl 返回 {}", status.trim())
        } else {
            said.lines().next().unwrap_or(said).trim().to_owned()
        },
    )
}

/// What reads a service's state in full, for its details.
pub fn status_command(name: &str) -> Option<String> {
    valid_name(name).then(|| {
        format!(
            "sh -c 'export LC_ALL=C; systemctl show {} -- \"{name}\"'",
            properties(&STATUS_PROPERTIES)
        )
    })
}

/// The service's last 200 lines of journal, the newest last. The journal
/// is root's to read, besides some groups', so a login that is not root
/// asks `sudo` first and settles for what it may see.
pub fn log_command(name: &str) -> Option<String> {
    valid_name(name).then(|| {
        let journal = format!("journalctl -u \"{name}\" -n 200 --no-pager");
        format!(
            "sh -c 'export LC_ALL=C; if test \"$(id -u)\" = 0; then {journal}; \
             else sudo -n {journal} 2>/dev/null || {journal}; fi 2>&1'"
        )
    })
}

/// What the list's command printed.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Table(ServiceTable),
    /// Not a host whose services systemd runs. Holds `uname -s` when the
    /// host has one: 「Linux」 without systemd, 「Darwin」.
    Unsupported(Option<String>),
}

pub fn parse(output: &str) -> Parsed {
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current = None;
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("@@") {
            current = Some(name.trim());
            sections.entry(name.trim()).or_default();
        } else if let Some(name) = current {
            sections.entry(name).or_default().push(line);
        }
    }
    let section = |name: &str| sections.get(name).map(Vec::as_slice).unwrap_or_default();
    let first = |name: &str| {
        section(name)
            .iter()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
    };
    if !sections.contains_key("units") {
        // A shell other than sh's (cmd.exe) prints nothing we know.
        return Parsed::Unsupported(first("unsupported").map(str::to_owned));
    }
    // 「systemd 249 (249.11-0ubuntu3.12)」.
    let version = first("version").map(|line| {
        line.split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
    });
    let services = blocks(section("units"))
        .into_iter()
        .filter_map(|properties| {
            let property = |name: &str| properties.get(name).copied().unwrap_or_default();
            let name = property("Id");
            // A unit no file is found for is only worth a line when it
            // failed.
            let missing = property("LoadState") == "not-found";
            (name.ends_with(".service") && (!missing || property("ActiveState") == "failed")).then(
                || {
                    Service::new(
                        name,
                        property("Description"),
                        property("ActiveState"),
                        property("UnitFileState"),
                        property("FragmentPath"),
                    )
                },
            )
        })
        .collect();
    Parsed::Table(ServiceTable::new(
        services,
        version,
        first("state").map(str::to_owned),
    ))
}

/// `systemctl show`'s `Key=value` lines, a blank line between units.
fn blocks<'a>(lines: &[&'a str]) -> Vec<HashMap<&'a str, &'a str>> {
    let mut blocks = vec![HashMap::new()];
    for line in lines {
        match line.split_once('=') {
            Some((key, value)) => {
                blocks
                    .last_mut()
                    .expect("one at least")
                    .insert(key.trim(), value.trim());
            }
            None if line.trim().is_empty() => blocks.push(HashMap::new()),
            None => {}
        }
    }
    blocks.retain(|block| !block.is_empty());
    blocks
}

/// A service's details from what `status_command` printed.
pub fn parse_status(output: &str) -> Option<ServiceStatus> {
    let lines: Vec<&str> = output.lines().collect();
    let properties = blocks(&lines).into_iter().next()?;
    let property = |name: &str| {
        properties
            .get(name)
            .copied()
            .filter(|value| !value.is_empty() && *value != "[not set]")
    };
    // systemd writes an unset number as the largest one it has.
    let number = |name: &str| {
        property(name)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value != u64::MAX)
    };
    Some(ServiceStatus {
        description: property("Description").unwrap_or_default().to_owned(),
        load: property("LoadState").unwrap_or_default().to_owned(),
        active: property("ActiveState").unwrap_or_default().to_owned(),
        sub: property("SubState").unwrap_or_default().to_owned(),
        file_state: property("UnitFileState").unwrap_or_default().to_owned(),
        main_pid: number("MainPID")
            .filter(|pid| *pid != 0)
            .map(|pid| pid as u32),
        memory: number("MemoryCurrent"),
        tasks: number("TasksCurrent"),
        restarts: number("NRestarts"),
        exit_status: property("ExecMainStatus").and_then(|value| value.parse().ok()),
        started: property("ActiveEnterTimestamp").map(str::to_owned),
        stopped: property("InactiveEnterTimestamp").map(str::to_owned),
        path: property("FragmentPath").map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::model::{ActiveState, ServiceFilter};

    const LIST: &str = "\
@@version
systemd 249 (249.11-0ubuntu3.12)
+PAM +AUDIT +SELINUX
@@state
degraded
@@units
Id=socialc.service
Description=Social Callback Proxy Service
ActiveState=active
UnitFileState=enabled
FragmentPath=/etc/systemd/system/socialc.service
LoadState=loaded

Id=apport.service
Description=LSB: automatic crash report generation
ActiveState=active
UnitFileState=generated
FragmentPath=/run/systemd/generator.late/apport.service
LoadState=loaded

Id=auditd.service
Description=auditd.service
ActiveState=inactive
UnitFileState=
FragmentPath=
LoadState=not-found

Id=certbot.service
Description=Certbot
ActiveState=failed
UnitFileState=static
FragmentPath=/lib/systemd/system/certbot.service
LoadState=loaded
";

    #[test]
    fn systemctl_show_gives_the_services_but_those_without_a_file() {
        let Parsed::Table(table) = parse(LIST) else {
            panic!("not a table");
        };
        assert_eq!(table.summary(), "systemd 249 · 降级运行");
        let services: Vec<(&str, ActiveState, &str, bool)> = table
            .services()
            .iter()
            .map(|service| {
                (
                    service.name.as_str(),
                    service.active,
                    service.file_state.as_str(),
                    service.custom,
                )
            })
            .collect();
        assert_eq!(
            services,
            [
                ("apport.service", ActiveState::Active, "generated", false),
                ("certbot.service", ActiveState::Failed, "static", false),
                ("socialc.service", ActiveState::Active, "enabled", true),
            ]
        );
        assert_eq!(table.counts(""), [3, 2, 0, 1]);
        assert_eq!(table.rows(ServiceFilter::All, "").len(), 5);
    }

    #[test]
    fn a_host_without_systemd_says_what_it_is() {
        assert_eq!(
            parse("@@unsupported\nLinux\n"),
            Parsed::Unsupported(Some("Linux".into()))
        );
        assert_eq!(parse(""), Parsed::Unsupported(None));
    }

    #[test]
    fn details_leave_out_what_systemd_has_not_set() {
        let status = parse_status(
            "Id=socialc.service\nDescription=Social Callback Proxy Service\nLoadState=loaded\n\
             ActiveState=active\nSubState=running\nUnitFileState=enabled\n\
             FragmentPath=/etc/systemd/system/socialc.service\nMainPID=781\n\
             MemoryCurrent=1990656\nTasksCurrent=5\nNRestarts=0\nExecMainStatus=0\n\
             ActiveEnterTimestamp=Tue 2026-09-15 17:49:07 CST\nInactiveEnterTimestamp=\n",
        )
        .expect("details");
        assert_eq!(status.main_pid, Some(781));
        assert_eq!(status.memory, Some(1_990_656));
        assert_eq!(status.tasks, Some(5));
        assert_eq!(
            status.started.as_deref(),
            Some("Tue 2026-09-15 17:49:07 CST")
        );
        assert_eq!(status.stopped, None);

        let stopped = parse_status(
            "Id=a.service\nMainPID=0\nMemoryCurrent=[not set]\nTasksCurrent=18446744073709551615\n",
        )
        .expect("details");
        assert_eq!(
            (stopped.main_pid, stopped.memory, stopped.tasks),
            (None, None, None)
        );
    }

    #[test]
    fn only_a_services_name_goes_into_a_command() {
        assert!(valid_name("nginx.service"));
        assert!(valid_name("getty@tty1.service"));
        assert!(valid_name("systemd-fsck@dev-disk-by\\x2duuid-1234.service"));
        assert!(!valid_name("nginx"));
        assert!(!valid_name("a;reboot.service"));
        assert!(!valid_name("a b.service"));
        assert!(!valid_name("$(reboot).service"));
        assert_eq!(control_command("a\"b.service", ServiceCommand::Stop), None);
        assert_eq!(
            control_command("nginx.service", ServiceCommand::Restart).as_deref(),
            Some(
                "sh -c 'if test \"$(id -u)\" = 0; then systemctl restart -- \"nginx.service\"; \
                 else sudo -n systemctl restart -- \"nginx.service\"; fi 2>&1; echo @@status $?'"
            )
        );
    }

    #[test]
    fn a_command_says_why_it_did_not_run() {
        assert_eq!(controlled("@@status 0\n"), Ok(()));
        assert_eq!(
            controlled("sudo: a password is required\n@@status 1\n"),
            Err("需要 root 权限：以 root 登录，或给这个用户配置免密码的 sudo".into())
        );
        assert_eq!(
            controlled(
                "Job for nginx.service failed because the control process exited with error code.\n\
                 See \"systemctl status nginx.service\" and \"journalctl -xeu nginx.service\" for details.\n\
                 @@status 1\n"
            ),
            Err(
                "Job for nginx.service failed because the control process exited with error code."
                    .into()
            )
        );
        assert_eq!(controlled(""), Err("主机没有回答".into()));
    }

    fn scripts() -> Vec<String> {
        vec![
            command(),
            control_command("getty@tty1.service", ServiceCommand::Stop).unwrap(),
            status_command("nginx.service").unwrap(),
            log_command("nginx.service").unwrap(),
        ]
    }

    #[test]
    fn the_commands_survive_any_login_shell() {
        for command in scripts() {
            let script = command
                .strip_prefix("sh -c '")
                .and_then(|rest| rest.strip_suffix('\''))
                .expect("one sh -c in single quotes");
            assert!(!script.contains('\''), "{script}");
            assert!(!script.contains('!'), "{script}");
            assert!(!script.contains('\n'), "{script}");
            assert!(!script.contains("\\\\"), "{script}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_commands_are_valid_sh() {
        for command in scripts() {
            let script = command
                .strip_prefix("sh -c '")
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap();
            assert!(crate::testing::sh_accepts(script), "{script}");
        }
    }

    /// The list's script, run by this machine's `sh` with a `systemctl`
    /// stood in for, asks `systemctl show` for the right services.
    #[cfg(unix)]
    #[test]
    fn the_script_shows_every_service_but_templates() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temp dir");
        let bin = root.path().join("bin");
        let booted = root.path().join("systemd");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&booted).unwrap();
        let systemctl = bin.join("systemctl");
        std::fs::write(
            &systemctl,
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo 'systemd 249 (249.11)';;\n\
             is-system-running) echo running;;\n\
             list-unit-files) printf '%s\\n' 'ssh.service enabled enabled' 'getty@.service enabled enabled' 'sshd.service alias -';;\n\
             list-units) printf '%s\\n' 'getty@tty1.service loaded active running Getty on tty1' 'ssh.service loaded active running OpenBSD Secure Shell server';;\n\
             show) shift; while test \"$1\" = -p; do shift 2; done; shift; for unit in \"$@\"; do \
             id=$unit; test $unit = sshd.service && id=ssh.service; \
             printf 'Id=%s\\nDescription=%s\\nActiveState=active\\nUnitFileState=enabled\\nFragmentPath=/lib/systemd/system/%s\\nLoadState=loaded\\n\\n' $id $id $id; done;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755)).unwrap();

        let command = command().replace("/run/systemd/system", &booted.display().to_string());
        let script = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = crate::testing::sh(script, Some(&path));
        let Parsed::Table(table) = parse(&String::from_utf8(output.stdout).unwrap()) else {
            panic!("not a table");
        };
        let names: Vec<&str> = table
            .services()
            .iter()
            .map(|service| service.name.as_str())
            .collect();
        // The template is left out, and the alias is ssh itself.
        assert_eq!(names, ["getty@tty1.service", "ssh.service"]);
        assert_eq!(table.summary(), "systemd 249 · 运行正常");
    }
}
