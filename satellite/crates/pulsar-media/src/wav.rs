//! The built-in fallback player for devices without the firmware's GStreamer (the Irbis A port):
//! RIFF/WAVE streams (PCM S16LE, mono or stereo, any rate) over HTTP or from local files, into
//! the model's playback PCM (`Model::playback_pcm`). HA transcodes TTS answers and announcements to the format the media
//! player entity declares (`ApiConfig::media_formats`: 16 kHz mono WAV on such devices), so the
//! voice pipeline needs no other codec. Intended limitations against the GStreamer player: no
//! codecs other than WAV, no HTTPS (HA serves its media on the LAN over HTTP), a Music retry
//! restarts the url from its beginning (no HTTP Range), and pause holds the stream in place
//! (a server that times the connection out ends the playback).
//!
//! One worker thread owns both channels; commands arrive over a channel and are served between
//! the ~25 ms ALSA blocks, so stop/pause/gain land within one block.

use std::fs::File;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use log::{debug, error, info, warn};
use pulsar_alsa::{Format, HwRequest, Pcm, Stream as AlmStream};

use crate::player::{Channel, PlayId, PlayState, Shared};

/// Length of one ALSA write block; bounds the command latency (stop, pause, gain).
const BLOCK_MS: u32 = 25;
/// Gain ramp of the GStreamer player: 0.05 per 20 ms (full scale in 400 ms), no duck clicks.
const RAMP_STEP: f64 = 0.05;
const RAMP_MS: u32 = 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Music retries of a network failure (1, 2, 4 s apart), as in the GStreamer player. Announce
/// and local files fail at once.
const MAX_RETRIES: u32 = 3;
const MAX_REDIRECTS: u32 = 3;

enum Cmd {
    Play(Channel, PlayId, Vec<String>),
    Pause(Channel),
    Resume(Channel),
    Stop(Channel),
    Gain(Channel, f64),
}

/// Commands to the worker thread; it ends when the last handle is dropped.
pub(crate) struct Handle {
    // Field order: the sender drops first, so the worker sees the disconnect, then the join.
    tx: Sender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    fn send(&self, cmd: Cmd) {
        if let Err(e) = self.tx.send(cmd) {
            warn!("wav player gone: {e}");
        }
    }

    pub(crate) fn play(&self, ch: Channel, id: PlayId, urls: Vec<String>) {
        self.send(Cmd::Play(ch, id, urls));
    }

    pub(crate) fn pause(&self, ch: Channel) {
        self.send(Cmd::Pause(ch));
    }

    pub(crate) fn resume(&self, ch: Channel) {
        self.send(Cmd::Resume(ch));
    }

    pub(crate) fn stop(&self, ch: Channel) {
        self.send(Cmd::Stop(ch));
    }

