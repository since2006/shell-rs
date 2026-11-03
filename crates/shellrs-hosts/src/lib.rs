//! ShellRS's hosts core: the saved hosts and groups, credentials, port
//! forwarding rules, command snippets and the local terminals of 项目;
//! the SQLite database they are written through to; the secrets' place in
//! the keychain; and sync between devices.
//!
//! [`HostData`] holds it all in memory, which is what counts, and writes
//! each change through to the database. It says nothing in words and
//! notifies no one: failures and changes queue up as typed [`HostEvent`]s,
//! and keychain writes as [`SecretJob`]s, for the app to take, word in its
//! own language and run off the UI thread. The desktop app wraps it in a
//! GPUI entity; the mobile apps will reach it through their bindings.

mod credential;
mod data;
mod database;
mod forward;
mod login;
mod mark;
mod model;
mod node;
mod private_key;
mod protocol;
mod saved_terminal;
pub mod secrets;
mod serial_settings;
mod snippet;
pub mod sync;

pub use credential::*;
pub use data::{
    ApplyError, BookmarkMeta, Change, Failure, HostData, HostEvent, KeyFileError, SecretEndpoint,
    SecretError, SecretJob, SecretOutcome, SecretPlace, SecretSource, SecretWork, SecretsRead,
    SecretsWritten, SyncExport, read_secrets,
};
pub use database::{HostDatabase, OpenError, StoredData};
pub use forward::*;
pub use gpui_shared_string::SharedString;
pub use login::{
    DEFAULT_KEY_FILES, HostConnection, HostLogin, HostLoginExt, JumpLogin, LoginMethod, LoginRoute,
    ProxyLogin, SerialLine, TelnetAutoLogin, TelnetLogin,
};
pub use mark::mark_color;
pub use model::*;
pub use node::{HostNode, NodeDrop};
pub use private_key::{
    GeneratedKey, KeyAlgorithm, PastedKey, PastedKeyError, read_public_key, read_small,
};
pub use protocol::{Protocol, ProtocolKind, TelnetSettings};
pub use saved_terminal::*;
pub use serial_settings::*;
pub use snippet::*;
