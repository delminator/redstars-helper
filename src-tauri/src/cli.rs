// Ligne de commande — « lancer RedStars simplement ».
//
// Le helper est d'abord un agent de barre système sans fenêtre : lancé sans
// argument (login autostart, clic du tray, bouton `redstars-helper://` du
// dashboard, drapeau `--minimized`), il démarre le tray comme avant. Rien de
// tout cela ne passe par ici.
//
// Mais on veut aussi pouvoir OUVRIR RedStars en console d'une seule commande,
// depuis un terminal déjà ouvert, sans passer par le menu du tray (qui, lui,
// doit deviner un émulateur de terminal à ouvrir — voir `open_console`). D'où :
//
//     redhelper console            # le client console, taille réelle
//     redhelper console --app eau  # options du client console
//     redhelper minitel            # raccourci : console 40×24 façon Minitel
//     minitel                      # idem, via le lien symbolique `minitel`
//
// `redhelper` et `minitel` sont des liens symboliques vers le binaire, posés
// sur le PATH par le postinst du paquet .deb/.rpm (voir scripts/postinst.sh).
// Ici on se contente de reconnaître la sous-commande — ou le nom `minitel` par
// lequel on a été appelé — et de lancer le client console natif
// (`crate::console`) EN AVANT-PLAN dans ce terminal. On rend ensuite son code
// de sortie. (Avant la v1.0 c'était `python3 helper.py console …`.)
//
// Différence clé avec le menu tray : ici on ne démarre NI le tray, NI les
// serveurs HTTP, NI la surveillance matériel — on ne fait que rejouer le client
// console dans le terminal courant.

use std::path::PathBuf;
use std::process::Command;

/// Script d'installation/mise à jour universel, rejoué par `redhelper upgrade`.
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/delminator/redstars-helper/main/install.sh";

/// Regarde les arguments (et le nom sous lequel on a été appelé). Si c'est une
/// invocation console, on la lance en avant-plan et on NE REVIENT PAS (exit avec
/// le code du client console). Sinon on rend la main : `run()` démarre le tray normal.
pub fn intercept() {
    // Poser/rafraîchir les lanceurs courts dans ~/.local/bin. Fait à CHAQUE
    // démarrage (tray, console, autostart…), avant tout le reste : c'est le
    // binaire en cours d'exécution qui les crée, donc ça marche aussi pour
    // l'AppImage et pour un OS immuable (Fedora Silverblue…) où le postinst du
    // paquet ne peut rien écrire dans /usr. No-op hors Linux.
    ensure_launchers();

    let args: Vec<String> = std::env::args().collect();

    // Nom de commande (argv[0]) : appelé via le lien `minitel` → mode Minitel
    // direct, sans sous-commande. On compare le basename pour tolérer un chemin
    // absolu (`/usr/local/bin/minitel`) comme un simple `minitel`.
    let invoked_as = args
        .first()
        .map(|a| PathBuf::from(a))
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();

    // Sous-commande éventuelle (argv[1]).
    let sub = args.get(1).map(|s| s.as_str());

    // Options à transmettre au client console et faut-il forcer 40×24 ?
    let (console, minitel, passthrough): (bool, bool, &[String]) = if invoked_as == "minitel" {
        // `minitel [opts…]` : tout argv[1..] est passé tel quel, mode 40×24.
        (true, true, &args[1.min(args.len())..])
    } else {
        match sub {
            Some("console") => (true, false, &args[2.min(args.len())..]),
            Some("minitel") => (true, true, &args[2.min(args.len())..]),
            Some("status") => {
                // Version + état du daemon, sans démarrer le tray ni GTK.
                std::process::exit(run_status());
            }
            Some("update") => {
                // Plus de script auto-updatable : on oriente vers `upgrade`.
                std::process::exit(run_update());
            }
            Some("upgrade") | Some("install") => {
                std::process::exit(run_upgrade(&args[2.min(args.len())..]));
            }
            Some("help") | Some("--help") | Some("-h") => {
                print_usage();
                std::process::exit(0);
            }
            // Tout le reste (aucun argument, `--minimized`, une URL
            // `redstars-helper://…`) n'est PAS une invocation console : on
            // laisse `run()` démarrer le tray.
            _ => return,
        }
    };

    if !console {
        return;
    }

    std::process::exit(run_console_foreground(minitel, passthrough));
}

