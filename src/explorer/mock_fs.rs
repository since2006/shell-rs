//! Seed directory trees for the mock local and remote file systems.

use crate::session::Session;

use super::{DirTree, FileEntry};

/// The mock local machine, rooted at `/Users/xuz`.
pub fn local_tree() -> DirTree {
    let mut tree = DirTree::new();
    tree.add_dir(
        "/",
        vec![FileEntry::dir("Users", "2026-01-12 09:10", "drwxr-xr-x")],
    )
    .add_dir(
        "/Users",
        vec![FileEntry::dir("xuz", "2026-01-12 09:10", "drwxr-xr-x")],
    )
    .add_dir(
        "/Users/xuz",
        vec![
            FileEntry::dir("Downloads", "2026-09-09 18:42", "drwxr-xr-x"),
            FileEntry::dir("Projects", "2026-09-10 11:05", "drwxr-xr-x"),
            FileEntry::dir("Documents", "2026-08-30 20:14", "drwxr-xr-x"),
            FileEntry::file(".zshrc", 2_310, "2026-07-02 08:15", "-rw-r--r--"),
            FileEntry::file("notes.md", 8_412, "2026-09-08 14:32", "-rw-r--r--"),
        ],
    )
    .add_dir(
        "/Users/xuz/Downloads",
        vec![
            FileEntry::file(
                "shellr-0.1.0.dmg",
                41_943_040,
                "2026-09-09 18:42",
                "-rw-r--r--",
            ),
            FileEntry::file("report.pdf", 1_258_291, "2026-09-05 16:20", "-rw-r--r--"),
            FileEntry::file("nginx.conf", 3_072, "2026-09-01 10:03", "-rw-r--r--"),
        ],
    )
    .add_dir(
        "/Users/xuz/Projects",
        vec![
            FileEntry::dir("shellr", "2026-09-10 11:05", "drwxr-xr-x"),
            FileEntry::dir("gpui-kit", "2026-09-10 10:40", "drwxr-xr-x"),
        ],
    )
    .add_dir(
        "/Users/xuz/Projects/shellr",
        vec![
            FileEntry::dir("src", "2026-09-10 11:05", "drwxr-xr-x"),
            FileEntry::file("Cargo.toml", 612, "2026-09-10 11:05", "-rw-r--r--"),
            FileEntry::file("README.md", 1_480, "2026-09-10 11:05", "-rw-r--r--"),
        ],
    )
    .add_dir(
        "/Users/xuz/Projects/shellr/src",
        vec![
            FileEntry::file("main.rs", 902, "2026-09-10 11:05", "-rw-r--r--"),
            FileEntry::file("lib.rs", 388, "2026-09-10 11:05", "-rw-r--r--"),
        ],
    )
    .add_dir(
        "/Users/xuz/Projects/gpui-kit",
        vec![FileEntry::file(
            "Cargo.toml",
            4_120,
            "2026-09-10 10:40",
            "-rw-r--r--",
        )],
    )
    .add_dir(
        "/Users/xuz/Documents",
        vec![
            FileEntry::file("架构设计.docx", 356_352, "2026-08-30 20:14", "-rw-r--r--"),
            FileEntry::file("服务器清单.xlsx", 24_576, "2026-08-21 09:00", "-rw-r--r--"),
        ],
    );
    tree
}

