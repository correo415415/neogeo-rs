# pydmg-neogeo — Android port

Port nativo a Android del emulador `pydmg-neogeo` (Neo Geo en Rust).
Arquitectura: **core Rust → cdylib JNI → Activity Kotlin**, sin SDL2 (la
UI es 100 % Android nativa: `SurfaceView` + `AudioTrack` + controles
 táctiles).

## Novedades v0.2 (rendimiento + interfaz + release firmada)

- **⚡ Render por GPU**: `lockHardwareCanvas` (API 26+) con fallback
  software — el blit+escala baja de 4-10 ms/frame a <1 ms en SoCs de
  gama baja. Clear de fondo solo tras cambios de geometría.
- **🎯 Pacing preciso a 59.185606 Hz** con deadline monotónico
  (`parkNanos` + yield-spin acotado). Corrige la velocidad x1.5-x2.4
  en pantallas de 90/120 Hz. `Surface.setFrameRate(60, FIXED_SOURCE)`.
- **🛟 Frameskip adaptativo** (máx. 2 presentaciones seguidas): en
  hardware débil el juego mantiene velocidad y audio perfectos aunque
  el blit no llegue a 60 fps.
- **🔋 `setSustainedPerformanceMode`** + `THREAD_PRIORITY_DISPLAY` en
  el hilo del emulador: clocks sostenidos sin throttling térmico.
- **📚 Biblioteca con títulos reales**: base de datos de ~150 sets
  ("Metal Slug 3 · 2000 · SNK" en vez de "mslug3"), tiles con acento
  de color por juego, filtrado fluido con DiffUtil, ripple y chevron.
- **✨ Pulido de UX**: overlay de pausa con fade, vibración háptica en
  botones táctiles (conmutable en Ajustes).
- **📦 APK release firmada automáticamente**:
  `./build-android.sh --release` genera el keystore (primer uso) y
  produce `app-release.apk` instalable directamente. R8 minify +
  shrinkResources (~40 % menos APK).

## Novedades de la revisión anterior (v3.2 — audio-fix + LAN multiplayer)

- **🌐 Multijugador LAN (2 dispositivos).** Nueva pantalla "Crear
  partida / Unirse" con autodescubrimiento por mDNS y fallback
  manual por IP. Uno de los dispositivos hace de HOST (P1) y el
  otro de CLIENT (P2); ambos rueden el emulador determinísticamente
  con lockstep + input-delay = 2 frames sobre UDP. Detección de
  desync vía CRC-32 del work RAM cada segundo emulado. Ver
  `docs/CHANGES_v33_lan_multiplayer.md`.
- **🎵 Audio limpio (fix del resampling 55555 → 48000 Hz).** Antes, el
  emulador entregaba samples a los 55 555 Hz nativos del YM2610 y el
  AudioFlinger de Android hacía resampling en modo *interpolated
  polyphase* (excluyendo el FastMixer y añadiendo aliasing +
  chasquidos). Ahora el resampling se hace **dentro del core Rust**
  con un FIR polifase windowed-sinc de Kaiser (β = 8.6, 65 taps,
  contador fraccional entero), y el `AudioTrack` de Android va al
  rate nativo del dispositivo (48 kHz) por la ruta FastMixer.
  Ver `docs/CHANGES_v32_audio_resampling.md`.
- **Launcher / biblioteca de ROMs** con autodetección desde carpeta SAF.
- **Panel de ajustes** con joystick ↔ flechas, opacidad y escala.
- **HUD in-game más limpio**: el juego queda menos tapado.
- **Multijugador local opcional (P2)** con overlay compacto (mismo
  dispositivo, dos pads en pantalla).

---

## Qué hay aquí

