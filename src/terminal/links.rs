//! Links in the terminal: web addresses found in the text, and the ones a
//! program marks itself with OSC 8 (`ls --hyperlink`, gcc, systemd), whose
//! text may differ from where they lead.
//!
//! Only `http` and `https` count. Any other scheme names something on the
//! server (`file://`) or starts a local program, neither of which a click in
//! a remote terminal should open.

use alacritty_terminal::index::{Boundary, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Hyperlink;
use alacritty_terminal::term::search::{Match, RegexSearch};
use gpui_kit::SharedString;

use super::search::visible_matches;

/// A web address in the text: the characters RFC 3986 allows after
/// `http://` or `https://`. Written in lower case, so the search ignores
/// case. Anything else, a space or a Chinese comma, ends it.
const URL_PATTERN: &str = r"https?://[a-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+";

pub fn url_search() -> RegexSearch {
    RegexSearch::new(URL_PATTERN).expect("the link pattern is valid")
}

pub fn is_web_link(uri: &str) -> bool {
    let lower = uri.get(..8).unwrap_or(uri).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// `text` without what ends a sentence rather than the address: a full
/// stop, a comma, a quote, or a closing bracket with no opening one inside.
/// `None` when nothing but the scheme is left.
fn trim_url(text: &str) -> Option<&str> {
    let mut url = text;
    while let Some(last) = url.chars().last() {
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(open).count() < url.matches(close).count()
        };
        if ".,;:!?'\"*".contains(last) || unbalanced('(', ')') || unbalanced('[', ']') {
            url = &url[..url.len() - last.len_utf8()];
        } else {
            break;
        }
    }
    let (_, rest) = url.split_once("://")?;
    (!rest.is_empty()).then_some(url)
}

/// The web addresses on screen, in reading order, each with the cells it
/// covers. One that wraps onto the next line is one link.
pub fn visible_links<T>(term: &Term<T>, regex: &mut RegexSearch) -> Vec<(Match, SharedString)> {
    visible_matches(term, regex)
        .into_iter()
        .filter_map(|found| {
            let text = term.bounds_to_string(*found.start(), *found.end());
            let url = trim_url(&text)?;
            // What was cut is ASCII punctuation, one cell a character.
            let cut = text[url.len()..].chars().count();
            let end = found.end().sub(term, Boundary::Grid, cut);
            Some((*found.start()..=end, SharedString::from(url.to_string())))
        })
        .collect()
}

/// Tells, cell by cell in reading order, which link a cell belongs to, and
/// collects the links' addresses. A link marked with OSC 8 takes its cells
/// over any address found in their text; the cells of one such link (the
/// same id and address) are one link wherever they are, as the spec asks.
pub struct LinkMarker {
    found: Vec<(Match, SharedString)>,
    next: usize,
    /// The link the found address `next` became, once a cell of it is marked.
    next_link: Option<usize>,
    marked: Vec<(Hyperlink, usize)>,
    links: Vec<SharedString>,
}

impl LinkMarker {
    /// `found` must be in reading order, as `visible_links` returns it.
    pub fn new(found: Vec<(Match, SharedString)>) -> Self {
        Self {
            found,
            next: 0,
            next_link: None,
            marked: Vec::new(),
            links: Vec::new(),
        }
    }

    /// The link of the cell at `point`, which carries `hyperlink`. Points
    /// must come in reading order.
    pub fn mark(&mut self, point: Point, hyperlink: Option<Hyperlink>) -> Option<usize> {
        if let Some(hyperlink) = hyperlink.filter(|link| is_web_link(link.uri())) {
            if let Some((_, link)) = self.marked.iter().find(|(seen, _)| *seen == hyperlink) {
                return Some(*link);
            }
            let link = self.push(hyperlink.uri());
            self.marked.push((hyperlink, link));
            return Some(link);
        }
        while self
            .found
            .get(self.next)
            .is_some_and(|(found, _)| *found.end() < point)
        {
            self.next += 1;
            self.next_link = None;
        }
        let (found, url) = self.found.get(self.next)?;
        if !found.contains(&point) {
            return None;
        }
        if self.next_link.is_none() {
            let url = url.clone();
            self.next_link = Some(self.push(&url));
        }
        self.next_link
    }

    /// The addresses, by the numbers `mark` gave.
    pub fn into_links(self) -> Vec<SharedString> {
        self.links
    }

    fn push(&mut self, uri: &str) -> usize {
        self.links.push(uri.to_string().into());
        self.links.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentence_punctuation_and_unmatched_brackets_end_an_address() {
        assert_eq!(
            trim_url("https://help.ubuntu.com."),
            Some("https://help.ubuntu.com")
        );
        assert_eq!(
            trim_url("https://example.com/a?b=1,"),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(trim_url("https://a.b/c)"), Some("https://a.b/c"));
        assert_eq!(trim_url("https://a.b/c)."), Some("https://a.b/c"));
        assert_eq!(
            trim_url("https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            Some("https://en.wikipedia.org/wiki/Rust_(programming_language)")
        );
        assert_eq!(trim_url("https://a.b/[1]"), Some("https://a.b/[1]"));
        assert_eq!(trim_url("https://a.b/c'"), Some("https://a.b/c"));
        assert_eq!(trim_url("https://."), None);
        assert_eq!(trim_url("http://"), None);
    }

    #[test]
    fn only_web_addresses_are_links() {
        assert!(is_web_link("https://example.com"));
        assert!(is_web_link("HTTP://example.com"));
        assert!(!is_web_link("file://host/etc/passwd"));
        assert!(!is_web_link("mailto:a@b.c"));
        assert!(!is_web_link("http"));
    }
}
