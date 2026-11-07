//! The WebDAV store: what servers answer, read without a server; then the
//! store against a real one (the `server` tests, ignored by default): an
//! Apache with mod_dav started for them, as CI runs them and a computer
//! with Apache can (macOS's own included), or any server one names.

use super::*;

/// Apache's answer for a folder with one file.
const APACHE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:ns0="DAV:">
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://apache.org/dav/props/">
<D:href>/sync/shellrs-sync/v1/devices/</D:href>
<D:propstat>
<D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:getlastmodified>Sat, 25 Oct 2026 10:00:00 GMT</lp1:getlastmodified>
<lp1:getetag>"1000-63f1"</lp1:getetag>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
<D:propstat>
<D:prop><getcontentlength/></D:prop>
<D:status>HTTP/1.1 404 Not Found</D:status>
</D:propstat>
</D:response>
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://apache.org/dav/props/">
<D:href>/sync/shellrs-sync/v1/devices/k3jq8x0m2pa7d4vn.srs</D:href>
<D:propstat>
<D:prop>
<lp1:resourcetype/>
<lp1:getcontentlength>1234</lp1:getcontentlength>
<lp1:getlastmodified>Sat, 25 Oct 2026 10:00:00 GMT</lp1:getlastmodified>
<lp1:getetag>"4d2-63f1a2b3c4d5e"</lp1:getetag>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
</D:response>
</D:multistatus>"#;

/// Nextcloud's: whole URLs, a default namespace, quotes escaped, a name
/// percent-encoded.
const NEXTCLOUD: &str = r#"<?xml version="1.0"?>
<multistatus xmlns="DAV:" xmlns:oc="http://owncloud.org/ns">
<response><href>https://cloud.example/remote.php/dav/files/me/My%20Sync/</href>
<propstat><prop><resourcetype><collection/></resourcetype><getetag>&quot;68f0&quot;</getetag></prop>
<status>HTTP/1.1 200 OK</status></propstat></response>
<response><href>https://cloud.example/remote.php/dav/files/me/My%20Sync/a%26b.srs</href>
<propstat><prop><resourcetype/><getetag>&quot;5f8a&amp;1&quot;</getetag><getcontentlength>7</getcontentlength></prop>
<status>HTTP/1.1 200 OK</status></propstat></response>
</multistatus>"#;

/// A server without ETags, and one response that failed as a whole.
const PLAIN: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
<d:response><d:href>/dav/x.srs</d:href>
<d:propstat><d:prop><d:getcontentlength>9</d:getcontentlength>
<d:getlastmodified>Sat, 25 Oct 2026 10:00:00 GMT</d:getlastmodified></d:prop>
<d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
<d:response><d:href>/dav/gone.srs</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>
</d:multistatus>"#;

