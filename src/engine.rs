//! Motore audio: thread decoder (symphonia) + ring buffer + output (cpal/PipeWire).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};
use symphonia::core::audio::AudioBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

use crate::meta::Track;

/// Sample rate di output (il sink PipeWire). L'audio viene risamplicato a questo.
const OUT_RATE: u32 = 48_000; // fallback se non riesco a leggere il rate del device

// Capacita' ring: 2s al rate massimo atteso (96k).
const MAX_RING: usize = OUT_RATE as usize * 2 / 5; // ~200ms di campioni stereo


pub enum DecCmd {
    LoadFile { track: Track },
    Seek { secs: f64 },
    SetVolume { v: f64 },
    Play,
    Pause,
    Stop,
}

pub enum DecEvt {
    TrackEnd,
}

/// Risamplicatore lineare stato (da in_rate a out_rate), interleaved.
struct Resampler {
    step: f64,
    phase: f64,
    nch: usize,
}

impl Resampler {
    fn new(in_rate: u32, out_rate: u32, nch: usize) -> Self {
        Self {
            step: in_rate as f64 / out_rate as f64,
            phase: 0.0,
            nch,
        }
    }

    fn process(&mut self, in_buf: &[f32]) -> Vec<f32> {
        let nch = self.nch;
        let n_in = in_buf.len() / nch;
        if n_in == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        while self.phase < n_in as f64 - 1e-9 {
            let i0 = self.phase as usize;
            let frac = (self.phase - i0 as f64) as f32;
            let i1 = (i0 + 1).min(n_in - 1);
            for ch in 0..nch {
                let s0 = in_buf[i0 * nch + ch];
                let s1 = in_buf[i1 * nch + ch];
                out.push(s0 * (1.0 - frac) + s1 * frac);
            }
            self.phase += self.step;
        }
        self.phase -= n_in as f64;
        out
    }
}

/// Numero di frame presenti in un buffer (dalla lunghezza del primo piano).
fn buf_frames(buf: &AudioBuffer<f32>) -> usize {
    let planes = buf.planes();
    planes.planes().first().map(|p| p.len()).unwrap_or(0)
}

/// Rileva l'EOF di symphonia (IO error con UnexpectedEof).
fn is_eof(e: &symphonia::core::errors::Error) -> bool {
    matches!(
        e,
        symphonia::core::errors::Error::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof
    )
}

/// Converte un frame symphonia (f32) in stereo interleaved (downmix/duplicate).
fn frame_to_stereo(buf: &AudioBuffer<f32>) -> Vec<f32> {
    let nch = buf.spec().channels.count() as usize;
    let nframes = buf_frames(buf);
    let ap = buf.planes();
    let planes = ap.planes();
    let mut out = Vec::with_capacity(nframes * 2);
    for i in 0..nframes {
        let l = match nch {
            0 => 0.0,
            1 => planes[0][i],
            2 => planes[0][i],
            _ => (0..nch).map(|c| planes[c][i]).sum::<f32>() / nch as f32,
        };
        let r = match nch {
            0 => 0.0,
            1 => planes[0][i],
            2 => planes[1][i],
            _ => (0..nch).map(|c| planes[c][i]).sum::<f32>() / nch as f32,
        };
        out.push(l);
        out.push(r);
    }
    out
}

struct Active {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    resample: Resampler,
    eof: bool,
    track: Track,
}

fn open_active(track: &Track, seek_secs: f64, out_rate: u32) -> Option<Active> {
    let file = std::fs::File::open(&track.path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let probe = symphonia::default::get_probe();
    let hint = symphonia::core::probe::Hint::new();
    let fo = FormatOptions::default();
    let mo = MetadataOptions::default();
    let probed = probe.format(&hint, mss, &fo, &mo).ok()?;
    let mut format = probed.format;
    let tr = format.default_track()?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&tr.codec_params, &DecoderOptions::default())
        .ok()?;
    let spec = decoder.codec_params();
    let in_rate = spec.sample_rate.unwrap_or(44100);
    // Seek: usa format.seek (accurato) + reset decoder, piu' veloce e preciso
    // del decodificare dall'inizio.
    if seek_secs > 0.0 {
        let _ = format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from(seek_secs as f32),
                track_id: Some(tr.id),
            },
        );
        decoder.reset();
    }
    Some(Active {
        format,
        decoder,
        // nch=2: frame_to_stereo produce SEMPRE stereo, quindi il resampler
        // lavora su dati gia' stereo (claude: se nch=1 e' mono, 2x speed).
        resample: Resampler::new(in_rate, out_rate, 2),
        eof: false,
        track: track.clone(),
    })
}

