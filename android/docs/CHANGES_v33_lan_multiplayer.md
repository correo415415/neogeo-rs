# v33 — Multijugador LAN (2 dispositivos, netplay por Wi-Fi)

## Problema pedido

> "El multiplayer no es en la misma pantalla, sino que será desde 2
> dispositivos distintos dentro de red local, y en cada uno se
> recibirá la información del otro."

## Diagnóstico (OODA)

### Observación

La v3 ya tenía un modo *local co-op* (dos pads en pantalla del mismo
teléfono, controlado por `Prefs.localMultiplayer`). No servía para
dos dispositivos.

Lo que hace falta:

1. **Descubrimiento** — que el móvil A vea al móvil B en la LAN sin
   escribir IPs.
2. **Transporte** — mover inputs entre ambos con RTT ≤ 20 ms y
   tolerar ligera pérdida de paquetes.
3. **Sincronización** — que ambos emuladores rueden el mismo frame
   con los mismos inputs, sin desincronizarse.

### Orientación

**Rollback netcode (GGPO-style)** es la opción premium para netplay
por Internet, pero para LAN es innecesariamente compleja (5-10 K LoC,
save-states rebobinables, mucho estado). La comunidad
(cf. wiki de RetroArch, docs de FBNeo, RetroPie NetPlay Guide) ha
convergido en:

> *"Delay frames zero is best to prevent de-syncs; works perfect on
>  a LAN, otherwise divide the RTT between server and client."*

Es decir: **lockstep determinista con input delay fijo**, un peer
autoritativo del reloj. Con RTT LAN de 1-5 ms, un delay de 2 frames
(33 ms) esconde toda la jitter.

### Decisión

**Arquitectura lockstep + input-delay = 2** con:

- Un peer **HOST** (autoritativo, es P1) y otro **CLIENT** (P2).
- TCP para handshake, ROM identity, keyframes y control.
- UDP para inputs por frame (700 B/s de ancho de banda, pérdida
  tolerada por *input hold*).
- Descubrimiento vía **NsdManager** (mDNS/DNS-SD) — nativo Android.
- Anti-desync: CRC32 del work RAM del 68K, comparado cada 60 frames.

### Acción

## Componentes nuevos

### Lado Rust (core)

**Fichero `android-jni/src/lib.rs`** — dos funciones JNI nuevas:

```rust
Java_com_pydmg_neogeo_NativeBridge_nativeFrameCounter()   -> jint
Java_com_pydmg_neogeo_NativeBridge_nativeStateChecksum()  -> jint
```

- `nativeFrameCounter` devuelve `System::dbg_frame` (u32 monotónico).
- `nativeStateChecksum` hace CRC-32 (IEEE 802.3 reflejado) de los
  64 KiB de `bus.work_ram`. Es suficiente para detectar desync
  porque todo el estado autoritativo (score, RNG, posiciones, timers)
  se escribe ahí. La palette RAM / VRAM son derivadas → no hacen
  falta.

Tabla CRC precomputada en `Lazy<[u32; 256]>` (~50 MB/s por core en
un ARM Cortex-A55). Coste por keyframe: 64 KiB en 1.3 ms → despreciable.

### Lado Kotlin (frontend)

Nuevo paquete `com.pydmg.neogeo.net`:

| Fichero | LoC | Rol |
|---|---:|---|
| `Protocol.kt` | 220 | Wire format binario, opcodes, packet encoders/decoders. |
| `NetplaySession.kt` | 380 | TCP+UDP transport, lockstep engine, desync detection. |
| `LanDiscovery.kt` | 130 | mDNS/NSD service register + discover. |

Y una activity nueva:

| Fichero | LoC | Rol |
|---|---:|---|
| `NetplayActivity.kt` | 220 | Wizard "Crear partida / Unirse" con auto-discovery + IP manual. |

## Protocolo (wire format)

Cada paquete arranca con `[0xD6, 0x64, 0x01, opcode]`. Little-endian.

### Handshake (TCP)

```
CLIENT ──HELLO(nick)──►         HOST
       ◄──HELLO_ACK(sess_id, input_delay)── HOST
CLIENT ──ROM_ID(cart, crc)──►   HOST         ┐  ambos validan
       ◄──ROM_ID(cart, crc)──   HOST         ┘  que la ROM coincide
       ◄──START(epoch_ms)────   HOST
```

### Partida en curso

- **UDP** (bidireccional, ~60 pkt/s por lado): `INPUT { frame, mask }`.
  Frame es el número al que se aplica el mask (ya con el delay
  aplicado por el emisor). 12 bytes por paquete.
- **TCP** (cada 60 frames): host manda `KEYFRAME { frame, crc32 }`;
  cliente responde `KEYFRAME_ACK { frame, ok }`.
