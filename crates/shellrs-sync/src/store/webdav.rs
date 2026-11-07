//! A folder on a WebDAV server: Nextcloud, ownCloud, 坚果云 (Jianguoyun), a
//! NAS, or anything else that speaks the protocol.
//!
//! Four requests do it: PROPFIND (depth 1) lists a folder with each file's
//! tag, size and time, and GET, PUT and DELETE read, write and delete a
//! file. Folders a write needs are made with MKCOL once the server says they
//! are missing.
//!
//! Servers have their ways, each met here:
//! - A file's tag is its ETag, weak or not: Apache makes a file's ETag weak
//!   for its first second, and `If-Match` never matches a weak one, so a
//!   write that a server refuses on its ETag is checked again with a
//!   PROPFIND before it counts as a conflict. A server without ETags gets
//!   one made of the file's time and size; like the local folder store's,
//!   a time too close to the server's own now (its `Date`) to trust makes a
//!   tag that matches nothing, as a rewrite within the same second could
//!   keep both.
//! - A server may give ETags when a file is read and not when it is listed
//!   (nginx's WebDAV does so): once a listing shows files without them,
//!   reads and writes go by time and size too, so a file's tag is the same
//!   whichever way it was had, and a round reads no file again for nothing.
//! - A write's condition goes as `If-None-Match: *` or `If-Match`. A server
//!   that ignores them overwrites, which a round survives: each device
//!   writes only its own file. A tag made of time and size is checked with
//!   a PROPFIND first instead, as the local store checks its own.
//! - A listing's links may be paths or whole URLs, percent-encoded or not:
//!   only their last segment is taken, decoded.
//! - Every request has a time limit. Redirects to another host lose the
//!   password, as `reqwest` drops it.

use std::fmt;
use std::io::Read as _;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{
    CONTENT_LENGTH, CONTENT_TYPE, DATE, ETAG, HeaderMap, IF_MATCH, IF_NONE_MATCH, LAST_MODIFIED,
};
use reqwest::{Method, StatusCode, Url};
use zeroize::Zeroizing;

use super::{Fetched, PutCondition, RemoteFile, SyncStore, tag_base};
use crate::StoreError;

/// A request's limit, its body included.
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A listing larger than this is not a folder of ours.
const MAX_LISTING: u64 = 8 << 20;
/// How close to the server's now a file's time must be for a tag made of
/// it to be in doubt, in seconds.
const RACY_SECS: u64 = 3;
/// The most folders a write makes above it.
const MAX_FOLDER_LEVELS: usize = 16;

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getetag/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

pub struct WebDavStore {
    client: Client,
    /// The folder sync goes in, ending in `/`.
    base: Url,
    username: String,
    password: Zeroizing<String>,
    /// Whether the server's listings carry ETags, once one listed a file.
    listed_etags: AtomicU8,
}

/// What the listings so far said of ETags.
const ETAGS_UNKNOWN: u8 = 0;
const ETAGS_LISTED: u8 = 1;
const ETAGS_NOT_LISTED: u8 = 2;

impl fmt::Debug for WebDavStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebDavStore")
            .field("base", &self.base.as_str())
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

