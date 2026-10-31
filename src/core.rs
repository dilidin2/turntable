//! Stato di riproduzione + logica next/shuffle/repeat + Player (glue con Engine).

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Result;
use rand::Rng;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::engine::{DecCmd, DecEvt, Engine};
use crate::meta::Track;
use crate::playlist;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Stopped,
    Playing,
    Paused,
}

impl Status {
    pub fn to_mpris(self) -> &'static str {
        match self {
            Status::Stopped => "Stopped",
            Status::Playing => "Playing",
            Status::Paused => "Paused",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Off,
    Track,
    Playlist,
}

impl Repeat {
    pub fn from_mpris(s: &str) -> Self {
        // Case-insensitive: i client MPRIS inviano sia "Track" sia "track".
        // Accetta anche "continuous" (termine standard MPRIS per loop playlist).
        match s.to_ascii_lowercase().as_str() {
            "track" => Repeat::Track,
            "playlist" | "continuous" => Repeat::Playlist,
            _ => Repeat::Off,
        }
    }
    pub fn to_mpris(self) -> &'static str {
        match self {
            Repeat::Off => "none",
            Repeat::Track => "track",
            Repeat::Playlist => "playlist",
        }
    }
}

/// Cosa e' cambiato (per far emettere i segnali giusti al layer MPRIS).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Change {
    None,
    TrackChanged,
    StatusChanged,
    VolumeChanged,
    ShuffleChanged,
    RepeatChanged,
    Seeked,
}

pub struct Inner {
    pub queue: Vec<Track>,
    pub current: usize,
    pub status: Status,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub volume: f64,
}

/// Indice del prossimo brano (None = fine lista, stop).
fn next_index(g: &Inner) -> Option<usize> {
    let n = g.queue.len();
    if n == 0 {
        return None;
    }
    match g.repeat {
        Repeat::Track => Some(g.current),
        _ => {
            if g.shuffle {
                if n == 1 {
                    Some(0)
                } else {
                    let mut idx = rand::thread_rng().gen_range(0..n);
                    while idx == g.current {
                        idx = rand::thread_rng().gen_range(0..n);
                    }
                    Some(idx)
                }
            } else {
                let i = g.current + 1;
                if i < n {
                    Some(i)
                } else if g.repeat == Repeat::Playlist {
                    Some(0)
                } else {
                    None
                }
            }
        }
    }
}

/// Indice del brano precedente.
fn prev_index(g: &Inner) -> usize {
    let n = g.queue.len();
    if n <= 1 {
        return 0;
    }
    if g.shuffle {
        let mut idx = rand::thread_rng().gen_range(0..n);
        while idx == g.current {
            idx = rand::thread_rng().gen_range(0..n);
        }
        idx
    } else if g.current > 0 {
        g.current - 1
    } else if g.repeat == Repeat::Playlist {
        n - 1
    } else {
        0
    }
}

pub struct Player {
    inner: Mutex<Inner>,
    engine: Engine,
}

impl Player {
    pub fn new() -> Result<(Player, UnboundedReceiver<DecEvt>)> {
        let (evt_tx, evt_rx) = tokio::sync::mpsc::unbounded_channel::<DecEvt>();
        let engine = Engine::new(evt_tx)?;
        let player = Player {
            inner: Mutex::new(Inner {
                queue: Vec::new(),
                current: 0,
                status: Status::Stopped,
                shuffle: false,
                repeat: Repeat::Off,
                volume: 1.0,
            }),
            engine,
        };
        Ok((player, evt_rx))
    }

    // ---- Comandi ----

    pub fn load_source(&self, path: PathBuf) -> Change {
        let tracks = playlist::load_source(&path);
        if tracks.is_empty() {
            return Change::None;
        }
        let first = tracks[0].clone();
        {
            let mut g = self.inner.lock().unwrap();
            g.queue = tracks;
            g.current = 0;
            g.status = Status::Playing;
        }
        self.engine.send(DecCmd::LoadFile { track: first });
        Change::TrackChanged
    }

