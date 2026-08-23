# Tests deshabilitados por defecto

Estos tests requieren corpora externos enormes (cientos de MiB de JSON) de:

* https://github.com/SingleStepTests/m68000
* https://github.com/SingleStepTests/z80

Cargo los descubre automáticamente si están en `tests/*.rs`, pero arrastran
`serde_json` + `regex-automata` + ~50 transitive deps, lo que agota la RAM del
sandbox (1 GiB sin swap) y dispara timeouts.

## Cómo ejecutarlos

```bash
git clone https://github.com/SingleStepTests/m68000 tests/_disabled/m68000-corpus
git clone https://github.com/SingleStepTests/z80    tests/_disabled/z80-corpus
mv tests/_disabled/m68k_single_step_tests.rs tests/
mv tests/_disabled/z80_single_step_tests.rs  tests/
# Añadir `serde_json = "1"` a dev-dependencies en Cargo.toml
cargo test --release --test m68k_single_step_tests
cargo test --release --test z80_single_step_tests
```

Resultado conocido en v31 upstream:

* M68000: 317 500 / 317 500 sub-tests OK
* Z80:    1 604 000 / 1 604 000 sub-tests OK
