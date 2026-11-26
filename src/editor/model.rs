//! What the editor decides without a window: which highlighter a file gets,
//! how it indents, and what the status line says.

use crate::host::HostId;
use crate::i18n::t;
use crate::sftp::{RemotePath, TextFormat};
use gpui_kit::component::input::TabSize;
use std::path::PathBuf;

/// Identity of an editor tab; one per file it edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EditorId(pub u64);

/// The file an editor edits. Opening it again shows that editor instead of
/// a second one: a remote file by host and path (whichever SFTP tab of the
/// host opened it), a local one by path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EditorKey {
    Remote(HostId, RemotePath),
    Local(PathBuf),
}

/// The highlighter for a file at `path` (a name alone will do): the
/// languages ShellRS is built with, `text` for anything else. Most go by
/// the name. `.conf` is no one format: only nginx's are told apart, by the
/// folder they are in (issue #142); the rest stay text.
pub fn language_for(path: &str) -> &'static str {
    let mut components = path.rsplit(['/', '\\']);
    let name = components.next().unwrap_or(path).to_ascii_lowercase();
    // The folders it is in, the nearest first.
    let folders: Vec<String> = components.map(str::to_ascii_lowercase).collect();
    match name.as_str() {
        "makefile" | "gnumakefile" => return "make",
        ".bashrc" | ".bash_profile" | ".bash_aliases" | ".bash_logout" | ".profile" | ".zshrc"
        | ".zprofile" | ".envrc" => return "bash",
        "pipfile" | "cargo.lock" => return "toml",
        "nginx.conf" => return "nginx",
        ".gitconfig" | ".editorconfig" => return "ini",
        _ => {}
    }
    // In nginx's folders (`/etc/nginx`, `/usr/local/nginx`, `nginx-1.26`):
    // its `.conf` files, and the sites, which have no extension.
    let site = folders
        .first()
        .is_some_and(|parent| parent == "sites-available" || parent == "sites-enabled");
    if (name.ends_with(".conf") || site) && folders.iter().any(|folder| folder.starts_with("nginx"))
    {
        return "nginx";
    }
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "text";
    };
    match extension {
        "sh" | "bash" | "zsh" | "ksh" | "env" => "bash",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "py" | "pyi" | "pyw" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "md" | "markdown" => "markdown",
        "sql" => "sql",
        "php" | "phtml" => "php",
        "go" => "go",
        "lua" => "lua",
        "mk" | "mak" => "make",
        "diff" | "patch" => "diff",
        "xml" | "xsd" | "xsl" | "xslt" | "svg" | "wsdl" => "xml",
        "properties" => "properties",
        // INI's, and systemd's units and yum's repositories, which are.
        "ini" | "cfg" | "cnf" | "repo" | "desktop" | "service" | "socket" | "timer" | "target"
        | "mount" | "automount" | "path" | "slice" | "swap" => "ini",
        _ => "text",
    }
}

/// How Tab indents. A Makefile needs real tabs, and so does a file that
/// mostly indents with them; others get spaces, as wide as is usual for
/// their language.
pub fn indentation(language: &str, text: &str) -> TabSize {
    let (mut tabs, mut spaces) = (0, 0);
    for line in text.lines().take(2000) {
        if line.starts_with('\t') {
            tabs += 1;
        } else if line.starts_with("  ") {
            spaces += 1;
        }
    }
    let hard_tabs = language == "make" || tabs > spaces;
    let tab_size = match language {
        "yaml" | "json" | "html" | "css" | "javascript" | "lua" => 2,
        _ => 4,
    };
    TabSize {
        tab_size,
        hard_tabs,
    }
}

/// 「行 12，列 5」, from the editor's zero-based position.
pub fn cursor_label(line: u32, character: u32) -> String {
    t!(
        "editor.status.cursor",
        line = line + 1,
        column = character + 1
    )
    .to_string()
}

/// 「UTF-8 · LF」, 「UTF-8 BOM · CRLF」, 「GBK · CRLF」.
pub fn format_label(format: TextFormat) -> String {
    let encoding = if format.bom() {
        "UTF-8 BOM"
    } else {
        format.encoding_name()
    };
    let lines = if format.crlf() { "CRLF" } else { "LF" };
    format!("{encoding} · {lines}")
}

#[cfg(test)]
mod tests {
    use super::super::languages::register_languages;
    use super::*;
    use gpui_kit::component::highlighter::LanguageRegistry;

    #[test]
    fn a_file_gets_the_highlighter_its_name_says() {
        for (name, language) in [
            ("docker-compose.yml", "yaml"),
            ("deploy.SH", "bash"),
            (".bashrc", "bash"),
            ("Makefile", "make"),
            ("rules.mk", "make"),
            ("package.json", "json"),
            ("app.py", "python"),
            ("README.md", "markdown"),
            ("init.sql", "sql"),
            ("fix.patch", "diff"),
            ("hosts", "text"),
            ("archive.tar.gz", "text"),
            ("pom.xml", "xml"),
            ("logo.SVG", "xml"),
            ("application.properties", "properties"),
            ("php.ini", "ini"),
            ("setup.cfg", "ini"),
            ("my.cnf", "ini"),
            ("nginx.service", "ini"),
            ("epel.repo", "ini"),
            (".gitconfig", "ini"),
            ("nginx.conf", "nginx"),
            // Other `.conf` files are no one format.
            ("redis.conf", "text"),
        ] {
            assert_eq!(language_for(name), language, "{name}");
        }
    }

