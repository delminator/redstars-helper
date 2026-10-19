//! Sous-processus façon `subprocess.run(capture_output=True, text=True)` et
//! `subprocess.Popen` (lancé, jamais attendu par l'appelant).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::py;

pub struct Out {
    /// Code de sortie ; négatif = tué par ce signal (comme Python).
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub enum RunErr {
    /// Programme introuvable / impossible à lancer : message `Type: str(e)` Python.
    Spawn(String),
    /// `subprocess.TimeoutExpired`.
    Timeout(String),
}

impl RunErr {
    pub fn msg(&self) -> String {
        match self {
            RunErr::Spawn(m) | RunErr::Timeout(m) => m.clone(),
        }
    }
}

fn command(argv: &[String]) -> Command {
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW : pas de console qui clignote
    }
    c
}

fn status_code(st: std::process::ExitStatus) -> i32 {
    if let Some(c) = st.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(s) = st.signal() {
            return -s;
        }
    }
    -1
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=timeout)`.
pub fn run(argv: &[String], timeout: Option<Duration>) -> Result<Out, RunErr> {
    let mut child = command(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| RunErr::Spawn(py::oserror_full(&e, &[&argv[0]])))?;
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let t_out = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut v);
        }
        v
    });
    let t_err = std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut v);
        }
        v
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => return Err(RunErr::Spawn(py::oserror_full(&e, &[]))),
        }
        if let Some(t) = timeout {
            if start.elapsed() >= t {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunErr::Timeout(format!(
                    "TimeoutExpired: Command '{}' timed out after {} seconds",
                    py::repr_list(argv),
                    t.as_secs()
                )));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
    Ok(Out { code: status_code(status), stdout, stderr })
}

/// `subprocess.run(argv, check=True)` : code ≠ 0 → `CalledProcessError`.
pub fn run_check(argv: &[String]) -> Result<Out, String> {
    let out = run(argv, None).map_err(|e| e.msg())?;
    if out.code != 0 {
        return Err(format!(
            "CalledProcessError: Command '{}' returned non-zero exit status {}.",
            py::repr_list(argv),
            out.code
        ));
    }
    Ok(out)
}

/// `subprocess.Popen(argv)` : lancé et oublié (un fil récolte le zombie).
pub fn popen(argv: &[String]) -> Result<(), String> {
    let mut child = command(argv)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| py::oserror_full(&e, &[&argv[0]]))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `shutil.which(name) is not None`.
pub fn which(name: &str) -> bool {
    which::which(name).is_ok()
}

pub fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}