```
android-build/
├── neogeo/, cli/, tests/, examples/, docs/  ← código original v37 (intacto)
├── Cargo.toml                                ← raíz workspace + feature-gates
├── android-jni/                              ← crate cdylib JNI
│   ├── Cargo.toml
│   └── src/lib.rs                            ← 12 funciones #[no_mangle]
├── android-app/                              ← proyecto Android Gradle
│   ├── settings.gradle.kts
│   ├── build.gradle.kts
│   ├── gradle.properties
│   ├── gradlew / gradlew.bat                 ← Gradle 8.7 wrapper oficial
│   ├── gradle/wrapper/
│   └── app/
│       ├── build.gradle.kts                  ← AGP 8.5, Kotlin 1.9, SDK 34
│       ├── proguard-rules.pro                ← keep JNI symbols
│       └── src/main/
│           ├── AndroidManifest.xml           ← SAF, sensorLandscape, fullscreen
│           ├── java/com/pydmg/neogeo/
│           │   ├── NativeBridge.kt           ← interfaz JNI
│           │   ├── EmulatorView.kt           ← SurfaceView + 320×224 bitmap
│           │   ├── AudioEngine.kt            ← AudioTrack @ 55 555 Hz
│           │   └── MainActivity.kt           ← orquestador + ciclo de vida
│           ├── res/                          ← layouts, themes, drawables, icons
│           └── jniLibs/                      ← .so destino (creado por build-android)
├── tests/from_bytes_smoke.rs                 ← test E2E con mslug.zip real ✅
├── build-android.sh                          ← script Linux/macOS/WSL
└── build-android.bat                         ← script Windows cmd
```

---

## Cambios al core (no rompen el CLI existente)

1. **`neogeo/memory/rom.rs`**: añadidas 4 funciones nuevas que aceptan
   bytes en lugar de paths — el CLI y los tests siguen usando las
   antiguas:

   ```rust
   pub fn load_bios_from_bytes(&mut self, data: Vec<u8>) -> Result<()>;
   pub fn load_parent_bios_zip_from_bytes(&mut self, zip: &[u8]) -> Result<()>;
   pub fn load_cart_zip_from_bytes(&mut self, name: &str, zip: &[u8]) -> Result<()>;
   pub fn pick_bios_from_zip_bytes(&mut self, zip: &[u8], wanted: &str) -> Result<()>;
   ```

2. **`Cargo.toml`** (raíz): convertido en **workspace** y `sdl2/clap/env_logger`
   movidas detrás de la feature `cli` (habilitada por defecto, así
   `cargo build --release` sigue produciendo el binario SDL2 como
   siempre). Para Android: `--no-default-features` corta SDL2 del grafo
   de compilación cruzada.

3. **Nuevo perfil `[profile.android-release]`** (inherits `release` +
   `strip=symbols`, `panic=abort`) que adelgaza el `.so` a 826 KB
   contra los 1019 KB del release normal.

Verificación en host Linux x86_64:

| Comprobación | Resultado |
|---|---|
| `cargo check --lib --no-default-features` | ✅ 0 warnings |
| `cargo build -p pydmg-neogeo-jni --lib --profile android-release` | ✅ 826 KB `.so` con 12 símbolos `Java_com_pydmg_*` exportados |
| `cargo check --features cli --bin pydmg-neogeo` | ✅ CLI v37 sigue compilando |
| `cargo test --release --test from_bytes_smoke` con `mslug.zip` real | ✅ 320 frames OK, 64 740/71 680 píxeles dibujados |

---

## Pre-requisitos del sistema

Lo MISMO que necesita cualquier proyecto Android moderno; nada nuestro:

| Pieza | Versión recomendada |
|---|---|
| Rust toolchain | stable (1.78+) |
| Android NDK | r26d (`26.1.10909125`) |
| Android SDK | API 34 (compileSdk) |
| Java JDK | 17 (Temurin / Microsoft / Oracle) |
| Gradle | 8.7 (lo trae el wrapper, no hay que instalarlo) |
| `cargo-ndk` | última (`cargo install cargo-ndk`) |

---

## ▶ Compilar — Linux / macOS / WSL2

### Una sola vez (setup)

```bash
# 1. Rust + targets Android
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk

# 2. Java 17 (Ubuntu 22.04+)
sudo apt update && sudo apt install -y openjdk-17-jdk unzip wget

# 3. Android command-line tools + NDK + platforms
mkdir -p ~/Android/Sdk/cmdline-tools && cd ~/Android/Sdk/cmdline-tools
wget https://dl.google.com/android/repository/commandlinetools-linux-11076708_latest.zip
unzip commandlinetools-linux-*.zip && mv cmdline-tools latest
export ANDROID_HOME=$HOME/Android/Sdk
export PATH=$PATH:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools
yes | sdkmanager --licenses
sdkmanager "platform-tools" "platforms;android-34" "build-tools;34.0.0" "ndk;26.1.10909125"

# 4. Variables de entorno persistentes
echo 'export ANDROID_HOME=$HOME/Android/Sdk'                               >> ~/.bashrc
echo 'export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/26.1.10909125'             >> ~/.bashrc
echo 'export PATH=$PATH:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools' >> ~/.bashrc
source ~/.bashrc
```

