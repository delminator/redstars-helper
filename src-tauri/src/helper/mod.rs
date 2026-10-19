//! Helper natif — le serveur local /helper/* qui remplace helper.py.
//!
//! Même contrat que helper.py 0.5.64 : mêmes ports (49080 HTTP, 49443 HTTPS),
//! mêmes routes, mêmes JSON, mêmes en-têtes CORS. Le site ne voit aucune
//! différence, sauf `version` qui passe à 1.x (au-dessus de tout helper.py,
//! pour que le bandeau « mise à jour du script » ne s'affiche jamais).

pub mod server;

/// Version annoncée par GET /helper/status. Suit la version du shell.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Démarre les serveurs HTTP + HTTPS et la surveillance d'inactivité, en
/// arrière-plan. Ne bloque pas. `on_idle_exit` est appelé quand aucun site
/// n'a donné signe de vie depuis REDSTARS_HELPER_IDLE_EXIT_S (720 s par défaut).
pub fn start(on_idle_exit: Box<dyn Fn() + Send + Sync + 'static>) {
    server::start(on_idle_exit)
}