    pub(crate) fn set_gain(&self, ch: Channel, gain: f64) {
        self.send(Cmd::Gain(ch, gain));
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Drop::drop runs before the fields: the sender must go explicitly, or the worker
        // would never see the disconnect and the join below would wait forever. With it gone,
        // the worker exits within one ALSA block (the daemon's teardown watchdog bounds the
        // join).
        let (dead, _) = channel();
        drop(std::mem::replace(&mut self.tx, dead));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts the worker thread; `pcm` is the ALSA playback device (`Model::playback_pcm`).
pub(crate) fn start(shared: Arc<Shared>, pcm: String) -> Handle {
    let (tx, rx) = channel();
    let thread = std::thread::Builder::new()
        .name("wav-player".into())
        .spawn(move || run(&shared, &pcm, rx))
        .expect("wav player thread");
    Handle {
        tx,
        thread: Some(thread),
    }
}

// ---------------------------------------------------------------- worker

/// State of one channel's url list.
struct Chan {
    id: PlayId,
    urls: Vec<String>,
    index: usize,
    active: bool,
    /// false: paused.
    want_play: bool,
    all_ok: bool,
    retries: u32,
    /// Not before: a pending retry of the current url.
    retry_at: Option<Instant>,
    /// The Playing state of the current url was published.
    started: bool,
    gain_cur: f64,
    gain_target: f64,
}

impl Chan {
    fn idle() -> Chan {
        Chan {
            id: PlayId(0),
            urls: Vec::new(),
            index: 0,
            active: false,
            want_play: true,
            all_ok: true,
            retries: 0,
            retry_at: None,
            started: false,
            gain_cur: 1.0,
            gain_target: 1.0,
        }
    }
}

/// An opened url: its body reader, the WAV data left and the ALSA handle.
struct Stream {
    body: Box<dyn Read>,
    /// Bytes of the data chunk not yet written; 0 ends the url.
    left: u64,
    rate: u32,
    channels: u16,
    pcm: Pcm,
}

impl Stream {
    fn frame_bytes(&self) -> usize {
        usize::from(self.channels) * 2
    }

    fn block_frames(&self) -> usize {
        (u64::from(self.rate) * u64::from(BLOCK_MS) / 1000).max(1) as usize
    }
}

/// Why opening a url failed; network failures are what the Music channel retries.
enum OpenError {
    Net(String),
    Fatal(String),
}

impl OpenError {
    fn fatal(&self) -> bool {
        matches!(self, OpenError::Fatal(_))
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Net(e) | OpenError::Fatal(e) => f.write_str(e),
        }
    }
}

fn run(shared: &Arc<Shared>, pcm: &str, rx: std::sync::mpsc::Receiver<Cmd>) {
    let mut chans = [Chan::idle(), Chan::idle()];
    let mut streams: [Option<Stream>; 2] = [None, None];
    info!("wav player: HTTP/local WAV into the '{pcm}' PCM");
    loop {
        let busy = chans.iter().any(|c| c.active);
        let timeout = Duration::from_millis(if busy { u64::from(BLOCK_MS) } else { 200 });
        match rx.recv_timeout(timeout) {
            Ok(Cmd::Play(ch, id, urls)) => {
                let i = ch.index();
                // The handle superseded the previous list: its Done(false) is out already.
                streams[i] = None;
                let gains = (chans[i].gain_target, chans[i].gain_target);
                chans[i] = Chan {
                    id,
                    urls,
                    index: 0,
                    active: true,
                    want_play: true,
                    all_ok: true,
                    retries: 0,
                    retry_at: None,
                    started: false,
                    // A play() during a duck keeps the duck (set_channel_gain comes first).
                    gain_cur: gains.0,
                    gain_target: gains.1,
                };
                debug!("wav: ch{} play {}", i, id.0);
                shared.set_state(ch, PlayState::Loading);
            }
            Ok(Cmd::Stop(ch)) => {
                // Done(false) was sent by the handle's supersede; the state is ours.
                let i = ch.index();
                streams[i] = None;
                if chans[i].active {
                    chans[i].active = false;
                    shared.set_state(ch, PlayState::Idle);
                }
            }
            Ok(Cmd::Pause(ch)) => {
                let i = ch.index();
                if chans[i].active && chans[i].want_play {
                    chans[i].want_play = false;
                    if shared.state(ch) == PlayState::Playing {
                        shared.set_state(ch, PlayState::Paused);
                    }
                }
            }
            Ok(Cmd::Resume(ch)) => {
                let i = ch.index();
                if chans[i].active && !chans[i].want_play {
                    chans[i].want_play = true;
                    shared.set_state(ch, PlayState::Loading);
                }
            }
            Ok(Cmd::Gain(ch, g)) => chans[ch.index()].gain_target = g.clamp(0.0, 1.0),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for ch in [Channel::Music, Channel::Announce] {
            advance(
                shared,
                pcm,
                &mut chans[ch.index()],
                &mut streams[ch.index()],
                ch,
            );
        }
    }
    debug!("wav player: commands closed, exiting");
}

/// One step of a channel: opens the next url when needed and writes one block; ends the list
/// after its last url (Done + Idle) or after a fatal failure (Error, then Done + Idle).
fn advance(shared: &Shared, pcm: &str, c: &mut Chan, stream: &mut Option<Stream>, ch: Channel) {
    if !c.active || !c.want_play {
        return;
    }
    if stream.is_none() {
        if let Some(at) = c.retry_at
            && Instant::now() < at
        {
            return; // waiting out a retry backoff
        }
        let Some(url) = c.urls.get(c.index).cloned() else {
            finish(shared, c, ch, stream);
            return;
        };
        match open_stream(&url, pcm) {
            Ok(s) => {
                c.retries = 0;
                c.retry_at = None;
                c.started = false;
                *stream = Some(s);
            }
            Err(e) => {
                if ch == Channel::Music && !e.fatal() && c.retries < MAX_RETRIES {
                    let backoff = Duration::from_secs(u64::from(1u32 << c.retries));
                    c.retries += 1;
                    c.retry_at = Some(Instant::now() + backoff);
                    warn!(
                        "wav: ch{} '{}' failed ({}), retry {} in {} s",
                        ch.index(),
                        url,
                        e,
                        c.retries,
                        backoff.as_secs()
                    );
                    shared.set_state(ch, PlayState::Loading);
                    return;
                }
                error!("wav: ch{} '{}': {e}", ch.index(), url);
                c.all_ok = false;
                shared.set_state(ch, PlayState::Error);
                finish(shared, c, ch, stream);
                return;
            }
        }
    }
    let Some(s) = stream.as_mut() else { return };
    match write_block(shared, s, c, ch) {
        Ok(Block::More) => {}
        Ok(Block::UrlDone) => {
            *stream = None;
            c.index += 1;
            if c.index < c.urls.len() {
                shared.set_state(ch, PlayState::Loading);
            } else {
                finish(shared, c, ch, stream);
            }
        }
        Ok(Block::Idle) => {} // paused between commands
        Err(e) => {
            error!("wav: ch{} playback failed: {e}", ch.index());
            c.all_ok = false;
            *stream = None;
            shared.set_state(ch, PlayState::Error);
            finish(shared, c, ch, stream);
        }
    }
}

enum Block {
    /// A block was written.
    More,
    /// The url's data ended.
    UrlDone,
    /// Nothing written (paused meanwhile).
    Idle,
}

/// Writes one block of the stream, applying the (ramped) channel gain; publishes Playing on the
/// first block of the url.
fn write_block(
    shared: &Shared,
    s: &mut Stream,
    c: &mut Chan,
    ch: Channel,
) -> Result<Block, String> {
    if !c.want_play {
        return Ok(Block::Idle);
    }
    if s.left == 0 {
        return Ok(Block::UrlDone);
    }
    let fb = s.frame_bytes();
    let frames = s.block_frames().min((s.left as usize) / fb).max(1);
    let bytes = frames * fb;
    let mut buf = vec![0u8; bytes];
    let mut got = 0;
    while got < bytes {
        match s.body.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    if got == 0 {
        // The source ended before the data chunk did.
        return Err("stream ended inside the WAV data".into());
    }
    buf.truncate(got - got % fb);
    if buf.is_empty() {
        return Err("stream ended inside a WAV frame".into());
    }
    let written_frames = buf.len() / fb;
    s.left = s.left.saturating_sub(buf.len() as u64);
    apply_gain(&mut buf, written_frames, usize::from(s.channels), s.rate, c);
    let mut off = 0;
    while off < buf.len() {
        let frames_left = (buf.len() - off) / fb;
        match s.pcm.writei(&buf[off..], frames_left) {
            Ok(w) if w > 0 => off += w * fb,
            Ok(_) => return Err("ALSA wrote nothing".into()),
            Err(e) => {
                s.pcm
                    .recover(e, true)
                    .map_err(|e2| format!("ALSA: {}", pulsar_alsa::strerror(e2)))?;
            }
        }
    }
    if !c.started {
        c.started = true;
        shared.set_state(ch, PlayState::Playing);
    }
    Ok(if s.left == 0 {
        Block::UrlDone
    } else {
        Block::More
    })
}

/// Scales the block's interleaved S16 samples by the channel gain, ramping `gain_cur` towards
/// `gain_target` (RAMP_STEP per RAMP_MS of audio).
fn apply_gain(buf: &mut [u8], frames: usize, channels: usize, rate: u32, c: &mut Chan) {
    let target = c.gain_target;
    let block_ms = frames as f64 * 1000.0 / f64::from(rate);
    let step = RAMP_STEP * block_ms / f64::from(RAMP_MS);
    let ramping = (target - c.gain_cur).abs() > 1e-4 && step > 0.0;
    if target >= 1.0 && c.gain_cur >= 1.0 {
        c.gain_cur = target;
        return; // unity: leave the samples alone
    }
    let n = (frames * channels).max(1);
    for (i, sm) in buf.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let g = if ramping {
            let t = i as f64 / n as f64;
            c.gain_cur + (target - c.gain_cur) * t
        } else {
            target
        };
        let v = i16::from_le_bytes(*sm);
        let out = (f64::from(v) * g).clamp(-32768.0, 32767.0) as i16;
        *sm = out.to_le_bytes();
    }
    if ramping {
        let d = target - c.gain_cur;
        c.gain_cur += d.clamp(-step, step);
    } else {
        c.gain_cur = target;
    }
}

/// Ends the channel's list: Done(all_ok); an Error state stays until the next play (as in the
/// GStreamer player), otherwise Idle.
fn finish(shared: &Shared, c: &mut Chan, ch: Channel, stream: &mut Option<Stream>) {
    *stream = None;
    if !c.active {
        return;
    }
    c.active = false;
    let ok = c.all_ok;
    shared.complete(ch, c.id, ok);
    if shared.state(ch) != PlayState::Error {
        shared.set_state(ch, PlayState::Idle);
    }
    debug!("wav: ch{} list {} done (ok={ok})", ch.index(), c.id.0);
}

// ---------------------------------------------------------------- sources

/// Opens `url` (http://, file:// or an absolute path), parses the WAV header and opens the ALSA
/// PCM for it.
fn open_stream(url: &str, dev: &str) -> Result<Stream, OpenError> {
    let (body, wav) = open_wav(url)?;
    let mut pcm = Pcm::open(dev, AlmStream::Playback, false).map_err(|e| {
        OpenError::Fatal(format!("snd_pcm_open {dev}: {}", pulsar_alsa::strerror(e)))
    })?;
    let t = pcm.set_hw_params_traced(&HwRequest {
        format: Format::S16Le,
        channels: u32::from(wav.channels),
        rate: wav.rate,
        rate_exact: false,
        period_frames: None,
        buffer_frames: None,
    });
    if let Some((step, e)) = t.failed {
        return Err(OpenError::Fatal(format!(
            "hw_params {step}: {}",
            pulsar_alsa::strerror(e)
        )));
    }
    let period = t.actual.period_frames.max(1);
    pcm.set_sw_params(period, period)
        .map_err(|e| OpenError::Fatal(format!("sw_params: {}", pulsar_alsa::strerror(e))))?;
    pcm.prepare()
        .map_err(|e| OpenError::Fatal(format!("prepare: {}", pulsar_alsa::strerror(e))))?;
    Ok(Stream {
        body,
        left: wav.data_len,
        rate: wav.rate.max(1),
        channels: wav.channels,
        pcm,
    })
}

/// A parsed RIFF/WAVE header (PCM S16LE only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WavFmt {
    pub(crate) rate: u32,
    pub(crate) channels: u16,
    pub(crate) data_len: u64,
}

/// Reads the RIFF header of `r`: the fmt chunk must be PCM (or extensible-PCM) S16LE, 1-2
/// channels; returns the format and the data chunk's length, with `r` positioned at the data.
pub(crate) fn parse_wav_header(r: &mut impl Read) -> io::Result<WavFmt> {
    fn exact(r: &mut impl Read, n: usize) -> io::Result<Vec<u8>> {
        let mut b = vec![0u8; n];
        r.read_exact(&mut b)?;
        Ok(b)
    }
    let head = exact(r, 12)?;
    if &head[..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "not a RIFF/WAVE stream",
        ));
    }
    let mut fmt: Option<(u16, u16, u32)> = None; // (format tag, channels, rate)
    loop {
        let mut ch = exact(r, 8)?;
        let id = [ch[0], ch[1], ch[2], ch[3]];
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if id == *b"fmt " {
            ch.clear();
            let f = exact(r, size.min(64) as usize)?;
            if f.len() < 16 {
                return Err(io::Error::new(ErrorKind::InvalidData, "short fmt chunk"));
            }
            let tag = u16::from_le_bytes([f[0], f[1]]);
            let channels = u16::from_le_bytes([f[2], f[3]]);
            let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
            let bits = u16::from_le_bytes([f[14], f[15]]);
            let tag = if tag == 0xFFFE && f.len() >= 26 {
                // WAVE_FORMAT_EXTENSIBLE: the real tag is the first word of the subformat GUID;
                // the cbSize bytes between were read as part of `f` when present.
                let cb = u16::from_le_bytes([f[16], f[17]]) as usize;
                if f.len() >= 18 + cb + 2 {
                    u16::from_le_bytes([f[18 + cb], f[19 + cb]])
                } else {
                    tag
                }
            } else {
                tag
            };
            if tag != 1 || bits != 16 || !(1..=2).contains(&channels) || rate == 0 {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unsupported WAV: tag {tag}, {channels} ch, {rate} Hz, {bits} bit"),
                ));
            }
            // Chunks are word-aligned.
            if size % 2 == 1 {
                exact(r, 1)?;
            }
            fmt = Some((tag, channels, rate));
        } else if id == *b"data" {
            let Some((_, channels, rate)) = fmt else {
                return Err(io::Error::new(ErrorKind::InvalidData, "data before fmt"));
            };
            return Ok(WavFmt {
                rate,
                channels,
                data_len: size,
            });
        } else {
            let mut left = size + size % 2;
            let mut skip = [0u8; 512];
            while left > 0 {
                let take = (left as usize).min(skip.len());
                let n = r.read(&mut skip[..take])?;
                if n == 0 {
                    break;
                }
                left -= n as u64;
            }
        }
    }
}

