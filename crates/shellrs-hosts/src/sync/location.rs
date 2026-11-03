//! Where sync goes through, as `settings.json` keeps it (`sync.location`):
//! the field names and the `kind` spellings never change once released.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Where sync goes through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncLocation {
    /// A folder on this computer, which a cloud drive may be syncing.
    Folder { path: PathBuf },
    /// A folder on a WebDAV server; its password is in the keychain.
    #[serde(rename = "webdav")]
    WebDav { url: String, username: String },
    /// A folder in an S3 bucket, or one of a service that speaks its API;
    /// its secret key is in the keychain.
    S3 {
        /// The service's address, such as `https://s3.us-east-1.amazonaws.com`.
        endpoint: String,
        region: String,
        bucket: String,
        /// The folder in the bucket; empty for its top.
        prefix: String,
        /// The bucket in the address's path rather than its host.
        path_style: bool,
        access_key: String,
    },
}

impl SyncLocation {
    /// Whether `other` is the same place, signed in to maybe otherwise:
    /// what a new login may change.
    pub fn same_place(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Folder { path: a }, Self::Folder { path: b }) => a == b,
            (Self::WebDav { url: a, .. }, Self::WebDav { url: b, .. }) => a == b,
            (
                Self::S3 {
                    endpoint,
                    region,
                    bucket,
                    prefix,
                    path_style,
                    ..
                },
                Self::S3 {
                    endpoint: other_endpoint,
                    region: other_region,
                    bucket: other_bucket,
                    prefix: other_prefix,
                    path_style: other_path_style,
                    ..
                },
            ) => {
                (endpoint, region, bucket, prefix, path_style)
                    == (
                        other_endpoint,
                        other_region,
                        other_bucket,
                        other_prefix,
                        other_path_style,
                    )
            }
            _ => false,
        }
    }

    /// Whether getting there takes a login the keychain keeps.
    pub fn signs_in(&self) -> bool {
        !matches!(self, Self::Folder { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::SyncLocation;

    /// What `settings.json` has, as released: reading it must keep working.
    #[test]
    fn the_settings_file_spells_each_place_as_released() {
        let places = [
            (
                SyncLocation::Folder {
                    path: "/Users/me/Dropbox/ShellRS".into(),
                },
                r#"{"kind":"folder","path":"/Users/me/Dropbox/ShellRS"}"#,
            ),
            (
                SyncLocation::WebDav {
                    url: "https://dav.jianguoyun.com/dav/shellrs".into(),
                    username: "me@example.com".into(),
                },
                r#"{"kind":"webdav","url":"https://dav.jianguoyun.com/dav/shellrs","username":"me@example.com"}"#,
            ),
            (
                SyncLocation::S3 {
                    endpoint: "https://s3.us-east-1.amazonaws.com".into(),
                    region: "us-east-1".into(),
                    bucket: "shellrs".into(),
                    prefix: "sync".into(),
                    path_style: false,
                    access_key: "AKIAEXAMPLE".into(),
                },
                r#"{"kind":"s3","endpoint":"https://s3.us-east-1.amazonaws.com","region":"us-east-1","bucket":"shellrs","prefix":"sync","path_style":false,"access_key":"AKIAEXAMPLE"}"#,
            ),
        ];
        for (place, json) in places {
            assert_eq!(serde_json::to_string(&place).unwrap(), json);
            assert_eq!(serde_json::from_str::<SyncLocation>(json).unwrap(), place);
        }
    }
}
