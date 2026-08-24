#!/usr/bin/env bash
# ============================================================
#  pydmg-neogeo — Android build script (Linux / macOS / WSL)
# ============================================================
#  Cross-compiles the Rust core+JNI to .so files for 3 Android
#  ABIs and drops them under android-app/app/src/main/jniLibs/.
#  Then (optionally) builds the APK with Gradle.
#
#  Requirements (one-time setup):
#    1. Rust toolchain (any recent stable):
#         curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#       and add the Android targets:
#         rustup target add aarch64-linux-android \
#                           armv7-linux-androideabi \
#                           x86_64-linux-android
#    2. Android NDK r26 or newer (we tested r26d):
#         sdkmanager --install "ndk;26.1.10909125"
#       OR download from https://developer.android.com/ndk/downloads
#       and set:  export ANDROID_NDK_HOME=/path/to/ndk
#    3. cargo-ndk helper:
#         cargo install cargo-ndk
#    4. Android SDK + Java 17 (for Gradle), only needed for step 2.
#
#  Usage:
#    ./build-android.sh                 # cross-compile .so for 3 ABIs
#    ./build-android.sh --apk           # also assemble the debug APK
#    ./build-android.sh --release       # release-signed APK (read NOTES)
# ============================================================
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
JNILIBS="$ROOT/android-app/app/src/main/jniLibs"
JNI_PKG="pydmg-neogeo-jni"
LIB_FILENAME="libpydmg_neogeo_jni.so"
PROFILE="android-release"

# --- 0. Sanity checks ---
command -v cargo >/dev/null || { echo "ERROR: cargo not found. Install Rust."; exit 1; }
command -v cargo-ndk >/dev/null 2>&1 || { echo "ERROR: cargo-ndk not installed. Run: cargo install cargo-ndk"; exit 1; }
if [[ -z "${ANDROID_NDK_HOME:-}" && -z "${NDK_HOME:-}" && -z "${ANDROID_NDK_ROOT:-}" ]]; then
    echo "ERROR: ANDROID_NDK_HOME (or NDK_HOME / ANDROID_NDK_ROOT) is not set."
    echo "       Point it to your NDK install, e.g."
    echo "       export ANDROID_NDK_HOME=\$HOME/Android/Sdk/ndk/26.1.10909125"
    exit 1
fi

echo "==> Adding Rust Android targets if missing"
for t in aarch64-linux-android armv7-linux-androideabi x86_64-linux-android; do
    rustup target add "$t" >/dev/null
done

# --- 1. Cross-compile for the 3 ABIs ---
mkdir -p "$JNILIBS/arm64-v8a" "$JNILIBS/armeabi-v7a" "$JNILIBS/x86_64"

echo "==> Cross-compiling Rust JNI lib (profile=$PROFILE)"
# cargo-ndk handles all the NDK toolchain plumbing (CC, AR, sysroot, etc.).
# It places the .so files under target/<rust-target>/<profile>/.
cd "$ROOT"
cargo ndk \
    -t arm64-v8a \
    -t armeabi-v7a \
    -t x86_64 \
    -o "$JNILIBS" \
    --manifest-path android-jni/Cargo.toml \
    build --profile "$PROFILE"

echo ""
echo "==> Native libs produced:"
find "$JNILIBS" -name "*.so" -exec ls -lh {} \;

# --- 2. Optional: build the APK ---
case "${1:-}" in
    --apk|--debug)
        echo ""
        echo "==> Assembling debug APK"
        cd "$ROOT/android-app"
        ./gradlew assembleDebug
        echo ""
        echo "APK written to: android-app/app/build/outputs/apk/debug/app-debug.apk"
        ;;
    --release)
        echo ""
        # --- Auto-generate a local signing keystore on first use ---------
        # Gradle's `pydmgRelease` signingConfig picks it up automatically
        # (see android-app/app/build.gradle.kts), so assembleRelease
        # produces a SIGNED, installable APK with zero manual steps.
        # Override credentials for CI with:
        #   PYDMG_KEYSTORE_PASS=... PYDMG_KEY_ALIAS=... ./build-android.sh --release
        KEYSTORE="$ROOT/android-app/release.keystore"
        KS_PASS="${PYDMG_KEYSTORE_PASS:-pydmg-neogeo}"
        KS_ALIAS="${PYDMG_KEY_ALIAS:-pydmg}"
        if [[ ! -f "$KEYSTORE" ]]; then
            if command -v keytool >/dev/null 2>&1; then
                echo "==> Generating local release keystore (first run)"
                keytool -genkeypair -v \
                    -keystore "$KEYSTORE" \
                    -alias "$KS_ALIAS" \
                    -keyalg RSA -keysize 2048 -validity 10000 \
                    -storepass "$KS_PASS" -keypass "$KS_PASS" \
                    -dname "CN=pydmg-neogeo, OU=dev, O=pydmg, L=local, S=local, C=ES"
            else
                echo "WARN: keytool not found (install a JDK). Building UNSIGNED release."
            fi
        fi
        echo "==> Assembling release APK"
        cd "$ROOT/android-app"
        ./gradlew assembleRelease
        echo ""
        if [[ -f "$KEYSTORE" ]]; then
            echo "Signed release APK: android-app/app/build/outputs/apk/release/app-release.apk"
            echo "Install directly:   adb install -r app/build/outputs/apk/release/app-release.apk"
        else
            echo "Unsigned APK: android-app/app/build/outputs/apk/release/app-release-unsigned.apk"
            echo "Sign it with apksigner before installing."
        fi
        ;;
    "")
        echo ""
        echo "Native libs ready. Next step:"
        echo "    cd android-app && ./gradlew assembleDebug     # APK de depuración"
        echo "    ./build-android.sh --release                  # APK release firmada"
        ;;
    *)
        echo "Unknown argument: $1"
        echo "Usage: $0 [--apk | --release]"
        exit 1
        ;;
esac
