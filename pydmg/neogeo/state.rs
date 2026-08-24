//! Savestates — serialización binaria del estado de emulación.
//!
//! Formato del fichero/buffer:
//! ```text
//!   "NGSS"            4 bytes  magic
//!   version: u16      little-endian (STATE_VERSION)
//!   name_len: u32     little-endian
//!   game_name         name_len bytes UTF-8 (set MAME, p.ej. "mslugx")
//!   payload           volcado campo-a-campo de System (ver system.rs)
//! ```
//!
//! Principios de diseño:
//!   * **Sin dependencias**: serde no cubre arrays grandes (`[u16; 0x8800]`
//!     de la VRAM, `[u16; 0x1000]` del PVC…) sin crates auxiliares, así que
//!     usamos un trait propio con impls const-genéricas.
//!   * **Las ROM no se serializan**: `p_rom`, `system_rom`, C/S/M/V-ROMs y
//!     gráficos predecodificados se conservan de la instancia viva. Un
//!     savestate ronda ~220 KiB.
//!   * **Guardia de identidad**: `load_state` rechaza estados de otro juego
//!     (GameMismatch) o de otra versión de formato.
//!   * **Carga transaccional**: `System::load_state` toma un estado de
//!     rescate antes de aplicar el payload; si la carga falla a medias se
//!     restaura el estado previo.
//!
//! Cada struct del núcleo implementa [`StateSer`] en su propio módulo (para
//! poder acceder a campos privados) mediante la macro [`state_fields!`].

use std::fmt;

/// Versión del formato. Incrementar en cambios incompatibles del payload.
pub const STATE_VERSION: u16 = 1;
/// Magic de cabecera: "NGSS" = Neo Geo Save State.
pub const STATE_MAGIC: [u8; 4] = *b"NGSS";

/// Errores de deserialización de un savestate.
#[derive(Debug)]
pub enum StateError {
    /// El buffer terminó antes de completar la lectura.
    Truncated,
    /// La cabecera no empieza por "NGSS".
    BadMagic,
    /// Versión de formato no soportada.
    BadVersion(u16),
    /// El estado pertenece a otro juego.
    GameMismatch { expected: String, found: String },
    /// Valor fuera de rango (tag de enum desconocido, etc.).
    Corrupt(&'static str),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "savestate truncado"),
            Self::BadMagic => write!(f, "cabecera invalida (no es un savestate NGSS)"),
            Self::BadVersion(v) => write!(f, "version de savestate no soportada: {v}"),
            Self::GameMismatch { expected, found } => write!(
                f,
                "el savestate es de '{found}' pero el juego cargado es '{expected}'"
            ),
            Self::Corrupt(what) => write!(f, "savestate corrupto: {what}"),
        }
    }
}

impl std::error::Error for StateError {}

/// Lector secuencial con comprobación de límites.
pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StateReader<'a> {
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Bytes aún no consumidos.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Consume exactamente `n` bytes.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], StateError> {
        if self.remaining() < n {
            return Err(StateError::Truncated);
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, StateError> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, StateError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u32(&mut self) -> Result<u32, StateError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Result<u64, StateError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

/// Serialización binaria campo a campo (little-endian).
pub trait StateSer {
    fn save(&self, out: &mut Vec<u8>);
    fn load(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError>;
}

macro_rules! impl_prim {
    ($($t:ty => $rd:ident),* $(,)?) => {$(
        impl StateSer for $t {
            #[inline]
            fn save(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            #[inline]
            fn load(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
                *self = r.$rd()? as $t;
                Ok(())
            }
        }
    )*};
}

impl_prim!(u8 => u8, u16 => u16, u32 => u32, u64 => u64);
impl_prim!(i8 => u8, i16 => u16, i32 => u32, i64 => u64);

impl StateSer for bool {
    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
    #[inline]
    fn load(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        *self = match r.u8()? {
            0 => false,
            1 => true,
            _ => return Err(StateError::Corrupt("bool fuera de rango")),
        };
        Ok(())
    }
}

/// `usize` se serializa siempre como u64 (portabilidad 32/64 bits).
impl StateSer for usize {
    #[inline]
    fn save(&self, out: &mut Vec<u8>) {
        (*self as u64).save(out);
    }
    #[inline]
    fn load(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        let v = r.u64()?;
        *self = usize::try_from(v).map_err(|_| StateError::Corrupt("usize desbordado"))?;
        Ok(())
    }
}

impl<T: StateSer, const N: usize> StateSer for [T; N] {
    fn save(&self, out: &mut Vec<u8>) {
        for e in self {
            e.save(out);
        }
    }
    fn load(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        for e in self.iter_mut() {
            e.load(r)?;
        }
        Ok(())
    }
}

impl<T: StateSer + ?Sized> StateSer for Box<T> {
    fn save(&self, out: &mut Vec<u8>) {
        (**self).save(out);
    }
    fn load(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        (**self).load(r)
    }
}

impl<T: StateSer + Default> StateSer for Option<T> {
    fn save(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(v) => {
                out.push(1);
                v.save(out);
            }
        }
    }
    fn load(&mut self, r: &mut StateReader<'_>) -> Result<(), StateError> {
        *self = match r.u8()? {
            0 => None,
            1 => {
                let mut v = T::default();
                v.load(r)?;
                Some(v)
            }
            _ => return Err(StateError::Corrupt("Option fuera de rango")),
        };
        Ok(())
    }
}

/// Implementa [`StateSer`] volcando/cargando los campos listados en orden.
/// Debe invocarse en el módulo del struct para poder tocar campos privados.
macro_rules! state_fields {
    ($t:ty { $($f:ident),* $(,)? }) => {
        impl $crate::state::StateSer for $t {
            fn save(&self, out: &mut Vec<u8>) {
                $( $crate::state::StateSer::save(&self.$f, out); )*
            }
            fn load(
                &mut self,
                r: &mut $crate::state::StateReader<'_>,
            ) -> Result<(), $crate::state::StateError> {
                $( $crate::state::StateSer::load(&mut self.$f, r)?; )*
                Ok(())
            }
        }
    };
}

pub(crate) use state_fields;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_round_trip() {
        let mut out = Vec::new();
        0xAAu8.save(&mut out);
        0x1234u16.save(&mut out);
        0xDEAD_BEEFu32.save(&mut out);
        (-5i64).save(&mut out);
        true.save(&mut out);
        [1u16, 2, 3].save(&mut out);
        Some(7u32).save(&mut out);
        (usize::MAX / 2).save(&mut out);

        let mut r = StateReader::new(&out);
        let (mut a, mut b, mut c, mut d, mut e) = (0u8, 0u16, 0u32, 0i64, false);
        let mut f = [0u16; 3];
        let mut g: Option<u32> = None;
        let mut h = 0usize;
        a.load(&mut r).unwrap();
        b.load(&mut r).unwrap();
        c.load(&mut r).unwrap();
        d.load(&mut r).unwrap();
        e.load(&mut r).unwrap();
        f.load(&mut r).unwrap();
        g.load(&mut r).unwrap();
        h.load(&mut r).unwrap();
        assert_eq!(
            (a, b, c, d, e, f, g, h),
            (
                0xAA,
                0x1234,
                0xDEAD_BEEF,
                -5,
                true,
                [1, 2, 3],
                Some(7),
                usize::MAX / 2
            )
        );
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn truncated_read_fails() {
        let out = vec![1u8, 2];
        let mut r = StateReader::new(&out);
        let mut v = 0u32;
        assert!(matches!(v.load(&mut r), Err(StateError::Truncated)));
    }
}
