//! Interfacce MPRIS su D-Bus (zbus). Traduce D-Bus -> Player e Player -> segnali.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use zbus::fdo::Error;
use zbus::zvariant::{ObjectPath, Value};
use zbus::Connection;

use crate::core::{Change, Player, Repeat, Status};
use crate::meta::{load_meta, write_art_to_file, Track};

const IFC_PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// Costruisce un `a{sv}` da una lista di coppie (nome, valore owned).
fn a_sv(pairs: Vec<(&'static str, Value<'static>)>) -> Value<'static> {
    let mut map: HashMap<String, Value<'static>> = HashMap::new();
    for (k, v) in pairs {
        map.insert(k.to_string(), v);
    }
    Value::from(map)
}

/// Costruisce il `Metadata` (a{sv}) dal brano corrente.
fn metadata_value(track: &Option<Track>) -> Value<'static> {
    let mut map: HashMap<String, Value<'static>> = HashMap::new();
    if let Some(t) = track {
        map.insert("xesam:title".into(), Value::from(t.title.clone()));
        map.insert("xesam:artist".into(), Value::from(vec![t.artist.clone()]));
        map.insert("xesam:album".into(), Value::from(t.album.clone()));
        map.insert("mpris:length".into(), Value::from((t.duration * 1_000_000.0) as i64));
        if let Some(url) = write_art_to_file(t) {
            map.insert("mpris:artUrl".into(), Value::from(url));
        }
    }
    Value::from(map)
}

/// Decodifica le sequenze percent `%%XX` (es. gli spazi `%%20`) che i
/// client MPRIS inviano nelle URI `file://`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(h) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let s = uri.strip_prefix("file://").unwrap_or(uri);
    Some(PathBuf::from(percent_decode(s)))
}

/// Esegue `load_source` (I/O pesante: parse M3U + metadati lofty) su un thread
/// std, per non bloccare l'executor di zbus. Restituisce il `Change`.
async fn blocking_load_source(player: Arc<Player>, path: PathBuf) -> Result<Change, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let ch = player.load_source(path);
        let _ = tx.send(ch);
    });
    rx.await.map_err(|e| e.to_string())
}

/// Costruisce la coda da una lista di URI (I/O pesante: metadati lofty)
/// su un thread std. Restituisce il `Change`.
async fn blocking_load_uris(
    player: Arc<Player>,
    uris: Vec<String>,
) -> Result<Change, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let mut tracks = Vec::new();
        for u in &uris {
            if let Some(p) = uri_to_path(u) {
                if p.exists() {
                    tracks.push(load_meta(&p));
                }
            }
        }
        let ch = player.load_tracks(tracks);
        let _ = tx.send(ch);
    });
    rx.await.map_err(|e| e.to_string())
}

/// Interfaccia `org.mpris.MediaPlayer2.Player`.
pub struct MprisPlayer {
    pub player: Arc<Player>,
    notify_tx: tokio::sync::mpsc::UnboundedSender<Change>,
}

impl MprisPlayer {
    pub fn new(
        player: Arc<Player>,
        notify_tx: tokio::sync::mpsc::UnboundedSender<Change>,
    ) -> Self {
        Self { player, notify_tx }
    }

    pub fn metadata_value(&self) -> Value<'static> {
        metadata_value(&self.player.current_track())
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayer {
    // ---- Proprietà ----

    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.player.status().to_mpris().to_string()
    }
    #[zbus(property)]
    fn set_playback_status(&self, _v: String) {
        // read-only di fatto: cambia con Play/Pause/Stop
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        (self.player.position_secs() * 1_000_000.0) as i64
    }

