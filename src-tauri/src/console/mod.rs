//! Client console (portage de la partie `_con_*` / `run_console` de helper.py).

pub mod session;

/// `redhelper console [args…]` / `minitel` : lance le client dans le terminal
/// courant et rend le code de sortie. `args` = ce qui suit `console`.
pub fn run(_args: &[String]) -> i32 {
    unimplemented!("console::run")
}

/// Ouvre un émulateur de terminal qui lance la console avec `token` (et les
/// options JSON de POST /helper/console). Renvoie le nom du terminal utilisé,
/// ou None si aucun n'a été trouvé. Ne renvoie JAMAIS le jeton.
pub fn spawn_terminal(_token: &str, _opts: &serde_json::Value) -> Option<String> {
    unimplemented!("console::spawn_terminal")
}
