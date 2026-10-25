import type { BusEvent } from "../types";
import { BaseDetector } from "./Detector";

/**
 * Detector del backend — analiza las respuestas de capi-engine
 * (obtener_stream, obtener_playlist, etc.) y los eventos de su ciclo de vida
 * (timeout, errores de red) buscando desviaciones del comportamiento esperado.
 */
export class BackendDetector extends BaseDetector {
  readonly id = "backend";
  readonly name = "Detector del backend (capi-engine)";

  onEvent(evt: BusEvent): void {
    switch (evt.type) {
      case "backend.error": {
        const raw = String(evt.payload.reason ?? evt.payload.error ?? "");
        const reason = extractInnerError(raw);
        if (/engine|motor|dejó de responder|timeout|timed out/i.test(raw)) {
          this.engine.reportAnomaly({
            code: "BACKEND_TIMEOUT",
            severity: "error",
            message: `El motor Rust no respondió a tiempo; la petición fue cancelada${reason ? ` (${reason})` : ""}.`,
            detectorId: this.id,
            suggestion:
              "El motor no respondió dentro del plazo. Los reintentos automáticos deberían recuperar la reproducción; si persiste, revisa la red y los logs.",
          });
        } else if (looksLikeNetworkError(raw)) {
          this.engine.reportAnomaly({
            code: "NETWORK_ERROR",
            severity: "error",
            message: `El backend no pudo completar la petición por un error de red${reason ? `: ${reason}` : ""}.`,
            detectorId: this.id,
            suggestion: "La conexión del backend con YouTube Music falló. Verifica la conectividad general.",
          });
        } else {
          this.engine.reportAnomaly({
            code: "BACKEND_ERROR",
            severity: "error",
            message: `El backend devolvió un error inesperado: ${reason || "sin detalles"}.`,
            detectorId: this.id,
            suggestion:
              "La respuesta del motor no es la esperada. Revisa los logs y el mensaje de error original.",
          });
        }
        break;
      }

      case "stream.resolve_failed": {
        const raw = String(evt.payload.reason ?? "");
        if (looksLikeNetworkError(raw) && !/motor|engine/i.test(raw)) {
          this.engine.reportAnomaly({
            code: "NETWORK_ERROR",
            severity: "error",
            message: `La resolución del stream falló por un error de red: ${raw}.`,
            detectorId: this.id,
            suggestion: "La conexión con YouTube Music se interrumpió al resolver el stream.",
          });
        } else if (/no stream found|no se encontró|not available|unavailable/i.test(raw)) {
          this.engine.reportAnomaly({
            code: "INVALID_STREAM",
            severity: "error",
            message: `El backend no encontró ningún stream válido para esta canción: ${raw}.`,
            detectorId: this.id,
            suggestion:
              "YouTube Music no devolvió formatos de audio para este video (contenido restringido, geobloqueo o video eliminado).",
          });
        }
        break;
      }

      case "backend.daemon_restarted":
        this.engine.reportAnomaly({
          code: "DAEMON_RESTARTED",
          severity: "warning",
          message: `El backend de streaming se reinició (motivo: ${evt.payload.reason ?? "desconocido"}). Las URLs de stream en caché pueden haber quedado inválidas.`,
          detectorId: this.id,
          suggestion:
            "Tras el reinicio se regeneran los tokens PO. Los reintentos automáticos de reproducción deberían usar credenciales frescas.",
        });
        break;

      case "backend.daemon_ready":
        this.engine.emit("backend.info", { message: "Backend de streaming listo" }, "backend");
        break;

      default:
        break;
    }
  }
}

function extractInnerError(raw: string): string {
  const trimmed = raw.trim();
  try {
    const parsed = JSON.parse(trimmed);
    if (parsed && typeof parsed.error === "string") return parsed.error;
    if (parsed && typeof parsed.message === "string") return parsed.message;
  } catch {
    /* no es JSON */
  }
  return trimmed.length > 240 ? `${trimmed.slice(0, 240)}…` : trimmed;
}

function looksLikeNetworkError(raw: string): boolean {
  return /network|red|conexi|connect|socket|timed out|timeout|ECONN|ETIMEDOUT|DNS|sin conexi/i.test(raw);
}
