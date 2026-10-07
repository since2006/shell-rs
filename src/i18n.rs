//! 多语言: ShellRS's own text, in Simplified Chinese and English.
//!
//! The text lives in `locales/*.yml`, one file per module, each key with
//! its `zh-CN` and `en` side by side. Code asks for a key with `t!`, or
//! `tn!` where a count picks the English singular. A key names what the
//! text is for (`host.dialog.title_new`), never the Chinese sentence, and a
//! sentence is always one key: each language has its own word order.
//!
//! The interface language changes while ShellRS runs (界面语言), so text is
//! translated where it is shown and a redraw brings the new language.
//! Entities that hand text to gpui-kit to keep (an input's placeholder, a
//! table's columns) give it again when `UiLocale` changes.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui_kit::{Global, SharedString};

/// Every language there is text in. The first is the one the text is
/// written in, and what everything starts in until the settings are read:
/// unit tests see the text as written.
pub const LOCALES: [&str; 2] = ["zh-CN", "en"];

static LOCALE: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// A thread's own language, once `isolate_thread` gave it one.
    static THREAD_LOCALE: Cell<Option<usize>> = const { Cell::new(None) };
}

/// The language ShellRS's text is in now, one of `LOCALES`.
pub fn locale() -> &'static str {
    let index = THREAD_LOCALE
        .with(Cell::get)
        .unwrap_or_else(|| LOCALE.load(Ordering::Relaxed));
    LOCALES[index]
}

/// Put ShellRS's text, and gpui-kit's own, in `locale`. One that is not in
/// `LOCALES` gives the language the text is written in.
pub fn set_locale(locale: &str) {
    let index = LOCALES
        .iter()
        .position(|known| *known == locale)
        .unwrap_or(0);
    if THREAD_LOCALE.with(|own| own.get().is_some()) {
        THREAD_LOCALE.with(|own| own.set(Some(index)));
        return;
    }
    LOCALE.store(index, Ordering::Relaxed);
    gpui_kit::component::set_locale(LOCALES[index]);
}

/// From now on this thread has a language of its own, which `set_locale`
/// changes without touching any other thread or gpui-kit's text. For UI
/// tests: they run side by side in one process, and one that switches to
/// English must not switch the others.
#[doc(hidden)]
pub fn isolate_thread() {
    let current = LOCALES.iter().position(|known| *known == locale());
    THREAD_LOCALE.with(|own| own.set(current));
}

/// The interface language the window was last brought in line with.
/// Entities that keep translated text observe it and give the text again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiLocale(pub &'static str);

impl Global for UiLocale {}

/// The text of `key` in the interface language, with each `%{name}` in it
/// replaced, as a `SharedString`: `t!("host.panel.search")`,
/// `t!("host.delete.title", name = host.name)`.
macro_rules! t {
    ($key:expr $(,)?) => {
        crate::i18n::shared(crate::_rust_i18n_t!($key, locale = crate::i18n::locale()))
    };
    ($key:expr, $($args:tt)+) => {
        crate::i18n::shared(crate::_rust_i18n_t!(
            $key,
            locale = crate::i18n::locale(),
            $($args)+
        ))
    };
}
pub(crate) use t;

/// `t!` for text with a count in it, which also fills `%{count}`: the key's
/// `.one` text when the count is 1 and the language has one (English), its
/// `.other` text otherwise. Chinese has only `.other`.
macro_rules! tn {
    ($key:literal, $count:expr $(, $($args:tt)+)?) => {{
        let count = $count;
        crate::i18n::shared(crate::_rust_i18n_t!(
            {
                if count == 1 && crate::i18n::has(concat!($key, ".one")) {
                    concat!($key, ".one")
                } else {
                    concat!($key, ".other")
                }
            },
            locale = crate::i18n::locale(),
            count = count
            $(, $($args)+)?
        ))
    }};
}
pub(crate) use tn;

/// Text as gpui wants it, without copying what is already `'static`.
#[doc(hidden)]
pub fn shared(text: Cow<'static, str>) -> SharedString {
    match text {
        Cow::Borrowed(text) => SharedString::new_static(text),
        Cow::Owned(text) => text.into(),
    }
}

/// Whether the interface language has text for `key` of its own.
pub fn has(key: &str) -> bool {
    crate::_rust_i18n_backend()
        .translate(locale(), key)
        .is_some()
}

/// `items` as one phrase: 「A、B 和 C」, "A, B and C".
pub fn join_list<S: AsRef<str>>(items: &[S]) -> String {
    let Some((last, rest)) = items.split_last() else {
        return String::new();
    };
    let mut joined = String::new();
    for (index, item) in rest.iter().enumerate() {
        if index > 0 {
            joined = spaced(&joined, &t!("common.list.separator"));
        }
        joined = spaced(&joined, item.as_ref());
    }
    if rest.is_empty() {
        return last.as_ref().to_string();
    }
    spaced(&spaced(&joined, &t!("common.list.and")), last.as_ref())
}

