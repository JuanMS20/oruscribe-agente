//! Un solo helper para tomar candados sin propagar el veneno.

use std::sync::{Mutex, MutexGuard, PoisonError};

pub(crate) fn lock<T>(candado: &Mutex<T>) -> MutexGuard<'_, T> {
    candado.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
// R7 exime a los tests: acá el pánico es el mecanismo que envenena el candado.
#[allow(clippy::panic)]
mod tests {
    use std::panic;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::lock;

    #[test]
    fn lock_recupera_un_candado_envenenado() {
        let candado = Arc::new(Mutex::new(7));
        let del_hilo = Arc::clone(&candado);

        let hilo = thread::spawn(move || {
            let _guard = lock(&del_hilo);
            panic::panic_any("poison");
        });
        let _ = hilo.join();

        let guard = lock(&candado);
        assert_eq!(*guard, 7);
    }
}
