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

/// The highlighter for a file, by its name: the languages ShellRS is built
/// with, `text` for anything else.
pub fn language_for(file_name: &str) -> &'static str {
    let name = file_name.to_ascii_lowercase();
    match name.as_str() {
        "makefile" | "gnumakefile" => return "make",
        ".bashrc" | ".bash_profile" | ".bash_aliases" | ".bash_logout" | ".profile" | ".zshrc"
        | ".zprofile" | ".envrc" => return "bash",
        "pipfile" | "cargo.lock" => return "toml",
        _ => {}
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

/// 「UTF-8 · LF」, 「UTF-8 BOM · CRLF」.
pub fn format_label(format: TextFormat) -> String {
    let encoding = if format.bom() { "UTF-8 BOM" } else { "UTF-8" };
    let lines = if format.crlf() { "CRLF" } else { "LF" };
    format!("{encoding} · {lines}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::highlighter::LanguageRegistry;

    #[test]
    fn a_file_gets_the_highlighter_its_name_says() {
        for (name, language) in [
            ("nginx.conf", "text"),
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
        ] {
            assert_eq!(language_for(name), language, "{name}");
        }
    }

    #[test]
    fn every_language_named_is_built_in() {
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
        ] {
            assert!(
                registry
                    .language(name)
                    .is_some_and(|config| config.has_grammar()),
                "{name} is not compiled in"
            );
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
    }
}
