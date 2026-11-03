# shellrs-mobile

The core of the ShellRS iOS and Android apps: the desktop app's own
connection and SFTP code (`crates/shellrs-ssh`) and its saved hosts and sync
(`crates/shellrs-hosts`) behind one interface, which Swift and Kotlin
bindings are generated from with [UniFFI](https://mozilla.github.io/uniffi-rs/).
This interface is the one the apps rely on and is kept stable; the cores
under it change as the desktop needs. MIT licensed.

It keeps the saved hosts, groups and credentials in the desktop's own
database, their passwords in the app's keychain, and syncs them with the
user's other devices as the desktop does. It logs in (passwords, private
keys, keyboard-interactive, jump hosts, HTTP and SOCKS5 proxies), checks
host keys against the app's own `known_hosts`, opens terminals and SFTP
sessions on one connection, runs commands and transfers files with resume.
It has no words: questions and failures are typed, and the app words them
in its own language.

## Building

| Platform | Command | Output in `target/mobile/` |
|---|---|---|
| iOS | `scripts/build-ios.sh` | `ios/ShellRSCore.xcframework` (iPhones, and the simulator on Apple silicon), `ios/Sources/ShellRSCore.swift` |
| Android | `scripts/build-android.sh` | `android/jniLibs/{arm64-v8a,x86_64}/libshellrs_mobile.so`, `android/kotlin/` |

- **iOS** needs Xcode and
  `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`. Add the
  xcframework and the Swift file to the app's target; nothing else links
  with it. `IPHONEOS_DEPLOYMENT_TARGET` sets the oldest iOS, 15.0 unless set.
- **Android** needs the NDK r28 or later (`ANDROID_NDK_HOME`), which lays the
  libraries out for 16 KB pages as Google Play asks, `cargo install cargo-ndk`
  and `rustup target add aarch64-linux-android x86_64-linux-android`. Copy
  `jniLibs` to `app/src/main/jniLibs` and the Kotlin sources into the app's.
  They need `net.java.dev.jna:jna:5.x@aar` and
  `org.jetbrains.kotlinx:kotlinx-coroutines-core`. `ANDROID_PLATFORM` sets
  the oldest Android, API level 24 unless set.
