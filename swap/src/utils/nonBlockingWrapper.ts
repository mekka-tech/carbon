/**
 * Crea un wrapper para una función asíncrona, de modo que se ejecute
 * de forma non-blocking usando setImmediate.
 *
 * @param action Función asíncrona que se desea envolver.
 * @param errCb Callback opcional para el manejo de errores.
 * @returns Una nueva función que al ejecutarse dispara la función original de forma asíncrona.
 */
export function nonBlockingWrapper<T extends any[], R>(
  action: (...args: T) => Promise<R>,
  errCb?: (err: any) => void
): (...args: T) => void {
  return (...args: T): void => {
    process.nextTick(() => {
      action(...args).catch((err) => {
        if (errCb) {
          errCb(err);
        }
      });
    });
  };
}
