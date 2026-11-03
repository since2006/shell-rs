//! What setting sync up asks of the user, checked: a sync password, and
//! the login of a WebDAV server or an S3 bucket as typed.

use rand::Rng as _;
use zeroize::Zeroizing;

use super::SyncLocation;

/// The shortest new sync password, in characters: it guards every secret
/// in the folder against anyone who gets a copy of it.
pub const MIN_PASSWORD: usize = 12;

/// What is wrong with a sync password as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasswordError {
    Missing,
    /// A new one shorter than [`MIN_PASSWORD`].
    TooShort,
    /// A new one typed differently the second time.
    Mismatch,
}

/// Check a new sync password, typed twice.
pub fn check_new_password(password: &str, repeat: &str) -> Result<(), PasswordError> {
    if password.chars().count() < MIN_PASSWORD {
        Err(PasswordError::TooShort)
    } else if repeat != password {
        Err(PasswordError::Mismatch)
    } else {
        Ok(())
    }
}

/// Check the password of a sync there already.
pub fn check_password(password: &str) -> Result<(), PasswordError> {
    match password.is_empty() {
        true => Err(PasswordError::Missing),
        false => Ok(()),
    }
}

/// Twenty letters and digits that cannot be mistaken for one another, in
/// groups of five: about a hundred bits.
pub fn random_password() -> Zeroizing<String> {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::rng();
    let mut password = Zeroizing::new(String::with_capacity(23));
    for ix in 0..20 {
        if ix > 0 && ix % 5 == 0 {
            password.push('-');
        }
        password.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
    }
    password
}

/// What is wrong with a login as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginError {
    UrlInvalid,
    UsernameMissing,
    PasswordMissing,
    EndpointInvalid,
    BucketInvalid,
    AccessKeyMissing,
    SecretKeyMissing,
}

/// A WebDAV server's fields as typed.
pub struct WebDavForm<'a> {
    pub url: &'a str,
    pub username: &'a str,
    pub password: &'a str,
}

impl WebDavForm<'_> {
    /// The place and the password, or what is wrong with them. The
    /// password is taken as typed, spaces and all.
    pub fn login(&self) -> Result<(SyncLocation, Zeroizing<String>), LoginError> {
        let (url, username) = (self.url.trim(), self.username.trim());
        if !is_address(url) {
            return Err(LoginError::UrlInvalid);
        }
        if username.is_empty() {
            return Err(LoginError::UsernameMissing);
        }
        if self.password.is_empty() {
            return Err(LoginError::PasswordMissing);
        }
        let location = SyncLocation::WebDav {
            url: url.into(),
            username: username.into(),
        };
        Ok((location, Zeroizing::new(self.password.into())))
    }
}

/// An S3 bucket's fields as typed.
pub struct S3Form<'a> {
    pub endpoint: &'a str,
    pub region: &'a str,
    pub bucket: &'a str,
    pub prefix: &'a str,
    pub path_style: bool,
    pub access_key: &'a str,
    pub secret_key: &'a str,
}

impl S3Form<'_> {
    /// The place and the secret key, or what is wrong with them.
    pub fn login(&self) -> Result<(SyncLocation, Zeroizing<String>), LoginError> {
        let endpoint = self.endpoint.trim();
        let bucket = self.bucket.trim();
        let access_key = self.access_key.trim();
        let secret_key = self.secret_key.trim();
        if !is_address(endpoint) {
            return Err(LoginError::EndpointInvalid);
        }
        if bucket.is_empty() || bucket.contains(['/', '\\', ' ']) {
            return Err(LoginError::BucketInvalid);
        }
        if access_key.is_empty() {
            return Err(LoginError::AccessKeyMissing);
        }
        if secret_key.is_empty() {
            return Err(LoginError::SecretKeyMissing);
        }
        let location = SyncLocation::S3 {
            endpoint: endpoint.into(),
            region: self.region.trim().into(),
            bucket: bucket.into(),
            prefix: self.prefix.trim().trim_matches('/').into(),
            path_style: self.path_style,
            access_key: access_key.into(),
        };
        Ok((location, Zeroizing::new(secret_key.into())))
    }
}