- `PAUSE` / `RESUME` / `BYE` bidireccionales.

## Lockstep con input delay

En cada frame `T`, cada peer:

1. Lee su mask local del touch pad (`localMask`).
2. **Publica**: pone `localMask` en su cola local con clave `T+N`
   y lo envía por UDP al peer con la misma clave.
3. **Consulta**: busca en su cola la mask remota para `T`. Si no
   está, espera hasta `10 ms` (LAN ping típico < 1 ms). Si sigue
   sin llegar → *input hold* (reusa la última mask remota
   conocida).
4. Combina según el rol: `(local, remote)` si es HOST, `(remote, local)`
   si es CLIENT. Alimenta a `nativeSetPlayerInputs`.
5. Llama `nativeRunFrame` y avanza el contador de frames.

Con `N = 2` y RTT ≤ 33 ms, la mask remota llega a tiempo el 99.9%
de frames incluso sobre WiFi 2.4 GHz doméstico con interferencias.
Si la LAN es peor, el host puede subir el delay durante el
handshake.

## Descubrimiento

Servicio mDNS: `_pydmg-neogeo._tcp.` puerto 27750.

- **Host**: `NsdManager.registerService(name, type, port)`.
- **Client**: `NsdManager.discoverServices(type)`, resuelve peers a
  IP+puerto y los muestra en un ListView.
- Fallback manual: EditText para IP directa (para redes con
  client-isolation activado).

## Anti-desync

Cada 60 frames (1 segundo emulado):

1. Ambos calculan `crc = nativeStateChecksum()`.
2. Host envía `KEYFRAME { frame, host_crc }` por TCP.
3. Cliente compara con su propio CRC y responde `KEYFRAME_ACK`
   `ok = (host_crc == local_crc)`.
4. Si difiere → `session.desync` se rellena, ambos peers pausan y
   muestran toast: *"Desincronización detectada en el frame X. Pausado."*

v33 sólo *detecta* el desync. La *recuperación* (re-sync via save-
state completo) queda para v34 — para eso hay que implementar antes
la serialización de `System` (pendiente, mencionado en la sección
"Limitaciones" del README_ANDROID.md v3).

## Permisos añadidos al Manifest

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_WIFI_STATE" />
<uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE" />
```

- `INTERNET` es lo único imprescindible para sockets locales.
- `ACCESS_WIFI_STATE` para saber si el Wi-Fi está activo y avisar
  al usuario si intenta hostear sin Wi-Fi.
- `CHANGE_WIFI_MULTICAST_STATE` es lo que `NsdManager` necesita
  para hacer mDNS en algunos dispositivos (Samsung en particular).

## UX

- Nuevo toggle en Ajustes: **"Multijugador en red LAN"**.
- Al activarlo y pulsar una ROM, la app entra a `NetplayActivity`
  en vez de `EmulatorActivity`:
  - Botón "Crear partida" → registra mDNS, bindea TCP, espera.
    Muestra tu IP local por si el peer no ve el broadcast.
  - Botón "Unirse a partida" → escanea mDNS, muestra los hosts en
    una lista. Un tap conecta. Fallback "IP manual".
- Una vez conectados, ambos van a `EmulatorActivity` con el
  `PydmgApp.netSession` seteado; el bucle de emulación detecta la
  sesión y cambia al path netplay automáticamente.
- Si el peer se desconecta durante la partida, toast + `finish()`
  vuelta al launcher.

## Coste de rendimiento

Por frame (60 Hz):
- 1 UDP send (12 bytes) → < 50 µs.
- 1 UDP receive (12 bytes) → < 50 µs (no hay copia, es zero-copy
  con `DatagramPacket`).
- Cada 60 frames: 1 CRC32 de 64 KiB → 1.3 ms.

Total: **< 0.5% de CPU extra sobre el path solo**. La emulación en
sí (M68K + Z80 + LSPC + YM2610) es 10-20× más cara.

## Referencias externas

- Ars Technica, *"Explaining how fighting games use delay-based and
  rollback netcode"* — <https://arstechnica.com/gaming/2019/10/explaining-how-fighting-games-use-delay-based-and-rollback-netcode/>
- RetroPie NetPlay guide — *"Delay Frames zero is best to prevent
  de-syncs, works perfect on a LAN"* — <https://retropie.org.uk/forum/topic/6231/any-netplay-experts-around>
- SnapNet, *"Netcode Architectures Part 2: Rollback"* — <https://www.snapnet.dev/blog/netcode-architectures-part-2-rollback/>
- Android developer docs, `NsdManager`: <https://developer.android.com/develop/connectivity/wifi/use-nsd>
