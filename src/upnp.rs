// UPnP / Sonos casting module.
//
// SSDP discovery of LAN renderers (Sonos in particular) plus UPnP SOAP control
// to cast a video's pre-baked audio to a speaker. LAN-only; no external
// exposure. See docs/plans/upnp-sonos-casting.md.
//
// Design (D2): hand-rolled tokio UDP SSDP loop + reqwest SOAP-HTTP control.
// No extra crates beyond the existing reqwest.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use reqwest::Client;
use serde::Serialize;

/// A discovered UPnP renderer (speaker).
#[derive(Clone, Debug)]
pub struct Speaker {
    /// The SSDP USN (unique serial number); used as the speaker id.
    pub usn: String,
    /// Friendly name from the device description.
    pub name: String,
    /// AVTransport control URL (SetAVTransportURI, Play).
    pub av_transport_url: String,
    /// RenderingControl control URL (SetVolume).
    pub rendering_control_url: String,
    /// When the speaker was last seen by discovery.
    pub last_seen: SystemTime,
}

impl Speaker {
    /// True if seen within `grace` of now (grace period so one missed SSDP
    /// window, e.g. UDP loss, does not mark a healthy speaker offline).
    pub fn is_online(&self, grace: Duration) -> bool {
        SystemTime::now()
            .duration_since(self.last_seen)
            .map(|d| d < grace)
            .unwrap_or(false)
    }
}

/// Serializable speaker for the API (Phase 2: `GET /api/speakers`).
#[derive(Clone, Debug, Serialize)]
pub struct SpeakerInfo {
    pub id: String,
    pub name: String,
    pub online: bool,
    /// Unix epoch seconds of the last discovery sighting.
    pub last_seen: u64,
}

/// Grace period after the last sighting within which a speaker is considered
/// "online" (absorbs transient SSDP loss such as UDP drops).
pub const ONLINE_GRACE: Duration = Duration::from_secs(60);

/// In-memory speaker store shared between the discovery daemon and handlers.
pub struct SpeakerStore {
    speakers: Mutex<HashMap<String, Speaker>>,
}

impl Default for SpeakerStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeakerStore {
    pub fn new() -> Self {
        Self { speakers: Mutex::new(HashMap::new()) }
    }

    /// Insert or replace a speaker (keyed by USN).
    pub fn upsert(&self, speaker: Speaker) {
        let mut guard = self.speakers.lock().unwrap();
        guard.insert(speaker.usn.clone(), speaker);
    }

