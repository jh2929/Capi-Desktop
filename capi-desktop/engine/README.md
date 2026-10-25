# capi-engine

Motor Rust de Capi. Sustituye al daemon Kotlin `capi-core` y se ejecuta dentro
del proceso Tauri, sin protocolo stdin/stdout, sin watchdog y sin binario
sidecar.

## Estado

- **Fase 0 (completada):** validación de `rustypipe`; el cliente iOS resuelve
  audio AAC/Opus sin PO tokens ni Botguard. Ver [`docs/phase-0.md`](docs/phase-0.md).
- **Fase 1 (completada):** API de catálogo, resolución de streams con caché
  TTL, letras y comandos Tauri con el mismo contrato JSON que el daemon.
- **Fase 2 (en curso):** QA runtime, Windows/macOS, endurecimiento del proxy
  local y descargas.
- **Fase 3 (completada):** retirada de Kotlin/Gradle y del binario de 61 MB.

## Uso

```bash
cargo test
cargo run --example engine_probe -- "daft punk"
cargo run --example probe -- "daft punk"   # sonda de bajo nivel
```

## Licencia

GPL-3.0-only, igual que el resto de Capi.
