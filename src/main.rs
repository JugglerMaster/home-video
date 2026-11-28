mod auth;
mod config;
mod db;
mod hls;
mod import;
mod ingest;
mod server;
mod upnp;

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
        Some("cast") => {
            let rt = tokio::runtime::Runtime::new().expect("create tokio runtime");
            if let Err(e) = rt.block_on(cast_speaker(&args)) {
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

/// `home-video cast <video_id> <speaker_id> [--volume N]`
///
/// Discovers speakers on the LAN, resolves the requested speaker, and sends
/// a real SOAP cast (SetVolume + SetAVTransportURI + Play) to it. This is a
/// stand-alone CLI for testing casting without the web UI. Errors loudly.
async fn cast_speaker(args: &[String]) -> anyhow::Result<()> {
    // NOTE: usage is re-created per error to avoid panics on a cloned Error.
    let video_id_str = args
        .get(2)
        .ok_or_else(|| anyhow::anyhow!("usage: home-video cast <videoId> <speakerId> [--volume N]"))?;
    let speaker_id = args
        .get(3)
        .ok_or_else(|| anyhow::anyhow!("usage: home-video cast <videoId> <speakerId> [--volume N]"))?;
    let video_id: u64 = video_id_str.parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid video id {video_id_str:?}"))?;

    let mut volume: u16 = 50;
    for (i, arg) in args.iter().enumerate().skip(4) {
        if arg == "--volume" {
            let v_str = args.get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("--volume needs a value (0-100)"))?;
            volume = v_str.parse::<u16>().map_err(|_| {
                anyhow::anyhow!("--volume must be a number 0-100 (got {v_str:?})")
            })?;
            if volume > 100 {
                return Err(anyhow::anyhow!("--volume must be 0-100 (got {volume})"));
            }
        }
    }

    let config_path = env::var("HOME_VIDEO_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./config.json"));
    let cfg = config::load(&config_path)?;
    let c = match cfg {
        Some(c) => c,
        None => {
            eprintln!(
                "No config file at {:?}. Run the setup wizard first.",
                config_path.display()
            );
            return Err(anyhow::anyhow!("no config file found"));
        }
    };
    if !c.upnp.enabled {
        return Err(anyhow::anyhow!(
            "upnp is disabled in the config; set upnp.enabled to true"
        ));
    }

    // Load the video and verify the audio file actually exists.
    let db_path = PathBuf::from(&c.data_dir).join("home-video.db");
    let db = db::Db::new(&db_path)?;
    let video = db.get_video_by_id(video_id)?
        .ok_or_else(|| anyhow::anyhow!("video {video_id} not found"))?;
    if video.audio_path.is_empty() {
        return Err(anyhow::anyhow!(
            "video {video_id} has no pre-baked audio (ingest it with audio enabled)"
        ));
    }
    let full_path = PathBuf::from(&c.data_dir).join("audio").join(&video.audio_path);
    if !full_path.exists() {
        return Err(anyhow::anyhow!(
            "audio file not found at {:?}",
            full_path
        ));
    }

    // Discover speakers and require the requested one to be online.
    let client = reqwest::Client::new();
    let store = upnp::SpeakerStore::new();
    upnp::discover_once(&client, &store).await;
    let speaker = store
        .get(speaker_id)
        .ok_or_else(|| anyhow::anyhow!("speaker {speaker_id:?} not found (run discovery and check the list)"))?;
    if !speaker.is_online(upnp::ONLINE_GRACE) {
        return Err(anyhow::anyhow!("speaker {speaker_id:?} is not online"));
    }

    // Resolve the LAN IP to tell the speaker where to fetch the audio from.
    let ip = upnp::resolve_server_ip(&c.upnp.server_ip)?;
    let uri = format!("http://{ip}:{}/audio/{video_id}", c.port);

    upnp::cast(&client, &speaker, &uri, volume).await?;
    println!(
        "Cast video {video_id} ({title}) to {name} at http://{ip}:{port} — volume {volume}",
        title = video.title,
        name = speaker.name,
        port = c.port,
    );
    Ok(())
}