    /// All speakers with computed online status (given a grace period).
    pub fn infos(&self, grace: Duration) -> Vec<SpeakerInfo> {
        self.speakers
            .lock()
            .unwrap()
            .values()
            .map(|s| SpeakerInfo {
                id: s.usn.clone(),
                name: s.name.clone(),
                online: s.is_online(grace),
                last_seen: epoch_seconds(s.last_seen),
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Speaker> {
        self.speakers.lock().unwrap().get(id).cloned()
    }

    }

/// Unix epoch seconds for a `SystemTime`.
fn epoch_seconds(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// SSDP discovery
// ---------------------------------------------------------------------------

/// Send an SSDP M-SEARCH for all devices and collect UPnP-Response packets.
/// Returns (usn, location) pairs.
async fn msearch(timeout: Duration) -> Result<Vec<(String, String)>> {
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    // The local bind address; responses are unicast back to it.
    let _local = socket.local_addr()?;
    let packet = "M-SEARCH * HTTP/1.1\r\nHost: 239.255.255.250:1900\r\nUPnP-Version: UPnP/1.0 Multicast\r\nCache-Control: no-cache\r\nCRLF: 0\r\n\r\n".to_string();
    let dest = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
    socket.send_to(packet.as_bytes(), SocketAddr::V4(dest)).await?;

    let mut responses = Vec::new();
    let deadline = SystemTime::now() + timeout;
    let mut buf = vec![0u8; 4096];
    while let Ok(remaining) = deadline.duration_since(SystemTime::now()) {
        match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            Ok(Ok((len, _addr))) => {
                let text = String::from_utf8_lossy(&buf[..len]);
                if let Some(pair) = parse_response(&text) {
                    responses.push(pair);
                }
            }
            Ok(Err(_)) => {} // io error on a single packet; keep collecting
            Err(_) => break, // timeout: done collecting
        }
    }
    Ok(responses)
}

/// Parse an SSDP UPnP-Response packet, returning (usn, location) if present.
fn parse_response(packet: &str) -> Option<(String, String)> {
    let first_line = packet.lines().next()?;
    if !first_line.starts_with("UPnP-Response") {
        return None;
    }
    let usn = header(packet, "USN")?;
    let location = header(packet, "HTTP-Location")
        .or_else(|| header(packet, "Location"))?;
    Some((usn, location))
}

/// Get a header value from a packet (case-insensitive header name).
fn header(packet: &str, name: &str) -> Option<String> {
    let want = format!("{}:", name.to_lowercase());
    for line in packet.lines() {
        if line.to_lowercase().starts_with(&want) {
            return line.split_once(':').map(|(_, v)| v.trim().to_string());
        }
    }
    None
}

/// Fetch the XML device description for a `location` URL.
async fn fetch_description(client: &Client, location: &str) -> Result<String> {
    let r = client
        .get(location)
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    r.text()
        .await
        .map_err(|e| anyhow::anyhow!("fetch description failed: {e}"))
}

/// Extract the first `<tag>value</tag>` from XML text (simple, attribute-free
/// tags).
fn xml_field(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    let value = xml[start..end].trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Find a service's controlUrl by its serviceType URN or serviceName.
fn service_control_url(xml: &str, key: &str) -> Option<String> {
    const OPEN: &str = "<service>";
    const CLOSE: &str = "</service>";
    let mut pos = 0usize;
    while let Some(start) = xml[pos..].find(OPEN) {
        let block_start = pos + start;
        let block_end = block_start + xml[block_start..].find(CLOSE)?;
        let block = &xml[block_start..block_end + CLOSE.len()];
        let service_type = xml_field(block, "serviceType").unwrap_or_default();
        let service_name = xml_field(block, "serviceName").unwrap_or_default();
        if service_type == key || service_name == key {
            return xml_field(block, "controlURL");
        }
        pos = block_end + CLOSE.len();
    }
    None
}

/// Parse a device XML description into a `Speaker`.
///
/// Requires the USN to contain `ZonePlayer` (the Sonos device URN) — so
/// plain AV-transport devices are not added (spec D2).
fn parse_description(xml: &str, usn: String) -> Result<Speaker> {
    if !usn.contains("ZonePlayer") {
        return Err(anyhow::anyhow!(
            "not a Sonos (ZonePlayer) device - USN {usn:?}"
        ));
    }
    let name = xml_field(xml, "friendlyName")
        .unwrap_or_else(|| "Unknown speaker".to_string());
    let av = service_control_url(xml, "AVTransport")
        .or_else(|| service_control_url(xml, "urn:upnpan:avtransport:1"))
        .ok_or(anyhow::anyhow!("no AVTransport control URL"))?;
    let rc = service_control_url(xml, "RenderingControl")
        .or_else(|| service_control_url(xml, "urn:upnpan:renderingcontrol:1"))
        .ok_or(anyhow::anyhow!("no RenderingControl control URL"))?;
    Ok(Speaker {
        usn,
        name,
        av_transport_url: av,
        rendering_control_url: rc,
        last_seen: SystemTime::now(),
    })
}

/// Do a single SSDP discovery pass, upserting any renderer into `store`.
pub async fn discover_once(client: &Client, store: &SpeakerStore) {
    if let Ok(responses) = msearch(Duration::from_secs(3)).await {
        for (usn, location) in responses {
            if let Ok(desc) = fetch_description(client, &location).await {
                if let Ok(speaker) = parse_description(&desc, usn) {
                    store.upsert(speaker);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UPnP SOAP control
// ---------------------------------------------------------------------------

fn soap_body(action: &str, args: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:u="urn:upnpan:1.0">
 <soapenv:Body>
  <u:{action} xmlns:u="urn:upnpan:1.0">
   {args}
  </u:{action}>
 </soapenv:Body>
</soapenv:Envelope>"#,
        action = action,
        args = args,
    )
}

/// Set the volume on a speaker. Must be sent before the URI (some firmware
/// rejects SetAVTransportURI otherwise).
async fn set_volume(client: &Client, speaker: &Speaker, volume: u16) -> Result<()> {
    let body = soap_body("SetVolume", &format!("<Volume>{volume}</Volume>"));
    let r = client
        .post(&speaker.rendering_control_url)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    if !r.status().is_success() {
        return Err(anyhow::anyhow!("SetVolume failed: {}", r.status()));
    }
    Ok(())
}

/// Cast the given audio URI to a speaker: set volume, set URI, play.
pub async fn cast(client: &Client, speaker: &Speaker, uri: &str, volume: u16) -> Result<()> {
    set_volume(client, speaker, volume).await?;

    // Point the speaker at the audio.
    let body = soap_body("SetAVTransportURI", &format!("<URI>{uri}</URI>"));
    let r = client
        .post(&speaker.av_transport_url)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    if !r.status().is_success() {
        return Err(anyhow::anyhow!("SetAVTransportURI failed: {}", r.status()));
    }

    // Start playback. Play takes no arguments.
    let body = soap_body("Play", "");
    let r = client
        .post(&speaker.av_transport_url)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;
    if !r.status().is_success() {
        return Err(anyhow::anyhow!("Play failed: {}", r.status()));
    }
    Ok(())
}

/// Resolve the LAN IP this server should be told to fetch audio from.
/// Priority: configured `server_ip` (must parse) → auto-detect via the UDP
/// connect trick. Never falls back to 127.0.0.1 for real LAN delivery —
/// a bad configured IP or a failed auto-detect is an error, not a silent
/// localhost fallback (use the Browse UI or set server_ip).
pub fn resolve_server_ip(configured: &str) -> Result<Ipv4Addr> {
    if !configured.is_empty() {
        return configured
            .parse::<Ipv4Addr>()
            .map_err(|e| anyhow::anyhow!("server_ip {configured:?} is not a valid IPv4 address: {e}"));
    }
    let ip = local_ip().ok_or_else(|| {
        anyhow::anyhow!(
            "could not auto-detect a LAN IP; set upnp.server_ip in the config"
        )
    })?;
    if ip.is_loopback() || ip.is_unspecified() {
        return Err(anyhow::anyhow!(
            "auto-detected address {ip} is not a usable LAN address; set upnp.server_ip in the config"
        ));
    }
    Ok(ip)
}

/// Best-effort local IPv4 address detection (UDP connect trick; no packets sent).
/// Returns the interface that would route to the public internet, or None.
pub fn local_ip() -> Option<Ipv4Addr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok() {
        Some(std::net::SocketAddr::V4(v4)) => {
            let ip = *v4.ip();
            // Loopback / unspecified are not usable for LAN discovery;
            // return None so the caller can fail loudly.
            if ip.is_loopback() || ip.is_unspecified() {
                None
            } else {
                Some(ip)
            }
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sonos_xml() -> &'static str {
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <root xmlns=\"urn:schemas-upnp-org:device-1-0\">\n\
           <friendlyName>Living Room</friendlyName>\n\
           <device><deviceType>urn:schemas-upnp-org:device:ZonePlayer</deviceType></device>\n\
           <specVersion><major>1</major><minor>0</minor></specVersion>\n\
           <URLBase>http://192.168.1.10:5000/</URLBase>\n\
           <Services>\n\
             <service><cacheDuration>180</cacheDuration><serviceId>AVTransport</serviceId><serviceType>urn:upnpan:avtransport:1</serviceType><specificVersion>1</specificVersion><controlURL>http://192.168.1.10:5000/MediaRenderer/AVTransport/Control</controlURL></service>\n\
             <service><cacheDuration>180</cacheDuration><serviceId>RenderingControl</serviceId><serviceType>urn:upnpan:renderingcontrol:1</serviceType><specificVersion>1</specificVersion><controlURL>http://192.168.1.10:5000/MediaRenderer/RenderingControl/Control</controlURL></service>\n\
           </Services>\n\
         </root>"
    }

    #[test]
    fn parse_description_requires_zoneplayer_usn() {
        let xml = sonos_xml();
        // Sonos-like USN passes.
        let speaker = parse_description(
            xml,
            "uuid:public-uuid:urn:schemas-upnp-org:service:ZonePlayer:1".to_string(),
        )
        .unwrap();
        assert_eq!(speaker.name, "Living Room");
        assert_eq!(speaker.av_transport_url, "http://192.168.1.10:5000/MediaRenderer/AVTransport/Control");
        assert_eq!(speaker.rendering_control_url, "http://192.168.1.10:5000/MediaRenderer/RenderingControl/Control");

        // Non-Sonos device is rejected.
        let err = parse_description(xml, "uuid:public-uuid:urn:schemas-upnp-org:service:AV1:1".to_string())
            .unwrap_err();
        assert!(err.to_string().contains("ZonePlayer"));
    }

    #[test]
    fn parse_description_requires_audio_and_rendering_control() {
        // AVTransport missing → error.
        let no_av = sonos_xml().replace("urn:upnpan:avtransport:1", "urn:schemas-upnp-org:device:BrokenAudio");
        let err = parse_description(
            &no_av,
            "urn:schemas-upnp-org:service:ZonePlayer:1".to_string(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("AVTransport"));
    }

    #[test]
    fn speaker_store_upsert_get_infos() {
        let store = SpeakerStore::default();
        assert_eq!(store.infos(Duration::from_secs(60)).len(), 0);

        let spk = Speaker {
            usn: "urn:schemas-upnp-org:service:ZonePlayer:1".to_string(),
            name: "Living Room".to_string(),
            av_transport_url: "http://x/av".to_string(),
            rendering_control_url: "http://x/rc".to_string(),
            last_seen: SystemTime::now(),
        };
        store.upsert(spk.clone());
        assert_eq!(store.infos(Duration::from_secs(60)).len(), 1);
        let s = store.get("urn:schemas-upnp-org:service:ZonePlayer:1").unwrap();
        assert_eq!(s.name, "Living Room");

        let infos = store.infos(ONLINE_GRACE);
        assert_eq!(infos.len(), 1);
        assert!(infos[0].online);
        assert_eq!(infos[0].id, "urn:schemas-upnp-org:service:ZonePlayer:1");
    }

    #[test]
    fn speaker_store_is_offline_outside_grace() {
        let store = SpeakerStore::default();
        let spk = Speaker {
            usn: "urn:schemas-upnp-org:service:ZonePlayer:1".to_string(),
            name: "x".to_string(),
            av_transport_url: String::new(),
            rendering_control_url: String::new(),
            last_seen: SystemTime::now() - Duration::from_secs(600),
        };
        store.upsert(spk);
        let info = store.infos(ONLINE_GRACE)[0].clone();
        assert!(!info.online, "speaker seen 10 min ago must be offline");
    }

    #[test]
    fn resolve_server_ip_uses_configured_explicit_ip() {
        assert_eq!(
            resolve_server_ip("192.168.1.50").unwrap(),
            Ipv4Addr::new(192, 168, 1, 50)
        );
    }

    #[test]
    fn resolve_server_ip_rejects_bad_configured_ip() {
        // No kind of fallback: bad configured IP must fail loudly.
        assert!(resolve_server_ip("not-an-ip").is_err());
    }

    #[test]
    fn local_ip_rejects_loopback() {
        // Best-effort check with the current machine (if loopback-only, None).
        // We don't assert a value here — just that it doesn't return the
        // impossible loopback-as-usable.
        if let Some(ip) = local_ip() {
            assert!(!ip.is_loopback());
        }
    }

    #[test]
    fn soap_body_produces_envelope_with_args() {
        let body = soap_body("SetVolume", "<Volume>42</Volume>");
        assert!(body.contains("<Volume>42</Volume>"));
        assert!(body.contains("<u:SetVolume"));
        let body = soap_body("Play", "");
        assert!(body.contains("<u:Play"));
    }
}