/// An http or https address with a host.
fn is_address(text: &str) -> bool {
    url::Url::parse(text).ok().is_some_and(|address| {
        matches!(address.scheme(), "http" | "https") && address.host_str().is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_random_password_is_long_and_plain_to_read() {
        let one = random_password();
        assert_eq!(one.len(), 23);
        assert!(one.chars().count() >= MIN_PASSWORD);
        assert!(check_new_password(&one, &one).is_ok());
        assert_eq!(one.split('-').count(), 4);
        assert!(!one.contains(['0', 'o', '1', 'l', 'i']));
        assert_ne!(*one, *random_password());
    }

    #[test]
    fn a_new_password_is_long_enough_and_typed_alike_twice() {
        assert_eq!(check_password(""), Err(PasswordError::Missing));
        assert_eq!(check_password("x"), Ok(()));
        assert_eq!(
            check_new_password("short", "short"),
            Err(PasswordError::TooShort)
        );
        // Counted in characters, not bytes.
        assert_eq!(
            check_new_password("同步密码同步密码同步密", "同步密码同步密码同步密"),
            Err(PasswordError::TooShort)
        );
        assert_eq!(
            check_new_password("correct horse", "correct house"),
            Err(PasswordError::Mismatch)
        );
        assert_eq!(check_new_password("correct horse", "correct horse"), Ok(()));
    }

    #[test]
    fn a_login_is_trimmed_and_checked_field_by_field() {
        let webdav = |url, username, password| {
            WebDavForm {
                url,
                username,
                password,
            }
            .login()
        };
        assert_eq!(
            webdav("dav.example.com", "me", "pw").unwrap_err(),
            LoginError::UrlInvalid
        );
        assert_eq!(
            webdav("ftp://dav.example.com", "me", "pw").unwrap_err(),
            LoginError::UrlInvalid
        );
        assert_eq!(
            webdav("https://dav.example.com", " ", "pw").unwrap_err(),
            LoginError::UsernameMissing
        );
        assert_eq!(
            webdav("https://dav.example.com", "me", "").unwrap_err(),
            LoginError::PasswordMissing
        );
        let (location, password) =
            webdav(" https://dav.example.com/dav/ ", " me ", " pw ").unwrap();
        assert_eq!(
            location,
            SyncLocation::WebDav {
                url: "https://dav.example.com/dav/".into(),
                username: "me".into(),
            }
        );
        assert_eq!(password.as_str(), " pw ");

        let form = S3Form {
            endpoint: " https://s3.example.com ",
            region: " auto ",
            bucket: " shellrs ",
            prefix: " /team/sync/ ",
            path_style: true,
            access_key: " AKID ",
            secret_key: " secret ",
        };
        let (location, secret) = form.login().unwrap();
        assert_eq!(
            location,
            SyncLocation::S3 {
                endpoint: "https://s3.example.com".into(),
                region: "auto".into(),
                bucket: "shellrs".into(),
                prefix: "team/sync".into(),
                path_style: true,
                access_key: "AKID".into(),
            }
        );
        assert_eq!(secret.as_str(), "secret");
        for (bucket, endpoint, access_key, secret_key, error) in [
            ("b", "s3.example.com", "a", "s", LoginError::EndpointInvalid),
            (
                "a/b",
                "https://s3.example.com",
                "a",
                "s",
                LoginError::BucketInvalid,
            ),
            (
                "",
                "https://s3.example.com",
                "a",
                "s",
                LoginError::BucketInvalid,
            ),
            (
                "b",
                "https://s3.example.com",
                " ",
                "s",
                LoginError::AccessKeyMissing,
            ),
            (
                "b",
                "https://s3.example.com",
                "a",
                " ",
                LoginError::SecretKeyMissing,
            ),
        ] {
            let form = S3Form {
                endpoint,
                bucket,
                access_key,
                secret_key,
                ..form
            };
            assert_eq!(form.login().unwrap_err(), error);
        }
    }
}
