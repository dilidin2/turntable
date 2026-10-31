//! Metadati e copertine (lofty). Indipendente da D-Bus.

use std::path::{Path, PathBuf};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::tag::ItemKey;

/// Un brano in coda.
#[derive(Clone, Debug)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Durata in secondi.
    pub duration: f64,
    /// Copertina embedded (APIC / FLAC picture / OGG), se presente.
    pub art: Option<Vec<u8>>,
}

impl Track {
    /// Nome file senza estensione, come fallback del titolo.
    fn stem(path: &Path) -> String {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "brano".into())
    }
}

/// Legge metadati + copertina da un file audio.
/// In caso di errore restituisce un Track con solo il titolo (nome file).
pub fn load_meta(path: &Path) -> Track {
    let mut track = Track {
        path: path.to_path_buf(),
        title: Track::stem(path),
        artist: String::new(),
        album: String::new(),
        duration: 0.0,
        art: None,
    };

    if let Ok(file) = lofty::read_from_path(path) {
        track.duration = file.properties().duration().as_secs_f64();
        if let Some(tag) = file.tags().first() {
            let get_text = |key: &ItemKey| {
                tag.get(key)
                    .and_then(|it| it.value().text())
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
            };
            track.title = get_text(&ItemKey::TrackTitle).unwrap_or_else(|| track.title.clone());
            track.artist = get_text(&ItemKey::TrackArtist).unwrap_or_default();
            track.album = get_text(&ItemKey::AlbumTitle).unwrap_or_default();
            // Copertina: preferita la cover frontale, altrimenti la prima.
            let pick = tag
                .get_picture_type(PictureType::CoverFront)
                .or_else(|| tag.pictures().first());
            if let Some(p) = pick {
                track.art = Some(p.data().to_vec());
            }
        }
    }

    track
}

/// Scrive la copertina corrente in un file stabile e restituisce `file://`.
/// Usato per `mpris:artUrl`. Restituisce None se il brano non ha arte.
pub fn write_art_to_file(track: &Track) -> Option<String> {
    let data = track.art.as_ref()?;
    let out = std::env::temp_dir().join("player_current_art.bin");
    if std::fs::write(&out, data).is_ok() {
        Some(format!("file://{}", out.to_string_lossy()))
    } else {
        None
    }
}

/// Estensioni audio gestite (per la scansione delle cartelle).
pub const AUDIO_EXTS: &[&str] = &["mp3", "flac", "ogg", "oga", "opus", "wav", "m4a", "aac"];

pub fn is_audio_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .map(|e| AUDIO_EXTS.contains(&e.to_string_lossy().to_lowercase().as_str()))
            .unwrap_or(false)
}

/// Verifica se un path è una playlist M3U/M3U8.
pub fn is_m3u(path: &Path) -> bool {
    path.extension()
        .map(|e| {
            let e = e.to_string_lossy().to_lowercase();
            e == "m3u" || e == "m3u8"
        })
        .unwrap_or(false)
}
