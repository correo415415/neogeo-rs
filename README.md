# neogeo-rs · pydmg-neogeo

Emulador de **Neo Geo (MVS/AES)** escrito en Rust, con dos frontends:

```
/pydmg     → Emulador de PC completo
             · core Rust (neogeo/) + CLI + GUI SDL2 fullscreen
             · tests, docs y examples
             · compilar:  cd pydmg && cargo build --release

/android   → Puerto Android nativo
             · neogeo/      — copia duplicada del core (sólo librería, sin SDL2)
             · android-jni/ — puente JNI (cdylib) para la app Kotlin
             · android-app/ — app Kotlin: SurfaceView + AAudio + controles
                              táctiles + biblioteca SAF + netplay LAN
             · compilar:    cd android && ./build-android.sh --release
                            (genera APK release firmada automáticamente)
```

Cada carpeta es autosuficiente: `pydmg/` es un paquete Cargo independiente y
`android/` es un workspace Cargo (`[".", "android-jni"]`) con su propia copia
del core, de modo que el puerto Android nunca rompe la build de PC (ni al
revés).

## Requisitos rápidos

- **PC (Linux)**: `sudo apt install libsdl2-dev pkg-config` + Rust estable.
- **Android**: NDK r26+, `cargo install cargo-ndk`, targets
  `aarch64-linux-android` / `armv7-linux-androideabi` / `x86_64-linux-android`,
  Android SDK + Java 17 para Gradle. Detalles en `android/README_ANDROID.md`.

## Uso del emulador de PC

```bash
cd pydmg
cargo build --release
./target/release/pydmg-neogeo --bios-zip neogeo.zip --cart mslug.zip
```

Modo headless para pruebas/CI: `--headless --max-frames N`.
