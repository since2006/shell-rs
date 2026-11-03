//! Android's own certificate checks for sync over HTTPS (WebDAV, S3). The
//! HTTP client checks a server's certificate with the platform's verifier
//! (`rustls-platform-verifier`), as the system's apps are checked, user
//! certificates included; on Android that verifier reaches the system
//! through the JVM, and needs the app's context before the first check.
//! `Tls.kt` declares the call.

use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject};

/// `com.shellrs.core.Tls.setUp(context)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_shellrs_core_Tls_setUp<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    context: JObject<'caller>,
) {
    env.with_env(|env| rustls_platform_verifier::android::init_with_env(env, context))
        .resolve::<ThrowRuntimeExAndDefault>()
}
