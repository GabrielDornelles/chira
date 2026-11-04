//! Finding and testing real cameras on the local network.
//!   discover()   ONVIF WS-Discovery (UDP multicast probe) + a look for an open RTSP port in the local /24
//!   streams()    asks the camera for its stream URLs (ONVIF GetProfiles / GetStreamUri with WS-Security digest);
//!                cameras without ONVIF are tried against a list of common RTSP paths
//!   check()      ffprobe a URL: is there video, and what kind
//! Nothing here is required to run Chira: any URL typed into "Add camera" works without discovery.

use base64::Engine;
use serde::Serialize;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Serialize, Clone, Default)]
pub struct Found {
    pub ip: String,
    pub name: String,
    pub hardware: String,
    /// ONVIF device service address, when the camera answered the discovery probe
    pub onvif: String,
    pub rtsp: bool,
}

#[derive(Serialize, Clone)]
pub struct Stream {
    pub label: String,
    /// full URL including credentials (never sent back to a browser except to be posted again)
    #[serde(skip)]
    pub url: String,
    pub shown: String,
    pub detail: String,
}

/// ffmpeg < 5 calls the RTSP socket timeout `-stimeout`, 5+ calls it `-timeout` (and 4.x reads `-timeout` as "listen")
pub fn rtsp_timeout_opt() -> &'static str {
    static OPT: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    OPT.get_or_init(|| {
        let out = Command::new("ffmpeg").arg("-version").output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        let major: u32 = out.split_whitespace().nth(2).and_then(|v| v.trim_start_matches('n').split('.').next()).and_then(|v| v.parse().ok()).unwrap_or(6);
        if major < 5 { "-stimeout" } else { "-timeout" }
    })
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

fn local_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // no packet is sent: this only picks the outgoing interface
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v) => Some(v),
        _ => None,
    }
}

fn ws_discovery() -> Vec<Found> {
    let probe = r#"<?xml version="1.0" encoding="UTF-8"?><e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope" xmlns:w="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery" xmlns:dn="http://www.onvif.org/ver10/network/wsdl"><e:Header><w:MessageID>uuid:6a2b8f10-3c1e-4c9a-9a57-0f1d5b0c7e11</w:MessageID><w:To e:mustUnderstand="true">urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To><w:Action e:mustUnderstand="true">http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action></e:Header><e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body></e:Envelope>"#;
    let mut out: Vec<Found> = Vec::new();
    let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else { return out };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));
    let _ = sock.send_to(probe.as_bytes(), "239.255.255.250:3702");
    let end = Instant::now() + Duration::from_millis(2500);
    let mut buf = vec![0u8; 16384];
    while Instant::now() < end {
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let txt = String::from_utf8_lossy(&buf[..n]).to_string();
        let xaddr = between(&txt, "XAddrs>", "</").unwrap_or("").split_whitespace().find(|u| u.starts_with("http://")).unwrap_or("").to_string();
        let scopes = between(&txt, "Scopes>", "</").unwrap_or("");
        let pick = |key: &str| scopes.split_whitespace().find_map(|s| s.split(key).nth(1)).map(|v| v.replace("%20", " ")).unwrap_or_default();
        let ip = from.ip().to_string();
        if out.iter().any(|f| f.ip == ip) {
            continue;
        }
        out.push(Found { ip, name: pick("/name/"), hardware: pick("/hardware/"), onvif: xaddr, rtsp: false });
    }
    out
}

fn scan_rtsp(base: Ipv4Addr) -> Vec<String> {
    let o = base.octets();
    let mut hits = Vec::new();
    let hosts: Vec<u8> = (1..=254).collect();
    for chunk in hosts.chunks(64) {
        let hs: Vec<_> = chunk
            .iter()
            .map(|&h| {
                std::thread::spawn(move || {
                    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], h)), 554);
                    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok().map(|_| addr.ip().to_string())
                })
            })
            .collect();
        hits.extend(hs.into_iter().filter_map(|h| h.join().ok().flatten()));
    }
    hits
}

pub fn discover() -> Vec<Found> {
    let scan = local_ip().map(|ip| std::thread::spawn(move || scan_rtsp(ip)));
    let mut found = ws_discovery();
    if let Some(Ok(ips)) = scan.map(|h| h.join()) {
        for ip in ips {
            match found.iter_mut().find(|f| f.ip == ip) {
                Some(f) => f.rtsp = true,
                None => found.push(Found { ip, rtsp: true, ..Default::default() }),
            }
        }
    }
    found.sort_by(|a, b| a.ip.cmp(&b.ip));
    found
}

// ---------------------------------------------------------------- ONVIF SOAP

fn iso_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn soap(url: &str, user: &str, pass: &str, body: &str) -> Option<String> {
    let nonce: [u8; 16] = {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(1);
        let mut h = Sha1::new();
        h.update(n.to_le_bytes());
        h.finalize()[..16].try_into().ok()?
    };
    let created = iso_now();
    let mut h = Sha1::new();
    h.update(nonce);
    h.update(created.as_bytes());
    h.update(pass.as_bytes());
    let digest = base64::engine::general_purpose::STANDARD.encode(h.finalize());
    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(nonce);
    let header = if user.is_empty() {
        String::new()
    } else {
        format!(
            r#"<s:Header><Security xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd"><UsernameToken><Username>{user}</Username><Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{digest}</Password><Nonce EncodingType="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-soap-message-security-1.0#Base64Binary">{nonce_b64}</Nonce><Created xmlns="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd">{created}</Created></UsernameToken></Security></s:Header>"#
        )
    };
    let xml = format!(r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://www.w3.org/2003/05/soap-envelope">{header}<s:Body>{body}</s:Body></s:Envelope>"#);
    let rest = url.strip_prefix("http://")?;
    let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let host = if hostport.contains(':') { hostport.to_string() } else { format!("{hostport}:80") };
    let addr = host.to_socket_addrs().ok()?.next()?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(4))).ok()?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: application/soap+xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
        xml.len()
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let txt = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = txt.split_once("\r\n\r\n")?;
    if !head.contains(" 200 ") {
        return None;
    }
    if head.to_ascii_lowercase().contains("chunked") {
        let mut out = String::new();
        let mut rest = body;
        while let Some((len, tail)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(len.trim(), 16).unwrap_or(0);
            if n == 0 || tail.len() < n {
                break;
            }
            out.push_str(&tail[..n]);
            rest = tail[n..].trim_start_matches("\r\n");
        }
        return Some(out);
    }
    Some(body.to_string())
}

