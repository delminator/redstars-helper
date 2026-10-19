//! Décodeur d'images `nn:` côté helper (portage de img_render.py).

/// Le décodeur est-il utilisable (modèle trouvé et chargeable) ?
pub fn capable() -> bool {
    unimplemented!("img::capable")
}

/// Décode le hash base64 et rend une tuile de `w_cells`×`h_cells` cellules en
/// texte quadrant truecolor : une String ANSI par ligne. None si impossible.
pub fn render_tile(_b64hash: &str, _w_cells: usize, _h_cells: usize) -> Option<Vec<String>> {
    unimplemented!("img::render_tile")
}