/// Lance le client console natif dans ce terminal et rend son code de sortie.
/// `--minitel` est ajouté en tête pour la forme `minitel` / `redhelper minitel`
/// (exactement ce que faisait `helper.py console --minitel`).
fn run_console_foreground(minitel: bool, passthrough: &[String]) -> i32 {
    let mut args: Vec<String> = Vec::with_capacity(passthrough.len() + 1);
    if minitel {
        args.push("--minitel".to_string());
    }
    args.extend(passthrough.iter().cloned());
    crate::console::run(&args)
}

/// Port du helper local à sonder (même variable que l'ancien helper.py).
fn helper_port() -> u16 {
    std::env::var("HELPER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(49080)
}

/// `redhelper status` : la version (le helper est désormais un seul binaire
/// natif — plus de helper.py à côté), où il vit, et si le daemon local répond.
/// Sans interface graphique : marche même quand le tray ne peut pas démarrer.
fn run_status() -> i32 {
    println!("redstars-helper : {} (natif)", env!("CARGO_PKG_VERSION"));
    if let Ok(exe) = std::env::current_exe() {
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        println!("  fichier       : {}", exe.display());
    }
    let port = helper_port();
    let url = format!("http://127.0.0.1:{port}/helper/status");
    let probe = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(&url).send().ok())
        .and_then(|r| r.json::<serde_json::Value>().ok());
    match probe {
        Some(v) => {
            let ver = v.get("version").and_then(|x| x.as_str()).unwrap_or("?");
            println!("  daemon :{port} : en marche (helper {ver})");
        }
        None => println!("  daemon :{port} : arrêté"),
    }
    0
}

/// `redhelper update` : il n'y a plus de script à mettre à jour à part — le
/// helper natif se met à jour avec l'application.
fn run_update() -> i32 {
    println!(
        "redstars-helper {} : le helper est natif et se met à jour avec l'application.",
        env!("CARGO_PKG_VERSION")
    );
    println!("  pour installer la dernière version : redhelper upgrade");
    0
}

/// `redhelper upgrade` : rejoue le script d'install/upgrade universel
/// (`install.sh`), qui tire la dernière release et remplace la variante en
/// place (AppImage / .deb / .rpm). En avant-plan, stdio hérité, on rend le code
/// de sortie du script. Les options éventuelles (`--appimage`, `--deb`,
/// `--rpm`, `--uninstall`) sont transmises telles quelles.
///
/// On passe par `curl … | sh` : c'est le même point d'entrée que l'install
/// initiale, donc une seule logique de détection de plateforme à maintenir.
fn run_upgrade(passthrough: &[String]) -> i32 {
    let fetch = if which("curl") {
        format!("curl -fsSL '{}'", INSTALL_SCRIPT_URL)
    } else if which("wget") {
        format!("wget -qO- '{}'", INSTALL_SCRIPT_URL)
    } else {
        eprintln!("[redhelper] curl ou wget est requis pour « upgrade ».");
        return 1;
    };

    // Options transmises au script après `sh -s --`, chacune protégée en
    // single-quote pour survivre à l'expansion du shell.
    let mut extra = String::new();
    for a in passthrough {
        extra.push(' ');
        extra.push_str(&shell_single_quote(a));
    }

    let cmd = format!("{fetch} | sh -s --{extra}");
    eprintln!("[redhelper] mise à jour depuis {INSTALL_SCRIPT_URL}");
    match Command::new("sh").arg("-c").arg(&cmd).status() {
        Ok(status) => status.code().unwrap_or(0),
        Err(e) => {
            eprintln!("[redhelper] échec du lancement de la mise à jour : {e}");
            1
        }
    }
}

/// Un exécutable est-il trouvable dans le PATH ? (évite une dépendance externe.)
fn which(prog: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|dir| dir.join(prog).is_file())
        })
        .unwrap_or(false)
}

/// Entoure `s` de single-quotes en échappant les single-quotes internes —
/// suffisant pour passer une option en toute sécurité à `sh`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Marqueur en tête des lanceurs qu'on génère, pour les reconnaître et les
/// rafraîchir sans jamais écraser un fichier que l'utilisateur aurait posé.
#[cfg(target_os = "linux")]
const LAUNCHER_MARKER: &str = "# redstars-helper launcher (auto-généré)";