/// `left` and `right` written together, with the space Chinese text puts
/// between a Chinese character and a Latin letter or digit: 「标签和 1 个」.
fn spaced(left: &str, right: &str) -> String {
    let han_meets_latin = match (left.chars().next_back(), right.chars().next()) {
        (Some(before), Some(after)) => {
            (is_han(before) && after.is_ascii_alphanumeric())
                || (before.is_ascii_alphanumeric() && is_han(after))
        }
        _ => false,
    };
    if han_meets_latin {
        format!("{left} {right}")
    } else {
        format!("{left}{right}")
    }
}

fn is_han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::{Path, PathBuf};

    use regex::Regex;

    use super::*;

    /// Directories and files whose code is through with Chinese literals:
    /// every text in them goes through `t!`. Grows as modules are
    /// translated, until it is all of `src`.
    const TRANSLATED: &[&str] = &[
        "src/app",
        "src/cli",
        "src/connection.rs",
        "src/credential",
        "src/forward",
        "src/host",
        "src/i18n.rs",
        "src/main.rs",
        "src/secrets",
        "src/settings",
        "src/shared",
        "src/ssh",
        "src/terminal",
        "src/update",
        "src/workspace/analytics.rs",
        "src/workspace/cli_changes.rs",
        "src/workspace/credentials.rs",
        "src/workspace/dock_skin.rs",
        "src/workspace/editors.rs",
        "src/workspace/forwards.rs",
        "src/workspace/links.rs",
        "src/workspace/mod.rs",
        "src/workspace/notices.rs",
        "src/workspace/recent_hosts.rs",
        "src/workspace/sidebar.rs",
        "src/workspace/snippets.rs",
        "src/workspace/status_bar.rs",
        "src/workspace/tabs.rs",
        "src/workspace/title_bar.rs",
        "src/workspace/updates.rs",
        "src/workspace/window_state.rs",
        "src/workspace/workspace_view.rs",
    ];

    /// Marks a line whose Chinese literal is meant to stay, such as a
    /// language named in itself.
    const KEEP: &str = "i18n: keep";

    fn messages(locale: &str) -> BTreeMap<String, String> {
        crate::_rust_i18n_backend()
            .messages_for_locale(locale)
            .unwrap_or_default()
            .into_iter()
            .map(|(key, text)| (key.into_owned(), text.into_owned()))
            .collect()
    }

    fn sources(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                sources(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    fn all_sources() -> Vec<(PathBuf, String)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        sources(&root.join("src"), &mut files);
        files.sort();
        files
            .into_iter()
            .map(|path| {
                let text = fs::read_to_string(&path).expect("readable source");
                let relative = path
                    .strip_prefix(root)
                    .expect("under the crate")
                    .to_path_buf();
                (relative, text)
            })
            .collect()
    }

    fn is_cjk(c: char) -> bool {
        matches!(c,
            '\u{3000}'..='\u{303f}'   // CJK punctuation: 、。「」
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{ff00}'..='\u{ffef}' // full-width forms: ，：（）
        )
    }

    fn placeholders(text: &str) -> BTreeSet<String> {
        let pattern = Regex::new(r"%\{(\w+)\}").expect("pattern");
        pattern
            .captures_iter(text)
            .map(|capture| capture[1].to_string())
            .collect()
    }

    /// The plural forms of one text share its key: `.one` and `.other`.
    fn base_key(key: &str) -> &str {
        key.strip_suffix(".one")
            .or_else(|| key.strip_suffix(".other"))
            .unwrap_or(key)
    }

    #[test]
    fn every_text_is_in_both_languages() {
        let chinese = messages("zh-CN");
        let english = messages("en");
        assert!(!chinese.is_empty() && !english.is_empty());
        let mut problems = Vec::new();
        for key in chinese.keys() {
            if key.ends_with(".one") {
                problems.push(format!("{key}: Chinese has no singular"));
            } else if !english.contains_key(key) {
                problems.push(format!("{key}: no English"));
            }
        }
        for key in english.keys() {
            match key.strip_suffix(".one") {
                Some(base) if !english.contains_key(&format!("{base}.other")) => {
                    problems.push(format!("{key}: no .other beside it"));
                }
                Some(_) => {}
                None if !chinese.contains_key(key) => {
                    problems.push(format!("{key}: no Chinese"));
                }
                None => {}
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn both_languages_fill_the_same_values() {
        let chinese = messages("zh-CN");
        let mut problems = Vec::new();
        for (key, text) in messages("en") {
            let Some(base) = key.strip_suffix(".one") else {
                if let Some(chinese) = chinese.get(&key)
                    && placeholders(chinese) != placeholders(&text)
                {
                    problems.push(key);
                }
                continue;
            };
            // "1 host" may say the number in words; it fills nothing else.
            let other = chinese
                .get(&format!("{base}.other"))
                .map(|text| placeholders(text));
            if !placeholders(&text).is_subset(&other.unwrap_or_default()) {
                problems.push(key);
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn texts_follow_the_interface_language_rules() {
        let mut problems = Vec::new();
        for (key, text) in messages("en") {
            if text.chars().any(is_cjk) {
                problems.push(format!("{key}: Chinese in English: {text}"));
            }
        }
        for locale in LOCALES {
            for (key, text) in messages(locale) {
                if text.contains("...") {
                    problems.push(format!("{key} ({locale}): three dots instead of …"));
                }
                // The list words carry their own spaces: ", ", " and ".
                if text.trim() != text && !key.starts_with("common.list.") {
                    problems.push(format!("{key} ({locale}): spaces around the text"));
                }
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn every_key_in_the_code_has_text_and_every_text_is_used() {
        let chinese = messages("zh-CN");
        let call = Regex::new(r#"\b(tn?)!\(\s*"([^"]+)""#).expect("pattern");
        let sources = all_sources();
        let mut problems = Vec::new();
        for (path, text) in &sources {
            // Doc comments show keys that are only examples.
            let code = text
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for capture in call.captures_iter(&code) {
                let key = match &capture[1] {
                    "tn" => format!("{}.other", &capture[2]),
                    _ => capture[2].to_string(),
                };
                if !chinese.contains_key(&key) {
                    problems.push(format!("{}: {key} has no text", path.display()));
                }
            }
        }
        let code = sources
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for key in chinese.keys() {
            let base = base_key(key);
            if !code.contains(&format!("\"{base}\"")) {
                problems.push(format!("{key} is used nowhere"));
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn translated_code_has_no_chinese_literals() {
        // More to check while a module is being translated, comma-separated:
        // `SHELLRS_I18N_CHECK=src/host,src/forward`.
        let extra = std::env::var("SHELLRS_I18N_CHECK").unwrap_or_default();
        let prefixes = TRANSLATED
            .iter()
            .copied()
            .chain(
                extra
                    .split(',')
                    .map(str::trim)
                    .filter(|prefix| !prefix.is_empty()),
            )
            .collect::<Vec<_>>();
        let mut problems = Vec::new();
        for (path, text) in all_sources() {
            let name = path.to_string_lossy().replace('\\', "/");
            let translated = prefixes
                .iter()
                .any(|prefix| name == *prefix || name.starts_with(&format!("{prefix}/")));
            if !translated || name.ends_with("tests.rs") {
                continue;
            }
            let lines = text.lines().collect::<Vec<_>>();
            for (line, literal) in literals_outside_tests(&text) {
                if literal.chars().any(is_cjk) && !lines[line].contains(KEEP) {
                    problems.push(format!("{name}:{}: {literal}", line + 1));
                }
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn the_scanner_skips_comments_tests_and_lifetimes() {
        let source = r##"
            // "注释"
            /* "块注释" /* "嵌套" */ */
            fn shown<'a>(x: &'a str) -> char { let _ = "显示"; let _ = r#"原样"#; '、' }
            #[cfg(test)]
            fn helper() { let _ = "测试"; }
            #[cfg(test)]
            mod tests { fn t() { let _ = "测试"; } }
            const AFTER: &str = "之后";
        "##;
        let found = literals_outside_tests(source)
            .into_iter()
            .map(|(_, literal)| literal)
            .collect::<Vec<_>>();
        assert_eq!(found, ["显示", "原样", "、", "之后"]);
    }

    /// The string and char literals of `source` with their line (from 0),
    /// leaving out comments and items under `#[cfg(test)]`.
    fn literals_outside_tests(source: &str) -> Vec<(usize, String)> {
        let chars = source.chars().collect::<Vec<_>>();
        let mut found = Vec::new();
        let mut line = 0;
        let mut i = 0;
        // Set by `#[cfg(test)]`: the brace depth below which the item ends,
        // once its body opened.
        let mut skipping = false;
        let mut skip_depth = None;
        let mut depth = 0usize;
        let at = |i: usize, text: &str| chars[i..].starts_with(&text.chars().collect::<Vec<_>>());
        while i < chars.len() {
            let c = chars[i];
            if c == '\n' {
                line += 1;
                i += 1;
            } else if at(i, "//") {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            } else if at(i, "/*") {
                let mut nesting = 0;
                while i < chars.len() {
                    if at(i, "/*") {
                        nesting += 1;
                        i += 2;
                    } else if at(i, "*/") {
                        nesting -= 1;
                        i += 2;
                        if nesting == 0 {
                            break;
                        }
                    } else {
                        if chars[i] == '\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                }
            } else if at(i, "#[cfg(test)]") || at(i, "#[cfg(any(test") {
                skipping = true;
                i += 1;
            } else if c == '"'
                || ((c == 'r' || c == 'b')
                    && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_'))
                    && raw_or_byte_start(&chars[i..]).is_some())
            {
                let start_line = line;
                let (hashes, open) = if c == '"' {
                    (None, 1)
                } else {
                    raw_or_byte_start(&chars[i..]).expect("checked")
                };
                i += open;
                let mut literal = String::new();
                while i < chars.len() {
                    let d = chars[i];
                    match hashes {
                        None if d == '\\' => {
                            literal.push(d);
                            if let Some(&next) = chars.get(i + 1) {
                                literal.push(next);
                                if next == '\n' {
                                    line += 1;
                                }
                            }
                            i += 2;
                            continue;
                        }
                        None if d == '"' => {
                            i += 1;
                            break;
                        }
                        Some(count)
                            if d == '"'
                                && chars[i + 1..]
                                    .iter()
                                    .take(count)
                                    .filter(|h| **h == '#')
                                    .count()
                                    == count =>
                        {
                            i += 1 + count;
                            break;
                        }
                        _ => {}
                    }
                    if d == '\n' {
                        line += 1;
                    }
                    literal.push(d);
                    i += 1;
                }
                if !skipping {
                    found.push((start_line, literal));
                }
            } else if c == '\'' {
                // A char literal ('x', '\n'), or a lifetime ('a).
                let end = if chars.get(i + 1) == Some(&'\\') {
                    // The escaped character may itself be a quote: '\''.
                    chars[i + 3..]
                        .iter()
                        .position(|d| *d == '\'')
                        .map(|n| i + 3 + n)
                } else if chars.get(i + 2) == Some(&'\'') {
                    Some(i + 2)
                } else {
                    None
                };
                match end {
                    Some(end) => {
                        if !skipping {
                            found.push((line, chars[i + 1..end].iter().collect()));
                        }
                        i = end + 1;
                    }
                    None => i += 1,
                }
            } else {
                match c {
                    '{' => {
                        depth += 1;
                        if skipping && skip_depth.is_none() {
                            skip_depth = Some(depth);
                        }
                    }
                    '}' => {
                        if skip_depth == Some(depth) {
                            skipping = false;
                            skip_depth = None;
                        }
                        depth = depth.saturating_sub(1);
                    }
                    ';' if skipping && skip_depth.is_none() => skipping = false,
                    _ => {}
                }
                i += 1;
            }
        }
        found
    }

    /// For a raw or byte string starting here (`r"`, `r#"`, `b"`, `br#"`):
    /// how many `#` close it (`None` for a byte string, which has escapes)
    /// and how many characters open it.
    fn raw_or_byte_start(chars: &[char]) -> Option<(Option<usize>, usize)> {
        let mut i = 0;
        let byte = chars.first() == Some(&'b');
        if byte {
            i += 1;
        }
        if chars.get(i) == Some(&'r') {
            i += 1;
            let hashes = chars[i..].iter().take_while(|c| **c == '#').count();
            i += hashes;
            (chars.get(i) == Some(&'"')).then_some((Some(hashes), i + 1))
        } else if byte && chars.get(i) == Some(&'"') {
            Some((None, i + 1))
        } else {
            None
        }
    }

    #[test]
    fn a_count_of_one_takes_the_singular_where_there_is_one() {
        isolate_thread();
        set_locale("en");
        assert_eq!(
            tn!("shared.duration.days", 1, clock = "01:02:03"),
            "1 day 01:02:03"
        );
        assert_eq!(
            tn!("shared.duration.days", 3, clock = "01:02:03"),
            "3 days 01:02:03"
        );
        set_locale("zh-CN");
        assert_eq!(
            tn!("shared.duration.days", 1, clock = "01:02:03"),
            "1 天 01:02:03"
        );
        assert_eq!(join_list(&["甲", "乙", "丙"]), "甲、乙和丙");
        assert_eq!(
            join_list(&["2 个远程终端", "1 个 SFTP 标签", "1 个本地终端"]),
            "2 个远程终端、1 个 SFTP 标签和 1 个本地终端"
        );
        set_locale("en");
        assert_eq!(join_list(&["a", "b", "c"]), "a, b and c");
        assert_eq!(join_list(&["a"]), "a");
    }

    #[test]
    fn an_isolated_thread_keeps_its_language_to_itself() {
        let other = std::thread::spawn(|| {
            isolate_thread();
            set_locale("en");
            locale()
        });
        assert_eq!(other.join().expect("thread"), "en");
        assert_eq!(locale(), "zh-CN");
    }
}