/// Avvolge un valore `!Send` che vive solo nel thread principale (lo stream
/// audio viene creato e distrutto qui, mai spostato). Suono: il callback dello
/// stream tocca solo l'Arc<Mutex> del ring, mai lo Stream in se.
struct NonSend<T>(T);
unsafe impl<T> Send for NonSend<T> {}
unsafe impl<T> Sync for NonSend<T> {}

pub struct Engine {
    cmd_tx: tokio::sync::mpsc::UnboundedSender<DecCmd>,
    pos_frames: Arc<AtomicU64>,
    volume: Arc<Mutex<f64>>,
    out_rate: Arc<AtomicU32>,
    _stream: NonSend<Option<cpal::Stream>>,
}

impl Engine {
    pub fn new(evt_tx: tokio::sync::mpsc::UnboundedSender<DecEvt>) -> Result<Self> {
        let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(MAX_RING)));
        let pos_frames = Arc::new(AtomicU64::new(0));
        let volume = Arc::new(Mutex::new(1.0f64));

        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<DecCmd>();

        // --- Stream output (cpal -> PipeWire) ---
        let ring_cb = ring.clone();
        let host = cpal::default_host();
        let device = host
            .output_devices()
            .map_err(|e| anyhow::anyhow!("dispositivi audio: {e}"))?
            .next()
            .ok_or_else(|| anyhow::anyhow!("nessun dispositivo di output audio"))?;
        // Rate nativo PipeWire = 48000. Chiedere un rate non-nativo (44100) con
        // buffer non-potenza-di-2 causa misallineamento/underrun (cpal #1250).
        // Usiamo 48000 + buffer 1024 (potenza di 2). I sorgenti sono gia' 48k.
        let out_rate = Arc::new(AtomicU32::new(OUT_RATE));
        let config = cpal::StreamConfig {
            channels: 2,
            sample_rate: cpal::SampleRate(OUT_RATE),
            buffer_size: cpal::BufferSize::Fixed(1024),
        };
        let pos_cb = pos_frames.clone();
        let stream = device.build_output_stream_raw(
            &config,
            cpal::SampleFormat::F32,
            move |data: &mut cpal::Data, _: &cpal::OutputCallbackInfo| {
                if let Some(buf) = data.as_slice_mut::<f32>() {
                    let mut g = ring_cb.lock().unwrap();
                    // Conta solo i campioni REALI drenati dal ring: in pause il
                    // ring e' vuoto (esce silenzio) e la posizione non deve avanzare.
                    let mut real = 0usize;
                    for slot in buf.iter_mut() {
                        *slot = match g.pop_front() {
                            Some(s) => {
                                real += 1;
                                s
                            }
                            None => 0.0,
                        };
                    }
                    // Posizione: i frame reali usciti dal ring (real/2 = frame stereo).
                    pos_cb.fetch_add((real / 2) as u64, Ordering::Relaxed);
                }
            },
            |err| eprintln!("audio: {err}"),
            None,
        )?;
        use cpal::traits::StreamTrait;
        stream.play()?;

        // --- Thread decoder ---
        {
            let ring = ring.clone();
            let pos = pos_frames.clone();
            let vol = volume.clone();
            let evt_tx = evt_tx;
            let out_rate_d = out_rate.clone();
            thread::spawn(move || {
                decoder_loop(&mut cmd_rx, &evt_tx, &ring, &pos, &vol, &out_rate_d);
            });
        }

        Ok(Self {
            cmd_tx,
            pos_frames,
            volume,
            out_rate,
            _stream: NonSend(Some(stream)),
        })
    }

    pub fn send(&self, cmd: DecCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Posizione corrente in secondi.
    pub fn position_secs(&self) -> f64 {
        let r = self.out_rate.load(Ordering::Relaxed) as f64;
        self.pos_frames.load(Ordering::Relaxed) as f64 / r
    }

    pub fn set_volume(&self, v: f64) {
        *self.volume.lock().unwrap() = v;
        self.send(DecCmd::SetVolume { v });
    }

    pub fn set_position_frames(&self, secs: f64) {
        let r = self.out_rate.load(Ordering::Relaxed) as f64;
        self.pos_frames.store((secs * r) as u64, Ordering::Relaxed);
    }
}