/// Pose/rafraîchit `~/.local/bin/redhelper` et `~/.local/bin/minitel`.
///
/// Complément aux liens du postinst (paquet .deb/.rpm) : ceci marche AUSSI pour
/// l'AppImage et pour un OS immuable (Fedora Silverblue/Kinoite, uBlue…) où
/// /usr est en lecture seule et les scriptlets du paquet ne peuvent rien y
/// écrire — parce que c'est le binaire en cours d'exécution qui les crée, dans
/// le HOME de l'utilisateur.
///
/// On écrit de petits scripts shell plutôt que des liens symboliques : lancée
/// via un lien nommé « minitel », l'AppImage re-exec son binaire interne et ne
/// préserve pas toujours argv[0] — le mode Minitel ne se déclencherait pas. Le
/// script, lui, passe explicitement la sous-commande à la cible.
#[cfg(target_os = "linux")]
fn ensure_launchers() {
    let target = match launcher_target() {
        Some(t) => t,
        None => return,
    };
    let home = match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h),
        None => return,
    };
    let bindir = home.join(".local/bin");
    if std::fs::create_dir_all(&bindir).is_err() {
        return;
    }
    let t = target.display();
    write_launcher(
        &bindir.join("redhelper"),
        &format!("#!/bin/sh\n{LAUNCHER_MARKER}\nexec \"{t}\" \"$@\"\n"),
    );
    write_launcher(
        &bindir.join("minitel"),
        &format!("#!/bin/sh\n{LAUNCHER_MARKER}\nexec \"{t}\" minitel \"$@\"\n"),
    );
}

#[cfg(not(target_os = "linux"))]
fn ensure_launchers() {
    // macOS / Windows : pas de convention ~/.local/bin, et l'install n'a pas le
    // même problème de /usr en lecture seule. Rien à faire.
}

/// Chemin STABLE et lançable du binaire, cible des lanceurs.
/// - AppImage : `current_exe()` pointe dans le montage squashfs éphémère
///   (/tmp/.mount_…) qui disparaît à la fermeture — inutilisable comme cible.
///   Le vrai fichier lançable est le .AppImage, exposé dans $APPIMAGE.
/// - .deb / .rpm / rpm-ostree / dev : `current_exe()` est stable.
#[cfg(target_os = "linux")]
fn launcher_target() -> Option<PathBuf> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let p = PathBuf::from(appimage);
        if p.is_file() {
            return Some(p);
        }
    }
    std::env::current_exe().ok()
}

/// Écrit un lanceur si l'emplacement est libre ou déjà l'un des nôtres (on le
/// rafraîchit alors vers la cible courante — utile quand l'AppImage a été
/// déplacé/mis à jour). On ne touche JAMAIS un fichier étranger, ni un lien
/// symbolique posé par l'utilisateur.
#[cfg(target_os = "linux")]
fn write_launcher(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::symlink_metadata(path) {
        Ok(md) => {
            if !md.file_type().is_file() {
                return; // lien symbolique / répertoire / autre : on n'y touche pas
            }
            match std::fs::read_to_string(path) {
                Ok(cur) if cur.contains(LAUNCHER_MARKER) => {
                    if cur == contents {
                        return; // déjà à jour → pas de réécriture inutile
                    }
                }
                _ => return, // illisible ou étranger : on laisse tel quel
            }
        }
        Err(_) => {} // absent : on crée
    }
    if std::fs::write(path, contents).is_ok() {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    }
}

fn print_usage() {
    println!(
        "\
RedStars Helper — agent local.

Sans argument, démarre l'agent dans la barre système (tray).

Pour ouvrir RedStars en console depuis ce terminal :

  redhelper console [options]   client console, taille réelle du terminal
  redhelper minitel [options]   raccourci : console 40×24, façon Minitel
  minitel [options]             idem (lien direct)

Options du client console, par ex. :

  redhelper console --app eau --user alice

Voir « redhelper console --help » pour la liste complète des options.

État :

  redhelper status              version du helper + état du daemon local

Mise à jour / installation de l'application :

  redhelper upgrade             télécharge et installe la dernière version de l'app
  redhelper upgrade --uninstall désinstalle
                                (choisit AppImage / .deb / .rpm selon le système)"
    );
}
