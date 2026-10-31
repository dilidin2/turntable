//! Playlist: parse M3U/M3U8 e scansione ricorsiva di cartelle.

use std::path::Path;

use crate::meta::{is_audio_file, is_m3u, load_meta, Track};

/// Carica un URI/path in una lista di brani:
/// - M3U/M3U8 → i brani della playlist
/// - cartella → scansione ricorsiva
/// - file audio → singolo brano
///
/// I path relativi degli M3U si risolvono rispetto alla cartella dell'M3U.
pub fn load_source(path: &Path) -> Vec<Track> {
    if !path.exists() {
        return Vec::new();
    }
    if path.is_dir() {
        scan_dir(path)
    } else if is_m3u(path) {
        parse_m3u(path)
    } else if is_audio_file(path) {
        vec![load_meta(path)]
    } else {
        Vec::new()
    }
}

/// Scansione ricorsiva di una cartella, ordinata per nome.
fn scan_dir(dir: &Path) -> Vec<Track> {
    let mut out = Vec::new();
    collect(dir, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn collect(dir: &Path, out: &mut Vec<Track>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect(&p, out);
        } else if is_audio_file(&p) {
            out.push(load_meta(&p));
        }
    }
}

/// Parse di un file M3U/M3U8. Ignora commenti (#) e righe vuote.
fn parse_m3u(file: &Path) -> Vec<Track> {
    let base = file.parent().unwrap_or(Path::new("."));
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Rimuovi eventuali URL "file://"
        let line = line.strip_prefix("file://").unwrap_or(line);
        let p = Path::new(line);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            base.join(p)
        };
        if is_audio_file(&abs) {
            out.push(load_meta(&abs));
        }
    }
    out
}
