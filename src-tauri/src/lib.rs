// Library entry — main.rs delegates to run() so cargo can build both a
// binary (Linux/Mac) and a static lib (mobile platforms).

mod cleanup;
#[cfg(desktop)]
mod console;
#[cfg(desktop)]
mod helper;
#[cfg(desktop)]
mod img;
#[cfg(desktop)]
mod cli;
mod script_updater;
mod shell_updater;

use std::process::Command;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

/// Tray → « Ouvrir RedStars en console ».
///
/// Le helper est un agent sans fenêtre : il n'a pas de webview où afficher quoi
/// que ce soit. Il ouvre donc un ÉMULATEUR DE TERMINAL et y relance son propre
/// binaire en `console` — le client console natif (src/console/).
///
/// Le choix du terminal est une liste d'essais, dans cet ordre : ce que
/// l'utilisateur a déclaré (`$TERMINAL`), puis les émulateurs courants. Il n'y a
/// pas d'API portable pour « ouvre un terminal » sous Linux, et prétendre le
/// contraire donne un menu qui ne fait rien sur la moitié des machines.
///
/// Si aucun n'est trouvé, on le DIT (stderr) plutôt que d'échouer en silence :
/// un item de menu qui ne réagit pas est pire qu'un item absent.
fn open_console(_app: &AppHandle) {
    // Le client console est natif : on relance CE binaire (ou l'AppImage qui le
    // porte) avec la sous-commande `console`. Plus de python ni de helper.py.
    let exe = console::self_exe();
    let cmd = format!("{} console", console::shell_quote(&exe.to_string_lossy()));

    // `-e` est le drapeau « exécute cette commande » de presque tous les
    // émulateurs. foot/alacritty/kitty le prennent tel quel ; les terminaux
    // GTK veulent parfois `--`, d'où les deux formes.
    let candidates: Vec<(String, Vec<String>)> = std::env::var("TERMINAL")
        .ok()
        .map(|t| vec![(t, vec!["-e".into()])])
        .unwrap_or_default()
        .into_iter()
        .chain([
            ("foot".to_string(),           vec!["-e".to_string()]),
            ("alacritty".to_string(),      vec!["-e".to_string()]),
            ("kitty".to_string(),          vec![]),
            ("wezterm".to_string(),        vec!["start".to_string(), "--".to_string()]),
            ("gnome-terminal".to_string(), vec!["--".to_string()]),
            ("konsole".to_string(),        vec!["-e".to_string()]),
            ("xfce4-terminal".to_string(), vec!["-e".to_string()]),
            ("xterm".to_string(),          vec!["-e".to_string()]),
        ])
        .collect();

    for (term, pre) in candidates {
        let mut c = Command::new(&term);
        for a in &pre {
            c.arg(a);
        }
        // `sh -lc` pour que le shell de connexion soit chargé et que le terminal
        // ne se referme pas à la seconde où le client rend la main.
        c.arg("sh").arg("-lc").arg(format!("{cmd}; echo; read -p 'Entrée pour fermer…' _"));
        console::clean_env(&mut c);
        if c.spawn().is_ok() {
            eprintln!("[helper] console ouverte via {term}");
            return;
        }
    }
    eprintln!("[helper] aucun émulateur de terminal trouvé — définissez $TERMINAL, ou lancez : {cmd}");
}

/// Make this process the ONLY helper on the machine: terminate any
/// other running helper shell and any orphan `helper.py` a previous
/// shell may have left holding port 49080.
///
/// Newest-wins by construction: we enumerate the process table once,
/// at startup, so we only ever target processes that already existed
/// before us. A helper launched *after* us isn't in our snapshot — and
/// when it boots it runs this same sweep and retires us in turn. That
/// avoids two concurrently-starting shells mutually SIGKILLing each
/// other (the realistic launches — login autostart, a tray click, the
/// dashboard's `redstars-helper://` button — are never simultaneous to
/// the millisecond anyway).
///
/// We must run this BEFORE starting our own server, so ports 49080/49443
/// are free — an older shell's python helper.py may still hold them. We match two shapes, mirroring
/// `scripts/preinst.sh`: the shell binary (by exe file name, robust to
/// the 15-char `comm` truncation on Linux) and any python whose command
/// line references `helper.py`.
fn terminate_other_helpers() {
    use sysinfo::{Pid, ProcessesToUpdate, System};

    let self_pid = match sysinfo::get_current_pid() {
        Ok(p) => p,
        Err(_) => return,
    };
    let our_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

    // Telling "a prior helper to retire" apart from "a process of our OWN just-
    // launched AppImage" (runtime wrapper, AppRun, the squashfs FUSE-mount
    // holder, the app binary) by name / pgid / mount / ancestor / APPIMAGE env
    // all proved unreliable across AppImage's process layouts — and getting it
    // wrong makes the agent kill its own mount holder and SIGBUS-crash on
    // launch. The one robust signal is AGE: our whole launch tree starts within
    // a second or two of us, while a genuinely prior instance is many seconds —
    // usually minutes — older. So we only ever retire helpers that started
    // clearly before us.
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    let self_start = sys.process(self_pid).map(|p| p.start_time());

    let mut protected: std::collections::HashSet<Pid> = std::collections::HashSet::new();
    protected.insert(self_pid);
    let mut cur = self_pid;
    while let Some(parent) = sys.process(cur).and_then(|p| p.parent()) {
        if !protected.insert(parent) {
            break; // cycle guard
        }
        cur = parent;
    }

    // Grace window covering the spread of our own launch tree's start times
    // (mount + fork + exec). Anything younger than this is treated as part of
    // us, not a prior instance.
    const GRACE_SECS: u64 = 10;

    for (pid, proc_) in sys.processes() {
        if protected.contains(pid) {
            continue;
        }
        let name = proc_.name().to_string_lossy().to_string();
        let exe_name = proc_
            .exe()
            .and_then(|e| e.file_name())
            .map(|n| n.to_string_lossy().to_string());
        let is_shell = our_name.is_some()
            && (our_name.as_deref() == Some(name.as_str()) || our_name == exe_name);
        let is_helper_py = proc_
            .cmd()
            .iter()
            .any(|a| a.to_string_lossy().ends_with("helper.py"));
        if !(is_shell || is_helper_py) {
            continue;
        }
        // Age gate: skip anything that started within GRACE of us — that's our
        // own AppImage tree, not a prior instance.
        if let Some(ours) = self_start {
            if proc_.start_time() + GRACE_SECS > ours {
                continue;
            }
            eprintln!(
                "[single] retiring prior helper pid={} ({}) — {}s older than us",
                pid,
                name,
                ours.saturating_sub(proc_.start_time())
            );
        } else {
            eprintln!("[single] retiring prior helper pid={} ({})", pid, name);
        }
        proc_.kill();
    }
}

