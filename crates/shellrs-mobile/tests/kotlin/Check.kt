// The Kotlin interface builds and works: a core keeps a host with its
// password in the app's keychain, makes a key credential, checks sync's
// forms, and connects to a closed port and fails, typed as the network.
// `run.sh` runs it on this computer's JVM.
import com.shellrs.core.*
import java.nio.file.Files
import java.util.concurrent.ConcurrentHashMap
import kotlin.system.exitProcess
import kotlinx.coroutines.runBlocking

/** The keychain, in memory. */
class MemoryKeychain : Keychain {
    val secrets = ConcurrentHashMap<String, String>()

    override fun available(): Boolean = true

    override fun get(account: String): String? = secrets[account]

    override fun set(account: String, value: String) {
        secrets[account] = value
    }

    override fun delete(account: String) {
        secrets.remove(account)
    }
}

fun check(condition: Boolean, message: () -> String) {
    if (!condition) {
        System.err.println("FAILED: ${message()}")
        exitProcess(1)
    }
}

fun main() = runBlocking {
    val folder = Files.createTempDirectory("shellrs-kotlin")
    val config = CoreConfig(folder.toString(), "Kotlin", "android", "0.0.0")
    val keychain = MemoryKeychain()
    val core = Core(config, keychain)

    try {
        Core(config, keychain)
        check(false) { "a second core ran on the same folder" }
    } catch (_: CoreException.DataFolderInUse) {}

    // A host, its password in the keychain under the desktop's account.
    val store = core.store()
    val id = store.addHost(
        HostDraft("web", HostProtocol.Ssh, "10.0.0.1", 22u, "root", HostAuth.Password, HostRoute.Direct, null, ""),
        SecretChange.Set("hunter2"),
        SecretChange.Keep,
    )
    check(store.hosts().map { it.id } == listOf(id)) { "the host is not listed" }
    check(keychain.secrets["password:root@10.0.0.1:22"] == "hunter2") { "no password" }
    check(store.nextEvent() == StoreEvent.Changed) { "no change said" }
    try {
        store.addHost(
            HostDraft("", HostProtocol.Ssh, " ", 22u, "", HostAuth.NoPassword, HostRoute.Direct, null, ""),
            SecretChange.Keep,
            SecretChange.Keep,
        )
        check(false) { "a host without an address was saved" }
    } catch (error: StoreException.Host) {
        check(error.problem == HostProblem.ADDRESS) { "not the address: ${error.problem}" }
    }

    // A key credential, its key made here.
    val key = store.generateKey(KeyAlgorithm.ED25519)
    check(key.publicKey("phone").startsWith("ssh-ed25519 ")) { "not an Ed25519 key" }
    val credential = store.addCredential(
        CredentialDraft("phone", CredentialKind.KEY, "deploy"),
        KeyChange.Generated(key, null),
        SecretChange.Keep,
    )
    check(store.credentials().map { it.id } == listOf(credential)) { "the credential is not listed" }
    try {
        checkPrivateKey(key.publicKey(""))
        check(false) { "a public key passed for a private one" }
    } catch (_: KeyProblem.PublicKey) {}

    // Sync's forms.
    check(checkNewSyncPassword("short", "short") == PasswordProblem.TOO_SHORT) { "short passed" }
    check(randomSyncPassword().length == 23) { "not a password in groups of five" }
    try {
        checkWebdavLogin("not a url", "me", "secret")
        check(false) { "a bad address passed" }
    } catch (_: LoginProblem.UrlInvalid) {}
    check(core.sync().status().location == null) { "sync is on" }

    // Nothing listens on port 1.
    val connection = core.connect(
        Login("127.0.0.1", 1u, "nobody", LoginMethod.NO_PASSWORD, null, SecretRef.Temporary(1u), Route.Direct)
    )
    var failure: Failure? = null
    while (true) {
        val event = connection.nextEvent() ?: break
        if (event is ConnectionEvent.Closed) failure = event.failure
    }
    check(failure?.network == true) { "not a network failure: $failure" }
    check(failure?.links?.first() == FailureLink.Connect(ConnectError.Unreachable("127.0.0.1", 1u))) {
        "not unreachable: $failure"
    }

    try {
        connection.exec("true")
        check(false) { "a closed connection ran a command" }
    } catch (_: SshException.NotConnected) {}

    check(accountFor(SecretRef.Password("root", "10.0.0.1", 22u)) == "password:root@10.0.0.1:22") {
        "the keychain account is not the desktop's"
    }

    // Kotlin's own close() frees the core at once.
    core.close()
    folder.toFile().deleteRecursively()
    println("ok: ${failure!!.links}")
}