/// The mock remote host for a session, rooted at `/`.
pub fn remote_tree(session: &Session) -> DirTree {
    let home = format!("/home/{}", session.user);
    let mut tree = DirTree::new();
    tree.add_dir(
        "/",
        vec![
            FileEntry::dir("etc", "2026-06-15 07:00", "drwxr-xr-x"),
            FileEntry::dir("var", "2026-06-15 07:00", "drwxr-xr-x"),
            FileEntry::dir("home", "2026-06-15 07:00", "drwxr-xr-x"),
            FileEntry::dir("opt", "2026-06-15 07:00", "drwxr-xr-x"),
            FileEntry::symlink("bin", "2026-06-15 07:00", "lrwxrwxrwx"),
        ],
    )
    .add_dir(
        "/etc",
        vec![
            FileEntry::dir("nginx", "2026-08-02 12:30", "drwxr-xr-x"),
            FileEntry::dir("ssh", "2026-06-15 07:00", "drwxr-xr-x"),
            FileEntry::file("hosts", 221, "2026-06-15 07:00", "-rw-r--r--"),
            FileEntry::file("hostname", 7, "2026-06-15 07:00", "-rw-r--r--"),
        ],
    )
    .add_dir(
        "/etc/nginx",
        vec![
            FileEntry::file("nginx.conf", 1_490, "2026-08-02 12:30", "-rw-r--r--"),
            FileEntry::dir("sites-enabled", "2026-08-02 12:30", "drwxr-xr-x"),
        ],
    )
    .add_dir(
        "/etc/nginx/sites-enabled",
        vec![FileEntry::file(
            "default",
            2_412,
            "2026-08-02 12:30",
            "-rw-r--r--",
        )],
    )
    .add_dir(
        "/etc/ssh",
        vec![FileEntry::file(
            "sshd_config",
            3_264,
            "2026-06-15 07:00",
            "-rw-------",
        )],
    )
    .add_dir(
        "/var",
        vec![
            FileEntry::dir("www", "2026-09-01 22:10", "drwxr-xr-x"),
            FileEntry::dir("log", "2026-09-10 00:00", "drwxr-xr-x"),
        ],
    )
    .add_dir(
        "/var/www",
        vec![
            FileEntry::dir("html", "2026-09-01 22:10", "drwxr-xr-x"),
            FileEntry::file("app.log", 5_242_880, "2026-09-10 08:41", "-rw-r--r--"),
        ],
    )
    .add_dir(
        "/var/www/html",
        vec![
            FileEntry::file("index.html", 612, "2026-09-01 22:10", "-rw-r--r--"),
            FileEntry::file("favicon.ico", 4_286, "2026-09-01 22:10", "-rw-r--r--"),
        ],
    )
    .add_dir(
        "/var/log",
        vec![
            FileEntry::file("syslog", 12_582_912, "2026-09-10 08:41", "-rw-r-----"),
            FileEntry::file("auth.log", 786_432, "2026-09-10 08:41", "-rw-r-----"),
        ],
    )
    .add_dir(
        "/home",
        vec![FileEntry::dir(
            &session.user,
            "2026-06-15 07:00",
            "drwxr-x---",
        )],
    )
    .add_dir(
        &home,
        vec![
            FileEntry::file(".bashrc", 3_771, "2026-06-15 07:00", "-rw-r--r--"),
            FileEntry::file("deploy.sh", 1_024, "2026-09-03 19:22", "-rwxr-xr-x"),
            FileEntry::dir("backups", "2026-09-08 02:00", "drwxr-xr-x"),
        ],
    )
    .add_dir(
        &format!("{home}/backups"),
        vec![FileEntry::file(
            "db-2026-09-08.sql.gz",
            73_400_320,
            "2026-09-08 02:00",
            "-rw-r--r--",
        )],
    )
    .add_dir(
        "/opt",
        vec![FileEntry::dir("app", "2026-07-20 15:00", "drwxr-xr-x")],
    )
    .add_dir(
        "/opt/app",
        vec![
            FileEntry::file("server", 18_874_368, "2026-07-20 15:00", "-rwxr-xr-x"),
            FileEntry::file("config.toml", 1_212, "2026-07-20 15:00", "-rw-r--r--"),
        ],
    );
    tree
}

/// The directory a remote pane starts in.
pub fn remote_home(session: &Session) -> String {
    format!("/home/{}", session.user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::Location;
    use crate::session::SessionStore;

    #[test]
    fn remote_tree_has_a_home_for_the_session_user() {
        let store = SessionStore::seed();
        let session = store.sessions().iter().find(|s| s.name == "db-01").unwrap();
        let tree = remote_tree(session);
        assert!(tree.contains("/home/postgres"));
        let home = remote_home(session);
        let location = Location::new(tree, &home, Some(&home));
        assert_eq!(location.display(), "~");
    }

    #[test]
    fn local_tree_starts_in_home() {
        let location = Location::new(local_tree(), "/Users/xuz", Some("/Users/xuz"));
        assert!(location.rows().iter().any(|e| e.name == "Downloads"));
    }
}