fn decoder_loop(
    cmd_rx: &mut tokio::sync::mpsc::UnboundedReceiver<DecCmd>,
    evt_tx: &tokio::sync::mpsc::UnboundedSender<DecEvt>,
    ring: &Arc<Mutex<VecDeque<f32>>>,
    pos: &Arc<AtomicU64>,
    vol: &Arc<Mutex<f64>>,
    out_rate: &Arc<AtomicU32>,
) {
    let mut active: Option<Active> = None;
    let mut playing = false;

    loop {
        // Fase di riempimento: decodifica e riempie il ring.
        if let Some(a) = active.as_mut() {
            if playing {
                fill_once(a, ring, vol);
            }
        }

        // Comandi pendenti (non bloccante).
        let mut load: Option<Track> = None;
        let mut do_stop = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DecCmd::LoadFile { track } => load = Some(track),
                DecCmd::Seek { secs } => {
                    let or = out_rate.load(Ordering::Relaxed) as f64;
                    pos.store((secs * or) as u64, Ordering::Relaxed);
                    // Seek = riapre il brano corrente alla posizione indicata.
                    if let Some(a) = active.as_mut() {
                        let t = a.track.clone();
                        if let Some(na) = open_active(&t, secs, out_rate.load(Ordering::Relaxed)) {
                            *a = na;
                        }
                    }
                }
                DecCmd::SetVolume { v } => *vol.lock().unwrap() = v,
                DecCmd::Play => playing = true,
                DecCmd::Pause => playing = false,
                DecCmd::Stop => {
                    do_stop = true;
                    playing = false;
                }
            }
        }

        if do_stop {
            active = None;
            ring.lock().unwrap().clear();
        }

        // Fine brano?
        if let Some(a) = active.as_mut() {
            if a.eof {
                active = None;
                let _ = evt_tx.send(DecEvt::TrackEnd);
            }
        }

        if let Some(track) = load {
            pos.store(0, Ordering::Relaxed);
            ring.lock().unwrap().clear();
            active = open_active(&track, 0.0, out_rate.load(Ordering::Relaxed));
            playing = true;
        }

        if active.is_none() {
            // Blocca in attesa di un comando.
            match cmd_rx.blocking_recv() {
                Some(DecCmd::LoadFile { track }) => {
                    pos.store(0, Ordering::Relaxed);
                    ring.lock().unwrap().clear();
                    active = open_active(&track, 0.0, out_rate.load(Ordering::Relaxed));
                    playing = true;
                }
                Some(DecCmd::SetVolume { v }) => *vol.lock().unwrap() = v,
                Some(DecCmd::Play) => playing = true,
                Some(DecCmd::Pause) => playing = false,
                Some(DecCmd::Stop) => {
                    active = None;
                    playing = false;
                    ring.lock().unwrap().clear();
                }
                Some(DecCmd::Seek { .. }) => {}
                None => break,
            }
        } else {
            thread::sleep(Duration::from_millis(3));
        }
    }
}

/// Decodifica un batch e lo spinge nel ring (con volume). Segnala EOF.
fn fill_once(
    a: &mut Active,
    ring: &Arc<Mutex<VecDeque<f32>>>,
    vol: &Arc<Mutex<f64>>,
) {
    // Backpressure: se il ring e' mezzo pieno, NON consumare pacchetti (il loop
    // fa sleep). Evita di buttare campioni gia' decodificati (salti audio).
    if ring.lock().unwrap().len() >= MAX_RING / 2 {
        return;
    }
    match a.format.next_packet() {
        Ok(packet) => match a.decoder.decode(&packet) {
            Ok(bufref) => {
                // make_equivalent crea il buffer; convert riempie i campioni f32.
                let mut buf: AudioBuffer<f32> = bufref.make_equivalent::<f32>();
                bufref.convert(&mut buf);
                if buf_frames(&buf) == 0 {
                    a.eof = true;
                    return;
                }
                let stereo = frame_to_stereo(&buf);
                let resampled = a.resample.process(&stereo);
                let v = *vol.lock().unwrap();
                let mut g = ring.lock().unwrap();
                for s in resampled {
                    g.push_back(s * v as f32);
                }
                drop(g);
            }
            Err(e) if is_eof(&e) => a.eof = true,
            Err(_) => a.eof = true,
        },
        Err(e) if is_eof(&e) => a.eof = true,
        Err(_) => a.eof = true,
    }
}
