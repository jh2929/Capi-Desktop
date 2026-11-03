import { Component, ErrorInfo, ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  constructor(props: Props) {
    super(props);
    this.state = { hasError: false, error: null };
  }

  static getDerivedStateFromError(error: Error): State {
    return { hasError: true, error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("ErrorBoundary ha capturado un error no controlado:", error, errorInfo);
  }

  handleReload = () => {
    window.location.reload();
  };

  handleResetData = () => {
    if (
      window.confirm(
        "¿Seguro que deseas restablecer los datos de la app? Esto borrará favoritos, descargas e historial locales para recuperar la aplicación.",
      )
    ) {
      try {
        const keysToRemove = Object.keys(localStorage).filter(
          (k) => k.startsWith("capi_") || k.startsWith("opentune_"),
        );
        for (const k of keysToRemove) {
          localStorage.removeItem(k);
        }
      } catch (e) {
        console.error("Error limpiando localStorage:", e);
      }
      window.location.reload();
    }
  };

  render() {
    if (this.state.hasError) {
      return (
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            height: "100vh",
            width: "100vw",
            backgroundColor: "#0d0d11",
            color: "#ffffff",
            fontFamily: "system-ui, -apple-system, sans-serif",
            padding: "24px",
            boxSizing: "border-box",
            textAlign: "center",
          }}
        >
          <div
            style={{
              maxWidth: "520px",
              backgroundColor: "#17171f",
              padding: "32px",
              borderRadius: "20px",
              border: "1px solid rgba(255,255,255,0.1)",
              boxShadow: "0 20px 40px rgba(0,0,0,0.5)",
            }}
          >
            <div
              style={{
                width: "48px",
                height: "48px",
                borderRadius: "14px",
                backgroundColor: "rgba(239, 68, 68, 0.2)",
                color: "#ef4444",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                fontSize: "24px",
                margin: "0 auto 16px auto",
                fontWeight: "bold",
              }}
            >
              !
            </div>
            <h2 style={{ fontSize: "20px", fontWeight: 700, marginBottom: "8px" }}>
              Algo salió mal en Capi
            </h2>
            <p
              style={{
                fontSize: "14px",
                color: "#a1a1aa",
                marginBottom: "20px",
                lineHeight: "1.5",
              }}
            >
              Ocurrió un error inesperado al renderizar la interfaz. Puedes intentar recargar la aplicación o restablecer los datos locales si el problema persiste.
            </p>
            {this.state.error?.message && (
              <pre
                style={{
                  backgroundColor: "rgba(0,0,0,0.4)",
                  padding: "12px",
                  borderRadius: "10px",
                  fontSize: "12px",
                  color: "#f87171",
                  textAlign: "left",
                  overflowX: "auto",
                  marginBottom: "20px",
                  maxHeight: "120px",
                }}
              >
                {this.state.error.message}
              </pre>
            )}
            <div style={{ display: "flex", gap: "12px", justifyContent: "center" }}>
              <button
                onClick={this.handleReload}
                style={{
                  backgroundColor: "#8b5cf6",
                  color: "#ffffff",
                  border: "none",
                  padding: "10px 20px",
                  borderRadius: "12px",
                  fontSize: "14px",
                  fontWeight: 600,
                  cursor: "pointer",
                }}
              >
                Recargar aplicación
              </button>
              <button
                onClick={this.handleResetData}
                style={{
                  backgroundColor: "rgba(255,255,255,0.08)",
                  color: "#e4e4e7",
                  border: "1px solid rgba(255,255,255,0.15)",
                  padding: "10px 20px",
                  borderRadius: "12px",
                  fontSize: "14px",
                  fontWeight: 600,
                  cursor: "pointer",
                }}
              >
                Restablecer datos
              </button>
            </div>
          </div>
        </div>
      );
    }

    return this.props.children;
  }
}