    pub fn play(&self) -> Change {
        let empty = self.inner.lock().unwrap().queue.is_empty();
        if empty {
            return Change::None;
        }
        self.inner.lock().unwrap().status = Status::Playing;
        self.engine.send(DecCmd::Play);
        Change::StatusChanged
    }

    pub fn pause(&self) -> Change {
        self.inner.lock().unwrap().status = Status::Paused;
        self.engine.send(DecCmd::Pause);
        Change::StatusChanged
    }

    pub fn play_pause(&self) -> Change {
        match self.inner.lock().unwrap().status {
            Status::Playing => self.pause(),
            _ => self.play(),
        }
    }

    pub fn stop(&self) -> Change {
        self.inner.lock().unwrap().status = Status::Stopped;
        self.engine.send(DecCmd::Stop);
        Change::StatusChanged
    }

    /// Avanza al brano successivo (usato sia da Next manuale sia da fine brano).
    pub fn advance_next(&self) -> Change {
        let track = {
            let mut g = self.inner.lock().unwrap();
            match next_index(&g) {
                Some(i) => {
                    g.current = i;
                    g.status = Status::Playing;
                    g.queue[i].clone()
                }
                None => {
                    g.status = Status::Stopped;
                    self.engine.send(DecCmd::Stop);
                    return Change::StatusChanged;
                }
            }
        };
        self.engine.send(DecCmd::LoadFile { track });
        Change::TrackChanged
    }

    pub fn prev(&self) -> Change {
        // Se ci siamo oltre 3s, fa rewind a 0.
        if self.engine.position_secs() > 3.0 {
            self.engine.send(DecCmd::Seek { secs: 0.0 });
            self.engine.set_position_frames(0.0);
            return Change::None;
        }
        let track = {
            let mut g = self.inner.lock().unwrap();
            if g.queue.is_empty() {
                return Change::None;
            }
            let i = prev_index(&g);
            g.current = i;
            g.status = Status::Playing;
            g.queue[i].clone()
        };
        self.engine.send(DecCmd::LoadFile { track });
        Change::TrackChanged
    }

    pub fn seek(&self, secs: f64) -> Change {
        // Clampa a [0, durata] per evitare seek oltre la fine del brano.
        let secs = {
            let g = self.inner.lock().unwrap();
            let dur = g.queue.get(g.current).map(|t| t.duration).unwrap_or(0.0);
            secs.clamp(0.0, dur)
        };
        self.engine.send(DecCmd::Seek { secs });
        self.engine.set_position_frames(secs);
        Change::Seeked
    }

    pub fn set_volume(&self, v: f64) -> Change {
        let v = v.clamp(0.0, 1.0);
        self.inner.lock().unwrap().volume = v;
        self.engine.set_volume(v);
        Change::VolumeChanged
    }

    pub fn set_shuffle(&self, b: bool) -> Change {
        self.inner.lock().unwrap().shuffle = b;
        Change::ShuffleChanged
    }

    pub fn set_repeat(&self, r: Repeat) -> Change {
        self.inner.lock().unwrap().repeat = r;
        Change::RepeatChanged
    }

    // ---- Letture ----

    pub fn position_secs(&self) -> f64 {
        self.engine.position_secs()
    }
    pub fn current_track(&self) -> Option<Track> {
        let g = self.inner.lock().unwrap();
        g.queue.get(g.current).cloned()
    }
    pub fn volume(&self) -> f64 {
        self.inner.lock().unwrap().volume
    }
    pub fn shuffle(&self) -> bool {
        self.inner.lock().unwrap().shuffle
    }
    pub fn repeat(&self) -> Repeat {
        self.inner.lock().unwrap().repeat
    }
    pub fn status(&self) -> Status {
        self.inner.lock().unwrap().status
    }
    pub fn queue_len(&self) -> usize {
        self.inner.lock().unwrap().queue.len()
    }
    pub fn current_index(&self) -> usize {
        self.inner.lock().unwrap().current
    }
    pub fn can_go_next(&self) -> bool {
        let g = self.inner.lock().unwrap();
        g.queue.len() > 1 || g.repeat == Repeat::Track
    }
    pub fn can_go_prev(&self) -> bool {
        self.inner.lock().unwrap().queue.len() > 0
    }
}
