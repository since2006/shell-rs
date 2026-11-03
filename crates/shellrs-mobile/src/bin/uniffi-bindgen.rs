//! UniFFI's generator, run by `scripts/build-ios.sh` and
//! `scripts/build-android.sh` on the library they build.

fn main() {
    uniffi::uniffi_bindgen_main();
}