/// Opens the url and parses the WAV header; the reader continues at the data chunk.
fn open_wav(url: &str) -> Result<(Box<dyn Read>, WavFmt), OpenError> {
    let net = |e: String| OpenError::Net(e);
    if let Some(rest) = url.strip_prefix("http://") {
        let mut redirects = 0;
        let mut target = rest.to_owned();
        loop {
            let (mut body, status, location) = http_get(&target).map_err(net)?;
            match status {
                200 => {
                    let wav = parse_wav_header(&mut body)
                        .map_err(|e| OpenError::Fatal(format!("WAV header: {e}")))?;
                    return Ok((body, wav));
                }
                301 | 302 | 303 | 307 | 308 if redirects < MAX_REDIRECTS => {
                    let Some(loc) = location else {
                        return Err(net(format!("redirect {status} without a Location")));
                    };
                    target = join_url(&target, &loc);
                    redirects += 1;
                }
                s => return Err(net(format!("HTTP status {s}"))),
            }
        }
    }
    if url.starts_with("https://") {
        return Err(OpenError::Fatal(
            "the fallback player does not do HTTPS; serve media over HTTP".into(),
        ));
    }
    let path = url.strip_prefix("file://").unwrap_or(url);
    let mut file =
        BufReader::new(File::open(path).map_err(|e| OpenError::Fatal(format!("{path}: {e}")))?);
    let wav =
        parse_wav_header(&mut file).map_err(|e| OpenError::Fatal(format!("WAV header: {e}")))?;
    Ok((Box::new(file), wav))
}

