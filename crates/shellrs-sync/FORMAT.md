# ShellRS sync format, version 1

What a store holds, so that every ShellRS — desktop, mobile, every version —
reads it the same. **Nothing here may change once released**: a change that an
older reader would misread needs a new `min_reader` (or, for the files' own
layout, a new folder `v2`).

## Files

Paths are relative to the root the user chose: a WebDAV folder, a bucket and
prefix, a local folder.

```text
shellrs-sync/v1/vault.json             the keys, wrapped by the sync password
shellrs-sync/v1/devices/<device>.srs   one device's encrypted snapshot
```

- A device id is 16 characters of `[a-z0-9]`, drawn at random once.
- Each device writes only its own file. A file whose name is not exactly
  `<device>.srs` (a cloud drive's conflicted copy, a placeholder) is ignored.

## `vault.json`

```json
{
  "format": 1,
  "vault_id": "<16 bytes, hex>",
  "kdf": {"algorithm": "argon2id", "version": 19, "memory_kib": 65536,
          "passes": 3, "parallelism": 1, "salt": "<16 bytes, hex>"},
  "current": 1,
  "keys": [{"generation": 1, "nonce": "<24 bytes, hex>", "wrapped": "<hex>"}]
}
```

- The key-encryption key is Argon2id (version 0x13, 32 bytes) of the sync
  password in Unicode NFC, with `salt` and the cost given. A reader refuses
  costs above 256 MiB, 10 passes or 4 lanes.
- Each data key (32 random bytes) is wrapped with XChaCha20-Poly1305 under
  the key-encryption key, with associated data
  `"shellrs-sync key" ‖ vault_id ‖ generation (u32, big-endian)`.
- `current` is the generation new files are encrypted with. Older
  generations stay, so files not yet rewritten remain readable.
- A reader refuses a `format` above its own (`VaultTooNew`). Adding a field
  leaves `format` as it is; it goes up only when an older reader could not
  use the vault correctly.
- A writer keeps the fields it does not know, at the top level, in `kdf` and
  in each entry of `keys`, so what a newer version added survives an older
  one rotating the key or changing the password. A password change replaces
  `kdf` whole, its unknown fields too; fields of a `keys` entry describe the
  key, not its wrapping, and stay when it is wrapped again.
- Only the entries of `keys` may be wrapped with the key-encryption key: an
  older version changing the password rewraps those and nothing else. A key
  added later is wrapped with a data key, or with a password of its own.
  (A setup code, below, is sealed with a key derived from it, but is not
  kept in the store: it is meant to stop opening when the password changes.)

## Device file (`.srs`)

```text
"SRSY" | version (1 byte) = 1 | vault_id (16) | generation (u32, big-endian) | nonce (24)
XChaCha20-Poly1305 ciphertext and tag, under the data key of that generation
```

The associated data is the 49-byte header followed by the device id as
written in the file name (16 ASCII bytes): a file renamed, moved to another
vault or relabelled with another generation does not open.

The plaintext is raw DEFLATE of the snapshot JSON. A reader refuses files over
16 MiB and JSON over 64 MiB.

## Snapshot

```json
{
  "format": 1,
  "min_reader": 1,
  "device": {"id": "<device>", "name": "MacBook Pro", "platform": "macos", "app": "0.2.0"},
  "clock": [<ms>, <counter>],
  "purged_before": 0,
  "records": [ <record>, … ]
}
```

- `min_reader`: the oldest format that can read the file. A reader below it
  leaves the file out and writes nothing until updated.
- `device.id` must be the file's name.
- `purged_before` is reserved for removing old deletions; version 1 never
  removes them, and writes 0.
- Unknown top-level fields are ignored.

## Records

```json
{"kind": "host", "id": "<id>", "v": [<ms>, <counter>, "<device>"], "data": {…}}
{"kind": "host", "id": "<id>", "v": [<ms>, <counter>, "<device>"], "deleted": true}
```

- `kind`: 1–32 of `[a-z0-9_.]`, starting with a letter. `id`: 1–64 of
  `[A-Za-z0-9_-]`.
- `v`: milliseconds since the epoch (at most 2^47), a counter, and the device
  that wrote the copy. Copies are ordered by `(ms, counter)`, then by device
  id; equal versions are decided by the SHA-256 of the record as written.
  A device's own edits advance its counter by 2 within a millisecond; a
  repair of a copy is dated `(ms, counter + 1)` of that copy.
- A record may carry `"min_reader": n`: a reader below `n` keeps it and
  writes it back unchanged but does not use it.
- Fields of a record a reader does not know, and records of kinds it does not
  know, are written back exactly as read.
- `secret` and `secret.*` are secrets. They appear in the clear only inside
  the encrypted device files: a device keeps them sealed with a data key of
  the vault, and one of a kind it does not know it keeps sealed and writes
  back without using it. Secrets never travel as fields of other kinds.

### Kinds

References are other records' ids. `null` means none.

| kind | `data` fields |
| --- | --- |
| `group` | `name` (not empty), `parent` (group or null), `sort_order` |
| `credential` | `name` (not empty), `kind` (`password`, `key`, `agent`), `username`, `sort_order` |
| `host` | `name`, `address` (not empty), `port` (1–65535), `group` (or null), `sort_order`, `protocol` (`ssh`, `telnet`), `telnet` {`user_prompt`, `pass_prompt`} when Telnet, `auth` (`password`, `no-password`, `credential`), `username` (null exactly when `auth` is `credential`), `credential` (set exactly then), `credential_username` (the user a host using a credential signs in as instead of the credential's, or null)², `route` (`direct`, `jump`, `proxy`, `proxy-command`), `jumps` [host or null] when `jump`, `proxy` {`kind` (`http`, `socks5`), `host`, `port`, `username`} when `proxy`, `command` when `proxy-command`, `notes`, `totp` ({`prompt` (what the question for a one-time code says, not empty, or null for the usual)} for an SSH host that fills in one-time codes, or null)² |
| `bookmark` | `host`, `path`, `sort_order`; its id is the first 16 bytes, hex, of SHA-256(`host` ‖ 0x00 ‖ `path`) |
| `forward` | `host`, `name`, `kind` (`local`, `remote`, `dynamic`), `bind_host`, `bind_port`, `target_host` and `target_port` (null exactly when dynamic), `sort_order` |
| `snippet_category` | `name` (not empty), `parent` (snippet category or null)¹, `sort_order`¹ |
| `snippet` | `category` (or null), `name`, `command` (not empty), `scope` (`all`, `group`, `host`), `scope_id` (null exactly when `all`), `run_on_click`, `sort_order`¹ |
| `secret.password` | `user` (may be empty), `host` (not empty), `port`, `value`: the password saved for signing in as `user` at `host:port`, shared by every host that does; its id is the first 16 bytes, hex, of SHA-256(`user` ‖ 0x00 ‖ `host` ‖ 0x00 ‖ `port` in decimal) |
| `secret.proxy` | as `secret.password`, for a proxy |
| `secret.credential` | `value`: the password of the password credential with this id |
| `secret.passphrase` | `value`: the passphrase of the key credential with this id |
| `secret.private_key` | `value`: the private key file, as text, of the key credential with this id |
| `secret.totp` | `value`: the base32 secret of the one-time codes of the host with this id² |

A secret's `value` is not empty and at most 64 KiB. A credential's secret is
used only while the credential is of the kind that has it, a host's
`secret.totp` only while the host's `totp` is set. Secrets always
sync; there is no setting to keep them out.

Every field listed is written, `null` where it has none: a record missing one
is damaged, kept and passed on but not used. The fields marked ¹ were added
after ShellRS 0.1.17, and a copy an older version wrote has none of them:
missing, `parent` reads as null and `sort_order` as 0. An older version keeps
them as fields it does not know, so they survive its edits; it lists snippet
categories flat.

The fields and kind marked ² were added after ShellRS 0.1.18. Missing,
`credential_username` reads as null, and so does an empty one. It counts only
while `auth` is `credential`: an older version that turns the host to another
sign-in keeps it as it was. Repairing a host whose credential was deleted sets
it to null. Missing, `totp` reads as null; it counts only for an SSH host. An
older version keeps `totp` as a field it does not know and `secret.totp` as a
secret of a kind it does not know.

Not synced, each device's own: serial hosts (their port is this computer's);
a host's detected system, last connection and sidebar tool; a group's expanded
state; a key credential's file path; a forward's automatic start; local SFTP
bookmarks; 项目 (local terminals); the app's settings; known hosts.

### After a merge

A copy pointing at a deleted record is repaired, as the device's own edit:

| deleted | repair |
| --- | --- |
| a host's group, a group's parent, a snippet category's parent | to the top level |
| a cycle of parents | the group (or snippet category) with the least version goes to the top |
| a host's credential | `auth` `password`, `username` "" |
| a jump host | its hop becomes null, in place |
| a forward's or bookmark's host | the forward or bookmark is deleted |
| a snippet's scope group or host | the snippet is deleted |
| a snippet's category | to the top level |
| a credential | its `secret.credential`, `secret.passphrase` and `secret.private_key` are deleted |
| a host | its `secret.totp` is deleted |

A record that is only missing (not deleted) is never repaired.

## Setup code

Not a file of the store: text a device in sync shows, also as a QR code, for
another to join with. It holds where the store is and the login to it, for
whoever knows the sync password.

```text
"shellrs-sync:" then the unpadded base64url (RFC 4648 §5) of:

"SRSC" | version (1 byte) = 1 | memory_kib (u32) | passes (u32) | parallelism (u32) | salt (16) | nonce (24)
XChaCha20-Poly1305 ciphertext and tag
check (4): the first bytes of the SHA-256 of all before it
```

- The numbers are big-endian. `memory_kib`, `passes`, `parallelism` and
  `salt` are the vault's `kdf` as it was when the code was made: a reader
  derives the key-encryption key from the sync password with them, as for
  the vault, and refuses the same costs.
- The key is HMAC-SHA256, keyed with the key-encryption key, of the ASCII
  text `shellrs-sync setup code`. The associated data is the 57-byte header.
  A device of the vault makes a code with the keys it holds, without the
  password. A code made before the password changed does not open with the
  new one.
- A reader passes over white space anywhere in the text. A text over 4096
  characters, without the prefix, not base64url, without the magic, of
  version 0, shorter than a header, a tag and a check, with a wrong check or
  a cost out of bounds is not a setup code. The check tells a code cut short
  from a wrong password; it protects nothing.
- A version above the reader's own needs a newer ShellRS, which is told
  before anything after the version is read: only the prefix, the base64url,
  the magic and the version byte are the same in every version. A writer
  keeps to version 1 while it can say what the code holds.
- The plaintext is JSON, `location` as the apps keep it in their settings:

```json
{"location": {"kind": "webdav", "url": "<the folder's address>", "username": "<user>"},
 "login": "<the WebDAV password>"}
{"location": {"kind": "s3", "endpoint": "<address>", "region": "<region>", "bucket": "<bucket>",
              "prefix": "<the folder in the bucket, or empty>", "path_style": false,
              "access_key": "<access key id>"},
 "login": "<the secret access key>"}
```

- Unknown fields are ignored. A `kind` the reader does not know needs a
  newer ShellRS. A local folder has no setup code: its path is one device's.
- The code is no easier to guess the password of than `vault.json`, which
  has the same salt and cost. Whoever has a code and the password it was made
  under has the login, even after the password changed.
