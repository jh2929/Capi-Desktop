# Guía de contribución

Gracias por querer mejorar **Capi**. Este documento describe cómo reportar
problemas, proponer cambios y preparar un entorno de desarrollo.

> Antes de participar, lee el [Código de Conducta](CODE_OF_CONDUCT.md).

## Arquitectura

- **Shell:** Tauri 2 (Rust) + React 19 + TypeScript + Tailwind CSS 4.
- **Motor:** `capi-desktop/engine` (`capi-engine`), un crate Rust sin
  dependencias de Tauri que resuelve catálogo, streams y letras.
- **Sin daemon externo:** no hay proceso Kotlin ni protocolo stdin/stdout.

## Cómo contribuir

### Reportar un bug

Abre un [issue](https://github.com/jh2929/Capi-Desktop/issues) con:

- SO y arquitectura (Linux/Windows/macOS, x64/arm64).
- Versión de Capi y método de instalación.
- Pasos para reproducir, resultado esperado y resultado real.
- Logs del motor y, si aplica, el reporte de diagnóstico de la app.
- Capturas o grabación si el problema es visual.

### Proponer una función

Describe el problema que resuelve, el comportamiento propuesto y su alcance.
Abre el issue antes de invertir en una implementación grande.

### Enviar un Pull Request

1. Haz fork y crea una rama desde `master`.
2. Mantén el cambio enfocado; evita mezclar refactors no relacionados.
3. Añade o actualiza tests cuando toques el motor o la lógica de reproducción.
4. Ejecuta las comprobaciones de la sección de desarrollo.
5. Describe el propósito, la motivación, los cambios y los issues relacionados.

## Entorno de desarrollo

Requisitos:

- Node.js 22+ y npm.
- Rust estable (`rustup`).
- En Linux: `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
  `libayatana-appindicator3-dev`, `librsvg2-dev`, `patchelf` y `build-essential`.

```bash
git clone https://github.com/jh2929/Capi-Desktop.git
cd Capi-Desktop/capi-desktop
npm ci
npm run tauri:dev
```

Comprobaciones antes de abrir un PR:

```bash
cd capi-desktop
npx tsc --noEmit
npm run build

cd engine
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

cd ../src-tauri
cargo check
```

La prueba de integración contra YouTube es opcional y requiere red:

```bash
cd ../engine
cargo test --test live -- --ignored --nocapture
```

## Estilo

- **Rust:** `rustfmt` y `clippy` sin warnings. Nada de `unwrap`/`expect` en
  rutas de red o parseo de respuestas.
- **TypeScript/React:** mantener `strict` y `noUnusedLocals`; evitar `any`
  nuevos y el `JSON.parse` sin validación.
- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat(engine): …`, `fix(tauri): …`, `docs: …`).
- **Mensajes y UI:** español por defecto; las cadenas visibles deben pasar por
  `i18n.ts`.

## Traducciones

Las traducciones viven en `capi-desktop/src/i18n.ts`. Añade la clave en todos
los idiomas o deja que el fallback inglés la cubra, y prueba que la interfaz no
se rompe con textos largos.

## Diseño

Capi usa el sistema visual existente (Tailwind + variables CSS de tema). Para
propuestas de diseño, adjunta mockups y explica cómo encajan con los temas y la
accesibilidad actuales.

## Seguridad

No publiques vulnerabilidades en issues públicos. Consulta [SECURITY.md](SECURITY.md) para el canal privado y el alcance.