#[test]
fn listings_read_alike_from_every_server() {
    let apache = parse_multistatus(APACHE.as_bytes()).unwrap();
    assert_eq!(apache.len(), 2);
    assert!(apache[0].collection);
    assert_eq!(
        apache[1],
        Entry {
            href: "/sync/shellrs-sync/v1/devices/k3jq8x0m2pa7d4vn.srs".into(),
            collection: false,
            etag: Some(r#""4d2-63f1a2b3c4d5e""#.into()),
            length: Some(1234),
            modified: Some("Sat, 25 Oct 2026 10:00:00 GMT".into()),
        }
    );

    let nextcloud = parse_multistatus(NEXTCLOUD.as_bytes()).unwrap();
    assert!(nextcloud[0].collection);
    assert_eq!(nextcloud[1].etag.as_deref(), Some(r#""5f8a&1""#));
    assert_eq!(last_segment(&nextcloud[1].href).as_deref(), Some("a&b.srs"));
    assert_eq!(last_segment(&nextcloud[0].href), None);

    let plain = parse_multistatus(PLAIN.as_bytes()).unwrap();
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].etag, None);
}

#[test]
fn an_etag_goes_back_to_the_server_as_it_came() {
    let entry = Entry {
        etag: Some(r#""a~r1%""#.into()),
        ..Entry::default()
    };
    let tag = entry.tag(None);
    // The rounds' marker of doubt does not show in it.
    assert_eq!(tag_base(&tag), tag);
    assert_eq!(etag_of(&tag).as_deref(), Some(r#""a~r1%""#));
    // Weak or strong, the same file.
    let weak = Entry {
        etag: Some(r#"W/"a~r1%""#.into()),
        ..Entry::default()
    };
    assert_eq!(weak.tag(None), tag);
}

#[test]
fn a_tag_of_time_and_size_is_in_doubt_while_recent() {
    let entry = Entry {
        length: Some(9),
        modified: Some("Sat, 25 Oct 2026 10:00:00 GMT".into()),
        ..Entry::default()
    };
    let at = parse_http_date("Sat, 25 Oct 2026 10:00:00 GMT").unwrap();
    let settled = entry.tag(Some(at + 60));
    assert_eq!(settled, entry.tag(Some(at + 61)));
    assert_eq!(etag_of(&settled), None);
    // Within seconds of it, it matches no other, its base the same.
    let (one, two) = (entry.tag(Some(at + 1)), entry.tag(Some(at + 1)));
    assert_ne!(one, two);
    assert_eq!(tag_base(&one), settled);
    assert_ne!(entry.tag(None), entry.tag(None));
}

#[test]
fn http_dates_read() {
    assert_eq!(
        parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
        Some(784_111_777)
    );
    assert_eq!(
        parse_http_date("Thu, 29 Feb 2024 23:59:59 GMT"),
        Some(1_709_251_199)
    );
    for text in [
        "",
        "Sunday, 06-Nov-94 08:49:37 GMT",
        "Sun, 06 Nov 1994 08:49:37 PST",
        "Sun, 32 Nov 1994 08:49:37 GMT",
    ] {
        assert_eq!(parse_http_date(text), None, "{text}");
    }
}

#[test]
fn an_address_is_a_folder_and_paths_stay_in_it() {
    let store = WebDavStore::new("https://dav.example/dav/My Sync", "me", "pw", "test").unwrap();
    assert_eq!(store.base.as_str(), "https://dav.example/dav/My%20Sync/");
    assert_eq!(
        store.url("shellrs-sync/v1/devices", true).unwrap().as_str(),
        "https://dav.example/dav/My%20Sync/shellrs-sync/v1/devices/"
    );
    assert_eq!(
        store
            .url("shellrs-sync/v1/vault.json", false)
            .unwrap()
            .as_str(),
        "https://dav.example/dav/My%20Sync/shellrs-sync/v1/vault.json"
    );
    assert!(store.url("../x", false).is_err());
    for url in ["ftp://dav.example/", "dav.example/dav", "https://"] {
        assert!(WebDavStore::new(url, "me", "pw", "test").is_err(), "{url}");
    }
    assert!(!format!("{store:?}").contains("pw"));
}

#[test]
fn refusals_say_what_to_do() {
    assert_eq!(refused(StatusCode::UNAUTHORIZED), StoreError::Unauthorized);
    assert_eq!(refused(StatusCode::FORBIDDEN), StoreError::Unauthorized);
    assert_eq!(
        refused(StatusCode::PRECONDITION_FAILED),
        StoreError::PreconditionFailed
    );
    assert!(matches!(
        refused(StatusCode::SERVICE_UNAVAILABLE),
        StoreError::Unavailable(_)
    ));
    assert!(matches!(
        refused(StatusCode::TOO_MANY_REQUESTS),
        StoreError::Unavailable(_)
    ));
    assert_eq!(refused(StatusCode::GATEWAY_TIMEOUT), StoreError::Timeout);
}

#[test]
fn a_server_that_lists_no_etags_gets_tags_of_time_and_size_throughout() {
    let store = WebDavStore::new("https://dav.example/dav", "me", "pw", "test").unwrap();
    let modified = "Sat, 25 Oct 2026 10:00:00 GMT";
    let at = parse_http_date(modified).unwrap() + 60;
    // A file read, its ETag in the response's headers.
    let read = || Entry {
        etag: Some(r#""5f-9""#.into()),
        length: Some(9),
        modified: Some(modified.into()),
        ..Entry::default()
    };
    // Until a listing says otherwise, the ETag is the tag.
    assert!(store.tag_as_listed(read(), Some(at)).starts_with("e:"));

    // Listed without, as nginx lists: the tag is the listing's own.
    let listed = parse_multistatus(PLAIN.as_bytes()).unwrap();
    store.learn(&listed);
    assert_eq!(
        store.tag_as_listed(read(), Some(at)),
        listed[0].tag(Some(at))
    );
    // A listing of folders only tells nothing.
    store.learn(&parse_multistatus(APACHE.as_bytes()).unwrap()[..1]);
    assert!(store.tag_as_listed(read(), Some(at)).starts_with("t:"));
    // A listing with ETags makes them the tags again.
    store.learn(&parse_multistatus(APACHE.as_bytes()).unwrap());
    assert!(store.tag_as_listed(read(), Some(at)).starts_with("e:"));
}

/// A server as ZSpace's WebDAV (webdav_rs) answers: a write with
/// `If-None-Match: *` into a folder that is not there is refused with 412,
/// not 409. In memory, on a port of its own; a connection per request.
mod zspace {
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub struct State {
        pub folders: BTreeSet<String>,
        /// Each file's content and the ETag it has.
        pub files: BTreeMap<String, (Vec<u8>, String)>,
        /// Every request: method, path, whether it had `If-None-Match`.
        pub log: Vec<(String, String, bool)>,
        writes: u64,
    }

    struct Request {
        method: String,
        path: String,
        if_none_match: bool,
        if_match: Option<String>,
        depth: String,
        body: Vec<u8>,
    }

    fn parent(path: &str) -> String {
        let trimmed = path.trim_end_matches('/');
        match trimmed.rfind('/') {
            Some(at) => trimmed[..=at].to_owned(),
            None => "/".to_owned(),
        }
    }

    fn entry(path: &str, file: Option<&(Vec<u8>, String)>) -> String {
        let prop = match file {
            Some((bytes, etag)) => format!(
                "<d:resourcetype/><d:getetag>{etag}</d:getetag><d:getcontentlength>{}</d:getcontentlength>",
                bytes.len()
            ),
            None => "<d:resourcetype><d:collection/></d:resourcetype>".to_owned(),
        };
        format!(
            "<d:response><d:href>{path}</d:href><d:propstat><d:prop>{prop}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        )
    }

    /// Status, headers and body for `request`.
    fn answer(state: &mut State, request: Request) -> (u16, String, Vec<u8>) {
        let Request {
            method,
            path,
            if_none_match,
            if_match,
            depth,
            body,
        } = request;
        state
            .log
            .push((method.clone(), path.clone(), if_none_match));
        let folder_there = state.folders.contains(&parent(&path));
        let existing = state.files.get(&path).map(|(_, etag)| etag.clone());
        match method.as_str() {
            "PUT" if if_none_match && (!folder_there || existing.is_some()) => {
                (412, "".into(), vec![])
            }
            "PUT" if if_match.is_some() && if_match != existing => (412, "".into(), vec![]),
            "PUT" if !folder_there => (409, "".into(), vec![]),
            "PUT" => {
                state.writes += 1;
                let etag = format!("\"{}\"", state.writes);
                state.files.insert(path, (body, etag.clone()));
                (201, format!("ETag: {etag}\r\n"), vec![])
            }
            "GET" => match state.files.get(&path) {
                Some((bytes, etag)) => (200, format!("ETag: {etag}\r\n"), bytes.clone()),
                None => (404, "".into(), vec![]),
            },
            "DELETE" => match state.files.remove(&path) {
                Some(_) => (204, "".into(), vec![]),
                None => (404, "".into(), vec![]),
            },
            "MKCOL" if state.folders.contains(&path) => (405, "".into(), vec![]),
            "MKCOL" if !folder_there => (409, "".into(), vec![]),
            "MKCOL" => {
                state.folders.insert(path);
                (201, "".into(), vec![])
            }
            "PROPFIND" => {
                let mut entries = Vec::new();
                if let Some(file) = state.files.get(&path) {
                    entries.push(entry(&path, Some(file)));
                } else if state.folders.contains(&path) {
                    entries.push(entry(&path, None));
                    if depth == "1" {
                        for (name, file) in &state.files {
                            if parent(name) == path {
                                entries.push(entry(name, Some(file)));
                            }
                        }
                        for name in &state.folders {
                            if name != &path && parent(name) == path {
                                entries.push(entry(name, None));
                            }
                        }
                    }
                } else {
                    return (404, "".into(), vec![]);
                }
                let xml = format!(
                    r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">{}</d:multistatus>"#,
                    entries.concat()
                );
                (207, "".into(), xml.into_bytes())
            }
            _ => (405, "".into(), vec![]),
        }
    }

    /// The server's address and what it holds; `/dav/` is there to begin with.
    pub fn start() -> (String, Arc<Mutex<State>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/dav", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        state.lock().unwrap().folders.insert("/dav/".into());
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                let (mut length, mut if_none_match, mut if_match) = (0, false, None);
                let mut depth = String::from("0");
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    let (name, value) = header.split_once(':').unwrap_or((&header, ""));
                    let value = value.trim().to_owned();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.parse().unwrap_or(0),
                        "if-none-match" => if_none_match = true,
                        "if-match" => if_match = Some(value),
                        "depth" => depth = value,
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let request = Request {
                    method,
                    path,
                    if_none_match,
                    if_match,
                    depth,
                    body,
                };
                let (status, headers, body) = answer(&mut shared.lock().unwrap(), request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        (url, state)
    }
}

#[test]
fn a_write_refused_with_412_for_a_missing_folder_makes_the_folder_and_keeps_its_condition() {
    let (url, state) = zspace::start();
    let store = WebDavStore::new(&url, "me", "pw", "test").unwrap();

    // The vault, as `create_vault` writes it: into folders not there yet.
    store
        .put(
            "shellrs-sync/v1/vault.json".into(),
            b"1".to_vec(),
            PutCondition::Absent,
        )
        .unwrap();
    {
        let state = state.lock().unwrap();
        assert!(state.files.contains_key("/dav/shellrs-sync/v1/vault.json"));
        // Never written without its condition.
        let puts: Vec<_> = state.log.iter().filter(|(m, ..)| m == "PUT").collect();
        assert!(puts.iter().all(|(.., inm)| *inm), "{:?}", state.log);
    }

    // There now: the same write is refused, as another device's would be.
    assert_eq!(
        store.put(
            "shellrs-sync/v1/vault.json".into(),
            b"2".to_vec(),
            PutCondition::Absent,
        ),
        Err(StoreError::PreconditionFailed)
    );
    // And one into a folder that is there writes, then is refused.
    store
        .put(
            "shellrs-sync/v1/other".into(),
            b"3".to_vec(),
            PutCondition::Absent,
        )
        .unwrap();
    assert_eq!(
        store.put(
            "shellrs-sync/v1/other".into(),
            b"4".to_vec(),
            PutCondition::Absent
        ),
        Err(StoreError::PreconditionFailed)
    );
}

/// On such a server, one device sets sync up, a second joins the same
/// folder, and both end up alike: the second's first upload goes into
/// `devices/`, a folder that is not there until a device writes in it.
#[test]
fn devices_sync_through_a_server_that_refuses_with_412_for_missing_folders() {
    use crate::testing::{Device, password};
    use crate::vault::tests::FAST;
    use crate::{FixedEnv, RecordData, SyncError, create_vault, unlock_vault};

    let (url, state) = zspace::start();
    let store = WebDavStore::new(&url, "me", "pw", "test").unwrap();
    let keys = create_vault(&store, "correct horse", FAST, &mut FixedEnv::new(0, 3)).unwrap();
    // Set up there already: making it again says so.
    assert_eq!(
        create_vault(&store, "correct horse", FAST, &mut FixedEnv::new(0, 4)).unwrap_err(),
        SyncError::VaultExists
    );

    // The second device joins with the password.
    let joined = unlock_vault(&store, "correct horse").unwrap();
    let mut a = Device::new('a', &keys, 1_800_000_000_000);
    let mut b = Device::new('b', &joined, 1_800_000_000_000);
    let (id, secret) = password("root", "web", "pw-web");
    a.put(&id, secret);
    a.put("g1", crate::testing::group("ops", None));
    b.put("g2", crate::testing::group("dev", None));

    a.sync(&store);
    b.sync(&store);
    a.sync(&store);
    assert_eq!(b.data(), a.data());
    assert_eq!(a.data().len(), 3);
    assert!(
        b.data()
            .values()
            .any(|data| matches!(data, RecordData::Password(_)))
    );
    let state = state.lock().unwrap();
    let devices: Vec<_> = state
        .files
        .keys()
        .filter(|name| name.starts_with("/dav/shellrs-sync/v1/devices/"))
        .collect();
    assert_eq!(devices.len(), 2, "{devices:?}");
}

/// The store against a real server, on Unix: an Apache started for the
/// test, or the server `SHELLRS_TEST_WEBDAV_URL` names, signed in to as
/// `SHELLRS_TEST_WEBDAV_USER` with `SHELLRS_TEST_WEBDAV_PASSWORD`. There,
/// each test goes in a folder of its own, and what it writes stays.
#[cfg(unix)]
mod server {
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::process::CommandExt as _;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};

    use super::*;
    use crate::testing::{Device, password};
    use crate::vault::tests::FAST;
    use crate::{RecordData, create_vault, layout};

    const USER: &str = "sync";
    const PASSWORD: &str = "pass word";

    enum Server {
        Apache(Apache),
        Named {
            url: String,
            user: String,
            password: String,
        },
    }

    impl Server {
        /// The named server, or else an Apache.
        fn start() -> Self {
            match std::env::var("SHELLRS_TEST_WEBDAV_URL") {
                Ok(url) => Self::Named {
                    url: url.trim_end_matches('/').to_owned(),
                    user: std::env::var("SHELLRS_TEST_WEBDAV_USER").unwrap_or_default(),
                    password: std::env::var("SHELLRS_TEST_WEBDAV_PASSWORD").unwrap_or_default(),
                },
                Err(_) => Self::Apache(Apache::start(true)),
            }
        }

        fn store(&self, folder: &str) -> WebDavStore {
            let password = match self {
                Self::Apache(_) => PASSWORD,
                Self::Named { password, .. } => password,
            };
            self.store_as(folder, password)
        }

        /// The store in `folder`, one of this run's own on a named server.
        fn store_as(&self, folder: &str, password: &str) -> WebDavStore {
            let (url, user) = match self {
                Self::Apache(apache) => {
                    (format!("http://127.0.0.1:{}/{folder}", apache.port), USER)
                }
                Self::Named { url, user, .. } => (
                    format!("{url}/shellrs-test-{}/{folder}", std::process::id()),
                    user.as_str(),
                ),
            };
            WebDavStore::new(&url, user, password, "shellrs-sync tests").unwrap()
        }

        /// A file on disk, when the server's files are this computer's.
        fn file(&self, path: &str) -> Option<PathBuf> {
            match self {
                Self::Apache(apache) => Some(apache.dir.path().join("root").join(path)),
                Self::Named { .. } => None,
            }
        }
    }

    /// Apache with mod_dav on a port of its own, in a folder of its own, with
    /// one user. `SHELLRS_TEST_HTTPD`, `SHELLRS_TEST_HTTPD_MODULES` and
    /// `SHELLRS_TEST_HTPASSWD` name its programs where they are not where
    /// macOS and Debian put them.
    struct Apache {
        child: Child,
        dir: tempfile::TempDir,
        port: u16,
    }

    fn found(variable: &str, candidates: &[&str]) -> PathBuf {
        if let Ok(path) = std::env::var(variable) {
            return path.into();
        }
        candidates
            .iter()
            .map(PathBuf::from)
            .find(|path| path.exists())
            .unwrap_or_else(|| panic!("no {candidates:?}; set {variable}"))
    }

    impl Apache {
        /// Without `etags`, it gives none, neither listing nor reading.
        fn start(etags: bool) -> Self {
            let httpd = found(
                "SHELLRS_TEST_HTTPD",
                &["/usr/sbin/httpd", "/usr/sbin/apache2"],
            );
            let modules = found(
                "SHELLRS_TEST_HTTPD_MODULES",
                &["/usr/libexec/apache2", "/usr/lib/apache2/modules"],
            );
            let htpasswd = found(
                "SHELLRS_TEST_HTPASSWD",
                &["/usr/sbin/htpasswd", "/usr/bin/htpasswd"],
            );
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("root")).unwrap();
            let made = Command::new(htpasswd)
                .arg("-bc")
                .arg(dir.path().join("users"))
                .args([USER, PASSWORD])
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(made.success());
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let load = |name: &str, file: &str| {
                format!(
                    "<IfModule !{name}>\nLoadModule {name} \"{}\"\n</IfModule>\n",
                    modules.join(file).display()
                )
            };
            let root = dir.path().display();
            let file_etag = if etags { "" } else { "FileETag None\n" };
            let config = format!(
                "ServerRoot \"{root}\"\nListen 127.0.0.1:{port}\nServerName 127.0.0.1\n\
                 PidFile \"{root}/httpd.pid\"\nErrorLog \"{root}/error.log\"\n{}{}{}{}{}{}{}{}{}\
                 DavLockDB \"{root}/davlock\"\nDocumentRoot \"{root}/root\"\n\
                 <Directory \"{root}/root\">\nDav On\n{file_etag}AuthType Basic\nAuthName shellrs\n\
                 AuthUserFile \"{root}/users\"\nRequire valid-user\n</Directory>\n",
                load("mpm_prefork_module", "mod_mpm_prefork.so"),
                load("unixd_module", "mod_unixd.so"),
                load("authn_core_module", "mod_authn_core.so"),
                load("authn_file_module", "mod_authn_file.so"),
                load("authz_core_module", "mod_authz_core.so"),
                load("authz_user_module", "mod_authz_user.so"),
                load("auth_basic_module", "mod_auth_basic.so"),
                load("dav_module", "mod_dav.so"),
                load("dav_fs_module", "mod_dav_fs.so"),
            );
            let conf = dir.path().join("httpd.conf");
            std::fs::write(&conf, config).unwrap();
            // In a process group of its own: stopping, Apache signals its
            // whole group, which would be the test's.
            let child = Command::new(httpd)
                .arg("-f")
                .arg(&conf)
                .args(["-D", "FOREGROUND"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap();
            let apache = Self { child, dir, port };
            for _ in 0..200 {
                if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    return apache;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let log =
                std::fs::read_to_string(apache.dir.path().join("error.log")).unwrap_or_default();
            panic!("Apache did not start: {log}");
        }
    }

    impl Drop for Apache {
        /// Stopped as Apache stops, so prefork's children go too.
        fn drop(&mut self) {
            let _ = Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .stderr(Stdio::null())
                .status();
            for _ in 0..80 {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    #[ignore = "needs a WebDAV server"]
    fn a_server_holds_files_as_every_store_does() {
        let server = Server::start();
        let store = server.store("files");
        assert_eq!(store.list("shellrs-sync/v1/devices".into()), Ok(Vec::new()));

        // The folders a file goes in are made for it, the store's own too.
        let first = store
            .put(
                "a/b/c.srs".into(),
                b"hello".to_vec(),
                PutCondition::Overwrite,
            )
            .unwrap();
        if let Some(file) = server.file("files/a/b/c.srs") {
            assert!(file.exists());
        }
        let fetched = store.get("a/b/c.srs".into(), 100).unwrap();
        assert_eq!(fetched.bytes, b"hello");
        assert_eq!(tag_base(&fetched.etag), tag_base(&first));
        let listed = store.list("a/b".into()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].name.as_str(), listed[0].size), ("c.srs", 5));
        assert_eq!(tag_base(&listed[0].etag), tag_base(&fetched.etag));
        assert_eq!(store.get("a/b/c.srs".into(), 2), Err(StoreError::TooLarge));
        assert_eq!(store.get("a/b/none".into(), 2), Err(StoreError::NotFound));

        store
            .put("a/b/c.srs".into(), b"4".to_vec(), PutCondition::Overwrite)
            .unwrap();
        assert_eq!(store.get("a/b/c.srs".into(), 9).unwrap().bytes, b"4");
        store.delete("a/b/c.srs".into()).unwrap();
        assert_eq!(store.delete("a/b/c.srs".into()), Err(StoreError::NotFound));
        assert_eq!(store.list("a/b".into()), Ok(Vec::new()));

        assert_eq!(
            server.store_as("files", "wrong").list("a".into()),
            Err(StoreError::Unauthorized)
        );
    }

    /// What a server that keeps the conditions of writes keeps. One that
    /// does not still syncs (each device writes only its own file); this
    /// tells which kind it is.
    #[test]
    #[ignore = "needs a WebDAV server"]
    fn a_server_keeps_the_conditions_of_writes() {
        let server = Server::start();
        let store = server.store("conditions");
        let first = store
            .put("f".into(), b"1".to_vec(), PutCondition::Absent)
            .unwrap();
        assert_eq!(
            store.put("f".into(), b"2".to_vec(), PutCondition::Absent),
            Err(StoreError::PreconditionFailed)
        );
        let second = store
            .put(
                "f".into(),
                b"22".to_vec(),
                PutCondition::Matches(first.clone()),
            )
            .unwrap();
        assert_ne!(tag_base(&second), tag_base(&first));
        assert_eq!(
            store.put("f".into(), b"3".to_vec(), PutCondition::Matches(first)),
            Err(StoreError::PreconditionFailed)
        );
        assert_eq!(
            store.put("g".into(), b"3".to_vec(), PutCondition::Matches(second)),
            Err(StoreError::PreconditionFailed)
        );
        assert_eq!(store.get("f".into(), 9).unwrap().bytes, b"22");
    }

    /// Two devices end up alike, and once nothing changes a round reads
    /// no file again. `settle` waits out tags in doubt first, for servers
    /// whose files go by time and size.
    fn devices_sync_through(server: &Server, folder: &str, settle: Duration) {
        let store = server.store(folder);
        let keys = create_vault(
            &store,
            "correct horse",
            FAST,
            &mut crate::FixedEnv::new(0, 3),
        )
        .unwrap();
        let mut a = Device::new('a', &keys, 1_800_000_000_000);
        let mut b = Device::new('b', &keys, 1_800_000_000_000);
        let (id, secret) = password("root", "web", "pw-web");
        a.put(&id, secret);
        a.put("g1", crate::testing::group("ops", None));
        a.sync(&store);
        b.sync(&store);
        assert_eq!(b.data(), a.data());
        assert!(
            b.data()
                .values()
                .any(|data| matches!(data, RecordData::Password(_)))
        );
        if let Some(file) = server.file(&format!(
            "{folder}/{}",
            layout::device_file(a.state.device())
        )) {
            assert!(file.exists());
        }

        std::thread::sleep(settle);
        a.sync(&store);
        a.wait(1000);
        let (report, _) = a.sync(&store);
        assert_eq!(report.downloaded, 0);
    }

    #[test]
    #[ignore = "needs a WebDAV server"]
    fn devices_sync_through_a_server() {
        let server = Server::start();
        devices_sync_through(&server, "ShellRS 同步", Duration::from_secs(4));
    }

    /// A server that gives no ETags at all: tags of time and size, the
    /// writes' conditions checked here.
    #[test]
    #[ignore = "needs Apache"]
    fn devices_sync_through_apache_without_etags() {
        if std::env::var("SHELLRS_TEST_WEBDAV_URL").is_ok() {
            return;
        }
        let server = Server::Apache(Apache::start(false));
        let store = server.store("plain");
        store
            .put("f".into(), b"1".to_vec(), PutCondition::Absent)
            .unwrap();
        let listed = store.list(String::new()).unwrap();
        assert!(listed[0].etag.starts_with("t:"), "{listed:?}");
        devices_sync_through(&server, "plain", Duration::from_secs(4));
    }
}