- **Android, sync over HTTPS.** WebDAV servers and S3 buckets have their
  certificates checked by Android's own `TrustManager`, the one OkHttp
  uses: native code reaches it only through the JVM. Call
  `Tls.setUp(applicationContext)` once before the core syncs, and add the
  verifier's Kotlin part, `org.rustls:rustls-platform-verifier`, at the
  version `Cargo.lock` has for `rustls-platform-verifier-android`, with its
  Proguard rule, as
  [its README](https://github.com/rustls/rustls-platform-verifier#android)
  says. Without them, every sync to an `https` address fails; `http`
  addresses and folders need neither.

`cargo test -p shellrs-mobile` runs its tests against an SSH server in the
test process, a keychain in memory and a sync folder on disk (`cargo test`
leaves this crate out). `tests/swift/run.sh` and
`tests/kotlin/run.sh` build the generated Swift and Kotlin against a build
for the computer and run a short check with them, after the build scripts.

## Using it

```swift
final class AppKeychain: Keychain {   // On Keychain Services, one service.
    func available() -> Bool { ... }
    func get(account: String) throws -> String? { ... }
    func set(account: String, value: String) throws { ... }
    func delete(account: String) throws { ... }
}

let core = try Core(
    config: CoreConfig(dataDir: appSupport.path, deviceName: UIDevice.current.name,
                       platform: "ios", appVersion: version),
    keychain: AppKeychain())
let store = core.store()
let id = try await store.addHost(
    draft: HostDraft(name: "web", protocol: .ssh, address: "10.0.0.1", port: 22,
                     user: "root", auth: .password, route: .direct, group: nil, notes: ""),
    password: .set(value: typed), proxyPassword: .keep)
let connection = try core.connectHost(id: id)

Task {
    while let event = await connection.nextEvent() {
        switch event {
        case .prompt(let prompt):     // Ask the user, then:
            connection.answer(promptId: prompt.id, reply: .trustAndSave)
        case .connected:              // Open terminals and sessions.
            let terminal = try await connection.openTerminal(options: TerminalOptions(
                term: "xterm-256color", columns: 80, rows: 24, width: 0, height: 0))
        case .latency(let ms): break
        case .closed(let failure): break   // nil: the app disconnected.
        }
    }
}
```

- **One `Core`** per data folder, made when the app starts and kept while it
  runs. The data folder holds what the desktop's does: the database
  (`shellrs.db`, upgraded in place from an older version's, with a backup
  first), the private keys ShellRS keeps (`keys/`), the trusted host keys,
  where sync goes through (`sync.json`) and the transfers' resume records.
  A second core on the same folder fails with `DataFolderInUse`. When the
  core goes, so does everything it opened.
- **Events.** `HostStore`, `Connection`, `Terminal` and `SftpSession` each
  say what happens through `nextEvent()`, which the app awaits in a loop
  until it returns nil. Calls answer for themselves; nothing calls back
  into the app except its `Keychain`. Terminal output comes in chunks of up
  to 64 KiB; an app that falls far behind loses the oldest output and gets
  `OutputDropped` with how much.
- **One login, many channels.** Terminals, SFTP sessions and `exec` all open
  on the connection's one login, which bastion hosts allow where a second
  login would not.
- **Failures** are typed. A connection's are a chain of links, outermost
  first; word each and join them with ": ", as the desktop does. `network`
  says whether trying again may help. Links the core adds later arrive as
  `other(detail:)`.
- **A phone has no agent and no `~/.ssh`.** `noPassword` gets in only
  where the server asks for nothing, `agent` fails, and keys come from
  key credentials or files the app keeps (`key` with `keyPath`).

## Saved hosts

`core.store()` holds the hosts, groups and credentials, the same as the
desktop's and synced with it. Serial hosts stay on the computer they were
made on and never show here; Telnet hosts do, but connect only on the
desktop for now (`connectHost` fails with `notSsh`).

- **Memory first.** Reads are quick and never fail. Each change is in the
  store at once and written to the database there and then; what could not
  be kept comes as `StoreEvent.failed`, the change standing all the same.
  `changed` says the lists changed (here, or through sync): read them again.
- **Ids** are valid while the core runs; across launches, name a host by
  its `publicId`, which every device shares.
- **The host's mark.** Each time `connectHost` gets in, the host is asked
  what system it runs on, on a channel of its own, as the desktop asks; the
  answer goes into its `os` and comes as `changed`. A host that will not
  say keeps the mark it had.
- **Secrets go through the store.** Saving a host or a credential takes
  what to do with its password (`SecretChange`: keep, clear or set) and,
  for a key credential, its key (`KeyChange`: keep, pasted text, or a key
  `generateKey` made). The store writes the keychain and dates the change
  for sync; a secret the app writes into the keychain itself is one sync
  never hears of. A password kept as it is moves with its host to a new
  address, port or user. `savedSecret` reads one back to show it.
- **Keys** live in `keys/` in the data folder, as on the desktop: pasted,
  read from a file the user picked, or generated. `checkPrivateKey` says
  whether text is a private key and whether it needs a passphrase.
- **Connections not saved** (`core.connect` with a `Login`) read their
  password as `SecretRef.temporary(host:)`, which
  `core.setTemporaryPassword` keeps in memory, never in the keychain.

## Sync

`core.sync()` syncs the store with the user's other devices through a
folder, a WebDAV server or an S3 bucket, the same as the desktop's: a phone
joins the sync a computer set up, or sets one up. The forms are checked
with `checkWebdavLogin`, `checkS3Login` and `checkNewSyncPassword`.

- **Setting up.** `inspect` says whether sync is set up at a place already;
  then `create` with a new sync password, or `join` with its password
  (`fresh` the first time). Then `syncNow`. `changeLogin`,
  `changePassword`, `removeDevice` and `turnOff` are the settings page's.
- **When to sync is the app's**: when it starts, when it comes to the
  foreground (at most once a minute), a few seconds after the user stops
  editing, and before it goes to the background if `status().hasChanges`.
  Not on a timer. Rounds wait their turn; a job beside one fails with
  `inProgress`.
- **Attention.** A round may stop for what only the user can settle
  (`SyncOutcome.attention`): a password changed elsewhere, sync reset, this
  device removed, many deletions at once, a login refused. Ask, and leave
  automatic rounds until they have; each case says what settles it.
- Passwords, passphrases and keys sync with everything else, encrypted
  with the sync password: as on the desktop, there is no switch.

## Threads

- Every method may be called from any thread. The ones that are not `async`
  never wait; the `async` ones run on the core's own threads.
- The `Keychain` is called on one of those threads, and may take its time,
  but must never wait for the main thread, Face ID or a dialog there:
  throw `unavailable` instead. A connection without its saved password
  asks for it through a prompt.
- `Core(config:keychain:)` opens the database on the calling thread, which
  takes a moment, longer when it upgrades one.
- **Ending things.** `connection.disconnect()`, `terminal.end()` and
  `session.end()` end them at once, and their `nextEvent()` returns nil.
  Do that rather than counting on the objects being freed: a call waiting
  on an object (such as the event loop's `nextEvent()`) holds it, so letting
  go of it does not end it. (They are not named `close`: Kotlin's
  `AutoCloseable.close()` only frees the handle.)
- **Cancelling.** Cancelling a Kotlin coroutine stops the call it waits on.
  Cancelling a Swift `Task` does not (UniFFI does not pass it on yet): the
  call returns when it is done, so end the object to stop it sooner.

## Foreground and background

iOS suspends an app soon after it goes to the background, and Android may
too; the server then drops its connections after its keepalive gives up.

- When the app comes back to the foreground, or the network changes, call
  `checkAlive()` on each connection. It answers with the round trip, or
  fails at once for a connection that no longer works, which then closes
  with `network` set. Without it, a dead connection is noticed only after
  its keepalive, up to 90 seconds.
- A closed connection stays closed: connect again with a new `Connection`.
  A shell does not survive this; a terminal starts a new one.
- A transfer reconnects its channel on its own while its connection is up.
  When the connection is gone, start the same transfer again on a new
  session: files continue where they stopped, from the resume records in
  the data folder.
- On iOS-only IPv6 networks (NAT64), an IPv4 address is reached through the
  network's NAT64 prefix, as App Review tests.