// ---------------------------------------------------------------- HTTP

/// The answer of one GET: the body reader (chunked bodies decoded), the status and the Location
/// header. The connection closes when the reader drops.
type HttpAnswer = (Box<dyn Read>, u16, Option<String>);

fn http_get(target: &str) -> Result<HttpAnswer, String> {
    let (host, port, path) = split_url(target)?;
    let mut stream = TcpStream::connect((host.as_str(), port))
        .map_err(|e| format!("connect {host}:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(CONNECT_TIMEOUT + Duration::from_secs(20)))
        .ok();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: pulsar\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| format!("request: {e}"))?;
    stream.flush().map_err(|e| format!("request: {e}"))?;
    let mut rw = BufReader::new(stream);
    let mut status_line = String::new();
    rw.read_line(&mut status_line)
        .map_err(|e| format!("answer: {e}"))?;
    let status =
        parse_status(&status_line).ok_or_else(|| format!("bad status line: {status_line:?}"))?;
    let mut length: Option<u64> = None;
    let mut chunked = false;
    let mut location = None;
    loop {
        let mut line = String::new();
        let n = rw
            .read_line(&mut line)
            .map_err(|e| format!("headers: {e}"))?;
        if n == 0 || line == "\n" || line == "\r\n" {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().ok(),
            "transfer-encoding" => chunked = value.to_ascii_lowercase().contains("chunked"),
            "location" => location = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    let body: Box<dyn Read> = if chunked {
        Box::new(Chunked::new(rw))
    } else if let Some(n) = length {
        Box::new(rw.take(n))
    } else {
        Box::new(rw) // read to EOF (Connection: close)
    };
    Ok((body, status, location))
}

fn parse_status(line: &str) -> Option<u16> {
    // "HTTP/1.1 200 OK"
    let mut it = line.split_whitespace();
    let v = it.next()?;
    if !v.starts_with("HTTP/") {
        return None;
    }
    it.next()?.parse().ok()
}

/// "host[:port]/path" of an http:// url's rest.
fn split_url(rest: &str) -> Result<(String, u16, String), String> {
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h.to_owned(), port),
            // An IPv6 literal without a port ("[::1]/x"): the colon is part of the address.
            Err(_) => (authority.to_owned(), 80),
        },
        None => (authority.to_owned(), 80),
    };
    if host.is_empty() {
        return Err("empty host in url".into());
    }
    Ok((host, port, path.to_owned()))
}