### Build

```bash
cd /ruta/a/android-build

# Compilar el .so para los 3 ABIs (~1 min en una máquina decente):
./build-android.sh

# Compilar APK debug instalable (~2 min primera vez por descarga de Gradle):
./build-android.sh --apk

# Instalar con ADB en un móvil conectado por USB (USB debugging ON):
adb install -r android-app/app/build/outputs/apk/debug/app-debug.apk
```

APK firmado release (necesitas tu propio keystore — instrucciones más abajo):

```bash
./build-android.sh --release
# Genera android-app/app/build/outputs/apk/release/app-release-unsigned.apk
# Lo firmas con apksigner:
$ANDROID_HOME/build-tools/34.0.0/apksigner sign \
    --ks ~/.android/release.keystore \
    --out app-release.apk app-release-unsigned.apk
```

---

## ▶ Compilar — Windows

### Una sola vez (setup)

1. **Rust**: descarga `rustup-init.exe` de <https://www.rust-lang.org/tools/install>
   y acepta los defaults (toolchain MSVC).
2. **Targets + cargo-ndk** (en `cmd.exe`):
   ```cmd
   rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
   cargo install cargo-ndk
   ```
3. **Java JDK 17**: instala Temurin desde
   <https://adoptium.net/temurin/releases/?version=17>. Verifica:
   ```cmd
   java -version
   ```
4. **Android Studio** (recomendado, lo más rápido):
   <https://developer.android.com/studio>. Tras instalar:
   - Abre Android Studio → *More Actions* → *SDK Manager*.
   - Pestaña *SDK Platforms*: marca **Android 14 (API 34)**.
   - Pestaña *SDK Tools*: marca **NDK (Side by side) 26.1.10909125**
     y **Android SDK Command-line Tools (latest)**.
   - Apply → acepta licencias → espera la descarga (~3 GB).
5. **Variables de entorno** (Win+R → `sysdm.cpl` → Avanzado → Variables
   de entorno; o `setx` desde cmd):
   ```cmd
   setx ANDROID_HOME      "%LOCALAPPDATA%\Android\Sdk"
   setx ANDROID_NDK_HOME  "%LOCALAPPDATA%\Android\Sdk\ndk\26.1.10909125"
   setx PATH "%PATH%;%LOCALAPPDATA%\Android\Sdk\platform-tools"
   ```
   **Cierra y reabre `cmd.exe`** para que coja los cambios.

### Build

```cmd
cd C:\ruta\a\android-build

REM 1) Compilar .so para los 3 ABIs:
build-android.bat

REM 2) Compilar APK debug:
build-android.bat --apk

REM 3) Instalar en móvil conectado:
adb install -r android-app\app\build\outputs\apk\debug\app-debug.apk
```

Si Defender / antivirus se queja del `gradle-wrapper.jar`, añade exclusión
en la carpeta del proyecto.

---

## ▶ Verificar la cadena en tu máquina (smoke test)

Antes de tirar el .so cruzado, valida que el host compila:

```bash
# Linux
cargo build -p pydmg-neogeo-jni --lib --profile android-release
ls -lh target/android-release/libpydmg_neogeo_jni.so
nm -D target/android-release/libpydmg_neogeo_jni.so | grep Java_com_pydmg | wc -l
# Debe imprimir 12.
```

Test end-to-end con tus ROMs (opcional pero recomendado — confirma que
las APIs nuevas cargan los zips correctamente):

```bash
PYDMG_BIOS_ZIP=/ruta/a/neogeo.zip \
PYDMG_MSLUG_ZIP=/ruta/a/mslug.zip \
cargo test --no-default-features --release --test from_bytes_smoke -- --nocapture
# Debe imprimir:
#   OK: mslug ran 320 frames, nonzero pixels = ~64000
```

---

## ▶ Cargar ROMs en el dispositivo

La app usa **Storage Access Framework (SAF)**, así que **no hace falta**
permiso `READ_EXTERNAL_STORAGE` ni `MANAGE_EXTERNAL_STORAGE`. Workflow:

1. Copia `neogeo.zip` y `mslug.zip` al móvil (cualquier carpeta:
   `/sdcard/Download/`, una USB-OTG, Google Drive, …).
2. Abre la app *pydmg-neogeo*.
3. Pulsa **"Elegir BIOS (neogeo.zip)"** → SAF picker → selecciona el
   archivo. Verás un toast verde *"Listo. Inserta moneda."*