fn with_creds(url: &str, user: &str, pass: &str) -> String {
    if user.is_empty() || url.contains('@') {
        return url.to_string();
    }
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    match url.split_once("://") {
        Some((sch, rest)) => format!("{sch}://{}:{}@{rest}", enc(user), enc(pass)),
        None => url.to_string(),
    }
}

fn onvif_streams(xaddr: &str, user: &str, pass: &str) -> Vec<Stream> {
    let mut out = Vec::new();
    // the media service address (falls back to the device service, which many cameras also answer for media)
    let caps = soap(xaddr, user, pass, r#"<GetCapabilities xmlns="http://www.onvif.org/ver10/device/wsdl"><Category>Media</Category></GetCapabilities>"#);
    let media = caps.as_deref().and_then(|c| between(c, "XAddr>", "</")).map(|s| s.to_string()).unwrap_or_else(|| xaddr.to_string());
    let Some(profiles) = soap(&media, user, pass, r#"<GetProfiles xmlns="http://www.onvif.org/ver10/media/wsdl"/>"#) else { return out };
    for p in profiles.split("<trt:Profiles").skip(1).chain(profiles.split("<Profiles").skip(1)) {
        let Some(token) = between(p, "token=\"", "\"") else { continue };
        let name = between(p, "<tt:Name>", "</").unwrap_or(token).to_string();
        let (w, h) = (between(p, "<tt:Width>", "</").unwrap_or("?"), between(p, "<tt:Height>", "</").unwrap_or("?"));
        let enc = between(p, "<tt:Encoding>", "</").unwrap_or("");
        let body = format!(r#"<GetStreamUri xmlns="http://www.onvif.org/ver10/media/wsdl"><StreamSetup><Stream xmlns="http://www.onvif.org/ver10/schema">RTP-Unicast</Stream><Transport xmlns="http://www.onvif.org/ver10/schema"><Protocol>RTSP</Protocol></Transport></StreamSetup><ProfileToken>{token}</ProfileToken></GetStreamUri>"#);
        if let Some(r) = soap(&media, user, pass, &body) {
            if let Some(uri) = between(&r, "<tt:Uri>", "</").or_else(|| between(&r, "Uri>", "</")) {
                let uri = uri.replace("&amp;", "&");
                let url = with_creds(&uri, user, pass);
                out.push(Stream { label: name, shown: crate::pipeline::redact(&url), url, detail: format!("{w}×{h} {enc}") });
            }
        }
    }
    out
}

const COMMON_PATHS: [&str; 14] = [
    "/stream1", "/stream", "/live", "/live/ch0", "/h264", "/11", "/12", "/Streaming/Channels/101", "/cam/realmonitor?channel=1&subtype=0",
    "/h264Preview_01_main", "/axis-media/media.amp", "/videoMain", "/onvif1", "/",
];

/// Streams a camera offers: ONVIF if it speaks it, otherwise the usual vendor paths (each verified with ffprobe).
pub fn streams(ip: &str, onvif: &str, user: &str, pass: &str) -> Vec<Stream> {
    if !onvif.is_empty() {
        let s = onvif_streams(onvif, user, pass);
        if !s.is_empty() {
            return s;
        }
    }
    let hs: Vec<_> = COMMON_PATHS
        .iter()
        .map(|p| {
            let url = with_creds(&format!("rtsp://{ip}:554{p}"), user, pass);
            std::thread::spawn(move || check(&url).map(|d| Stream { label: "RTSP".into(), shown: crate::pipeline::redact(&url), url, detail: d }))
        })
        .collect();
    hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
}

/// ffprobe a stream: Some("1920×1080 h264 25 fps") when it carries video.
pub fn check(url: &str) -> Option<String> {
    let mut c = Command::new("ffprobe");
    c.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height,avg_frame_rate", "-of", "csv=p=0"]);
    if url.starts_with("rtsp") {
        c.args(["-rtsp_transport", "tcp", rtsp_timeout_opt(), "4000000"]);
    } else if url.starts_with("rtmp") || url.starts_with("http") {
        c.args(["-rw_timeout", "4000000"]);
    }
    let child = c.arg(url).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn().ok()?;
    let out = wait_timeout(child, Duration::from_secs(8))?;
    let line = String::from_utf8_lossy(&out).lines().next()?.to_string();
    let f: Vec<&str> = line.split(',').collect(); // codec,w,h,fps
    if f.len() < 4 {
        return None;
    }
    let fps = f[3].split_once('/').and_then(|(a, b)| Some(a.parse::<f64>().ok()? / b.parse::<f64>().ok()?)).unwrap_or(0.0);
    Some(format!("{}×{} {} {:.0} fps", f[1], f[2], f[0], fps))
}

fn wait_timeout(mut child: std::process::Child, d: Duration) -> Option<Vec<u8>> {
    let end = Instant::now() + d;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let mut v = Vec::new();
                child.stdout.take()?.read_to_end(&mut v).ok()?;
                return st.success().then_some(v);
            }
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}
