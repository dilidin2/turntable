# turntable

Player musicale headless con interfaccia MPRIS su D-Bus.

`turntable` è un backend musicale dedicato a [Quickshell](https://quickshell.org/):
espone l'interfaccia standard MPRIS (`org.mpris.MediaPlayer2`) sul session bus
D-Bus, così il widget musicale di Quickshell può riprodurre musica, controllare
la riproduzione, mostrare metadati, ecc.

## Caratteristiche

- **MPRIS completo**: play/pause/stop/next/previous/seek, volume, posizione, lunghezza
- **Coda di riproduzione** con ripetizione (off / track / playlist) e shuffle
- **Decoder audio** via [symphonia](https://github.com/pemisthal/symphonia): MP3, FLAC, OGG, AAC/M4A
- **Metadati embedded** via [lofty](https://github.com/tauricatron/lofty): titolo, artista, album, lunghezza
- **Artwork** estratto dai tag e servito via `mpris:artUrl`
- **Riproduzione audio** via [cpal](https://github.com/RustAudio/cpal)
- **Service systemd** per l'avvio automatico

## Note su Quickshell

`turntable` è pensato specificamente per Quickshell. In particolare la
proprietà `LoopStatus` restituisce i valori che si aspetta
`DBusDataTransform<MprisLoopState>::fromWire` di Quickshell — cioè
`"None"` / `"Track"` / `"Playlist"` — anziché i valori standard MPRIS
(`"off"` / `"track"` / `"playlist"`). È intenzionale: Quickshell è l'unico
client di turntable.

Il shell Quickshell che integra turntable vive nel repo
[dotfiles](https://github.com/dilidin2/dotfiles).

## Requisiti

- Rust (cargo)
- PortAudio (per la riproduzione audio): `sudo zypper install portaudio-devel`

## Build

```bash
cargo build --release
```

Il binary finisce in `target/release/turntable`.

## Installazione

```bash
# installa il binary
install target/release/turntable ~/.local/bin/turntable

# installa il service systemd
cp systemd/turntable.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now turntable
```

## Uso

Avviato, `turntable` espone `org.mpris.MediaPlayer2.turntable` sul session bus.
Puoi controllarlo da terminale:

```bash
# leggi lo stato di riproduzione
gdbus call --session --dest org.mpris.MediaPlayer2.turntable \
  --object-path /org/mpris/MediaPlayer2 \
  --method org.freedesktop.DBus.Properties.Get \
  org.mpris.MediaPlayer2.Player PlaybackStatus

# riproduci / metti in pausa
gdbus call --session --dest org.mpris.MediaPlayer2.turntable \
  --object-path /org/mpris/MediaPlayer2 \
  --method org.mpris.MediaPlayer2.Player.Play
```

Oppure semplicemente dal widget musicale di Quickshell.

## Struttura

| File | Descrizione |
|------|-------------|
| `src/main.rs` | Punto di ingresso, setup bus D-Bus e service |
| `src/core.rs` | Logica di riproduzione, coda e stato |
| `src/engine.rs` | Motore audio (cpal) |
| `src/meta.rs` | Estrazione metadati e artwork (lofty) |
| `src/mpris.rs` | Interfaccia MPRIS su D-Bus (zbus) |
| `src/playlist.rs` | Gestione coda/playlist |

## Licenza

MIT — vedi [LICENSE](LICENSE).