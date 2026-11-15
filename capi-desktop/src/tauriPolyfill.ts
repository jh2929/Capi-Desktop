// Polyfill seguro de entorno para permitir visualizar la aplicación en modo desarrollo web
// (navegador convencional, DevTools web, etc.) sin que crashee por falta de bindings nativos de Tauri.

if (typeof window !== "undefined" && !(window as any).__TAURI_INTERNALS__) {
  console.info("[Capi] Inicializando polyfill de compatibilidad para vista de desarrollo web.");

  const callbacks = new Map<number, (res: any) => void>();
  let nextCallbackId = 1000;

  (window as any).__TAURI_INTERNALS__ = {
    transformCallback: (callback: (res: any) => void, once?: boolean) => {
      const id = ++nextCallbackId;
      callbacks.set(id, (res: any) => {
        if (once) {
          callbacks.delete(id);
          delete (window as any)[`_${id}`];
        }
        callback(res);
      });
      (window as any)[`_${id}`] = callbacks.get(id);
      return id;
    },

    unregisterCallback: (id: number) => {
      callbacks.delete(id);
      delete (window as any)[`_${id}`];
    },

    invoke: async (cmd: string, args: any = {}, options?: { callback?: number; error?: number }) => {
      console.debug(`[Tauri Web Dev] invoke: ${cmd}`, args);

      let result: any = null;
      let isError = false;

      try {
        switch (cmd) {
          case "plugin:autostart|is_enabled":
            result = false;
            break;
          case "plugin:autostart|enable":
          case "plugin:autostart|disable":
            result = undefined;
            break;
          case "plugin:event|listen":
            result = 1;
            break;
          case "plugin:event|unlisten":
            result = undefined;
            break;
          case "obtener_local_port":
            result = { port: 12762, token: "" };
            break;
          case "obtener_info_sistema":
            result = JSON.stringify({ platform: "browser", arch: "web" });
            break;
          case "obtener_estado_sesion_ytm":
            result = JSON.stringify({ loggedIn: false, cookiesCount: 0 });
            break;
          case "consolidar_carpetas_descargas":
            result = {};
            break;
          case "obtener_espacio_almacenamiento":
            result = { libre_gb: 50, total_gb: 250 };
            break;
          case "listar_archivos_locales":
            result = [];
            break;
          case "descubrir_dispositivos_cast":
            result = [];
            break;
          default:
            result = null;
            break;
        }
      } catch (err) {
        isError = true;
        result = err;
      }

      if (options?.callback && !isError) {
        const cb = callbacks.get(options.callback);
        if (cb) cb(result);
      } else if (options?.error && isError) {
        const errCb = callbacks.get(options.error);
        if (errCb) errCb(result);
      }

      return result;
    },

    plugins: {},
  };
}

export {};
