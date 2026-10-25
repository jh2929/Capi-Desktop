# capi-engine

Motor Rust de Capi. Sustituye al daemon Kotlin `capi-core` y se ejecuta dentro
del proceso Tauri, sin protocolo stdin/stdout, sin watchdog y sin binario
sidecar.

## Estado

- **Fase 0 (completada):** validación de `rustypipe`; el cliente iOS resuelve
  audio AAC/Opus sin PO tokens ni Botguard. Ver [`docs/phase-0.md`](docs/phase-0.md).
- **Fase 1 (en curso):** API de catálogo (`search`, `album`, `artist`,
  `playlist`, `home`, `explore`) y resolución de streams con caché TTL.
- **Fase 2:** letras, integración Tauri y paridad.
- **Fase 3:** retirada de Kotlin/Gradle.

## Uso

```bash
cargo test
cargo run --example engine_probe -- "daft punk"
cargo run --example probe -- "daft punk"   # sonda de bajo nivel
```

## Licencia

GPL-3.0-only, igual que el resto de Capi.
