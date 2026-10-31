//! Entry point: avvia il player, possiede il nome MPRIS, gestisce il loop eventi.

use std::sync::Arc;

use tokio::sync::mpsc;

use player::core::{Change, Player};
use player::mpris::{emit_change, MprisPlayer, MprisRoot};

const BUS_NAME: &str = "org.mpris.MediaPlayer2.player";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Motore + stato.
    let (player, mut evt_rx) = Player::new()?;
    let player = Arc::new(player);

    // Canale notifiche: i metodi MPRIS spingono i cambi qui, il loop li emette.
    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel::<Change>();

    // Connessione al session bus.
    let connection = zbus::Connection::session().await?;

    // Single instance: se il nome e' gia' posseduto, esci silenziosamente.
    if connection.request_name(BUS_NAME).await.is_err() {
        eprintln!("player: gia' un'istanza attiva");
        return Ok(());
    }

    // Oggetti MPRIS.
    let root = MprisRoot::new(player.clone());
    let mpris = MprisPlayer::new(player.clone(), notify_tx);

    connection
        .object_server()
        .at(OBJECT_PATH, root)
        .await?;
    connection
        .object_server()
        .at(OBJECT_PATH, mpris)
        .await?;

    eprintln!("player: pronto su {BUS_NAME}");

    // Loop eventi: fine brano + notifiche.
    loop {
        tokio::select! {
            Some(_) = evt_rx.recv() => {
                // Fine brano: avanza secondo le regole (shuffle/repeat).
                let ch = player.advance_next();
                emit_change(&connection, player.as_ref(), ch).await;
            }
            Some(ch) = notify_rx.recv() => {
                emit_change(&connection, player.as_ref(), ch).await;
            }
        }
    }
}