    #[zbus(property)]
    fn metadata(&self) -> Value<'static> {
        self.metadata_value()
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.player.volume()
    }
    #[zbus(property)]
    fn set_volume(&self, v: f64) {
        let _ = self.player.set_volume(v);
        let _ = self.notify_tx.send(Change::VolumeChanged);
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.player.shuffle()
    }
    #[zbus(property)]
    fn set_shuffle(&self, b: bool) {
        let _ = self.player.set_shuffle(b);
        let _ = self.notify_tx.send(Change::ShuffleChanged);
    }

    #[zbus(property)]
    fn loop_status(&self) -> String {
        self.player.repeat().to_mpris().to_string()
    }
    #[zbus(property)]
    fn set_loop_status(&self, s: String) {
        let _ = self.player.set_repeat(Repeat::from_mpris(&s));
        let _ = self.notify_tx.send(Change::RepeatChanged);
    }

    #[zbus(property)]
    fn playback_rates(&self) -> Vec<String> {
        vec!["1.0".into()]
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.player.can_go_next()
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.player.can_go_prev()
    }

    // ---- Metodi ----

    async fn play(&self) -> Result<(), Error> {
        let ch = self.player.play();
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn pause(&self) -> Result<(), Error> {
        let ch = self.player.pause();
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn play_pause(&self) -> Result<(), Error> {
        let ch = self.player.play_pause();
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn stop(&self) -> Result<(), Error> {
        let ch = self.player.stop();
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn next(&self) -> Result<(), Error> {
        let ch = self.player.advance_next();
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn previous(&self) -> Result<(), Error> {
        let ch = self.player.prev();
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn seek(&self, position: i64) -> Result<(), Error> {
        // MPRIS: Seek e' un offset RELATIVO in microsecondi rispetto alla
        // posizione corrente (i client inviano target - corrente).
        let cur = self.player.position_secs();
        let secs = (cur + position as f64 / 1_000_000.0).max(0.0);
        let ch = self.player.seek(secs);
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn set_position(
        &self,
        _track_id: ObjectPath<'_>,
        position: i64,
    ) -> Result<(), Error> {
        let secs = position as f64 / 1_000_000.0;
        let ch = self.player.seek(secs);
        let _ = self.notify_tx.send(ch);
        Ok(())
    }

    async fn open_uri(&self, uri: String) -> Result<(), Error> {
        if let Some(path) = uri_to_path(&uri) {
            // I/O pesante (parse M3U + metadati lofty): lo faccio su un thread std
            // per non bloccare l'executor di zbus (che non e' un runtime tokio).
            let ch = blocking_load_source(self.player.clone(), path)
                .await
                .map_err(|e| Error::from(zbus::fdo::Error::Failed(e.to_string())))?;
            let _ = self.notify_tx.send(ch);
        }
        Ok(())
    }

    async fn open_uris(&self, uris: Vec<String>) -> Result<(), Error> {
        if !uris.is_empty() {
            let ch = blocking_load_uris(self.player.clone(), uris)
                .await
                .map_err(|e| Error::from(zbus::fdo::Error::Failed(e.to_string())))?;
            let _ = self.notify_tx.send(ch);
        }
        Ok(())
    }

}

const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

async fn emit_changed(conn: &Connection, changed: Value<'static>) {
    let body = (IFC_PLAYER.to_string(), changed, vec![] as Vec<String>);
    let _ = conn.emit_signal(None::<String>, OBJECT_PATH, IFC_PLAYER, "PropertiesChanged", &body).await;
}

async fn emit_track_changed(conn: &Connection) {
    let body = (ObjectPath::from_str_unchecked("/"),);
    let _ = conn.emit_signal(None::<String>, OBJECT_PATH, IFC_PLAYER, "TrackChanged", &body).await;
}

async fn emit_seeked(conn: &Connection, pos: i64) {
    let _ = conn.emit_signal(None::<String>, OBJECT_PATH, IFC_PLAYER, "Seeked", &(pos,)).await;
}

/// Emette i segnali MPRIS corretti per un cambio di stato.
pub async fn emit_change(conn: &Connection, player: &Player, ch: Change) {
    match ch {
        Change::TrackChanged => {
            let meta = metadata_value(&player.current_track());
            let changed = a_sv(vec![
                ("Metadata", meta),
                ("Position", Value::from(0i64)),
                (
                    "PlaybackStatus",
                    Value::from(player.status().to_mpris().to_string()),
                ),
            ]);
            emit_changed(conn, changed).await;
            emit_track_changed(conn).await;
        }
        Change::StatusChanged => {
            let changed = a_sv(vec![(
                "PlaybackStatus",
                Value::from(player.status().to_mpris().to_string()),
            )]);
            emit_changed(conn, changed).await;
        }
        Change::VolumeChanged => {
            let changed = a_sv(vec![("Volume", Value::from(player.volume()))]);
            emit_changed(conn, changed).await;
        }
        Change::ShuffleChanged => {
            let changed = a_sv(vec![("Shuffle", Value::from(player.shuffle()))]);
            emit_changed(conn, changed).await;
        }
        Change::RepeatChanged => {
            let changed = a_sv(vec![(
                "LoopStatus",
                Value::from(player.repeat().to_mpris().to_string()),
            )]);
            emit_changed(conn, changed).await;
        }
        Change::Seeked => {
            let pos = (player.position_secs() * 1_000_000.0) as i64;
            let changed = a_sv(vec![("Position", Value::from(pos))]);
            emit_changed(conn, changed).await;
            emit_seeked(conn, pos).await;
        }
        Change::None => {}
    }
}

/// Interfaccia root `org.mpris.MediaPlayer2`.
pub struct MprisRoot {
    pub player: Arc<Player>,
}

impl MprisRoot {
    pub fn new(player: Arc<Player>) -> Self {
        Self { player }
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl MprisRoot {
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.player.volume()
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.player.can_go_next()
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.player.can_go_prev()
    }

    #[zbus(property)]
    fn playing(&self) -> bool {
        self.player.status() == Status::Playing
    }

    #[zbus(property)]
    fn playback_speed(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn max_rate(&self) -> f64 {
        1.0
    }

    async fn raise(&self) {}

    async fn activate(&self) {}

    async fn set_desktop_entry(&self, _entry: String) {}

    async fn close(&self) {}
}
