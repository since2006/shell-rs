//! 凭据: saved logins hosts share. The list the left dock shows and the
//! dialog that edits one; the credentials themselves, and how a host logs in
//! with one, live in `session`.

mod credential_dialog;
mod credential_panel;

pub use credential_dialog::{CredentialForm, open_credential_dialog};
pub use credential_panel::{CredentialPanel, kind_icon};