    #[test]
    fn nginx_s_files_are_told_by_their_folder() {
        for (path, language) in [
            ("/etc/nginx/nginx.conf", "nginx"),
            ("/etc/nginx/conf.d/default.conf", "nginx"),
            ("/usr/local/nginx/conf/proxy.conf", "nginx"),
            ("/opt/nginx-1.26.2/conf/upstream.conf", "nginx"),
            ("/etc/nginx/sites-available/example.com", "nginx"),
            ("/etc/nginx/sites-enabled/default", "nginx"),
            (r"C:\nginx\conf\gzip.conf", "nginx"),
            // Not nginx's.
            ("/etc/supervisor/conf.d/app.conf", "text"),
            ("/etc/sysctl.d/99-tuning.conf", "text"),
            ("/etc/apache2/sites-available/example.com", "text"),
            ("/etc/nginx/mime.types", "text"),
            ("/etc/systemd/system/nginx.service", "ini"),
            ("/etc/nginx/ssl/site.xml", "xml"),
        ] {
            assert_eq!(language_for(path), language, "{path}");
        }
    }

    #[test]
    fn every_language_named_is_built_in() {
        register_languages();
        let registry = LanguageRegistry::singleton();
        for name in [
            "bash",
            "yaml",
            "toml",
            "json",
            "python",
            "javascript",
            "html",
            "css",
            "markdown",
            "sql",
            "php",
            "go",
            "lua",
            "make",
            "diff",
        ]
        .into_iter()
        .chain(super::super::languages::ADDED)
        {
            assert!(
                registry
                    .language(name)
                    .is_some_and(|config| config.has_grammar()),
                "{name} is not compiled in"
            );
        }
    }

    /// What ShellRS adds parses, and its highlights take the theme's colors:
    /// a query that names what its grammar lacks would leave the file plain.
    #[test]
    fn the_added_highlighters_color_what_they_read() {
        use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};
        use gpui_kit::component::input::Rope;

        register_languages();
        let theme = HighlightTheme::default_light();
        for (language, text, styled) in [
            (
                "xml",
                "<?xml version=\"1.0\"?>\n<project id=\"a\"><!-- note --></project>\n",
                ["project", "id", "<!-- note -->"].as_slice(),
            ),
            (
                "properties",
                "# 数据库\nspring.datasource.url=jdbc:mysql://db/app\n",
                &["# 数据库", "spring", "jdbc:mysql://db/app"],
            ),
            (
                "ini",
                "; comment\n[mysqld]\nport = 3306\n",
                &["; comment", "mysqld", "port"],
            ),
            (
                "nginx",
                "# proxy\nserver {\n    listen 80;\n    location / { proxy_pass http://app; }\n}\n",
                &["# proxy", "listen", "location"],
            ),
        ] {
            let mut highlighter = SyntaxHighlighter::new(language);
            assert_eq!(highlighter.language().as_ref(), language);
            let rope = Rope::from(text);
            highlighter.update(None, &rope, None);
            let styles = highlighter.styles(&(0..text.len()), &*theme);
            for part in styled {
                let at = text.find(part).unwrap();
                assert!(
                    styles
                        .iter()
                        .any(|(range, style)| range.contains(&at) && style.color.is_some()),
                    "{language}: {part} is plain in {styles:?}"
                );
            }
        }
    }

    #[test]
    fn makefiles_and_tab_indented_files_indent_with_tabs() {
        assert!(indentation("make", "all:\n    echo").hard_tabs);
        assert!(indentation("text", "a {\n\tb;\n\tc;\n}").hard_tabs);
        let yaml = indentation("yaml", "a:\n  b: 1\n");
        assert!(!yaml.hard_tabs);
        assert_eq!(yaml.tab_size, 2);
        assert_eq!(indentation("python", "").tab_size, 4);
    }

    #[test]
    fn the_status_line_counts_from_one() {
        assert_eq!(cursor_label(11, 4), "行 12，列 5");
        assert_eq!(format_label(TextFormat::default()), "UTF-8 · LF");
        assert_eq!(
            format_label(TextFormat::new(true, true)),
            "UTF-8 BOM · CRLF"
        );
        // 「中文」 in GBK (issue #159).
        let gbk = crate::sftp::TextFile::decode::<()>(
            b"\xD6\xD0\xCE\xC4\r\n".to_vec(),
            crate::sftp::FileStamp::new(0, None),
        )
        .unwrap();
        assert_eq!(format_label(gbk.format()), "GBK · CRLF");
    }
}
