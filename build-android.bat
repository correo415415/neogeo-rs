@echo off
REM ============================================================
REM  pydmg-neogeo - Android build script (Windows)
REM ============================================================
REM  Cross-compiles the Rust core+JNI to .so files for 3 Android
REM  ABIs and drops them under android-app\app\src\main\jniLibs\.
REM  Then (optionally) builds the APK with Gradle.
REM
REM  Requirements (one-time setup):
REM    1. Rust toolchain (any recent stable):
REM         https://www.rust-lang.org/tools/install
REM         rustup target add aarch64-linux-android ^
REM                           armv7-linux-androideabi ^
REM                           x86_64-linux-android
REM    2. Android NDK r26 or newer:
REM         install via Android Studio's SDK manager, OR
REM         https://developer.android.com/ndk/downloads
REM       Then set:
REM         setx ANDROID_NDK_HOME "C:\Android\Sdk\ndk\26.1.10909125"
REM       (close & re-open cmd after setx)
REM    3. cargo-ndk:
REM         cargo install cargo-ndk
REM    4. Android SDK + Java 17 (for Gradle).
REM
REM  Usage:
REM    build-android.bat              REM cross-compile .so for 3 ABIs
REM    build-android.bat --apk        REM also assemble debug APK
REM    build-android.bat --release    REM release APK (UNSIGNED)
REM ============================================================
setlocal enabledelayedexpansion

set ROOT=%~dp0
set JNILIBS=%ROOT%android-app\app\src\main\jniLibs
set PROFILE=android-release

REM --- 0. Sanity checks ---
where cargo >nul 2>&1
if errorlevel 1 (
    echo ERROR: cargo not found. Install Rust from https://www.rust-lang.org/tools/install
    exit /b 1
)
cargo ndk --version >nul 2>&1
if errorlevel 1 (
    echo ERROR: cargo-ndk not installed. Run: cargo install cargo-ndk
    exit /b 1
)
if "%ANDROID_NDK_HOME%"=="" if "%NDK_HOME%"=="" if "%ANDROID_NDK_ROOT%"=="" (
    echo ERROR: ANDROID_NDK_HOME is not set.
    echo        Set it with:  setx ANDROID_NDK_HOME "C:\path\to\ndk"
    echo        Then close and re-open this cmd window.
    exit /b 1
)

echo ==^> Adding Rust Android targets if missing
rustup target add aarch64-linux-android      >nul
rustup target add armv7-linux-androideabi    >nul
rustup target add x86_64-linux-android       >nul

REM --- 1. Cross-compile for the 3 ABIs ---
if not exist "%JNILIBS%\arm64-v8a"   mkdir "%JNILIBS%\arm64-v8a"
if not exist "%JNILIBS%\armeabi-v7a" mkdir "%JNILIBS%\armeabi-v7a"
if not exist "%JNILIBS%\x86_64"      mkdir "%JNILIBS%\x86_64"

echo ==^> Cross-compiling Rust JNI lib (profile=%PROFILE%)
cd /d "%ROOT%"
cargo ndk ^
    -t arm64-v8a ^
    -t armeabi-v7a ^
    -t x86_64 ^
    -o "%JNILIBS%" ^
    --manifest-path android-jni\Cargo.toml ^
    build --profile %PROFILE%
if errorlevel 1 (
    echo Build failed.
    exit /b 1
)

echo.
echo ==^> Native libs produced:
dir /s /b "%JNILIBS%\*.so"

REM --- 2. Optional: build the APK ---
set ARG=%1
if /I "%ARG%"=="--apk"     goto APK_DEBUG
if /I "%ARG%"=="--debug"   goto APK_DEBUG
if /I "%ARG%"=="--release" goto APK_RELEASE
if "%ARG%"=="" goto DONE_LIBS_ONLY

echo Unknown argument: %ARG%
echo Usage: build-android.bat [--apk ^| --release]
exit /b 1

:APK_DEBUG
echo.
echo ==^> Assembling debug APK
cd /d "%ROOT%android-app"
call gradlew.bat assembleDebug
if errorlevel 1 exit /b 1
echo.
echo APK written to: android-app\app\build\outputs\apk\debug\app-debug.apk
goto END

:APK_RELEASE
echo.
echo ==^> Assembling release APK (unsigned)
cd /d "%ROOT%android-app"
call gradlew.bat assembleRelease
if errorlevel 1 exit /b 1
echo.
echo Unsigned APK: android-app\app\build\outputs\apk\release\app-release-unsigned.apk
echo Sign it with apksigner before installing.
goto END

:DONE_LIBS_ONLY
echo.
echo Native libs ready. Next step:
echo     cd android-app ^&^& gradlew.bat assembleDebug
echo OR re-run this script with --apk.

:END
endlocal