/// Resolves a (possibly relative) Location against the url that answered. Both the base and the
/// result are the part after `http://` (what [`split_url`] takes); an absolute https Location is
/// returned with its scheme, and opening it then fails as unsupported.
fn join_url(base: &str, loc: &str) -> String {
    if let Some(rest) = loc.strip_prefix("http://") {
        return rest.to_owned();
    }
    if loc.starts_with("https://") {
        return loc.to_owned();
    }
    let rest = base.strip_prefix("http://").unwrap_or(base);
    let authority = rest.split('/').next().unwrap_or(rest);
    if loc.starts_with('/') {
        format!("{authority}{loc}")
    } else {
        let dir = &rest[authority.len()..];
        let dir = dir.rsplit_once('/').map_or("/", |(d, _)| d);
        format!("{authority}{dir}/{loc}")
    }
}

/// Transfer-Encoding: chunked body.
struct Chunked<R> {
    r: R,
    /// Bytes left of the current chunk; None: between chunks (size line due).
    left: Option<u64>,
    done: bool,
}

impl<R> Chunked<R> {
    fn new(r: R) -> Chunked<R> {
        Chunked {
            r,
            left: None,
            done: false,
        }
    }
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(left) = &mut self.left {
                if *left > 0 {
                    let take = buf.len().min(*left as usize);
                    let n = self.r.read(&mut buf[..take])?;
                    if n == 0 {
                        return Err(io::Error::new(ErrorKind::UnexpectedEof, "chunked body"));
                    }
                    *left -= n as u64;
                    return Ok(n);
                }
                // Chunk ends with CRLF.
                let mut crlf = [0u8; 2];
                self.r.read_exact(&mut crlf)?;
                self.left = None;
            }
            let mut line = String::new();
            if self.r.read_line(&mut line)? == 0 {
                self.done = true;
                return Ok(0);
            }
            let size_part = line.trim_end().split(';').next().unwrap_or("");
            let size = u64::from_str_radix(size_part.trim(), 16)
                .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
            if size == 0 {
                self.done = true; // trailers (if any) are ignored
                return Ok(0);
            }
            self.left = Some(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_header(channels: u16, rate: u32, data: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        v.extend_from_slice(&(channels * 2).to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data.to_le_bytes());
        v
    }

    #[test]
    fn parses_a_plain_header() {
        let mut r = wav_header(1, 16000, 320);
        r.extend_from_slice(&[7u8; 320]);
        let mut c = r.as_slice();
        let f = parse_wav_header(&mut c).unwrap();
        assert_eq!(
            f,
            WavFmt {
                rate: 16000,
                channels: 1,
                data_len: 320
            }
        );
        assert_eq!(c.len(), 320); // positioned at the data
    }

    #[test]
    fn skips_unknown_chunks_and_padding() {
        let mut r = Vec::new();
        r.extend_from_slice(b"RIFF");
        r.extend_from_slice(&0u32.to_le_bytes()); // size unused by the parser
        r.extend_from_slice(b"WAVELIST");
        r.extend_from_slice(&5u32.to_le_bytes()); // odd size: one pad byte follows
        r.extend_from_slice(b"abcde\0");
        r.extend_from_slice(&wav_header(2, 48000, 4)[12..]); // fmt + data of a stereo WAV
        let mut c = r.as_slice();
        let f = parse_wav_header(&mut c).unwrap();
        assert_eq!((f.rate, f.channels, f.data_len), (48000, 2, 4));
    }

    #[test]
    fn rejects_non_wav_and_unsupported_formats() {
        assert!(parse_wav_header(&mut b"OggS....".as_slice()).is_err());
        // 8-bit PCM.
        let mut r = wav_header(1, 8000, 8);
        r[34] = 8; // bits per sample
        assert!(parse_wav_header(&mut r.as_slice()).is_err());
    }

    #[test]
    fn splits_and_joins_urls() {
        assert_eq!(
            split_url("host:8123/a/b.wav").unwrap(),
            ("host".into(), 8123, "/a/b.wav".into())
        );
        assert_eq!(
            split_url("host/a.wav").unwrap(),
            ("host".into(), 80, "/a.wav".into())
        );
        assert_eq!(split_url("host").unwrap(), ("host".into(), 80, "/".into()));
        assert_eq!(join_url("h:81/p/q.wav", "/x.wav"), "h:81/x.wav");
        assert_eq!(join_url("h:81/p/q.wav", "r.wav"), "h:81/p/r.wav");
        assert_eq!(join_url("h:81/p/q.wav", "http://g/z"), "g/z");
    }

    #[test]
    fn parses_status_lines() {
        assert_eq!(parse_status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_status("HTTP/1.0 404 Not Found\n"), Some(404));
        assert_eq!(parse_status("garbage\n"), None);
    }

    #[test]
    fn decodes_chunked_bodies() {
        let raw = b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n";
        let mut c = Chunked::new(BufReader::new(&raw[..]));
        let mut out = Vec::new();
        c.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello world");
    }

    #[test]
    fn gain_ramps_and_mutes() {
        let rate = 16000;
        let frames = 400; // 25 ms
        let mut buf = vec![0u8; frames * 2];
        for sm in buf.as_chunks_mut::<2>().0.iter_mut() {
            *sm = 10000i16.to_le_bytes();
        }
        let mut c = Chan::idle();
        c.gain_cur = 1.0;
        c.gain_target = 0.0;
        apply_gain(&mut buf, frames, 1, rate, &mut c);
        // Ramped: the head is louder than the tail, and the current gain moved by the step.
        let first = i16::from_le_bytes(buf[..2].try_into().unwrap());
        let last = i16::from_le_bytes(buf[buf.len() - 2..].try_into().unwrap());
        assert!(first.abs() > last.abs());
        assert!((c.gain_cur - (1.0 - 0.05 * 25.0 / 20.0)).abs() < 1e-9);
        // At the target: a plain scale, no ramp.
        c.gain_cur = 0.5;
        c.gain_target = 0.5;
        let mut b2 = vec![0u8; 4];
        b2[..2].copy_from_slice(&10000i16.to_le_bytes());
        b2[2..].copy_from_slice(&(-10000i16).to_le_bytes());
        apply_gain(&mut b2, 2, 1, rate, &mut c);
        assert_eq!(i16::from_le_bytes(b2[..2].try_into().unwrap()), 5000);
        assert_eq!(i16::from_le_bytes(b2[2..].try_into().unwrap()), -5000);
    }
}
