//! Helper natif — le serveur local /helper/* qui remplace helper.py.
//!
//! Même contrat que helper.py 0.5.64 : mêmes ports (49080 HTTP, 49443 HTTPS),
//! mêmes routes, mêmes JSON, mêmes en-têtes CORS. Le site ne voit aucune
//! différence, sauf `version` qui passe à 1.x (au-dessus de tout helper.py,
//! pour que le bandeau « mise à jour du script » ne s'affiche jamais).

mod iso;
mod proc;
pub mod py;
mod refs;
mod scale;
pub mod server;
pub mod tls;
mod webgpu;

/// Version annoncée par GET /helper/status. Suit la version du shell.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// (code HTTP, corps JSON) d'une route.
pub(crate) type Reply = (u16, py::J);

/// Démarre les serveurs HTTP + HTTPS et la surveillance d'inactivité, en
/// arrière-plan. Ne bloque pas. `on_idle_exit` est appelé quand aucun site
/// n'a donné signe de vie depuis REDSTARS_HELPER_IDLE_EXIT_S (720 s par défaut).
#[allow(dead_code)] // câblé par le shell (lib.rs)
pub fn start(on_idle_exit: Box<dyn Fn() + Send + Sync + 'static>) {
    server::start(on_idle_exit)
}

/// `redstars-helper serve` : le serveur au premier plan, sans tray (tests,
/// machines sans écran). L'inactivité termine le processus.
pub fn serve_foreground() -> i32 {
    server::run(Box::new(|| std::process::exit(0)))
}

/// Crochet appelé par POST /helper/update (le shell y branche son updater).
#[allow(dead_code)] // câblé par le shell (lib.rs)
pub fn set_update_hook(f: Box<dyn Fn() + Send + Sync + 'static>) {
    server::set_update_hook(f)
}

/// Relit le certificat TLS et l'applique sans redémarrer (fait aussi seul
/// toutes les 6 h).
#[allow(dead_code)] // câblé par le shell (après mise à jour du helper.py signé)
pub fn reload_tls() {
    server::reload_tls()
}
