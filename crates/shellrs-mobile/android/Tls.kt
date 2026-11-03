// Copied beside the generated Kotlin interface by scripts/build-android.sh.
package com.shellrs.core

/**
 * Android's own certificate checks for sync over HTTPS (WebDAV, S3), as the
 * system's apps get them. Call [setUp] once, with the application's context,
 * before the core syncs; it needs `rustls:rustls-platform-verifier` in the
 * app (see the core's README).
 */
object Tls {
    init {
        System.loadLibrary("shellrs_mobile")
    }

    /** `context` is an `android.content.Context`. */
    @JvmStatic
    external fun setUp(context: Any)
}
