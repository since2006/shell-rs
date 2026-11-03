// The Swift interface builds, in Swift 6, and works: a core keeps a host
// with its password in the app's keychain, makes a key credential, checks
// sync's forms, and connects to a closed port and fails, typed as the
// network. `run.sh` runs it.
import Foundation
import ShellRSCore

/// The keychain, in memory.
final class MemoryKeychain: Keychain, @unchecked Sendable {
    private let lock = NSLock()
    private var secrets: [String: String] = [:]

    func available() -> Bool { true }

    func get(account: String) throws -> String? {
        lock.withLock { secrets[account] }
    }

    func set(account: String, value: String) throws {
        lock.withLock { secrets[account] = value }
    }

    func delete(account: String) throws {
        lock.withLock { _ = secrets.removeValue(forKey: account) }
    }
}

func check(_ condition: Bool, _ message: @autoclosure () -> String) {
    if !condition {
        FileHandle.standardError.write(Data("FAILED: \(message())\n".utf8))
        exit(1)
    }
}

let folder = FileManager.default.temporaryDirectory
    .appendingPathComponent("shellrs-swift-\(UUID().uuidString)")
let config = CoreConfig(
    dataDir: folder.path, deviceName: "Swift", platform: "ios", appVersion: "0.0.0")
let keychain = MemoryKeychain()
let core = try Core(config: config, keychain: keychain)

do {
    _ = try Core(config: config, keychain: keychain)
    check(false, "a second core ran on the same folder")
} catch CoreError.DataFolderInUse {}

// A host, its password in the keychain under the desktop's account.
let store = core.store()
let id = try await store.addHost(
    draft: HostDraft(
        name: "web", protocol: .ssh, address: "10.0.0.1", port: 22, user: "root",
        auth: .password, route: .direct, group: nil, notes: ""),
    password: .set(value: "hunter2"), proxyPassword: .keep)
check(store.hosts().map(\.id) == [id], "the host is not listed")
check(try keychain.get(account: "password:root@10.0.0.1:22") == "hunter2", "no password")
check(await store.nextEvent() == .changed, "no change said")
do {
    _ = try await store.addHost(
        draft: HostDraft(
            name: "", protocol: .ssh, address: " ", port: 22, user: "",
            auth: .noPassword, route: .direct, group: nil, notes: ""),
        password: .keep, proxyPassword: .keep)
    check(false, "a host without an address was saved")
} catch StoreError.Host(.address) {}

// A key credential, its key made here.
let key = try await store.generateKey(algorithm: .ed25519)
check(key.publicKey(comment: "phone").hasPrefix("ssh-ed25519 "), "not an Ed25519 key")
let credential = try await store.addCredential(
    draft: CredentialDraft(name: "phone", kind: .key, user: "deploy"),
    key: .generated(key: key, passphrase: nil), password: .keep)
check(store.credentials().map(\.id) == [credential], "the credential is not listed")
do {
    _ = try checkPrivateKey(text: key.publicKey(comment: ""))
    check(false, "a public key passed for a private one")
} catch KeyProblem.PublicKey {}

// Sync's forms.
check(checkNewSyncPassword(password: "short", repeat: "short") == .tooShort, "short passed")
check(randomSyncPassword().count == 23, "not a password in groups of five")
do {
    _ = try checkWebdavLogin(url: "not a url", username: "me", password: "secret")
    check(false, "a bad address passed")
} catch LoginProblem.UrlInvalid {}
check(core.sync().status().location == nil, "sync is on")

// Nothing listens on port 1.
let connection = core.connect(
    login: Login(
        host: "127.0.0.1", port: 1, user: "nobody", method: .noPassword,
        keyPath: nil, password: .temporary(host: 1), route: .direct))
var failure: Failure?
while let event = await connection.nextEvent() {
    if case .closed(let closed) = event {
        failure = closed
    }
}
check(failure?.network == true, "not a network failure: \(String(describing: failure))")
check(
    failure?.links.first == .connect(error: .unreachable(host: "127.0.0.1", port: 1)),
    "not unreachable: \(String(describing: failure))")

do {
    _ = try await connection.exec(command: "true")
    check(false, "a closed connection ran a command")
} catch SshError.NotConnected {}

check(
    accountFor(secret: .password(user: "root", host: "10.0.0.1", port: 22))
        == "password:root@10.0.0.1:22",
    "the keychain account is not the desktop's")

try? FileManager.default.removeItem(at: folder)
print("ok: \(failure!.links)")
