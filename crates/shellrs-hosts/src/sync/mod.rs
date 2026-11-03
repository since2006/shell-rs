//! 同步: the hosts and the rest kept alike on the user's devices through a
//! place they choose (a folder a cloud drive syncs, a WebDAV server, an S3
//! bucket), with `shellrs_sync` doing the merging and the encryption.
//!
//! [`run_round`] runs one round against the app's data; the jobs that set
//! sync up, join it, change its password and turn it off are the blocking
//! functions in [`worker`]. When to sync is the app's to say.

mod error;
mod location;
mod round;
mod setup;
pub mod worker;

pub use error::JobError;
pub use location::SyncLocation;
pub use round::{Attention, Blocking, OpenStore, RoundEnd, RoundHost, SyncServices, run_round};
pub use setup::{
    LoginError, MIN_PASSWORD, PasswordError, S3Form, WebDavForm, check_new_password,
    check_password, random_password,
};
