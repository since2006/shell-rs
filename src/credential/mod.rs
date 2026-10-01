//! 凭据: saved logins hosts share. The list the left dock shows and the
//! dialog that edits one, which also takes pasted keys and generates new
//! ones; the credentials themselves, the keys ShellRS keeps, and how a host
//! logs in with one live in `host`.

mod credential_dialog;
mod credential_panel;

pub use credential_dialog::{CredentialDialog, CredentialForm, open_credential_dialog};
pub use credential_panel::{CredentialPanel, kind_icon};