impl WebDavStore {
    /// The folder at `url` (an `http` or `https` address, as the server's
    /// documentation gives it; made when first written to), signing in as
    /// `username` with `password`. Requests say they come from
    /// `user_agent`.
    pub fn new(
        url: &str,
        username: &str,
        password: &str,
        user_agent: &str,
    ) -> Result<Self, StoreError> {
        let mut base = Url::parse(url.trim())
            .map_err(|error| StoreError::Io(format!("not an address: {error}")))?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(StoreError::Io(format!("not a WebDAV address: {url}")));
        }
        base.set_query(None);
        base.set_fragment(None);
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let client = Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(user_agent)
            .build()
            .map_err(|error| StoreError::Io(error.to_string()))?;
        Ok(Self {
            client,
            base,
            username: username.to_owned(),
            password: Zeroizing::new(password.to_owned()),
            listed_etags: AtomicU8::new(ETAGS_UNKNOWN),
        })
    }

    /// Note whether a listing's files carry ETags.
    fn learn(&self, entries: &[Entry]) {
        let mut files = entries.iter().filter(|entry| !entry.collection).peekable();
        if files.peek().is_none() {
            return;
        }
        let listed = if files.all(|entry| entry.etag.is_some()) {
            ETAGS_LISTED
        } else {
            ETAGS_NOT_LISTED
        };
        self.listed_etags.store(listed, Ordering::Relaxed);
    }

    /// Whether this server lists files without ETags: then a file's tag is
    /// its time and size, however it was had.
    fn lists_without_etags(&self) -> bool {
        self.listed_etags.load(Ordering::Relaxed) == ETAGS_NOT_LISTED
    }

    /// The tag of a file read or written: the listing's kind of tag.
    fn tag_as_listed(&self, mut entry: Entry, now: Option<u64>) -> String {
        if self.lists_without_etags() {
            entry.etag = None;
        }
        entry.tag(now)
    }

    /// The address of `path` in the folder, a folder's with a final `/`.
    fn url(&self, path: &str, dir: bool) -> Result<Url, StoreError> {
        let mut url = self.base.clone();
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|()| StoreError::Io("not a WebDAV address".into()))?;
            segments.pop_if_empty();
            for part in path.split('/').filter(|part| !part.is_empty()) {
                if part == "." || part == ".." || part.contains(['\\', '\0']) {
                    return Err(StoreError::Io(format!("not a path in the store: {path}")));
                }
                segments.push(part);
            }
            if dir {
                segments.push("");
            }
        }
        Ok(url)
    }

    fn send(&self, request: RequestBuilder) -> Result<Response, StoreError> {
        request
            .basic_auth(&self.username, Some(self.password.as_str()))
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    StoreError::Timeout
                } else {
                    StoreError::Unavailable(error.to_string())
                }
            })
    }

    /// The entries at `url`, with the server's now; `None` when nothing
    /// is there.
    fn propfind(&self, url: Url, depth: &str) -> Result<Option<Listing>, StoreError> {
        let method = Method::from_bytes(b"PROPFIND").expect("a method name");
        let response = self.send(
            self.client
                .request(method, url)
                .header("Depth", depth)
                .header(CONTENT_TYPE, "application/xml; charset=utf-8")
                .body(PROPFIND_BODY),
        )?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(refused(status));
        }
        let now = server_now(response.headers());
        let body = read_limited(response, MAX_LISTING)?;
        Ok(Some((parse_multistatus(&body)?, now)))
    }

    /// The tag the server gives `path` now, `None` when it is not there.
    fn tag_of(&self, path: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .propfind(self.url(path, false)?, "0")?
            .and_then(|(entries, now)| {
                self.learn(&entries);
                entries
                    .into_iter()
                    .find(|entry| !entry.collection)
                    .map(|entry| entry.tag(now))
            }))
    }

    /// Make the folder at `url`, and the ones above it it needs, the
    /// configured folder's own included: up to `levels` of them.
    fn make_folder(&self, url: &Url, levels: usize) -> Result<(), StoreError> {
        let mkcol = || {
            let method = Method::from_bytes(b"MKCOL").expect("a method name");
            self.send(self.client.request(method, url.clone()))
                .map(|response| response.status())
        };
        let status = mkcol()?;
        // 405: there already. 409: the one above is missing.
        let status = match status {
            StatusCode::CONFLICT if levels > 0 => {
                let above = url
                    .join("..")
                    .map_err(|error| StoreError::Io(error.to_string()))?;
                if above == *url {
                    return Err(refused(status));
                }
                self.make_folder(&above, levels - 1)?;
                mkcol()?
            }
            status => status,
        };
        if status.is_success() || status == StatusCode::METHOD_NOT_ALLOWED {
            Ok(())
        } else {
            Err(refused(status))
        }
    }

    fn put_once(
        &self,
        url: &Url,
        bytes: &[u8],
        cond: &PutCondition,
    ) -> Result<Response, StoreError> {
        let mut request = self
            .client
            .put(url.clone())
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(bytes.to_vec());
        match cond {
            PutCondition::Overwrite => {}
            PutCondition::Absent => request = request.header(IF_NONE_MATCH, "*"),
            PutCondition::Matches(tag) => {
                if let Some(etag) = etag_of(tag) {
                    request = request.header(IF_MATCH, etag);
                }
            }
        }
        self.send(request)
    }
}

