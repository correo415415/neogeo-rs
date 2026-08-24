//! Savestates: round-trip determinista y guardias de identidad.
//!
//! Sin ROMs reales: se construye un `System` desnudo, se ejecuta un puñado
//! de frames sobre la BIOS vacía (0xFF) y se comprueba que guardar → seguir
//! corriendo → cargar → volver a correr produce estados idénticos.

use pydmg_neogeo::state::StateError;
use pydmg_neogeo::{Hardware, System, SystemConfig};

fn test_config() -> SystemConfig {
    SystemConfig {
        hardware: Hardware::Mvs,
        trace_cpu: false,
        trace_audio_io: false,
        audio_sample_rate: None,
    }
}

/// Huella del estado observable: registros de CPU + contadores + muestras de RAM.
fn fingerprint(sys: &System) -> (u32, u64, u64, Vec<u8>) {
    let mut ram = Vec::new();
    ram.extend_from_slice(&sys.bus.work_ram[0..256]);
    ram.extend_from_slice(&sys.bus.palette_ram[0..64]);
    (sys.m68k.pc, sys.m68k.cycles, sys.master_cycles, ram)
}

#[test]
fn save_load_round_trip_is_deterministic() {
    let mut a = System::new(test_config());
    a.game_name = "testgame".to_string();
    a.reset();
    for _ in 0..3 {
        a.run_frame();
    }

    // Guarda, sigue corriendo N frames y registra la huella final.
    let snap = a.save_state();
    assert!(
        snap.len() > 0x30000,
        "snapshot sospechosamente pequeño: {}",
        snap.len()
    );
    for _ in 0..5 {
        a.run_frame();
    }
    let expected = fingerprint(&a);

    // Restaura sobre la misma instancia y repite: debe converger a la misma huella.
    a.load_state(&snap)
        .expect("load_state debe aceptar su propio snapshot");
    for _ in 0..5 {
        a.run_frame();
    }
    assert_eq!(fingerprint(&a), expected, "replay tras load_state divergió");
}

#[test]
fn load_state_rejects_other_game() {
    let mut a = System::new(test_config());
    a.game_name = "mslugx".to_string();
    a.reset();
    let snap = a.save_state();

    let mut b = System::new(test_config());
    b.game_name = "kof98".to_string();
    b.reset();
    match b.load_state(&snap) {
        Err(StateError::GameMismatch { expected, found }) => {
            assert_eq!(expected, "kof98");
            assert_eq!(found, "mslugx");
        }
        other => panic!("esperaba GameMismatch, obtuve {other:?}"),
    }
}

#[test]
fn load_state_rejects_garbage_and_truncation() {
    let mut sys = System::new(test_config());
    sys.game_name = "testgame".to_string();
    sys.reset();

    // Basura sin cabecera.
    assert!(matches!(
        sys.load_state(b"not a savestate"),
        Err(StateError::BadMagic)
    ));

    // Snapshot truncado a la mitad: debe fallar Y dejar el sistema utilizable
    // (carga transaccional — el estado de rescate se restaura).
    let snap = sys.save_state();
    let before = sys.m68k.pc;
    let truncated = &snap[..snap.len() / 2];
    assert!(matches!(
        sys.load_state(truncated),
        Err(StateError::Truncated)
    ));
    assert_eq!(sys.m68k.pc, before, "estado corrompido tras carga fallida");
    sys.run_frame(); // no debe hacer panic
}

#[test]
fn version_guard() {
    let mut sys = System::new(test_config());
    sys.game_name = "testgame".to_string();
    sys.reset();
    let mut snap = sys.save_state();
    snap[4] = 0xFF; // versión LE bytes 4..6
    snap[5] = 0xFF;
    assert!(matches!(
        sys.load_state(&snap),
        Err(StateError::BadVersion(0xFFFF))
    ));
}
