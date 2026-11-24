mod auth;
mod config;
mod db;
mod hls;
mod import;
mod ingest;
mod server;

use std::env;
use std::io::Write;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str());
    match cmd {
        Some("set-admin-password") => {
            if let Err(e) = set_admin_password(&args) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        _ => {
            let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
            if let Err(e) = rt.block_on(server::run()) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// `home-video set-admin-password [password]` — reset the admin password in the
/// config file (documented recovery path for a lost/unknown password).
///
/// If no password is supplied it is read interactively from stdin. The running
/// server must be restarted for the new hash to take effect (the config is
/// loaded once at startup).
fn set_admin_password(args: &[String]) -> anyhow::Result<()> {
    // Password from argument (args[2]) or interactive prompt.
    let password = if let Some(p) = args.get(2) {
        p.as_str().to_string()
    } else {
        print!("New admin password: ");
        std::io::stdout().flush()?;
        let mut pw = String::new();
        std::io::stdin().read_line(&mut pw)?;
        pw.trim().to_string()
    };
    if password.is_empty() {
        return Err(anyhow::anyhow!("password is required"));
    }
    let config_path = env::var("HOME_VIDEO_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./config.json"));
    let cfg = config::load(&config_path)?;
    let mut c = match cfg {
        Some(c) => c,
        None => {
            eprintln!(
                "No config file at {:?}. Run the setup wizard first.",
                config_path.display()
            );
            return Err(anyhow::anyhow!("no config file found"));
        }
    };
    c.admin_password_hash = auth::hash_password(&password);
    config::save(&config_path, &c)?;
    println!(
        "Admin password updated. Restart the server (e.g. `docker compose restart`) for it to take effect."
    );
    Ok(())
}
