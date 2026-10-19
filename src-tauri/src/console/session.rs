//! Session console conservée sur disque (~/.local/state/redstars/session.json).

/// Écrit la session (droits 0600). Même format que `_con_session_save`.
pub fn save(_access: &str, _refresh: &str, _api: &str) -> std::io::Result<()> {
    unimplemented!("console::session::save")
}