4. Pulsa **"Elegir cartucho (mslug.zip)"** → mismo proceso. La emulación
   arranca automáticamente; tras unos segundos verás el eyecatcher de
   Neo Geo y luego la pantalla de título de Metal Slug.
5. Pulsa **COIN** → **START** para empezar a jugar.

> **Nota legal:** el repositorio solo trae código. Las BIOS y los
> cartuchos del Neo Geo son propiedad de SNK Playmore. Dump-éalos de
> tu propio hardware o consigue una licencia.

---

## ▶ Controles en pantalla

| Botón | Acción Neo Geo |
|---|---|
| Cruceta ▲▼◀▶ | Movimiento P1 |
| A B C D     | Botones de acción P1 (cuatro botones del MVS) |
| START       | Botón START / Pause |
| SELECT      | Botón SELECT |
| COIN        | Insertar moneda (Coin-in 1) |

---

## ▶ Tamaño del APK estimado

| Componente | Tamaño |
|---|---:|
| `libpydmg_neogeo_jni.so` (arm64-v8a) | ~ 850 KB |
| `libpydmg_neogeo_jni.so` (armeabi-v7a) | ~ 750 KB |
| `libpydmg_neogeo_jni.so` (x86_64) | ~ 900 KB |
| AndroidX + Material3 + Activity | ~ 5 MB |
| Recursos + DEX Kotlin | ~ 500 KB |
| **APK universal (3 ABIs)** | **~ 8 MB** |

Para reducir a ~ 4 MB usa ABI splits en `build.gradle.kts`:

```kotlin
android {
    splits {
        abi {
            isEnable = true
            reset()
            include("arm64-v8a", "armeabi-v7a")  // sin x86_64
            isUniversalApk = false
        }
    }
}
```

---

## ▶ Limitaciones conocidas

* **Sólo Player 1** en esta UI (los controles P2 no están expuestos
  en el layout — el bus emulado los soporta, sería 30 minutos de XML).
* **Sin save-states**: el core no los implementa (esfuerzo no trivial
  porque `System` tiene `Box<[u8; ...]>` que no derivan `Serialize`).
* **Sin grabación de audio/video** en la app (el core lo soporta vía
  `--audio-out` / `--dump-frames-dir`, sólo desde el CLI).
* **FM no es cycle-accurate** (aproximación funcional; ver
  `docs/AUDIO_AUDIT.md` y `neogeo/dead_code/nuked_opn_scaffold.rs.txt`).
* **Carts con NEO-PVC runtime** (`mslug5`, `svc`, `kof2003`) llegan a la
  pantalla de presentación pero el `PvcRuntime` aún no está cableado al
  bus 68K (pendiente, doc en `CHANGES_v37`).

---

## ▶ Para añadir Player 2

En `MainActivity.kt`, expón un segundo bitmask y mapéalo a
`sys.bus.p2_input` mediante una función JNI análoga a `nativeSetInputs`
(la duplicación es trivial: copia/pega cambiando `p1_input` → `p2_input`
y `start_select` ya cubre P2 con bits 2/3).

---

## ▶ Si algo falla

| Síntoma | Causa típica | Fix |
|---|---|---|
| `cargo-ndk` no encuentra targets | Faltan `rustup target add ...` | Ver setup arriba |
| `linker not found` cruzando | `ANDROID_NDK_HOME` mal | `echo $ANDROID_NDK_HOME && ls $ANDROID_NDK_HOME` |
| Gradle se queja de Java < 17 | JDK 11/8 instalado | `JAVA_HOME=/path/to/jdk17 ./gradlew ...` |
| APK se instala pero crashea al abrir | `.so` no embebida o ABI no incluida | `unzip -l app-debug.apk | grep .so` |
| Pantalla negra con sonido | Surface aún no creado en frame 0 | Es normal — espera ~1 s |
| Audio cracking | Buffer demasiado pequeño | Sube `bufBytes` en `AudioEngine.kt` a `minBuf * 8` |

---

## ▶ Métricas internas (FYI)

Build host Linux x86_64 (i5-1135G7), libs:

| Profile | Tamaño `.so` | Tiempo compile clean |
|---|---:|---:|
| `dev`              | 4.7 MB | 35 s |
| `release`          | 1019 KB | 23 s |
| `android-release`  | **826 KB** | 22 s |

Tests en host:

```
running 1 test
test from_bytes_loads_mslug_and_runs_one_frame ... ok (0.75 s)
```