/// Le helper natif a-t-il de quoi se mettre à jour ? Le certificat HTTPS de
/// local.redlinks.fr voyage dans le helper.py signé publié à chaque
/// renouvellement : on le télécharge (canal script-py, inchangé pour les
/// anciens shells) et le serveur relit son certificat dedans.
fn refresh_cert(app: &AppHandle) {
    if script_updater::fetch_and_cache(app) {
        eprintln!("[helper] nouveau helper.py signé — rechargement du certificat");
    }
    helper::server::reload_tls();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Ligne de commande d'abord : `redhelper console` / `minitel` ouvrent le
    // client console dans CE terminal et sortent, sans jamais démarrer le tray.
    // Une invocation ordinaire (aucun arg, `--minimized`, URL) rend la main et
    // le tray démarre normalement. No-op sur mobile (pas de sous-commande).
    #[cfg(desktop)]
    cli::intercept();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Autostart at session login (XDG autostart / login items / registry
        // depending on OS). The helper is a windowless tray agent — there's
        // nothing to show on launch, it just appears in the system tray.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            let handle = app.handle().clone();

            // Windowless tray agent: on macOS hide the Dock icon so we are a
            // pure background item (no-op / not present on Linux + Windows).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Single-instance sweep: retire any other helper shell and
            // free port 49080 from an orphan helper.py before we spawn
            // ours. Newest launch wins; see terminate_other_helpers.
            terminate_other_helpers();

            // First-run autostart enable. The plugin's `is_enabled` /
            // `enable` are idempotent; calling enable() on every run
            // is harmless if it's already set. We never DISABLE it
            // automatically — that requires explicit user action in
            // the helper's settings / tray menu.
            use tauri_plugin_autostart::ManagerExt;
            if let Ok(am) = handle.autolaunch().is_enabled() {
                if !am {
                    let _ = handle.autolaunch().enable();
                    eprintln!("[autostart] enabled — helper will auto-launch at next session login");
                }
            }

            // Boot-time cleanup: data-dir cruft + stale old-version artefacts
            // (old AppImage siblings, old downloaded packages, dead autostart
            // entries). Runs AFTER the autostart enable above so the current,
            // live entry is never mistaken for a leftover. Conservative and
            // best-effort — see cleanup.rs.
            cleanup::run(&handle);

            // Certificat d'abord (synchrone, délai court : si GitHub traîne on
            // démarre avec le certificat déjà en cache ou embarqué), puis le
            // helper natif. Plus de Python : les routes /helper/* sont servies
            // par ce processus.
            let _ = script_updater::fetch_and_cache(&handle);
            {
                let h = handle.clone();
                helper::server::set_update_hook(Box::new(move || shell_updater::check_now(&h)));
                let h = handle.clone();
                // Plus aucun site ouvert depuis 12 min : on quitte, tray compris.
                // Le site relance le helper via redstars-helper:// à la prochaine visite.
                helper::start(Box::new(move || {
                    eprintln!("[helper] aucun signe du site — arrêt");
                    h.exit(0);
                }));
            }

            // Toutes les 6 h : nouveau helper.py signé ⇒ nouveau certificat.
            script_updater::start_background_poller(handle.clone(), |app| {
                refresh_cert(app);
            });

            // Shell self-update (the Tauri binary itself): check now + every
            // 6 h. Moved out of the now-removed webview into Rust so this
            // windowless tray agent keeps itself current. See shell_updater.rs.
            shell_updater::check_now(&handle);
            shell_updater::start_poller(handle.clone());

            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, "console", "Ouvrir RedStars en console", true, None::<&str>)?,
                    &MenuItem::with_id(app, "restart", "Recharger le certificat", true, None::<&str>)?,
                    &MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?,
                    &MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?,
                ],
            )?;
            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Redstars Helper")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "console" => { open_console(app); }
                    "restart" => { refresh_cert(app); }
                    "update" => { shell_updater::check_now(app); }
                    "quit" => { app.exit(0); }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri app")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                // Tray-only agent: there are no windows, so a code-less exit
                // request is spurious (e.g. a stray "last window closed") —
                // keep running. An explicit quit (tray, idle exit) uses
                // app.exit(0) (code = Some) and is allowed through.
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