impl SyncStore for WebDavStore {
    fn list(&self, dir: String) -> Result<Vec<RemoteFile>, StoreError> {
        let Some((entries, now)) = self.propfind(self.url(&dir, true)?, "1")? else {
            return Ok(Vec::new());
        };
        self.learn(&entries);
        let mut out: Vec<RemoteFile> = entries
            .into_iter()
            .filter(|entry| !entry.collection)
            .filter_map(|entry| {
                Some(RemoteFile {
                    name: last_segment(&entry.href)?,
                    etag: entry.tag(now),
                    size: entry.length.unwrap_or(0),
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn get(&self, path: String, max_bytes: u64) -> Result<Fetched, StoreError> {
        let response = self.send(self.client.get(self.url(&path, false)?))?;
        let status = response.status();
        if !status.is_success() {
            return Err(refused(status));
        }
        let headers = response.headers().clone();
        if header_number(&headers, CONTENT_LENGTH.as_str()).is_some_and(|length| length > max_bytes)
        {
            return Err(StoreError::TooLarge);
        }
        let bytes = read_limited(response, max_bytes)?;
        let entry = Entry {
            href: String::new(),
            collection: false,
            etag: header_text(&headers, ETAG.as_str()),
            length: Some(bytes.len() as u64),
            modified: header_text(&headers, LAST_MODIFIED.as_str()),
        };
        Ok(Fetched {
            etag: self.tag_as_listed(entry, server_now(&headers)),
            bytes,
        })
    }

    fn put(&self, path: String, bytes: Vec<u8>, cond: PutCondition) -> Result<String, StoreError> {
        let url = self.url(&path, false)?;
        // A tag of time and size the server cannot check: checked here,
        // then written, as a folder does.
        if let PutCondition::Matches(expected) = &cond
            && etag_of(expected).is_none()
            && self
                .tag_of(&path)?
                .is_none_or(|now| tag_base(&now) != tag_base(expected))
        {
            return Err(StoreError::PreconditionFailed);
        }
        let mut response = self.put_once(&url, &bytes, &cond)?;
        // Refused on an ETag the file still has: a weak one, which
        // `If-Match` never matches. It is the file asked for.
        if let PutCondition::Matches(expected) = &cond
            && response.status() == StatusCode::PRECONDITION_FAILED
            && etag_of(expected).is_some()
            && self
                .tag_of(&path)?
                .is_some_and(|now| tag_base(&now) == tag_base(expected))
        {
            response = self.put_once(&url, &bytes, &PutCondition::Overwrite)?;
        }
        // 409: a folder it goes in is missing (some servers say 404, and
        // some, as ZSpace's, 412 to `If-None-Match: *`: then the folder is
        // asked for, and the write keeps its condition).
        let folder = url
            .join(".")
            .map_err(|error| StoreError::Io(error.to_string()))?;
        let folder_missing = match response.status() {
            StatusCode::CONFLICT | StatusCode::NOT_FOUND => {
                !matches!(cond, PutCondition::Matches(_))
            }
            StatusCode::PRECONDITION_FAILED => {
                matches!(cond, PutCondition::Absent)
                    && self.propfind(folder.clone(), "0")?.is_none()
            }
            _ => false,
        };
        if folder_missing {
            self.make_folder(&folder, MAX_FOLDER_LEVELS)?;
            response = self.put_once(&url, &bytes, &cond)?;
        }
        let status = response.status();
        if status == StatusCode::NOT_FOUND && matches!(cond, PutCondition::Matches(_)) {
            return Err(StoreError::PreconditionFailed);
        }
        if !status.is_success() {
            return Err(refused(status));
        }
        let etag =
            header_text(response.headers(), ETAG.as_str()).filter(|_| !self.lists_without_etags());
        match etag {
            Some(etag) => Ok(Tag::Etag(strong(&etag)).to_string()),
            // Asked for: a tag made here would not be the listing's.
            None => self
                .tag_of(&path)?
                .ok_or_else(|| StoreError::Io("the file is not there after writing it".into())),
        }
    }

    fn delete(&self, path: String) -> Result<(), StoreError> {
        let status = self
            .send(self.client.delete(self.url(&path, false)?))?
            .status();
        if status.is_success() {
            Ok(())
        } else {
            Err(refused(status))
        }
    }
}

/// A refusal, the same for every backend.
fn refused(status: StatusCode) -> StoreError {
    match status.as_u16() {
        401 | 403 => StoreError::Unauthorized,
        404 => StoreError::NotFound,
        412 => StoreError::PreconditionFailed,
        408 | 504 => StoreError::Timeout,
        429 | 500..=599 => StoreError::Unavailable(format!("HTTP {status}")),
        _ => StoreError::Io(format!("HTTP {status}")),
    }
}

fn read_limited(response: Response, max_bytes: u64) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    response
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| StoreError::Unavailable(error.to_string()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(StoreError::TooLarge);
    }
    Ok(bytes)
}

fn header_text(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn header_number(headers: &HeaderMap, name: &str) -> Option<u64> {
    header_text(headers, name)?.parse().ok()
}

/// The server's now, from its `Date`, in seconds since the epoch.
fn server_now(headers: &HeaderMap) -> Option<u64> {
    parse_http_date(&header_text(headers, DATE.as_str())?)
}

/// A PROPFIND's entries, with the server's now.
type Listing = (Vec<Entry>, Option<u64>);

/// A file or folder of a listing.
#[derive(Debug, Default, PartialEq, Eq)]
struct Entry {
    href: String,
    collection: bool,
    etag: Option<String>,
    length: Option<u64>,
    modified: Option<String>,
}

impl Entry {
    /// The file's tag, its ETag when the server gives one; `now` is the
    /// server's, for a tag made of time and size.
    fn tag(&self, now: Option<u64>) -> String {
        match &self.etag {
            Some(etag) => Tag::Etag(strong(etag)),
            None => {
                let modified = self.modified.clone().unwrap_or_default();
                let size = self.length.unwrap_or(0);
                let racy = match (parse_http_date(&modified), now) {
                    (Some(at), Some(now)) => now.saturating_sub(at) < RACY_SECS,
                    _ => true,
                };
                Tag::Made {
                    modified,
                    size,
                    racy,
                }
            }
        }
        .to_string()
    }
}

/// A tag as this store writes it: `e` and the ETag, or `t` and the time and
/// size it was made of. `~` is escaped, for the rounds' own marker of doubt
/// (`~r`, see the local folder store) to stay unambiguous.
enum Tag {
    Etag(String),
    Made {
        modified: String,
        size: u64,
        racy: bool,
    },
}

/// Gives each doubtful tag its own suffix, so it matches no other.
static RACY_COUNTER: AtomicU64 = AtomicU64::new(0);

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let escape = |text: &str| text.replace('%', "%25").replace('~', "%7E");
        match self {
            Self::Etag(etag) => write!(f, "e:{}", escape(etag)),
            Self::Made {
                modified,
                size,
                racy,
            } => {
                write!(f, "t:{}:{size}", escape(modified))?;
                if *racy {
                    write!(f, "~r{}", RACY_COUNTER.fetch_add(1, Ordering::Relaxed))?;
                }
                Ok(())
            }
        }
    }
}

/// An ETag without the mark of a weak one: the same file, sure or not.
fn strong(etag: &str) -> String {
    etag.strip_prefix("W/").unwrap_or(etag).to_owned()
}

/// The tag of an ETag, as the S3 store makes them too.
pub(super) fn etag_tag(etag: &str) -> String {
    Tag::Etag(strong(etag)).to_string()
}

/// The ETag to send as `If-Match`, for a tag made of one.
pub(super) fn etag_of(tag: &str) -> Option<String> {
    let escaped = tag.strip_prefix("e:")?;
    Some(escaped.replace("%7E", "~").replace("%25", "%"))
}

/// The entries of a PROPFIND answer. Names are matched without their
/// namespace prefix, which servers choose freely (`d:`, `D:`, `lp1:`, none);
/// properties count only from a propstat whose status is 200.
fn parse_multistatus(xml: &[u8]) -> Result<Vec<Entry>, StoreError> {
    let bad = |error: &dyn fmt::Display| StoreError::Io(format!("an unreadable listing: {error}"));
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut entries = Vec::new();
    let mut entry: Option<Entry> = None;
    // A response may say it failed as a whole.
    let mut response_ok = true;
    // The current propstat's properties, kept until its status is known.
    let mut found = Entry::default();
    let mut status_ok = false;
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut text = String::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|error| bad(&error))?
        {
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_ascii_lowercase();
                match name.as_slice() {
                    b"response" => {
                        entry = Some(Entry::default());
                        response_ok = true;
                    }
                    b"propstat" => {
                        found = Entry::default();
                        status_ok = false;
                    }
                    _ => {}
                }
                path.push(name);
                text.clear();
            }
            Event::Empty(empty) => {
                let name = empty.local_name().as_ref().to_ascii_lowercase();
                if name == b"collection" && path.iter().any(|part| part == b"resourcetype") {
                    found.collection = true;
                }
            }
            Event::Text(chunk) => text.push_str(&chunk.decode().map_err(|error| bad(&error))?),
            Event::CData(chunk) => text.push_str(&chunk.decode().map_err(|error| bad(&error))?),
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|error| bad(&error))?;
                let raw = format!("&{name};");
                match quick_xml::escape::unescape(&raw) {
                    Ok(resolved) => text.push_str(&resolved),
                    Err(_) => text.push_str(&raw),
                }
            }
            Event::End(_) => {
                let Some(name) = path.pop() else {
                    continue;
                };
                let value = std::mem::take(&mut text);
                let value = value.trim();
                match name.as_slice() {
                    b"href" if path.last().is_some_and(|parent| parent == b"response") => {
                        if let Some(entry) = &mut entry {
                            entry.href = value.to_owned();
                        }
                    }
                    b"status" if path.last().is_some_and(|parent| parent == b"propstat") => {
                        status_ok = value.split_whitespace().nth(1) == Some("200");
                    }
                    b"status" if path.last().is_some_and(|parent| parent == b"response") => {
                        response_ok = value.split_whitespace().nth(1) == Some("200");
                    }
                    b"getetag" if !value.is_empty() => found.etag = Some(value.to_owned()),
                    b"getcontentlength" => found.length = value.parse().ok(),
                    b"getlastmodified" if !value.is_empty() => {
                        found.modified = Some(value.to_owned());
                    }
                    b"propstat" => {
                        if let Some(entry) = &mut entry
                            && status_ok
                        {
                            let found = std::mem::take(&mut found);
                            entry.collection |= found.collection;
                            entry.etag = entry.etag.take().or(found.etag);
                            entry.length = entry.length.or(found.length);
                            entry.modified = entry.modified.take().or(found.modified);
                        }
                    }
                    b"response" => entries.extend(
                        entry
                            .take()
                            .filter(|entry| response_ok && !entry.href.is_empty()),
                    ),
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(entries)
}

/// The name a link ends with, decoded: `/dav/a%20b/x.srs` and
/// `https://host/dav/a b/x.srs` both end with `x.srs`. `None` for a link to
/// a folder's own address or one that does not decode.
fn last_segment(href: &str) -> Option<String> {
    let path = match href.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |at| &rest[at..]),
        None => href,
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    if path.ends_with('/') {
        return None;
    }
    let raw = path.rsplit('/').next().filter(|name| !name.is_empty())?;
    percent_decode(raw)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'%' {
            let hex = text.get(ix + 1..ix + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            ix += 3;
        } else {
            out.push(bytes[ix]);
            ix += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// An HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`) in seconds since the
/// epoch. Only this form: the others are obsolete, and a date that does
/// not read only makes a tag doubtful.
pub(super) fn parse_http_date(text: &str) -> Option<u64> {
    let mut parts = text.split_whitespace().skip(1);
    let day: u64 = parts.next()?.parse().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts.next()?.parse().ok()?;
    let mut clock = parts
        .next()?
        .split(':')
        .map(|part| part.parse::<u64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    if parts.next()? != "GMT" || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60
    {
        return None;
    }
    // Days from the civil date, after Howard Hinnant's algorithm.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days)
        .ok()
        .map(|days| days * 86_400 + hour * 3600 + minute * 60 + second)
}

#[cfg(test)]
#[path = "webdav_tests.rs"]
mod tests;
